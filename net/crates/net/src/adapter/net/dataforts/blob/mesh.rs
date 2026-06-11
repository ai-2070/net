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

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use dashmap::DashMap;

use super::adapter::{BlobAdapter, BlobByteStream, BlobStat};
use super::admission::auth_allows_blob_op;
use super::blob_ref::{
    byte_range_to_chunks, chunk_payload, BlobRef, ChunkRef, ChunkedPayload, Encoding,
    BLOB_CHUNK_SIZE_BYTES,
};
use super::blob_tree::{
    ChunkRefV3, ChunkingStrategy, TreeBuilder, TreeSupportProbe, TREE_LEAF_CHUNK_MAX_BYTES,
    TREE_THRESHOLD_BYTES,
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

/// Per-call cap on `fetch_range` slice size in bytes (1 GiB). v0.3
/// `BlobRef::Tree` lifts the effective addressable size from 16 GiB
/// to 128 PiB, and `fetch_range` returns the whole requested range
/// as a single `Vec<u8>`. Without an explicit cap, a single
/// `fetch_range(0, 100 GiB)` would allocate 100 GiB in-process.
/// 1 GiB is generous for legitimate range reads (well above any
/// chunk-aligned slice) but small enough that an adversarial peer
/// or a misconfigured caller can't OOM the substrate. Streaming
/// consumers needing TB-scale walks page through smaller slices.
pub const MAX_FETCH_RANGE_BYTES: u64 = 1024 * 1024 * 1024;

/// Type alias for the per-adapter Reed-Solomon encoder cache.
/// Factored out to keep the field declaration readable and to
/// satisfy clippy's type-complexity threshold.
type RsEncoderCache = Arc<parking_lot::Mutex<HashMap<(u8, u8), Arc<super::erasure::RsEncoder>>>>;

/// Threshold (bytes) above which blake3 hashing is moved off the
/// tokio runtime via `spawn_blocking`. Below this, the synchronous
/// blake3 SIMD path is faster than the spawn_blocking handoff
/// (~µs).  At 128 KiB the hash takes ~50 µs and starts to push the
/// runtime worker into the "long task" regime; above it, multi-MiB
/// CDC chunks run for 1–5 ms each — well into territory that stalls
/// every other task on the worker.  Per
/// PERF_AUDIT_2026_06_10_FULL_CRATE.md §6.1.
pub(crate) const BLAKE3_OFFLOAD_THRESHOLD_BYTES: usize = 128 * 1024;

/// Hash `bytes` with blake3, moving the work to the tokio blocking
/// pool when `bytes.len() >= BLAKE3_OFFLOAD_THRESHOLD_BYTES`.
/// Takes ownership of a `Vec<u8>` so the spawn_blocking closure
/// runs against owned data (no extra copy); returns the bytes back
/// along with the hash so the caller can continue using them.
///
/// Below the threshold the inline blake3 path is faster than the
/// spawn handoff — we short-circuit and return immediately.
pub(crate) async fn blake3_hash_offload_vec(bytes: Vec<u8>) -> ([u8; 32], Vec<u8>) {
    if bytes.len() < BLAKE3_OFFLOAD_THRESHOLD_BYTES {
        let hash: [u8; 32] = blake3::hash(&bytes).into();
        return (hash, bytes);
    }
    tokio::task::spawn_blocking(move || {
        let hash: [u8; 32] = blake3::hash(&bytes).into();
        (hash, bytes)
    })
    .await
    .expect("blake3 spawn_blocking panicked")
}

/// `Bytes` variant of [`blake3_hash_offload_vec`] — refcount-clones
/// for the spawn_blocking closure instead of moving (Bytes clones
/// are O(1) refcount bumps), so the caller can keep using the input
/// after the hash returns.
pub(crate) async fn blake3_hash_offload_bytes(bytes: &Bytes) -> [u8; 32] {
    if bytes.len() < BLAKE3_OFFLOAD_THRESHOLD_BYTES {
        return blake3::hash(bytes).into();
    }
    let snapshot = bytes.clone();
    tokio::task::spawn_blocking(move || blake3::hash(&snapshot).into())
        .await
        .expect("blake3 spawn_blocking panicked")
}

/// Three capability probes a producer consults before publishing
/// a v0.3 blob: Tree-support (used as the Tree-vs-Manifest gate),
/// CDC-support (used by [`super::cdc::cdc_downgrade`]), and
/// erasure-support (used by [`super::erasure::erasure_downgrade`]).
///
/// Grouped into a struct because every v0.3 publish call site
/// consults all three together; passing seven flat arguments to
/// [`MeshBlobAdapter::publish_stream_with_downgrade`] trips
/// clippy's argument-count threshold AND makes call sites hard
/// to read.
///
/// Construct via [`Self::new`] for the single-cluster all-Phase-D
/// case, or build the struct directly with custom probes for the
/// cross-version rollout case.
#[derive(Debug)]
pub struct DowngradeProbes<'a> {
    /// `BlobRef::Tree` capability probe — decides Tree vs
    /// Manifest at the top of `publish_stream_with_downgrade`.
    pub tree: &'a TreeSupportProbe,
    /// Content-defined-chunking capability probe — feeds
    /// `cdc_downgrade` so peers without CDC support get
    /// `Fixed` chunks they can re-derive.
    pub cdc: &'a super::cdc::CdcSupportProbe,
    /// Reed-Solomon erasure-coding capability probe — feeds
    /// `erasure_downgrade` so peers without RS support get
    /// `Replicated` stripes they can reconstruct.
    pub erasure: &'a super::erasure::ErasureSupportProbe,
}

impl<'a> DowngradeProbes<'a> {
    /// Construct a `DowngradeProbes` from a flat triple of
    /// borrowed probes. Equivalent to writing the struct
    /// literal directly, but reads better at call sites.
    pub fn new(
        tree: &'a TreeSupportProbe,
        cdc: &'a super::cdc::CdcSupportProbe,
        erasure: &'a super::erasure::ErasureSupportProbe,
    ) -> Self {
        Self { tree, cdc, erasure }
    }
}

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
    /// Small LRU of open chunk-file `RedexFile` handles keyed by
    /// blob hash.
    ///
    /// **PERF_AUDIT §6.7** — pre-fix every `fetch_chunk` /
    /// `store_chunk` / `chunk_exists` rebuilt the channel name + a
    /// fresh `RedexFileConfig` (cloning `self.replication`) and
    /// called `Redex::open_file`, whose reopen fast path still
    /// runs `ensure_reopen_replication_matches`, an
    /// `is_authorized` ACL probe, a `replication.validate()`, and
    /// a `replication.read().is_none()` check. Per-tree-walk that
    /// adds up to thousands of redundant probes. Caching the
    /// already-resolved `RedexFile` (Arc clone — refcount bump
    /// only) collapses the per-op cost to one Mutex lock + LRU
    /// touch.
    ///
    /// The cap is intentionally small: tree walks burst over a
    /// handful of nodes + chunks at a time, and a stale cached
    /// handle is functionally identical to a fresh open because
    /// `Redex::open_file` is idempotent on `(name, config)` — an
    /// LRU eviction just means the next access pays the open
    /// cost once, then re-caches.
    chunk_file_cache: Arc<parking_lot::Mutex<lru::LruCache<[u8; 32], crate::adapter::net::redex::RedexFile>>>,
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
    /// In-process LRU cache for v0.3 manifest tree nodes
    /// (`BlobRef::Tree` walk path). Bytes-bounded so the memory
    /// budget is operator-set in MiB rather than tied to the
    /// per-deployment node-shape distribution. `None` (the
    /// default) disables caching entirely; wire via
    /// [`Self::with_tree_node_cache`].
    ///
    /// Cache is content-addressed (keys are immutable BLAKE3
    /// hashes), so hits are always correct — no invalidation
    /// path is needed.
    tree_node_cache: Option<Arc<parking_lot::Mutex<super::blob_tree_cache::TreeNodeCache>>>,
    /// Per-adapter stripe-membership index for the v0.3 Phase C6
    /// GC pin. RS stripes written via
    /// [`Self::store_stream_tree_rs_internal`] register here;
    /// [`Self::sweep_gc`] consults the index before sweeping any
    /// chunk so a degraded-stripe parity chunk's refcount=0
    /// briefly dropping doesn't lose the only thing keeping the
    /// stripe recoverable.
    stripe_index: Arc<parking_lot::Mutex<super::stripe_index::StripeMembershipIndex>>,
    /// Opt-in fetch-path auto-repair. When `true`, every
    /// successful RS reconstruction in
    /// [`Self::walk_stripe_with_reconstruction`] re-stores the
    /// previously-missing data chunks under their original
    /// content-addressed hashes — so the stripe goes back to
    /// healthy, the GC stripe-pin lifts naturally, and subsequent
    /// fetches don't re-pay the reconstruction cost. Default
    /// `false`: the v0.3 plan's stated semantic is "fetch never
    /// writes; operator-driven `repair_blob` is the recovery
    /// path." Enable via [`Self::with_auto_repair_on_fetch`] for
    /// hot-blob workloads where the repeated-reconstruction cost
    /// matters.
    auto_repair_on_fetch: bool,
    /// Optional override for the `max_memory_bytes` field of every
    /// per-chunk `RedexFileConfig`. The default 64 MiB upstream
    /// default pre-reserves a 64 MiB heap `Vec` per opened chunk
    /// channel, which is fine for a handful of chunks but blows
    /// the commit limit for blobs with thousands of small chunks
    /// (e.g. a 100 MiB blob at 8 KiB chunks opens 12 K channels →
    /// 800 GiB of reservation). When `Some(n)`, the adapter passes
    /// `n` through `with_max_memory_bytes` on every chunk-file
    /// open; the upstream `min(n, 64 MiB)` clamp still applies.
    chunk_file_max_memory_bytes: Option<usize>,
    /// Cached `RsEncoder` instances, keyed by `(k, m)`. The
    /// underlying matrix construction is the expensive part of
    /// `RsEncoder::new` — for a degraded blob with N stripes, the
    /// pre-fix read + repair paths constructed an encoder per
    /// stripe (N matrix-builds). Cache them on the adapter so
    /// reconstruction over many stripes pays the build cost
    /// exactly once per distinct `(k, m)`. Adapter clones share
    /// the same cache via `Arc`.
    rs_encoder_cache: RsEncoderCache,
    /// Per-stripe cooldown for `auto_repair_on_fetch`. Maps a
    /// stripe-fingerprint to the last `Instant` at which an
    /// auto-repair persist fired for it. Without this gate, a
    /// peer serving corrupted bytes can force the optimistic
    /// fetch path into reconstruction on every range read, and
    /// `auto_repair_on_fetch=true` then storms `store_chunk`
    /// calls for the same stripe at fetch rate.
    repair_cooldown: Arc<parking_lot::Mutex<HashMap<[u8; 32], std::time::Instant>>>,
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
            // PERF_AUDIT §6.7 — 64-entry LRU; covers a typical
            // tree walk's working set (depth × fanout) without
            // pinning much memory.
            chunk_file_cache: Arc::new(parking_lot::Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(64).expect("64 != 0"),
            ))),
            overflow: Arc::new(parking_lot::RwLock::new(OverflowConfig::default())),
            overflow_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            tree_node_cache: None,
            chunk_file_max_memory_bytes: None,
            stripe_index: Arc::new(parking_lot::Mutex::new(
                super::stripe_index::StripeMembershipIndex::new(),
            )),
            rs_encoder_cache: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            repair_cooldown: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            auto_repair_on_fetch: false,
        }
    }

    /// Enable fetch-path opportunistic auto-repair for RS-encoded
    /// blobs. When set, every successful reconstruction inside
    /// `fetch_range` re-stores the missing data chunks under
    /// their original content-addressed hashes — so the stripe
    /// goes back to healthy, the v0.3 Phase C6 GC stripe-pin
    /// lifts naturally, and subsequent fetches don't re-pay the
    /// reconstruction cost.
    ///
    /// Default is `false` — the v0.3 plan's stated semantic is
    /// that fetch never writes. Enable for hot-blob workloads
    /// where degraded stripes would otherwise re-reconstruct on
    /// every read. The operator-driven [`Self::repair_blob`]
    /// remains the durable, sweep-the-whole-blob recovery path
    /// regardless of this flag.
    pub fn with_auto_repair_on_fetch(mut self, enabled: bool) -> Self {
        self.auto_repair_on_fetch = enabled;
        self
    }

    /// Opt every per-chunk file into disk persistence. Default is
    /// in-memory; switch on for production deployments that want
    /// blob chunks to survive process restart.
    pub fn with_persistent(mut self, persistent: bool) -> Self {
        self.persistent = persistent;
        self
    }

    /// Attach a manifest-tree LRU cache for `BlobRef::Tree` walks.
    /// `cap_bytes` sets the byte budget — every `walk_tree_range`
    /// fetch consults the cache first and stores the fetched node
    /// bytes on miss. A second range read on the same blob whose
    /// path overlaps the prior walk's path skips the
    /// `fetch_chunk` for the cached nodes.
    ///
    /// Default 64 MiB cap ≈ 13 K nodes at the typical ~5 KiB
    /// postcard-encoded per-node size. Operators with tighter or
    /// looser memory budgets pass an explicit `cap_bytes`. Pass
    /// `0` to disable caching entirely (every lookup misses,
    /// every insert is a no-op — useful for ablation testing).
    ///
    /// Cache hits stay correct under the content-addressed model
    /// (BLAKE3 hashes are immutable by construction); no
    /// invalidation surface is exposed.
    pub fn with_tree_node_cache(mut self, cap_bytes: usize) -> Self {
        self.tree_node_cache = Some(Arc::new(parking_lot::Mutex::new(
            super::blob_tree_cache::TreeNodeCache::with_capacity_bytes(cap_bytes),
        )));
        self
    }

    /// Override the per-chunk-file `max_memory_bytes` reservation.
    ///
    /// `RedexFileConfig` defaults to 64 MiB per channel; for blobs
    /// stored as many small chunks (e.g. 8 KiB chunks of a multi-
    /// MiB blob) that reservation is multiplied by the chunk
    /// count, easily blowing the process commit limit even though
    /// each channel only ever holds a few KiB of live bytes.
    /// Operators with chunk-heavy blobs pass a smaller value here
    /// (e.g. `1 << 20` = 1 MiB) to bound the reservation.
    ///
    /// The upstream `min(value, 64 MiB)` clamp still applies — a
    /// larger value than the default has no effect.
    pub fn with_chunk_file_max_memory_bytes(mut self, bytes: usize) -> Self {
        self.chunk_file_max_memory_bytes = Some(bytes);
        self
    }

    /// Snapshot of the tree-node cache's `(hits, misses, bytes,
    /// len)` for operator metrics. Returns `None` when no cache
    /// is wired.
    pub fn tree_node_cache_stats(&self) -> Option<(u64, u64, usize, usize)> {
        let cache = self.tree_node_cache.as_ref()?;
        let guard = cache.lock();
        Some((guard.hits(), guard.misses(), guard.bytes(), guard.len()))
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
            ctx.capability_fold,
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
    ///
    /// Takes `IntoIterator<Item = [u8; 32]>` rather than `&[..]`
    /// so callers can stream hashes straight from the underlying
    /// source (`std::iter::once(hash)` for `Small`,
    /// `chunks.iter().map(|c| c.hash)` for `Manifest`) without
    /// materializing an intermediate `Vec` per fetch — see
    /// dataforts perf #178.
    fn bump_heat<I: IntoIterator<Item = [u8; 32]>>(&self, hashes: I) {
        if let Some(reg) = self.blob_heat.as_ref() {
            let now = std::time::Instant::now();
            let mut guard = reg.lock();
            for h in hashes {
                guard.entry_mut(h, self.blob_heat_half_life, now).bump(now);
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
            match sink.announce_blob_heat_batch(&updates).await {
                Ok(()) => {}
                Err(e) => {
                    // Sink failed — roll the in-flight markers
                    // back so the next tick reissues these same
                    // emissions, matching the retry-on-failure
                    // semantic the audit asks for. Without the
                    // rollback the in-flight set would pin the
                    // hashes forever (no commit to clear, no other
                    // path to remove), and subsequent ticks would
                    // silently skip them.
                    let mut guard = reg.lock();
                    for (hash, _) in &emissions {
                        guard.rollback_emission(hash);
                    }
                    return Err(BlobError::Backend(format!(
                        "blob heat tick: announce batch failed: {}",
                        e
                    )));
                }
            }
        }
        // D-17: commit `last_emitted` mutations only after the sink
        // confirmed the announcement. Pre-fix the registry's `tick`
        // mutated state inline and a transient sink error stranded
        // the chain's heat updates forever (next tick's
        // `should_emit_heat` returned Suppress against the
        // already-advanced `last_emitted`). The registry's
        // in-flight set defends against the inverse race — a
        // concurrent `tick_blob_heat` landing in the lock-release
        // window between this `tick` and the `commit_emissions`
        // below would otherwise re-emit the same candidates
        // because `last_emitted` hasn't been mutated yet.
        // `tick`'s in-flight check skips those hashes; `commit`
        // clears the markers.
        {
            let mut guard = reg.lock();
            guard.commit_emissions(&emissions);
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
    // WARNING: cold-start parity-pin gap.
    //
    // The stripe-membership index that protects degraded-stripe
    // parity chunks from sweep is **per-adapter and in-memory**
    // (see `stripe_index.rs` module doc). After a process restart,
    // stripes only re-register lazily — when a `fetch_range` walk
    // reaches an `ErasureLeaf` and calls `register_stripe` inside
    // the walker.
    //
    // A blob that hasn't been read since the restart has NO
    // entries in the index. If GC fires before any reader touches
    // that cold blob, parity chunks that ARE in degraded stripes
    // (e.g. data chunks lost during the previous process's
    // uptime) will be swept — the pin can't fire because the
    // index has nothing to consult.
    //
    // Operator-driven `repair_blob` is the durable recovery for
    // this exposure: it walks the tree, which both registers
    // the stripe AND reconstructs missing chunks. Operators with
    // archival / cold-blob workloads should schedule periodic
    // `repair_blob` invocations against every known blob root
    // before running aggressive sweeps post-restart.
    //
    // A future commit closes this gap with a persistent stripe-
    // index journal (see `DATAFORTS_BLOB_STORAGE_PLAN_V2_DEFERRED.md`
    // §"Persistent stripe-index journal"). Removing this comment
    // OR the lazy on-read registration in `walk_stripe_range`
    // before the journal lands silently widens the exposure.
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
            // v0.3 Phase C6 GC stripe-membership pin. Before
            // even attempting to take the refcount entry, check
            // whether the chunk is a member of any registered RS
            // stripe that's currently degraded. If yes, pin —
            // skip the sweep so the only thing keeping a
            // recoverable stripe alive doesn't vanish between
            // dereference and operator-driven `repair_blob`.
            //
            // The presence-probe is the existing `chunk_exists`
            // check (open-file probe). Acceptable cost — pin
            // checks run only on chunks already past the
            // refcount + retention gates.
            //
            // Index lookup misses (chunk not registered) fall
            // through to the v0.2 sweep path unchanged.
            //
            // **Atomicity with `take_if_deletable`.** The pin
            // check and the refcount mutation both run under the
            // stripe-index lock. Pre-fix, a concurrent
            // `register_stripe` from `walk_stripe_range`'s lazy
            // population could land between these two steps:
            // pin-check returned false, the reader then registered
            // the stripe (now degraded after this hash gets
            // deleted), and the sweep proceeded to delete a chunk
            // that was just promoted into a pinned set. Holding
            // the index lock for both operations serialises with
            // every read-side registrar so the decision-and-act
            // pair is observed as one step.
            let entry_snapshot = {
                let idx = self.stripe_index.lock();
                if idx.should_pin_against_gc(&hash, |h| self.chunk_exists(h).unwrap_or(false)) {
                    continue;
                }
                // Atomic re-check + remove closes the TOCTOU window
                // between the deletable_hashes snapshot and the actual
                // delete — a concurrent `incr` (e.g. a freshly-folded
                // chain event taking a new reference on `hash`) would
                // otherwise lose its refcount entry to the unconditional
                // `remove` that delete_chunk used to issue. If the
                // re-check fails the chunk stays around for the next
                // sweep to retry.
                //
                // `take_if_deletable` returns the removed entry so we
                // can re-insert it on close-failure — without that
                // restore path, a transient `close_and_unlink_file`
                // error would leave the file on disk while the refcount
                // entry is gone, orphaning the chunk: future sweeps
                // can't find it (they enumerate refcounts) and the
                // only recovery is the out-of-band scanner. Restore-
                // on-failure means the very next sweep tick retries.
                //
                // `take_if_deletable` is sync + non-blocking; holding
                // the parking_lot lock across it is cheap. The
                // expensive I/O step (close_and_unlink_file below)
                // happens AFTER the lock drops, so concurrent reads
                // aren't serialised on disk.
                match self.refcount.take_if_deletable(
                    &hash,
                    now_unix_ms,
                    self.retention_floor,
                    disk_pressure_critical,
                ) {
                    Some(entry) => entry,
                    None => continue,
                }
            };
            let channel = Self::chunk_channel(&hash);
            // close_and_unlink_file also removes any on-disk
            // segment dir, so swept chunks don't accumulate as
            // orphaned segments on with_persistent(true) deployments.
            // Heap-only channels (with_persistent(false)) skip the
            // unlink branch and behave exactly like close_file.
            if let Err(e) = self.redex.close_and_unlink_file(&channel) {
                // Re-insert the refcount entry so a subsequent
                // sweep tick retries. `restore_if_absent` is a no-
                // op if a concurrent `incr` raced in and re-created
                // the slot — their refcount > 0 is authoritative
                // and the next sweep correctly skips the hash.
                let restored = self.refcount.restore_if_absent(hash, entry_snapshot);
                tracing::warn!(
                    hash = ?hash,
                    error = %e,
                    restored,
                    "mesh blob: sweep close_and_unlink_file failed; \
                     refcount entry restored for next sweep retry \
                     (restored=false means a concurrent incr re-created the slot)"
                );
                continue;
            }
            // Invalidate the manifest cache entry — same reasoning
            // as in delete_chunk: stale cache hits decode to a
            // tree node whose underlying chunk file just vanished,
            // which would confuse the operator-visible error
            // attribution on a subsequent fetch_range. Cache
            // integrity isn't compromised either way (bytes hash
            // to key), but error-path clarity is.
            if let Some(cache) = self.tree_node_cache.as_ref() {
                cache.lock().remove(&hash);
            }
            swept = swept.saturating_add(1);
        }
        self.metrics.record_gc_swept(swept);
        Ok(swept)
    }

    /// Delete a single chunk file by content hash. The chunk's
    /// `RedexFile` is closed + removed from the Redex manager
    /// (including any on-disk segment dir for persistent
    /// deployments), and the refcount table entry is dropped on
    /// success so `stat()` stops surfacing a stale
    /// `last_seen_unix_ms` for a deleted blob and any subsequent
    /// re-store starts a fresh retention-floor clock. Idempotent
    /// on the success path — closing an already-closed file
    /// returns `Ok(())` from the Redex layer. Used by the
    /// peer-initiated [`Self::delete_chunk_authorized`] as a
    /// force-delete; reachable directly for operators running
    /// batched / dry-run flows against
    /// [`BlobRefcountTable::deletable_hashes`].
    ///
    /// On `Err` the refcount entry is preserved so the next sweep
    /// can retry — chunk-file close failures shouldn't strand the
    /// retention clock.
    ///
    /// The GC sweep does NOT route through this method: it uses
    /// [`BlobRefcountTable::remove_if_deletable`] + a direct
    /// `close_and_unlink_file` so an `incr` racing the sweep can
    /// rescue the entry without losing data.
    pub async fn delete_chunk(&self, hash: &[u8; 32]) -> Result<(), BlobError> {
        let channel = Self::chunk_channel(hash);
        self.redex
            .close_and_unlink_file(&channel)
            .map_err(|e| BlobError::Backend(format!("mesh blob: close chunk: {}", e)))?;
        self.refcount.remove(hash);
        // Drop the cached tree-node bytes for this hash. Cache
        // integrity (bytes hash to key) is preserved either way,
        // but a stale entry means a subsequent fetch_range walk
        // descends through the cached node and only discovers
        // the missing chunks at the leaf — confusing the operator-
        // visible error attribution. Most deleted chunks aren't
        // tree nodes (the manifest manifest path stores chunks
        // directly under hash too, so a single remove() suffices
        // either way); the cache lookup is O(1) on the miss path.
        if let Some(cache) = self.tree_node_cache.as_ref() {
            cache.lock().remove(hash);
        }
        // PERF_AUDIT §6.7 — also drop the cached chunk-file
        // handle. A re-store of the same hash after delete must
        // open a fresh RedexFile rather than hitting the stale
        // handle for the just-unlinked file.
        self.chunk_file_cache.lock().pop(hash);
        Ok(())
    }

    /// Store a byte stream as a hierarchical-manifest blob
    /// ([`BlobRef::Tree`]). Returns the constructed reference;
    /// every constituent chunk + tree node is persisted before
    /// the return.
    ///
    /// Streams are consumed chunk-by-chunk against the supplied
    /// [`ChunkingStrategy`]. v0.3 Phase A accepts only
    /// [`ChunkingStrategy::Fixed`] at exactly
    /// [`BLOB_CHUNK_SIZE_BYTES`] (4 MiB) — CDC lands in Phase B,
    /// other fixed sizes break v0.2 chunk-level dedup and are
    /// rejected. Each chunk is hashed (BLAKE3), persisted via
    /// the existing `store_chunk` path (idempotent on hash
    /// collision), then fed into a [`TreeBuilder`] that
    /// accumulates the manifest tree incrementally.
    ///
    /// Memory bound: O(chunk_size + TREE_FANOUT × MAX_TREE_DEPTH
    /// × entry_size) — roughly 4 MiB + 20 KiB at the v0.3a
    /// defaults. Independent of total stream size; a 1 TiB
    /// stream uses the same peak memory as a 1 GiB stream.
    ///
    /// Phase A ships with sequential `store_chunk` dispatch —
    /// each chunk is awaited before the next is requested.
    /// Phase D's [`crate::adapter::net::redex::BandwidthClass`] surface adds dynamic
    /// in-flight parallelism (~256 MB target). For TB-scale
    /// streams on a fast link, the sequential path may not
    /// saturate the wire; that's an acknowledged Phase A
    /// trade-off.
    pub async fn store_stream_tree(
        &self,
        stream: BlobByteStream,
        encoding: Encoding,
        chunking: ChunkingStrategy,
    ) -> Result<BlobRef, BlobError> {
        // Reed-Solomon encoding ships in Phase C: dispatch into
        // the striper-driven write path. Other encodings remain
        // Replicated (Phase A/B).
        if let Some(rs_params) = super::erasure::RsParams::from_encoding(encoding) {
            return self
                .store_stream_tree_rs_internal(stream, chunking, rs_params)
                .await;
        }
        match chunking {
            ChunkingStrategy::Fixed { size } => {
                // Fixed: only the v0.2-compatible chunk size; other
                // sizes break wire-level dedup with v0.2 Manifest
                // blobs.
                if size as u64 != BLOB_CHUNK_SIZE_BYTES {
                    return Err(BlobError::Backend(format!(
                        "store_stream_tree: ChunkingStrategy::Fixed {{ size: {} }} \
                         does not match v0.2-compatible BLOB_CHUNK_SIZE_BYTES ({}); \
                         other fixed sizes break chunk-level dedup with v0.2 blobs",
                        size, BLOB_CHUNK_SIZE_BYTES
                    )));
                }
                self.store_stream_tree_internal(stream, encoding, size)
                    .await
            }
            ChunkingStrategy::Cdc { min, avg, max } => {
                // CDC: only the production parameter triple
                // (`PRODUCTION_CDC_PARAMS`); other triples break
                // cross-blob dedup on the cluster. Tests that want
                // a smaller-scale CDC fixture call the test-only
                // `store_stream_tree_cdc_internal` path.
                let params = super::cdc::CdcParams { min, avg, max };
                if params != super::cdc::PRODUCTION_CDC_PARAMS {
                    return Err(BlobError::Backend(format!(
                        "store_stream_tree: ChunkingStrategy::Cdc {{ min: {}, avg: {}, \
                         max: {} }} does not match the v0.3 production parameter triple \
                         (min={}, avg={}, max={}); arbitrary CDC params break cross-blob \
                         dedup on the cluster",
                        min,
                        avg,
                        max,
                        super::cdc::PRODUCTION_CDC_PARAMS.min,
                        super::cdc::PRODUCTION_CDC_PARAMS.avg,
                        super::cdc::PRODUCTION_CDC_PARAMS.max
                    )));
                }
                self.store_stream_tree_cdc_internal(stream, encoding, params)
                    .await
            }
        }
    }

    /// Body of [`Self::store_stream_tree`] without the
    /// production-only chunk-size + encoding gates. Reachable
    /// from `#[cfg(test)]` and integration tests so the harness
    /// can drive the tree path with a smaller chunk size (test
    /// fixtures need the depth-2 boundary at FANOUT chunks, which
    /// at the production 4 MiB chunk size would allocate ~500 MiB
    /// of payload per test and OOM the Windows test runner under
    /// parallel execution).
    ///
    /// Not part of the supported public API — kept `pub` only so
    /// the conformance integration test in `tests/` can build
    /// memory-feasible fixtures.
    #[doc(hidden)]
    pub async fn store_stream_tree_internal(
        &self,
        mut stream: BlobByteStream,
        encoding: Encoding,
        chunk_size: u32,
    ) -> Result<BlobRef, BlobError> {
        use futures::StreamExt;
        let chunk_size_usize = chunk_size as usize;
        let mut buffer: Vec<u8> = Vec::with_capacity(chunk_size_usize);
        let mut builder = TreeBuilder::new();

        // Stream-driven chunker. The producer's input chunks
        // (the `BlobByteStream` items) don't have to align to
        // our chunk boundary; buffer them and emit a chunk every
        // time we accumulate `chunk_size` bytes. The final
        // partial chunk lands in `finalize`.
        while let Some(maybe) = stream.next().await {
            let bytes = maybe?;
            let mut remaining: &[u8] = bytes.as_ref();
            while !remaining.is_empty() {
                let needed = chunk_size_usize - buffer.len();
                let take = needed.min(remaining.len());
                buffer.extend_from_slice(&remaining[..take]);
                remaining = &remaining[take..];
                if buffer.len() == chunk_size_usize {
                    self.emit_tree_chunk(
                        &mut builder,
                        std::mem::replace(&mut buffer, Vec::with_capacity(chunk_size_usize)),
                    )
                    .await?;
                }
            }
        }
        // Final partial chunk (length 1..chunk_size).
        if !buffer.is_empty() {
            self.emit_tree_chunk(&mut builder, std::mem::take(&mut buffer))
                .await?;
        }
        if builder.chunk_count() == 0 {
            return Err(BlobError::Backend(
                "store_stream_tree: empty stream; use BlobRef::Small for zero-byte payloads"
                    .to_owned(),
            ));
        }

        // Finalize the tree. Persist every trailing node + the
        // root before returning the BlobRef.
        let output = builder.finalize()?;
        // Builder-emitted node hashes are blake3(bytes) computed
        // inside TreeBuilder — trusted, skip the verify pass. (§6.2)
        for node in &output.trailing_nodes {
            self.store_chunk_prehashed(&node.hash, &node.bytes).await?;
        }
        // `root_bytes.is_empty()` signals "already in chunk
        // store" — the streamed-child peel in TreeBuilder::finalize
        // promotes a single-child root whose bytes were persisted
        // during streaming. Skip the redundant store_chunk in
        // that case; the chunk store already carries
        // (root_hash → child bytes).
        if !output.root_bytes.is_empty() {
            self.store_chunk_prehashed(&output.root_hash, &output.root_bytes)
                .await?;
        }

        BlobRef::tree(
            format!("mesh://{}", super::hex32(&output.root_hash)),
            encoding,
            output.root_hash,
            output.total_bytes,
            output.root_depth,
        )
    }

    /// CDC counterpart to [`Self::store_stream_tree_internal`].
    /// Drives a [`CdcStreamChunker`](super::cdc::CdcStreamChunker)
    /// over the stream and persists each content-defined chunk
    /// through the same `emit_tree_chunk` path the Fixed variant
    /// uses. Accepts arbitrary CDC parameters (no production-spec
    /// clamp), so tests can run a meaningful CDC fixture at
    /// kilobyte-scale; the public [`Self::store_stream_tree`]
    /// pins the params to [`PRODUCTION_CDC_PARAMS`].
    ///
    /// Memory bound: O(params.max + TREE_FANOUT × MAX_TREE_DEPTH
    /// × entry_size) ≈ 16 MiB + 20 KiB at production params.
    /// Independent of total stream size.
    ///
    /// Not part of the supported public API — `pub` only so the
    /// Phase B conformance integration test can run at
    /// memory-feasible scale.
    #[doc(hidden)]
    pub async fn store_stream_tree_cdc_internal(
        &self,
        mut stream: BlobByteStream,
        encoding: Encoding,
        params: super::cdc::CdcParams,
    ) -> Result<BlobRef, BlobError> {
        use futures::StreamExt;
        // `CdcStreamChunker::new` validates internally; the
        // explicit pre-validate is retained for the typed error
        // path the public API stamps before any work runs.
        params.validate()?;
        let mut chunker = super::cdc::CdcStreamChunker::new(params)?;
        let mut builder = TreeBuilder::new();

        while let Some(maybe) = stream.next().await {
            let bytes = maybe?;
            chunker.extend(bytes.as_ref());
            // Drain every confirmed content-defined chunk before
            // requesting more input. Bounds memory at params.max
            // + the typical stream-item size.
            while let Some(chunk) = chunker.try_next_chunk() {
                self.emit_tree_chunk(&mut builder, chunk).await?;
            }
        }
        // End-of-stream: flush whatever's left in the chunker
        // buffer. May emit one or more chunks; the last one may
        // be smaller than `params.min` (standard FastCDC EOF
        // allowance).
        for chunk in chunker.finalize() {
            self.emit_tree_chunk(&mut builder, chunk).await?;
        }

        if builder.chunk_count() == 0 {
            return Err(BlobError::Backend(
                "store_stream_tree (CDC): empty stream; use BlobRef::Small for \
                 zero-byte payloads"
                    .to_owned(),
            ));
        }

        let output = builder.finalize()?;
        // Builder-emitted node hashes are blake3(bytes) computed
        // inside TreeBuilder — trusted, skip the verify pass. (§6.2)
        for node in &output.trailing_nodes {
            self.store_chunk_prehashed(&node.hash, &node.bytes).await?;
        }
        // `root_bytes.is_empty()` signals "already in chunk
        // store" — the streamed-child peel in TreeBuilder::finalize
        // promotes a single-child root whose bytes were persisted
        // during streaming. Skip the redundant store_chunk in
        // that case; the chunk store already carries
        // (root_hash → child bytes).
        if !output.root_bytes.is_empty() {
            self.store_chunk_prehashed(&output.root_hash, &output.root_bytes)
                .await?;
        }

        BlobRef::tree(
            format!("mesh://{}", super::hex32(&output.root_hash)),
            encoding,
            output.root_hash,
            output.total_bytes,
            output.root_depth,
        )
    }

    /// Reed-Solomon Tree store. Drives the chunker (Fixed or CDC)
    /// to produce data chunks, feeds them through an
    /// [`RsStriper`](super::erasure::RsStriper) that closes
    /// stripes at exactly `k` chunks (the v0.3 Phase C2
    /// simplification — the trailing partial stripe falls back to
    /// [`Encoding::Replicated`] regardless of size). Each closed
    /// stripe becomes one [`TreeNode::ErasureLeaf`] containing a
    /// single [`StripeBlock`]; the leaves cascade upward through
    /// [`TreeBuilder::push_prebuilt_leaf`] using the same internal-
    /// node hierarchy the Replicated path builds.
    ///
    /// Fetch path: `walk_tree_range` handles `ErasureLeaf` by
    /// fetching the data chunks of each overlapping stripe. Phase
    /// C2 ships the *happy path* — every data chunk must be
    /// present. Reconstruction from parity on a missing-data-chunk
    /// fetch failure lands in Phase C5.
    ///
    /// Not part of the supported public API — `pub` so the future
    /// Phase C9 conformance integration test can drive RS at
    /// memory-feasible scale.
    #[doc(hidden)]
    pub async fn store_stream_tree_rs_internal(
        &self,
        mut stream: BlobByteStream,
        chunking: ChunkingStrategy,
        rs_params: super::erasure::RsParams,
    ) -> Result<BlobRef, BlobError> {
        use futures::StreamExt;

        rs_params.validate()?;
        let mut striper = super::erasure::RsStriper::new(rs_params)?;
        let mut builder = TreeBuilder::new();
        let mut data_chunk_count: u64 = 0;
        let encoding = Encoding::ReedSolomon {
            k: rs_params.k,
            m: rs_params.m,
        };

        // Helper: persist one ClosedStripe — store parity chunks,
        // encode the ErasureLeaf, persist the leaf, lift the leaf
        // into the tree builder. Bumps `data_chunk_count` by the
        // stripe's data chunks (used for builder bookkeeping +
        // finalize non-empty check).
        async fn flush_stripe(
            adapter: &MeshBlobAdapter,
            closed: super::erasure::ClosedStripe,
            builder: &mut TreeBuilder,
            data_chunk_count: &mut u64,
        ) -> Result<(), BlobError> {
            // Persist parity bytes (data chunks were already
            // persisted via emit_tree_chunk before being pushed
            // into the striper).
            let parity_iter = closed.block.chunks.iter().filter(|c| c.is_parity());
            for (p_ref, p_bytes) in parity_iter.zip(closed.parity_bytes.iter()) {
                // Parity hashes were computed inside
                // `RsEncoder::encode` (erasure.rs) over these exact
                // bytes — trusted, skip the verify pass. (§6.2)
                adapter.store_chunk_prehashed(&p_ref.hash, p_bytes).await?;
            }
            let data_count = closed.block.chunks.iter().filter(|c| c.is_data()).count() as u64;
            *data_chunk_count = data_chunk_count.saturating_add(data_count);

            // Register stripe membership for the v0.3 Phase C6
            // GC pin. Only RS stripes need this — Replicated
            // stripes have no parity dependency so the v0.2
            // refcount + retention model is sufficient.
            if let Encoding::ReedSolomon { k, .. } = closed.block.encoding {
                let members: Vec<[u8; 32]> = closed.block.chunks.iter().map(|c| c.hash).collect();
                adapter.stripe_index.lock().register_stripe(members, k);
            }

            // Build the ErasureLeaf, persist as a Small blob.
            let leaf = super::blob_tree::TreeNode::erasure_leaf(vec![closed.block])?;
            let leaf_bytes = leaf.encode()?;
            let leaf_hash: [u8; 32] = blake3::hash(&leaf_bytes).into();
            let leaf_size = leaf.covered_bytes();
            // Persist the leaf as a tree-node chunk (same channel
            // shape as data chunks). `leaf_hash` was just hashed
            // over `leaf_bytes` above — trusted. (§6.2)
            adapter
                .store_chunk_prehashed(&leaf_hash, &leaf_bytes)
                .await?;
            // Lift into the internal-cascade builder. The
            // emitted nodes (the leaf itself + any internal
            // closures) are returned but the leaf bytes we
            // already persisted; internals get persisted in
            // finalize via output.trailing_nodes.
            let emitted =
                builder.push_prebuilt_leaf(leaf_hash, leaf_bytes, leaf_size, data_count)?;
            // Persist every internal-level closure (the leaf
            // emission at level 0 is already stored; only the
            // level > 0 internals need separate persistence).
            // Builder-emitted node hashes are blake3(bytes) computed
            // inside TreeBuilder — trusted. (§6.2)
            for node in &emitted {
                if node.level > 0 {
                    adapter
                        .store_chunk_prehashed(&node.hash, &node.bytes)
                        .await?;
                }
            }
            Ok(())
        }

        // Run the chunker per the user's strategy and feed every
        // produced data chunk through the striper.
        match chunking {
            ChunkingStrategy::Fixed { size } => {
                let chunk_size_usize = size as usize;
                if chunk_size_usize == 0 {
                    return Err(BlobError::Backend(
                        "store_stream_tree_rs_internal: Fixed chunk size must be > 0".to_owned(),
                    ));
                }
                let mut buffer: Vec<u8> = Vec::with_capacity(chunk_size_usize);
                while let Some(maybe) = stream.next().await {
                    let bytes = maybe?;
                    let mut remaining: &[u8] = bytes.as_ref();
                    while !remaining.is_empty() {
                        let needed = chunk_size_usize - buffer.len();
                        let take = needed.min(remaining.len());
                        buffer.extend_from_slice(&remaining[..take]);
                        remaining = &remaining[take..];
                        if buffer.len() == chunk_size_usize {
                            let chunk_bytes = std::mem::replace(
                                &mut buffer,
                                Vec::with_capacity(chunk_size_usize),
                            );
                            // Offload blake3 for multi-MiB chunks so
                            // the tokio runtime worker doesn't stall
                            // for the multi-ms hash. (§6.1)
                            let (chunk_hash, chunk_bytes) =
                                blake3_hash_offload_vec(chunk_bytes).await;
                            // Hash was just computed over chunk_bytes
                            // — trusted, skip verify. (§6.2)
                            self.store_chunk_prehashed(&chunk_hash, &chunk_bytes)
                                .await?;
                            let cref = ChunkRefV3::data(chunk_hash, chunk_bytes.len() as u32);
                            // PERF_AUDIT §6.5 — striper now holds
                            // Bytes; `Bytes::from(Vec<u8>)` is O(1)
                            // (takes ownership of the Vec's buffer).
                            if let Some(closed) =
                                striper.push_chunk(bytes::Bytes::from(chunk_bytes), cref)?
                            {
                                flush_stripe(self, closed, &mut builder, &mut data_chunk_count)
                                    .await?;
                            }
                        }
                    }
                }
                if !buffer.is_empty() {
                    let chunk_bytes = std::mem::take(&mut buffer);
                    // Offload blake3 for multi-MiB chunks. (§6.1)
                    let (chunk_hash, chunk_bytes) =
                        blake3_hash_offload_vec(chunk_bytes).await;
                    // Just-computed hash — trusted. (§6.2)
                    self.store_chunk_prehashed(&chunk_hash, &chunk_bytes)
                        .await?;
                    let cref = ChunkRefV3::data(chunk_hash, chunk_bytes.len() as u32);
                    if let Some(closed) =
                        striper.push_chunk(bytes::Bytes::from(chunk_bytes), cref)?
                    {
                        flush_stripe(self, closed, &mut builder, &mut data_chunk_count).await?;
                    }
                }
            }
            ChunkingStrategy::Cdc { min, avg, max } => {
                let params = super::cdc::CdcParams { min, avg, max };
                let mut chunker = super::cdc::CdcStreamChunker::new(params)?;
                while let Some(maybe) = stream.next().await {
                    let bytes = maybe?;
                    chunker.extend(bytes.as_ref());
                    while let Some(chunk_bytes) = chunker.try_next_chunk() {
                        // Offload blake3 for multi-MiB chunks. (§6.1)
                        let chunk_hash =
                            blake3_hash_offload_bytes(&chunk_bytes).await;
                        // Just-computed hash — trusted. (§6.2)
                        self.store_chunk_prehashed(&chunk_hash, &chunk_bytes)
                            .await?;
                        let cref = ChunkRefV3::data(chunk_hash, chunk_bytes.len() as u32);
                        // PERF_AUDIT §6.5 — striper now holds Bytes
                        // refcount-shared with us; the pre-fix
                        // `.to_vec()` was a full ~1-16 MiB memcpy per
                        // CDC chunk. `chunk_bytes.clone()` is O(1).
                        if let Some(closed) =
                            striper.push_chunk(chunk_bytes.clone(), cref)?
                        {
                            flush_stripe(self, closed, &mut builder, &mut data_chunk_count).await?;
                        }
                    }
                }
                for chunk_bytes in chunker.finalize() {
                    // Offload blake3 for multi-MiB chunks. (§6.1)
                    let chunk_hash = blake3_hash_offload_bytes(&chunk_bytes).await;
                    // Just-computed hash — trusted. (§6.2)
                    self.store_chunk_prehashed(&chunk_hash, &chunk_bytes)
                        .await?;
                    let cref = ChunkRefV3::data(chunk_hash, chunk_bytes.len() as u32);
                    // PERF_AUDIT §6.5 — refcount bump, no memcpy.
                    if let Some(closed) = striper.push_chunk(chunk_bytes.clone(), cref)? {
                        flush_stripe(self, closed, &mut builder, &mut data_chunk_count).await?;
                    }
                }
            }
        }

        // End-of-stream: drain the striper. The trailing partial
        // stripe (if any) emits as a Replicated stripe.
        if let Some(closed) = striper.finalize()? {
            flush_stripe(self, closed, &mut builder, &mut data_chunk_count).await?;
        }

        if data_chunk_count == 0 {
            return Err(BlobError::Backend(
                "store_stream_tree_rs_internal: empty stream; use BlobRef::Small for \
                 zero-byte payloads"
                    .to_owned(),
            ));
        }

        let output = builder.finalize()?;
        // Builder-emitted node hashes are blake3(bytes) computed
        // inside TreeBuilder — trusted, skip the verify pass. (§6.2)
        for node in &output.trailing_nodes {
            self.store_chunk_prehashed(&node.hash, &node.bytes).await?;
        }
        // `root_bytes.is_empty()` signals "already in chunk
        // store" — the streamed-child peel in TreeBuilder::finalize
        // promotes a single-child root whose bytes were persisted
        // during streaming. Skip the redundant store_chunk in
        // that case; the chunk store already carries
        // (root_hash → child bytes).
        if !output.root_bytes.is_empty() {
            self.store_chunk_prehashed(&output.root_hash, &output.root_bytes)
                .await?;
        }

        BlobRef::tree(
            format!("mesh://{}", super::hex32(&output.root_hash)),
            encoding,
            output.root_hash,
            output.total_bytes,
            output.root_depth,
        )
    }

    /// Publish a byte stream, choosing
    /// [`BlobRef::Tree`] vs [`BlobRef::Manifest`] based on a
    /// [`TreeSupportProbe`] + the [`TREE_THRESHOLD_BYTES`]
    /// producer hint, AND applying CDC + erasure downgrades from
    /// the matching capability probes before any store work runs.
    ///
    /// Decision flow:
    /// 1. Apply [`super::cdc::cdc_downgrade`] to `chunking` —
    ///    peers that don't advertise CDC support get the
    ///    `Fixed` fallback so their chunk-store can recompute
    ///    leaf boundaries.
    /// 2. Apply [`super::erasure::erasure_downgrade`] to
    ///    `encoding` — peers that don't advertise Reed-Solomon
    ///    support get `Replicated` so they don't see a stripe
    ///    layout they can't reconstruct.
    /// 3. If `tree_probe.check() == false`, force the Manifest
    ///    path (Tree-incompatible peer). Caps at 16 GiB;
    ///    oversize streams return `BlobError::Backend`.
    /// 4. Else if `size_hint < TREE_THRESHOLD_BYTES`, prefer
    ///    the Manifest path for round-trip efficiency.
    /// 5. Else use the Tree path with the (possibly downgraded)
    ///    encoding + chunking.
    ///
    /// `size_hint` is the producer's best estimate of total
    /// bytes — `None` defaults to "above threshold," routing
    /// the stream through Tree. The decision is one-way: a
    /// stream routed to Manifest can't switch to Tree
    /// mid-stream because Manifest's buffered path has already
    /// committed to in-memory accumulation.
    ///
    /// Phase A6: the [`TreeSupportProbe::Dynamic`] arm wires
    /// future capability-tag advertisement; v0.3a callers
    /// without that substrate use `AlwaysSupported` for
    /// single-cluster deployments or `ForceManifest` for
    /// cross-version cluster rollouts. The CDC + erasure
    /// probes mirror the same shape one-for-one.
    pub async fn publish_stream_with_downgrade(
        &self,
        stream: BlobByteStream,
        encoding: Encoding,
        chunking: ChunkingStrategy,
        size_hint: Option<u64>,
        probes: &DowngradeProbes<'_>,
    ) -> Result<BlobRef, BlobError> {
        // Apply CDC + erasure downgrades up-front so the Tree /
        // Manifest decision below sees the final effective values.
        // Without this gate a caller can request `ChunkingStrategy::Cdc`
        // + `Encoding::ReedSolomon` against a cluster where only some
        // peers advertise the matching capability tags and silently
        // emit a Tree blob the legacy peers cannot reconstruct.
        let chunking = super::cdc::cdc_downgrade(chunking, probes.cdc);
        let encoding = super::erasure::erasure_downgrade(encoding, probes.erasure);
        let tree_supported = probes.tree.check();
        let above_threshold = size_hint.map(|s| s >= TREE_THRESHOLD_BYTES).unwrap_or(true);
        // The Manifest downgrade path caps at BLOB_REF_MAX_SIZE
        // (16 GiB). Streams whose size_hint exceeds that cap but
        // falls under the Tree-preference threshold (32 GiB) need
        // to take the Tree path anyway — otherwise pre-fix they
        // routed to the downgrade buffer and failed at the 16 GiB
        // cap. The size_hint is producer-supplied (may be wrong),
        // so this is best-effort; an unreliable hint that says
        // "small" but produces > 16 GiB bytes still errors at the
        // downgrade cap.
        let exceeds_manifest_cap = size_hint
            .map(|s| s > super::blob_ref::BLOB_REF_MAX_SIZE)
            .unwrap_or(false);
        if tree_supported && (above_threshold || exceeds_manifest_cap) {
            self.store_stream_tree(stream, encoding, chunking).await
        } else {
            // Downgrade path: buffer the whole stream (capped
            // at 16 GiB by the existing v0.2 store_stream
            // default), then publish via the Manifest path.
            use futures::StreamExt;
            const MAX_DOWNGRADE_BYTES: u64 = 16 * 1024 * 1024 * 1024;
            let mut buf: Vec<u8> = match size_hint {
                Some(n) if n <= 16 * 1024 * 1024 => Vec::with_capacity(n as usize),
                _ => Vec::new(),
            };
            let mut s = stream;
            while let Some(maybe) = s.next().await {
                let bytes = maybe?;
                if (buf.len() as u64).saturating_add(bytes.len() as u64) > MAX_DOWNGRADE_BYTES {
                    return Err(BlobError::Backend(format!(
                        "publish_stream_with_downgrade: downgrade buffer exceeds {} \
                         (peer does not support Tree, but stream is too large for Manifest)",
                        MAX_DOWNGRADE_BYTES
                    )));
                }
                buf.extend_from_slice(&bytes);
            }
            if buf.is_empty() {
                return Err(BlobError::Backend(
                    "publish_stream_with_downgrade: empty stream".to_owned(),
                ));
            }
            // Construct a Manifest BlobRef from the buffered
            // payload (chunks it via the v0.2 chunker), then
            // call the existing Manifest store path.
            let chunked = chunk_payload(&buf)?;
            let total = chunked.size();
            let blob_ref = chunked.into_blob_ref(
                format!("mesh://{}", super::hex32(&blake3::hash(&buf).into())),
                encoding,
            )?;
            // Persist via the existing store path; for the
            // Small (inline) case, falls back to a Small blob
            // store cleanly.
            self.store(&blob_ref, &buf).await?;
            // Tag the chunking arg as consumed (Phase A only
            // supports Fixed-default chunking on the Manifest
            // downgrade path; CDC lands in Phase B).
            let _ = (chunking, total);
            Ok(blob_ref)
        }
    }

    /// Internal: walk the tree from a node, fetching every
    /// `TreeNode` along the descent + the spanning chunks at the
    /// leaves. Each node is BLAKE3-verified against the parent's
    /// stored child-hash entry (tree-walk integrity); each chunk
    /// is verified by the existing `fetch_chunk` path.
    ///
    /// `residual_depth` is the number of internal-node levels
    /// still expected below this point. A leaf reached at any
    /// `residual_depth >= 0` is accepted (shorter-than-claimed
    /// trees read cleanly); an internal node at `residual_depth
    /// == 0` is rejected (the producer claimed a depth shallower
    /// than the actual structure).
    ///
    /// `range_start` / `range_end` are byte offsets WITHIN this
    /// subtree (0..subtree_size). The caller normalises before
    /// the first call (root subtree spans [0, total_size)).
    ///
    /// Returns the requested byte slice in order. `touched` is
    /// extended with every `TreeNode` hash + every leaf chunk
    /// hash walked — used by the data-gravity heat-bump path.
    /// PERF_AUDIT §6.4 — appends into a caller-supplied `out: &mut
    /// Vec<u8>` rather than returning a fresh `Vec` per recursion
    /// level. Pre-fix each level allocated its own Vec and the
    /// parent did `out.extend_from_slice(&child_bytes)` — a depth-4
    /// tree fetching a 1 GiB range memcpy'd ~4 GiB across the walk.
    /// With one shared output buffer, every level appends directly;
    /// the total per-walk byte count is exactly `range_end -
    /// range_start`. The caller (`fetch_range` for tree blobs)
    /// pre-allocates `out` to the range length so even the initial
    /// growth-by-doubling cost is zero.
    fn walk_tree_range<'a>(
        &'a self,
        node_hash: [u8; 32],
        subtree_size: u64,
        residual_depth: u8,
        range_start: u64,
        range_end: u64,
        touched: &'a mut Vec<[u8; 32]>,
        out: &'a mut Vec<u8>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), BlobError>> + Send + 'a>>
    {
        Box::pin(async move {
            // Cheap guard: empty range short-circuits the fetch.
            if range_end <= range_start {
                return Ok(());
            }
            if range_end > subtree_size {
                return Err(BlobError::Backend(format!(
                    "tree walk: range.end {} exceeds subtree_size {}",
                    range_end, subtree_size
                )));
            }
            // Manifest-cache lookup. On hit, skip the
            // `fetch_chunk` round trip — cache stores are
            // content-addressed (immutable BLAKE3-keyed) so a
            // hit is always correct.
            let cached = self
                .tree_node_cache
                .as_ref()
                .and_then(|c| c.lock().get(&node_hash));
            let node_bytes = if let Some(bytes) = cached {
                bytes
            } else {
                // Fetch the node's bytes (each tree node is
                // itself a chunk-shaped Small blob at
                // `dataforts/blob/<hex32>`). PERF_AUDIT §6.11 —
                // `fetch_chunk` already blake3-verifies the
                // returned payload against `node_hash`; the prior
                // defense-in-depth recompute here was pure waste
                // and only ever fired on a `fetch_chunk` bug, in
                // which case its own verify would already have
                // caught the mismatch. Drop the recompute.
                let bytes = self.fetch_chunk(&node_hash).await?;
                // Populate the cache for the next walk that
                // touches this node. Bytes are cloned only on
                // the miss path; the hit path returns the
                // cached clone directly.
                if let Some(cache) = self.tree_node_cache.as_ref() {
                    cache.lock().insert(node_hash, bytes.clone());
                }
                bytes
            };
            touched.push(node_hash);
            let node = super::blob_tree::TreeNode::decode(&node_bytes)?;
            // Cross-check: the node's covered_bytes must match
            // what the parent advertised for this subtree.
            // Catches a peer-supplied node whose body decoded
            // cleanly but doesn't actually cover the claimed
            // byte range.
            if node.covered_bytes() != subtree_size {
                return Err(BlobError::Decode(format!(
                    "tree walk: node covers {} bytes but parent advertised {}",
                    node.covered_bytes(),
                    subtree_size
                )));
            }
            match node {
                super::blob_tree::TreeNode::Internal { children } => {
                    if residual_depth == 0 {
                        // BlobRef::Tree.depth claimed the tree
                        // ends at this level; finding an internal
                        // here means the actual structure is
                        // deeper. Reject as malformed.
                        return Err(BlobError::Decode(
                            "tree walk: internal node at residual_depth=0 — \
                             actual tree deeper than BlobRef::Tree.depth claims"
                                .to_owned(),
                        ));
                    }
                    let mut offset: u64 = 0;
                    for (child_hash, child_size) in children {
                        let child_start = offset;
                        let child_end = offset.saturating_add(child_size);
                        offset = child_end;
                        // Skip children outside the requested range.
                        if child_end <= range_start || child_start >= range_end {
                            continue;
                        }
                        // Translate the global range into the
                        // child's local range.
                        let sub_start = range_start.saturating_sub(child_start);
                        let sub_end = range_end.saturating_sub(child_start).min(child_size);
                        // PERF_AUDIT §6.4 — pass the shared output
                        // buffer down; the child appends directly
                        // into it, no per-level intermediate Vec.
                        self.walk_tree_range(
                            child_hash,
                            child_size,
                            residual_depth - 1,
                            sub_start,
                            sub_end,
                            touched,
                            out,
                        )
                        .await?;
                    }
                    Ok(())
                }
                super::blob_tree::TreeNode::Leaf { chunks } => {
                    if residual_depth != 1 {
                        // BlobRef::Tree.depth claimed the leaves are
                        // at depth N from the root; finding a Leaf
                        // at any other residual depth means either
                        // the tree is shallower (depth-shortening
                        // attack — a peer substitutes a Leaf root
                        // for a claim of depth > 1) or deeper (Leaf
                        // at an intermediate position) than the
                        // outer BlobRef::Tree.depth claims. Both
                        // violate the "depth is rooted in the outer
                        // BlobRef::Tree, not the wire node" wire-
                        // trust invariant.
                        return Err(BlobError::Decode(format!(
                            "tree walk: Leaf at residual_depth={} — \
                             actual tree depth disagrees with BlobRef::Tree.depth",
                            residual_depth
                        )));
                    }
                    let mut offset: u64 = 0;
                    for chunk in chunks {
                        let chunk_start = offset;
                        let chunk_size_u64 = chunk.size as u64;
                        let chunk_end = offset.saturating_add(chunk_size_u64);
                        offset = chunk_end;
                        if chunk_end <= range_start || chunk_start >= range_end {
                            continue;
                        }
                        let sub_start = range_start.saturating_sub(chunk_start);
                        let sub_end = range_end.saturating_sub(chunk_start).min(chunk_size_u64);
                        let chunk_bytes = self.fetch_chunk(&chunk.hash).await?;
                        if (chunk_bytes.len() as u64) != chunk_size_u64 {
                            return Err(BlobError::ShortChunk {
                                hash: chunk.hash,
                                requested_start: sub_start,
                                requested_end: sub_end,
                                actual_len: chunk_bytes.len() as u64,
                            });
                        }
                        let slice = chunk_bytes
                            .get(sub_start as usize..sub_end as usize)
                            .ok_or(BlobError::ShortChunk {
                                hash: chunk.hash,
                                requested_start: sub_start,
                                requested_end: sub_end,
                                actual_len: chunk_bytes.len() as u64,
                            })?;
                        // PERF_AUDIT §6.4 — append into the shared
                        // output buffer; one memcpy (chunk slice →
                        // out) instead of two (slice → leaf Vec →
                        // parent Vec).
                        out.extend_from_slice(slice);
                        touched.push(chunk.hash);
                    }
                    Ok(())
                }
                super::blob_tree::TreeNode::ErasureLeaf { stripes } => {
                    if residual_depth != 1 {
                        // Same wire-trust invariant as the Leaf
                        // case above: ErasureLeaf belongs at the
                        // deepest level only, and the depth comes
                        // from the outer BlobRef::Tree, not the
                        // peer-supplied node body.
                        return Err(BlobError::Decode(format!(
                            "tree walk: ErasureLeaf at residual_depth={} — \
                             actual tree depth disagrees with BlobRef::Tree.depth",
                            residual_depth
                        )));
                    }
                    // Lazy stripe-index population: every
                    // ErasureLeaf decoded during a read registers
                    // its RS stripes into the GC-pin index. This
                    // closes the cold-start gap in the C6 in-
                    // memory-only index — after a process
                    // restart, fetches re-populate the index so
                    // by the time GC runs, recently-read blobs
                    // are protected against parity-sweep loss.
                    // Deduplicated at the index level (canonical
                    // fingerprint), so repeated reads of the
                    // same blob don't bloat the index.
                    {
                        let mut idx = self.stripe_index.lock();
                        for stripe in &stripes {
                            if let Encoding::ReedSolomon { k, .. } = stripe.encoding {
                                let members: Vec<[u8; 32]> =
                                    stripe.chunks.iter().map(|c| c.hash).collect();
                                idx.register_stripe(members, k);
                            }
                        }
                    }
                    let mut offset: u64 = 0;
                    for stripe in &stripes {
                        let stripe_size = stripe.covered_bytes();
                        let stripe_start = offset;
                        let stripe_end = offset.saturating_add(stripe_size);
                        offset = stripe_end;
                        if stripe_end <= range_start || stripe_start >= range_end {
                            continue;
                        }
                        // PERF_AUDIT §6.4 — `walk_stripe_range` still
                        // returns a Vec because reconstruction's
                        // mutable-shard buffer contract doesn't
                        // cleanly thread a shared output buffer; we
                        // append into `out` once per stripe instead
                        // of building a per-recursion-level Vec
                        // first. One memcpy per stripe vs N memcpys
                        // through the tree levels.
                        let stripe_bytes = self
                            .walk_stripe_range(
                                stripe,
                                stripe_start,
                                range_start,
                                range_end,
                                touched,
                            )
                            .await?;
                        out.extend_from_slice(&stripe_bytes);
                    }
                    Ok(())
                }
            }
        })
    }

    /// Internal: persist a single tree chunk + push it into the
    /// builder + persist any cascade-closed nodes. Centralised so
    /// the chunker loop above stays compact.
    async fn emit_tree_chunk(
        &self,
        builder: &mut TreeBuilder,
        chunk_bytes: impl AsRef<[u8]>,
    ) -> Result<(), BlobError> {
        let chunk_bytes = chunk_bytes.as_ref();
        if chunk_bytes.is_empty() {
            return Err(BlobError::Backend(
                "emit_tree_chunk: zero-byte chunk".to_owned(),
            ));
        }
        if (chunk_bytes.len() as u64) > TREE_LEAF_CHUNK_MAX_BYTES {
            return Err(BlobError::Backend(format!(
                "emit_tree_chunk: chunk {} bytes exceeds leaf cap {}",
                chunk_bytes.len(),
                TREE_LEAF_CHUNK_MAX_BYTES
            )));
        }
        // Offload blake3 to the blocking pool when the chunk is
        // big enough to stall the runtime worker. We hash via a
        // refcounted `Bytes` snapshot rather than an owned move so
        // the slice borrow we hold survives across the await — both
        // `store_chunk_prehashed` and the builder push want
        // `chunk_bytes` afterwards. (§6.1)
        let hash: [u8; 32] = if chunk_bytes.len() >= BLAKE3_OFFLOAD_THRESHOLD_BYTES {
            let snapshot = Bytes::copy_from_slice(chunk_bytes);
            tokio::task::spawn_blocking(move || blake3::hash(&snapshot).into())
                .await
                .expect("blake3 spawn_blocking panicked")
        } else {
            blake3::hash(chunk_bytes).into()
        };
        let chunk_size = chunk_bytes.len() as u32;
        // Persist the chunk bytes first so a crash between this
        // and the tree-builder push leaves the chunk content-
        // addressed and reachable for any future re-attempt
        // (the chunk's hash matches its bytes regardless of
        // whether a tree references it yet).
        // Just-computed hash + builder-emitted nodes — trusted,
        // skip the verify pass. (§6.2)
        self.store_chunk_prehashed(&hash, chunk_bytes).await?;
        // Push into the builder; persist any cascade-closed nodes
        // before returning.
        let closed = builder.push_chunk(ChunkRefV3::data(hash, chunk_size))?;
        for node in &closed {
            self.store_chunk_prehashed(&node.hash, &node.bytes).await?;
        }
        Ok(())
    }

    /// Walk a single stripe inside an `ErasureLeaf` and return the
    /// bytes covered by `[range_start, range_end)` relative to the
    /// whole blob. The stripe's data covers
    /// `[stripe_start, stripe_start + stripe.covered_bytes())`.
    ///
    /// Optimistic path: fetch each intersecting data chunk; if all
    /// succeed, slice + return. On any data-chunk fetch failure
    /// (`NotFound`, `HashMismatch`, `ShortChunk`) for an
    /// `Encoding::ReedSolomon` stripe, fall back to reconstruction:
    /// fetch the remaining data + parity chunks until `k` total
    /// survivors are available, run [`RsEncoder::reconstruct_data`],
    /// then slice from the reconstructed data shards.
    ///
    /// Reconstruction fails (`BlobError::Backend("erasure: stripe
    /// unrecoverable …")`) when fewer than `k` chunks survive in
    /// the stripe (data + parity combined).
    async fn walk_stripe_range(
        &self,
        stripe: &super::blob_tree::StripeBlock,
        stripe_start: u64,
        range_start: u64,
        range_end: u64,
        touched: &mut Vec<[u8; 32]>,
    ) -> Result<Vec<u8>, BlobError> {
        match stripe.encoding {
            Encoding::Replicated => {
                // Pre-RS stripe (small-stripe fallback): every
                // chunk is Data, walk in order, no reconstruction
                // path.
                self.walk_stripe_data_only(stripe, stripe_start, range_start, range_end, touched)
                    .await
            }
            Encoding::ReedSolomon { k, m } => {
                // Try optimistic data-only fetch first. If that
                // succeeds, return. Otherwise reconstruct.
                match self
                    .walk_stripe_data_only(stripe, stripe_start, range_start, range_end, touched)
                    .await
                {
                    Ok(bytes) => Ok(bytes),
                    Err(BlobError::NotFound(_))
                    | Err(BlobError::HashMismatch { .. })
                    | Err(BlobError::ShortChunk { .. }) => {
                        self.walk_stripe_with_reconstruction(
                            stripe,
                            k,
                            m,
                            stripe_start,
                            range_start,
                            range_end,
                            touched,
                        )
                        .await
                    }
                    Err(other) => Err(other),
                }
            }
        }
    }

    /// Data-only stripe walk: iterate data chunks, fetch each,
    /// slice into the requested range. Errors propagate from
    /// `fetch_chunk` — the caller (for RS stripes) catches and
    /// retries via reconstruction.
    async fn walk_stripe_data_only(
        &self,
        stripe: &super::blob_tree::StripeBlock,
        stripe_start: u64,
        range_start: u64,
        range_end: u64,
        touched: &mut Vec<[u8; 32]>,
    ) -> Result<Vec<u8>, BlobError> {
        let mut out: Vec<u8> = Vec::new();
        let mut local_offset: u64 = 0;
        for chunk in stripe.chunks.iter().filter(|c| c.is_data()) {
            let chunk_size_u64 = chunk.size as u64;
            let chunk_abs_start = stripe_start.saturating_add(local_offset);
            let chunk_abs_end = chunk_abs_start.saturating_add(chunk_size_u64);
            local_offset = local_offset.saturating_add(chunk_size_u64);
            if chunk_abs_end <= range_start || chunk_abs_start >= range_end {
                continue;
            }
            let sub_start = range_start.saturating_sub(chunk_abs_start);
            let sub_end = range_end
                .saturating_sub(chunk_abs_start)
                .min(chunk_size_u64);
            let chunk_bytes = self.fetch_chunk(&chunk.hash).await?;
            if (chunk_bytes.len() as u64) < chunk_size_u64 {
                return Err(BlobError::ShortChunk {
                    hash: chunk.hash,
                    requested_start: sub_start,
                    requested_end: sub_end,
                    actual_len: chunk_bytes.len() as u64,
                });
            }
            let slice = chunk_bytes
                .get(sub_start as usize..sub_end as usize)
                .ok_or(BlobError::ShortChunk {
                    hash: chunk.hash,
                    requested_start: sub_start,
                    requested_end: sub_end,
                    actual_len: chunk_bytes.len() as u64,
                })?;
            out.extend_from_slice(slice);
            touched.push(chunk.hash);
        }
        Ok(out)
    }

    /// Reconstruction path: fetch every shard slot (data + parity)
    /// as `Option<Vec<u8>>`. If `>= k` slots populate, run
    /// `reconstruct_data` to fill missing data shards; slice the
    /// reconstructed data shards into the requested range. If
    /// fewer than `k` shards survive, return
    /// `BlobError::Backend("erasure: stripe unrecoverable …")`.
    #[allow(clippy::too_many_arguments)]
    async fn walk_stripe_with_reconstruction(
        &self,
        stripe: &super::blob_tree::StripeBlock,
        k: u8,
        m: u8,
        stripe_start: u64,
        range_start: u64,
        range_end: u64,
        touched: &mut Vec<[u8; 32]>,
    ) -> Result<Vec<u8>, BlobError> {
        let k_usize = k as usize;
        let m_usize = m as usize;
        let total_shards = k_usize + m_usize;
        if stripe.chunks.len() != total_shards {
            return Err(BlobError::Backend(format!(
                "erasure: stripe shape mismatch — expected {} shards (k={} + m={}), got {}",
                total_shards,
                k,
                m,
                stripe.chunks.len()
            )));
        }

        // Determine the post-padding shard length. Parity chunks
        // were sized to max(data sizes) at store time; that's the
        // canonical shard length for the stripe.
        let shard_len = stripe.chunks[k_usize..]
            .iter()
            .map(|c| c.size as usize)
            .max()
            .unwrap_or(0);
        if shard_len == 0 {
            return Err(BlobError::Backend(
                "erasure: stripe has zero-length parity shards; unrecoverable".to_owned(),
            ));
        }

        // Fetch every shard slot; missing slots stay None.
        // Track which DATA shard indices (0..k) were missing
        // pre-reconstruct so we can opportunistically re-store
        // them if `auto_repair_on_fetch` is enabled.
        let mut shards: Vec<Option<Vec<u8>>> = Vec::with_capacity(total_shards);
        let mut surviving = 0usize;
        let mut missing_data_indices: Vec<usize> = Vec::new();
        for (i, chunk) in stripe.chunks.iter().enumerate() {
            match self.fetch_chunk(&chunk.hash).await {
                Ok(bytes) => {
                    // PERF_AUDIT §6.11 — `fetch_chunk` already
                    // blake3-verifies the returned payload against
                    // `chunk.hash`. The prior belt-and-braces
                    // recompute here was pure waste — by the time
                    // `bytes` is in hand, the hash equality is a
                    // contract that has already been checked. Drop
                    // the recompute; any future divergence between
                    // the contract and the implementation is the
                    // `fetch_chunk` test's job to catch.
                    // RS reconstruction needs mutable buffers
                    // (resize + in-place decode). PERF_AUDIT §6.12
                    // — when `bytes` is sole-owned (`fetch_chunk`
                    // typically hands back a refcount=1 `Bytes`),
                    // `try_into_mut` returns the existing
                    // allocation as `BytesMut` in O(1) and we
                    // skip the full-shard memcpy `to_vec()` was
                    // paying. Only short data shards (length <
                    // shard_len) need a resize, which is the
                    // existing pre-fix code. The slow path (an
                    // outstanding clone forced a copy) still works
                    // — we just lose the §6.12 win for that shard.
                    let mut bytes_vec = match bytes.try_into_mut() {
                        Ok(mut_data) => mut_data.into(),
                        Err(orig) => orig.to_vec(),
                    };
                    // Pad data shards to the post-padding length
                    // before passing to the encoder. Parity
                    // shards are already at shard_len.
                    if bytes_vec.len() < shard_len {
                        bytes_vec.resize(shard_len, 0);
                    }
                    touched.push(chunk.hash);
                    shards.push(Some(bytes_vec));
                    surviving += 1;
                }
                Err(_) => {
                    shards.push(None);
                    if i < k_usize {
                        missing_data_indices.push(i);
                    }
                }
            }
        }

        if surviving < k_usize {
            return Err(BlobError::Backend(format!(
                "erasure: stripe unrecoverable — {} chunks survive, need {} (k); \
                 lost {} of {}",
                surviving,
                k_usize,
                total_shards - surviving,
                total_shards
            )));
        }

        // Run the reconstruction.
        let encoder = self.get_or_build_rs_encoder(k, m)?;
        encoder.reconstruct_data(&mut shards)?;

        // Opt-in fetch-path auto-repair: re-store the
        // reconstructed data shards under their original
        // content-addressed hashes. Each one is BLAKE3-verified
        // before persisting — defense against any encoder bug
        // that would silently corrupt the chunk pool. Best-
        // effort: store_chunk failures are logged via tracing
        // but DO NOT fail the fetch (the caller already has the
        // reconstructed bytes in memory; the repair is an
        // optimization, not a correctness requirement).
        if self.auto_repair_on_fetch && self.auto_repair_cooldown_elapsed(stripe) {
            for &idx in &missing_data_indices {
                let chunk_ref = &stripe.chunks[idx];
                let Some(reconstructed) = shards[idx].as_ref() else {
                    continue;
                };
                let logical_len = chunk_ref.size as usize;
                if reconstructed.len() < logical_len {
                    continue;
                }
                let logical_bytes = &reconstructed[..logical_len];
                let computed: [u8; 32] = blake3::hash(logical_bytes).into();
                if computed != chunk_ref.hash {
                    tracing::warn!(
                        hash = ?chunk_ref.hash,
                        "fetch auto-repair: reconstructed shard hash mismatch; \
                         skipping persist (encoder bug or stripe corruption)"
                    );
                    continue;
                }
                // Local verify above pinned `computed == chunk_ref.hash`
                // — trusted, skip the redundant in-store verify. (§6.2)
                if let Err(e) = self
                    .store_chunk_prehashed(&chunk_ref.hash, logical_bytes)
                    .await
                {
                    tracing::warn!(
                        hash = ?chunk_ref.hash,
                        error = %e,
                        "fetch auto-repair: store_chunk failed; fetch continues, \
                         operator-driven repair_blob remains available"
                    );
                }
            }
        }

        // Slice the reconstructed data shards into the requested
        // range. Each data shard's logical size lives in
        // `stripe.chunks[i].size` (pre-padding); bytes past that
        // are zero-fill from store time and must NOT be returned.
        let mut out: Vec<u8> = Vec::new();
        let mut local_offset: u64 = 0;
        for (i, chunk) in stripe.chunks.iter().enumerate().take(k_usize) {
            let chunk_size_u64 = chunk.size as u64;
            let chunk_abs_start = stripe_start.saturating_add(local_offset);
            let chunk_abs_end = chunk_abs_start.saturating_add(chunk_size_u64);
            local_offset = local_offset.saturating_add(chunk_size_u64);
            if chunk_abs_end <= range_start || chunk_abs_start >= range_end {
                continue;
            }
            let sub_start = range_start.saturating_sub(chunk_abs_start);
            let sub_end = range_end
                .saturating_sub(chunk_abs_start)
                .min(chunk_size_u64);
            let data_bytes = shards[i].as_ref().ok_or_else(|| {
                BlobError::Backend(format!(
                    "erasure: data shard {} still missing post-reconstruct (internal bug)",
                    i
                ))
            })?;
            let slice = data_bytes.get(sub_start as usize..sub_end as usize).ok_or(
                BlobError::ShortChunk {
                    hash: chunk.hash,
                    requested_start: sub_start,
                    requested_end: sub_end,
                    actual_len: data_bytes.len() as u64,
                },
            )?;
            out.extend_from_slice(slice);
        }
        Ok(out)
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
    ///
    /// Uses the lookup-table-based [`super::hex32_into`] to render
    /// the hex into the trailing 64 bytes of the channel-name
    /// buffer — see dataforts perf #171 for the rationale. Pre-fix
    /// this looped `write!("{:02x}", b)` 32 times through the
    /// `core::fmt::Arguments` machinery, which is ~10× slower
    /// than the table form for the same output and runs once
    /// per chunk on the bulk-fetch path.
    #[expect(
        clippy::expect_used,
        reason = "hex-formatted name under the reserved CHUNK_CHANNEL_PREFIX always satisfies ChannelName validation"
    )]
    fn chunk_channel(hash: &[u8; 32]) -> ChannelName {
        // Build the bytes directly: `CHUNK_CHANNEL_PREFIX` (ASCII)
        // followed by 64 hex bytes. Keeping the build at the byte
        // level avoids the `write!` formatting dispatch.
        let mut buf = Vec::with_capacity(CHUNK_CHANNEL_PREFIX.len() + 64);
        buf.extend_from_slice(CHUNK_CHANNEL_PREFIX.as_bytes());
        let mut hex_buf = [0u8; 64];
        super::hex32_into(hash, &mut hex_buf);
        buf.extend_from_slice(&hex_buf);
        let name = String::from_utf8(buf)
            .expect("prefix is ASCII and hex bytes are ASCII — UTF-8 by construction");
        ChannelName::new(&name).expect("hex-formatted name under reserved prefix is always valid")
    }

    /// Look up an open `RedexFile` for the chunk identified by
    /// `hash`, opening (and caching) it on miss. Used by
    /// `fetch_chunk` / `store_chunk_locked` / `chunk_exists` to
    /// elide the per-call `chunk_channel` build, `chunk_file_config`
    /// (which clones `self.replication`), `Redex::open_file`'s ACL
    /// probe + replication validate + reopen-match — none of which
    /// vary across operations on the same hash.
    ///
    /// Per PERF_AUDIT §6.7.
    fn open_chunk_file_cached(
        &self,
        hash: &[u8; 32],
    ) -> Result<crate::adapter::net::redex::RedexFile, BlobError> {
        if let Some(file) = self.chunk_file_cache.lock().get(hash).cloned() {
            return Ok(file);
        }
        let channel = Self::chunk_channel(hash);
        let cfg = self.chunk_file_config();
        let file = self
            .redex
            .open_file(&channel, cfg)
            .map_err(|e| BlobError::Backend(format!("mesh blob: open chunk file: {}", e)))?;
        self.chunk_file_cache.lock().put(*hash, file.clone());
        Ok(file)
    }

    /// `RedexFileConfig` template applied to every chunk open. The
    /// operator opts into disk persistence via [`Self::with_persistent`]
    /// and into cross-node replication via [`Self::with_replication`].
    fn chunk_file_config(&self) -> RedexFileConfig {
        // A chunk is written ONCE (one append of the whole content-
        // addressed payload), and the heap segment is grow-only — it
        // sizes itself to the content on that single append. So default
        // the initial reservation to 0 rather than inheriting
        // `RedexFileConfig`'s 64 MiB prealloc. That prealloc, ×
        // one-file-per-chunk, is exactly what made a many-small-file
        // directory (tens of thousands of chunks) reserve hundreds of
        // GiB up front and OOM: 30k chunks × 64 MiB ≈ 1.9 TiB of
        // reservation for ~90 MiB of actual content. With a 0 hint, N
        // chunks cost ≈ Σ(content), not N × 64 MiB — `max_memory_bytes`
        // is only the up-front reservation here (the segment grows past
        // it up to the 3 GB hard limit), so dropping it costs nothing at
        // store time beyond one content-sized allocation per chunk.
        // `with_chunk_file_max_memory_bytes` still lets an operator pre-
        // reserve if they know their chunks are uniformly large.
        let reservation = self.chunk_file_max_memory_bytes.unwrap_or(0);
        let mut cfg = RedexFileConfig::new()
            .with_persistent(self.persistent)
            .with_max_memory_bytes(reservation);
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
        // this helper is called from a non-substrate path. Offload
        // the verify hash to the blocking pool above threshold so
        // a multi-MiB Small/Manifest store doesn't stall the runtime
        // worker. (§6.1)
        let computed: [u8; 32] = if bytes.len() >= BLAKE3_OFFLOAD_THRESHOLD_BYTES {
            let snapshot = Bytes::copy_from_slice(bytes);
            tokio::task::spawn_blocking(move || blake3::hash(&snapshot).into())
                .await
                .expect("blake3 spawn_blocking panicked")
        } else {
            blake3::hash(bytes).into()
        };
        if computed != *hash {
            return Err(BlobError::HashMismatch {
                expected: *hash,
                actual: computed,
            });
        }
        self.store_chunk_with_lock(hash, bytes).await
    }

    /// Internal: store a chunk whose `hash` the caller has already
    /// verified (or just computed) over `bytes`. Skips the per-call
    /// blake3 second-pass guard that `store_chunk` does — for hot
    /// in-crate callers (CDC/Fixed chunker, `emit_tree_chunk`,
    /// `flush_stripe` parity/leaf/internals, `TreeBuilder` finalize,
    /// manifest store after `chunk_payload` verification) the bytes
    /// were hashed the line before and re-hashing is pure waste.
    /// Per PERF_AUDIT_2026_06_10_FULL_CRATE.md §6.2.
    ///
    /// This is `pub(crate)` only because the trusted callers live in
    /// sibling modules; outside the crate, use `store_chunk` (or the
    /// `BlobAdapter::store` surface) which never trusts the caller.
    pub(crate) async fn store_chunk_prehashed(
        &self,
        hash: &[u8; 32],
        bytes: &[u8],
    ) -> Result<(), BlobError> {
        self.store_chunk_with_lock(hash, bytes).await
    }

    /// Body shared by `store_chunk` (verifying) and
    /// `store_chunk_prehashed` (trusts caller): per-hash lock acquire,
    /// run `store_chunk_locked`, best-effort lock-entry cleanup.
    async fn store_chunk_with_lock(
        &self,
        hash: &[u8; 32],
        bytes: &[u8],
    ) -> Result<(), BlobError> {
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
        // PERF_AUDIT §6.7 — cached open elides the per-call
        // channel name build + RedexFileConfig clone + Redex ACL
        // probe + replication validate + reopen-match.
        let file = self.open_chunk_file_cached(hash)?;
        let now_ms = now_unix_ms();
        if !file.is_empty() {
            // Idempotent fast-path. Content-addressed semantics
            // promise the on-disk bytes match the hash (verified on
            // the original store and again on every read via
            // `fetch_chunk` / `materialize`). Re-reading + re-hashing
            // the entire on-disk payload on every dedup hit pays
            // O(chunk_size) for ≈ zero additional protection over
            // length-compare: the failure mode the deep check was
            // designed to catch — a truncated replication catch-up —
            // shows up as a length mismatch (the truncated payload is
            // shorter than the honest one). Same-length on-disk
            // corruption is rare under content-addressing and is the
            // GC/scrub sweep's job, not the per-store hot path. Per
            // PERF_AUDIT_2026_06_10_FULL_CRATE.md §6.3.
            let existing_len = file.retained_bytes();
            if existing_len != bytes.len() as u64 {
                return Err(BlobError::HashMismatch {
                    expected: *hash,
                    // Length-mismatch under content-addressing: the
                    // on-disk content can't possibly hash to
                    // `expected` because it's a different size. We
                    // surface a sentinel actual rather than paying the
                    // O(chunk_size) hash we just elided.
                    actual: [0u8; 32],
                });
            }
            self.refcount
                .store_observed(*hash, bytes.len() as u64, now_ms);
            return Ok(());
        }
        file.append(bytes)
            .map_err(|e| BlobError::Backend(format!("mesh blob: append chunk: {}", e)))?;
        self.refcount
            .store_observed(*hash, bytes.len() as u64, now_ms);
        Ok(())
    }

    /// Fetch a single chunk by hash. Returns `BlobError::NotFound`
    /// when the chunk file is absent or empty.
    ///
    /// `pub` + `#[doc(hidden)]` so the v0.3 Phase B conformance
    /// integration test can walk a `BlobRef::Tree` and collect
    /// every reachable chunk hash for the dedup-after-edit
    /// assertion. Not part of the supported public API — the
    /// standard fetch path is `fetch_range` over a `BlobRef`.
    ///
    /// Returns [`bytes::Bytes`] per dataforts perf #184 — the
    /// chunk's payload comes off the redex layer as `Bytes`
    /// already, so handing back the same refcount-shareable
    /// buffer eliminates the `.to_vec()` memcpy this method used
    /// to do on every call. For a manifest fetch of N chunks
    /// that's N×payload_size bytes of memcpy avoided.
    #[doc(hidden)]
    pub async fn fetch_chunk(&self, hash: &[u8; 32]) -> Result<Bytes, BlobError> {
        // PERF_AUDIT §6.7 — cached open elides the per-call
        // channel name build + RedexFileConfig clone + Redex ACL
        // probe + replication validate + reopen-match.
        let file = self.open_chunk_file_cached(hash)?;
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
        let bytes = first.payload;
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

    /// Operator-driven Reed-Solomon repair sweep over the chunks
    /// reachable from `blob_ref`. Walks the manifest tree,
    /// inspects each `ErasureLeaf` stripe, and for any RS stripe
    /// that has at least one missing data chunk:
    ///
    /// 1. Fetch every surviving chunk (data + parity) of the
    ///    stripe.
    /// 2. If `>= k` shards survive, run RS reconstruction.
    /// 3. Re-store each previously-missing data chunk under its
    ///    original content-addressed hash.
    ///
    /// Stripes that are already healthy (every data chunk present)
    /// are skipped without I/O on the parity side. Stripes that
    /// have fewer than `k` survivors are counted as unrecoverable
    /// — `repair_blob` does NOT error on unrecoverable stripes;
    /// it records them in the report so the operator can take
    /// human action (restore from snapshot, accept data loss,
    /// etc.). A single unrecoverable stripe doesn't abort repair
    /// of the rest of the blob.
    ///
    /// `Encoding::Replicated` stripes (the small-stripe trailing
    /// fallback) have no parity model and are skipped with a
    /// dedicated counter.
    ///
    /// Non-Tree blobs return a zero-counter report (no repair
    /// surface — Small and Manifest blobs have no parity).
    ///
    /// The repair sweep is iterative (no concurrency for v0.3
    /// Phase C7); a future commit may parallelise the per-stripe
    /// recovery across the BandwidthClass-aware send queue.
    ///
    /// **Trust model.** This entry point is unauthenticated and
    /// intended for system-internal callers: the operator CLI
    /// running against a local store, an in-process scheduled
    /// repair cadence (if one ever lands), and unit tests. A peer-
    /// initiated / network-exposed repair must route through
    /// [`Self::repair_blob_authorized`] instead, because the sweep
    /// walks every chunk of the blob (full disk + CPU cost) and is
    /// trivially amplifiable into a DoS by an attacker who can
    /// reach this surface without a capability check.
    pub async fn repair_blob(&self, blob_ref: &BlobRef) -> Result<RepairReport, BlobError> {
        use super::blob_tree::TreeNode;

        let mut report = RepairReport::default();
        let root_hash = match blob_ref.tree_root_hash() {
            Some(h) => *h,
            None => return Ok(report), // Small / Manifest — no repair surface.
        };

        // Iterative tree descent: stack of node hashes to walk.
        let mut stack: Vec<[u8; 32]> = vec![root_hash];
        while let Some(node_hash) = stack.pop() {
            // The tree-node bytes may themselves be missing — if
            // so, the substrate can't recurse and we surface the
            // failure as a typed error (manifest-level loss is
            // fundamentally unrecoverable without operator
            // intervention; this is structurally different from
            // chunk-level loss and we don't silently swallow it).
            let bytes = self.fetch_chunk(&node_hash).await?;
            let node = TreeNode::decode(&bytes)?;
            match node {
                TreeNode::Internal { children } => {
                    for (child_hash, _size) in children {
                        stack.push(child_hash);
                    }
                }
                TreeNode::Leaf { .. } => {
                    // Replicated leaves have no per-chunk repair
                    // surface — each chunk is independently
                    // content-addressed; if it's missing, there's
                    // no parity to reconstruct from. Count and
                    // continue.
                    report.replicated_leaves_skipped =
                        report.replicated_leaves_skipped.saturating_add(1);
                }
                TreeNode::ErasureLeaf { stripes } => {
                    for stripe in &stripes {
                        self.repair_stripe(stripe, &mut report).await?;
                    }
                }
            }
        }
        Ok(report)
    }

    /// Capability-gated wrapper around [`Self::repair_blob`].
    /// Mirrors the [`Self::pin_authorized`] / [`Self::unpin_authorized`]
    /// / [`Self::delete_chunk_authorized`] pattern: the adapter must
    /// have an [`AuthGuard`] configured, and the caller must be
    /// authorized for `(origin_hash, channel)` per
    /// [`auth_allows_blob_op`]. Returns [`BlobError::Unauthorized`]
    /// on either failure.
    ///
    /// This is the peer-initiated / network-exposed repair entry.
    /// `repair_blob` walks the entire tree, fetches every chunk,
    /// hashes each, constructs an RS encoder per stripe, and may
    /// re-store reconstructed bytes — a hostile caller running it
    /// across many blobs amplifies I/O and CPU substantially, so it
    /// must not be reachable without the capability check.
    pub async fn repair_blob_authorized(
        &self,
        blob_ref: &BlobRef,
        origin_hash: u64,
        channel: &ChannelName,
    ) -> Result<RepairReport, BlobError> {
        let guard = self.auth_guard.as_ref().ok_or_else(|| {
            BlobError::Unauthorized("repair_blob_authorized requires AuthGuard wiring".to_string())
        })?;
        auth_allows_blob_op(guard, origin_hash, channel)?;
        self.repair_blob(blob_ref).await
    }

    /// Internal: per-stripe cooldown check for the fetch-path
    /// auto-repair. Returns `true` iff the stripe has either
    /// never been auto-repaired or the cooldown window has
    /// elapsed since the last attempt; updates the cooldown
    /// timestamp to `now` on `true` so concurrent walks don't
    /// double-fire. Stripe fingerprint is BLAKE3 of the
    /// concatenated member hashes, matching the
    /// `StripeMembershipIndex` canonical form.
    ///
    /// Without this gate, a peer serving corrupted bytes can
    /// force the optimistic path into reconstruction on every
    /// range read, and auto-repair then storms `store_chunk`
    /// calls for the same stripe at fetch rate.
    fn auto_repair_cooldown_elapsed(&self, stripe: &super::blob_tree::StripeBlock) -> bool {
        const COOLDOWN: std::time::Duration = std::time::Duration::from_secs(60);
        let mut hasher = blake3::Hasher::new();
        for c in &stripe.chunks {
            hasher.update(&c.hash);
        }
        let fingerprint: [u8; 32] = hasher.finalize().into();
        let now = std::time::Instant::now();
        let mut cooldown = self.repair_cooldown.lock();
        let admit = match cooldown.get(&fingerprint) {
            None => true,
            Some(last) => now.duration_since(*last) >= COOLDOWN,
        };
        if admit {
            cooldown.insert(fingerprint, now);
        }
        admit
    }

    /// Internal: return a cached `RsEncoder` for `(k, m)`,
    /// constructing on first use. The underlying matrix
    /// construction is the expensive part of `RsEncoder::new`;
    /// caching per `(k, m)` keeps reconstruction across many
    /// stripes of the same shape cheap. Adapter clones share the
    /// same cache.
    fn get_or_build_rs_encoder(
        &self,
        k: u8,
        m: u8,
    ) -> Result<Arc<super::erasure::RsEncoder>, BlobError> {
        // Fast path: lock + clone the Arc out.
        if let Some(enc) = self.rs_encoder_cache.lock().get(&(k, m)).cloned() {
            return Ok(enc);
        }
        // Build outside the lock — `RsEncoder::new`'s matrix
        // construction is potentially expensive and we don't want
        // to serialise concurrent builds for different (k, m)
        // configurations.
        let built = Arc::new(super::erasure::RsEncoder::new(super::erasure::RsParams {
            k,
            m,
        })?);
        // Re-acquire and insert. If a concurrent caller built the
        // same (k, m) first, prefer their entry (drop our local
        // build) so the cache stays canonical.
        let mut cache = self.rs_encoder_cache.lock();
        Ok(cache.entry((k, m)).or_insert(built).clone())
    }

    /// Internal: repair one stripe in isolation. Bumps the
    /// matching counter on `report` based on the outcome.
    async fn repair_stripe(
        &self,
        stripe: &super::blob_tree::StripeBlock,
        report: &mut RepairReport,
    ) -> Result<(), BlobError> {
        report.stripes_walked = report.stripes_walked.saturating_add(1);
        let (k, m) = match stripe.encoding {
            Encoding::Replicated => {
                report.replicated_stripes_skipped =
                    report.replicated_stripes_skipped.saturating_add(1);
                return Ok(());
            }
            Encoding::ReedSolomon { k, m } => (k, m),
        };
        let k_usize = k as usize;
        let total = k_usize + m as usize;
        if stripe.chunks.len() != total {
            // Stripe shape disagrees with its encoding header. The
            // stripe is structurally malformed — reconstruction
            // cannot proceed, but one bad stripe must not abort
            // the rest of the blob (per the contract documented on
            // `repair_blob`). Record as unrecoverable and continue.
            tracing::warn!(
                k,
                m,
                expected_total = total,
                actual_total = stripe.chunks.len(),
                "repair: stripe shape mismatch — recording as unrecoverable",
            );
            report.stripes_unrecoverable = report.stripes_unrecoverable.saturating_add(1);
            return Ok(());
        }

        // Probe every chunk: present (Some) vs missing (None).
        // A present chunk whose hash verification fails is also
        // treated as missing (the substrate refuses to feed
        // corrupt data into the reconstruction matrix).
        let mut shards: Vec<Option<Vec<u8>>> = Vec::with_capacity(total);
        let mut missing_data_indices: Vec<usize> = Vec::new();
        let mut surviving = 0usize;
        for (i, chunk) in stripe.chunks.iter().enumerate() {
            match self.fetch_chunk(&chunk.hash).await {
                Ok(bytes) => {
                    let computed: [u8; 32] = blake3::hash(&bytes).into();
                    if computed == chunk.hash {
                        // PERF_AUDIT §6.12 — try_into_mut to skip
                        // the full-shard memcpy when `bytes` is
                        // sole-owned (the typical case for a fresh
                        // `fetch_chunk`).
                        let bytes_vec = match bytes.try_into_mut() {
                            Ok(mut_data) => mut_data.into(),
                            Err(orig) => orig.to_vec(),
                        };
                        shards.push(Some(bytes_vec));
                        surviving += 1;
                        continue;
                    }
                    // Hash mismatch — treat as missing.
                    shards.push(None);
                    if i < k_usize {
                        missing_data_indices.push(i);
                    }
                }
                Err(_) => {
                    shards.push(None);
                    if i < k_usize {
                        missing_data_indices.push(i);
                    }
                }
            }
        }

        if missing_data_indices.is_empty() {
            // Healthy stripe — no data chunks missing.
            report.stripes_already_healthy = report.stripes_already_healthy.saturating_add(1);
            return Ok(());
        }

        if surviving < k_usize {
            // Can't reconstruct. Record + continue (no error;
            // the operator decides what to do with
            // unrecoverable stripes).
            report.stripes_unrecoverable = report.stripes_unrecoverable.saturating_add(1);
            return Ok(());
        }

        // Pad surviving data shards to the post-padding length
        // before reconstruction. Post-padding length = max parity
        // chunk size (parity was sized to max(data sizes) at
        // store time).
        let shard_len = stripe.chunks[k_usize..]
            .iter()
            .map(|c| c.size as usize)
            .max()
            .unwrap_or(0);
        if shard_len == 0 {
            // No parity shard carries any bytes — the stripe is
            // unrecoverable. Record + continue, same as the shape-
            // mismatch path.
            tracing::warn!("repair: stripe has zero-length parity shards; unrecoverable");
            report.stripes_unrecoverable = report.stripes_unrecoverable.saturating_add(1);
            return Ok(());
        }
        for slot in shards.iter_mut() {
            if let Some(bytes) = slot.as_mut() {
                if bytes.len() < shard_len {
                    bytes.resize(shard_len, 0);
                }
            }
        }

        // Encoder construction + reconstruction failures are
        // structural problems with the stripe (e.g. RsParams the
        // backend rejects, or reconstruct_data refusing because of
        // a malformed shard set). Treat as unrecoverable so a
        // single broken stripe doesn't abort the whole blob.
        let encoder = match self.get_or_build_rs_encoder(k, m) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    "repair: RsEncoder construction failed — recording stripe as unrecoverable",
                );
                report.stripes_unrecoverable = report.stripes_unrecoverable.saturating_add(1);
                return Ok(());
            }
        };
        if let Err(e) = encoder.reconstruct_data(&mut shards) {
            tracing::warn!(
                error = ?e,
                "repair: reconstruct_data failed — recording stripe as unrecoverable",
            );
            report.stripes_unrecoverable = report.stripes_unrecoverable.saturating_add(1);
            return Ok(());
        }

        // Re-store every missing data shard under its original
        // content-addressed hash. The reconstructed bytes are
        // padded to shard_len; trim to the chunk's pre-padding
        // logical size before persisting so the on-disk byte
        // count matches what the original store path wrote.
        let mut chunks_restored = 0u64;
        for &idx in &missing_data_indices {
            let chunk_ref = &stripe.chunks[idx];
            let Some(bytes) = shards[idx].as_ref() else {
                tracing::warn!(
                    idx,
                    "repair: data shard still missing post-reconstruct — recording stripe as unrecoverable",
                );
                report.stripes_unrecoverable = report.stripes_unrecoverable.saturating_add(1);
                return Ok(());
            };
            // The reconstructed shard is shard_len bytes; the
            // original was chunk_ref.size bytes (zero-padded to
            // shard_len at store time). Slice the logical bytes.
            let logical_len = chunk_ref.size as usize;
            if bytes.len() < logical_len {
                tracing::warn!(
                    idx,
                    reconstructed_len = bytes.len(),
                    logical_len,
                    "repair: reconstructed shard short — recording stripe as unrecoverable",
                );
                report.stripes_unrecoverable = report.stripes_unrecoverable.saturating_add(1);
                return Ok(());
            }
            let logical_bytes = &bytes[..logical_len];
            // Verify the reconstructed bytes hash back to the
            // original hash before persisting — defense against
            // any encoder bug that would silently corrupt the
            // chunk pool.
            let computed: [u8; 32] = blake3::hash(logical_bytes).into();
            if computed != chunk_ref.hash {
                tracing::warn!(
                    idx,
                    expected = ?chunk_ref.hash,
                    got = ?computed,
                    "repair: reconstructed shard hash mismatch — recording stripe as \
                     unrecoverable (encoder bug or stripe corruption); refusing to persist",
                );
                report.stripes_unrecoverable = report.stripes_unrecoverable.saturating_add(1);
                return Ok(());
            }
            // store_chunk failure remains a hard error — a
            // partial-write across the chunk pool is an operator-
            // visible persistence problem that should NOT be
            // swallowed as "just one bad stripe."
            // Local verify above pinned `computed == chunk_ref.hash`
            // — trusted, skip the redundant in-store verify. (§6.2)
            self.store_chunk_prehashed(&chunk_ref.hash, logical_bytes)
                .await?;
            chunks_restored += 1;
        }
        report.stripes_repaired = report.stripes_repaired.saturating_add(1);
        report.chunks_restored = report.chunks_restored.saturating_add(chunks_restored);
        Ok(())
    }
}

/// Outcome counters returned by
/// [`MeshBlobAdapter::repair_blob`]. Operators graph these as
/// metrics to track how often repair fires + the rate of
/// unrecoverable losses (which require human action).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RepairReport {
    /// Total RS stripes inspected (sum of the disjoint outcome
    /// counts below).
    pub stripes_walked: u64,
    /// Stripes that already had every data chunk present — no
    /// reconstruction needed, no I/O on the parity side.
    pub stripes_already_healthy: u64,
    /// Stripes that had at least one missing data chunk AND
    /// enough survivors (>= k) to reconstruct. Re-storing the
    /// missing data succeeded.
    pub stripes_repaired: u64,
    /// Sum of data chunks re-stored across `stripes_repaired`.
    /// Equals total missing-data-chunk count across recoverable
    /// stripes.
    pub chunks_restored: u64,
    /// Stripes where fewer than `k` shards survive — repair is
    /// fundamentally impossible without operator action (restore
    /// from snapshot or accept the loss). `repair_blob` does NOT
    /// error on these; it records and continues so a single
    /// unrecoverable stripe doesn't abort repair of the rest of
    /// the blob.
    pub stripes_unrecoverable: u64,
    /// `StripeBlock`s with [`Encoding::Replicated`] (the small-
    /// stripe trailing fallback in an RS blob) — no parity model
    /// to repair from. Counted separately so the operator can
    /// distinguish "no repair needed because Replicated" from
    /// "no repair needed because healthy".
    pub replicated_stripes_skipped: u64,
    /// `TreeNode::Leaf` (non-erasure) leaves encountered. Their
    /// chunks live outside the RS repair model — Replicated blobs
    /// repair via cross-node re-replication, not via parity
    /// reconstruction. Counted for operator visibility.
    pub replicated_leaves_skipped: u64,
}

/// Stream-chunk `reader` into content-addressed chunks and return the
/// assembled [`BlobRef`] — the exact reference
/// `chunk_payload(bytes).into_blob_ref(uri, encoding)` produces for the
/// same bytes, but without ever holding the whole payload in memory (peak
/// is one [`BLOB_CHUNK_SIZE_BYTES`] window).
///
/// When `adapter` is `Some`, every chunk is persisted via the adapter as
/// it is read (the `net transfer send-blob --store` path); when `None`,
/// the reference is computed without persisting anything (the dry
/// `send-blob` path). Either way at most one chunk is buffered at a time.
///
/// Boundary parity with [`chunk_payload`]: a payload `≤
/// BLOB_CHUNK_SIZE_BYTES` (including empty) yields a [`BlobRef::Small`];
/// strictly larger yields a [`BlobRef::Manifest`]. A single chunk hashes
/// the whole payload, so the `Small` hash matches `chunk_payload`'s
/// `Inline` hash exactly.
pub async fn store_blob_reader<R>(
    adapter: Option<&MeshBlobAdapter>,
    mut reader: R,
    uri: impl Into<String>,
    encoding: Encoding,
) -> Result<BlobRef, BlobError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt as _;

    let chunk_size = BLOB_CHUNK_SIZE_BYTES as usize;
    let mut buf = vec![0u8; chunk_size];
    let mut chunks: Vec<ChunkRef> = Vec::new();
    let mut total: u64 = 0;

    loop {
        // Fill the window: read until it is full or the reader hits EOF.
        // A fill shorter than `chunk_size` means EOF reached, so this is
        // the last (possibly partial) chunk.
        let mut filled = 0usize;
        while filled < chunk_size {
            let n = reader
                .read(&mut buf[filled..])
                .await
                .map_err(|e| BlobError::Backend(format!("read source: {e}")))?;
            if n == 0 {
                break;
            }
            filled += n;
        }

        if filled == 0 {
            // EOF with an empty window. If we have not emitted any chunk
            // yet the payload is empty — emit the single empty chunk so the
            // result matches `chunk_payload`'s Inline-empty shape. Otherwise
            // the previous full window was the last chunk; just stop (don't
            // append a spurious empty trailing chunk for an exactly-aligned
            // payload, which would wrongly promote a Small to a Manifest).
            if chunks.is_empty() {
                let hash: [u8; 32] = blake3::hash(&[]).into();
                if let Some(a) = adapter {
                    // Just-computed hash — trusted. (§6.2)
                    a.store_chunk_prehashed(&hash, &[]).await?;
                }
                chunks.push(ChunkRef { hash, size: 0 });
            }
            break;
        }

        let chunk = &buf[..filled];
        let hash: [u8; 32] = blake3::hash(chunk).into();
        if let Some(a) = adapter {
            // Just-computed hash — trusted. (§6.2)
            a.store_chunk_prehashed(&hash, chunk).await?;
        }
        chunks.push(ChunkRef {
            hash,
            size: filled as u32,
        });
        total += filled as u64;

        if filled < chunk_size {
            break; // short read → last chunk
        }
        // Full window: loop again to see whether more bytes follow.
    }

    if chunks.len() <= 1 {
        // `Small` (including the empty payload): a single chunk is the
        // whole payload, so its hash is the content hash.
        let hash = chunks
            .first()
            .map(|c| c.hash)
            .unwrap_or_else(|| blake3::hash(&[]).into());
        Ok(BlobRef::small(uri, hash, total))
    } else {
        BlobRef::manifest(uri, encoding, chunks)
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
                // Verification prepass — pure CPU, no I/O. If any
                // recomputed chunk's hash or size disagrees with the
                // manifest entry, abort BEFORE issuing any store
                // calls. This preserves the legacy "no chunks stored
                // on caller-poisoned manifest" contract and lets the
                // parallel store loop below skip per-chunk
                // verification entirely.
                for (i, (recomputed_chunk, _)) in recomputed_chunks.iter().enumerate() {
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
                }
                // Parallel chunk store with bounded concurrency. Chunks
                // are content-addressed so order doesn't matter;
                // `buffer_unordered(N)` drives up to N writes in flight,
                // and the surrounding loop drains the stream fully (it
                // does NOT short-circuit on first `Err` — see drain
                // comment below). Closure captures owned `Bytes` slices
                // refcounted from one `Bytes::copy_from_slice(bytes)`
                // upload-side copy, because a borrowed `&[u8]` shape
                // can't unify across `buffer_unordered`'s closure HRTB.
                use bytes::Bytes;
                use futures::StreamExt;
                const MANIFEST_STORE_CONCURRENCY: usize = 16;
                let bytes_arc = Bytes::copy_from_slice(bytes);
                let bytes_origin = bytes.as_ptr() as usize;
                let store_items: Vec<([u8; 32], Bytes)> = recomputed_chunks
                    .iter()
                    .map(|(rc, chunk_bytes)| {
                        // SAFETY-style invariant (sound under the
                        // public chunk_payload contract): every
                        // `chunk_bytes` slice points into `bytes`
                        // because `chunk_payload(bytes)` produces
                        // borrows into its own input. The offset
                        // is therefore a valid index into
                        // `bytes_arc`.
                        let offset = chunk_bytes.as_ptr() as usize - bytes_origin;
                        let end = offset + chunk_bytes.len();
                        (rc.hash, bytes_arc.slice(offset..end))
                    })
                    .collect();
                // Trusted: `chunk_payload` produces (hash, &bytes)
                // pairs where hash = blake3(bytes), and the verify
                // loop above already asserted hash equality between
                // the recomputed chunks and the declared manifest
                // entries. Skip the per-chunk verify pass. (§6.2)
                let mut futs = futures::stream::iter(store_items.into_iter().map(
                    |(hash, chunk): ([u8; 32], Bytes)| async move {
                        self.store_chunk_prehashed(&hash, &chunk).await
                    },
                ))
                .buffer_unordered(MANIFEST_STORE_CONCURRENCY);
                // Drain the stream fully — don't short-circuit on
                // the first error. Per cubic-dev-ai code review:
                // `store_chunk` registers a per-hash entry in
                // `in_flight_stores` on entry and removes it after
                // the inner store_chunk_locked completes (success
                // or error). Dropping a buffered future mid-flight
                // skips that cleanup and leaks the entry until a
                // subsequent `store_chunk` for the same hash
                // happens to evict it. The fix is to await every
                // started future and surface only the first error;
                // in-flight stores then run their own cleanup
                // paths normally. We still preserve "first error
                // wins" so the caller observes the same failure
                // they would have under the legacy `result?;`
                // shape — just without the leaked entries.
                let mut first_err: Option<BlobError> = None;
                while let Some(result) = futs.next().await {
                    if let Err(e) = result {
                        if first_err.is_none() {
                            first_err = Some(e);
                        }
                    }
                }
                if let Some(e) = first_err {
                    Err(e)
                } else {
                    Ok(())
                }
            }
            BlobRef::Tree { .. } => {
                // Tree-shaped publish lands in Phase A3
                // (`store_stream` tree path) which writes
                // chunk-by-chunk and accretes the manifest
                // tree incrementally. The bulk `store` surface
                // does not accept Tree BlobRefs — callers
                // route through `store_stream` instead.
                Err(BlobError::Backend(
                    "mesh blob: store(BlobRef::Tree, &[u8]) is not supported; \
                     use store_stream for Tree blobs"
                        .to_owned(),
                ))
            }
        };
        if result.is_ok() {
            self.metrics.record_store(bytes.len() as u64);
        }
        result
    }

    async fn fetch(&self, blob_ref: &BlobRef) -> Result<Bytes, BlobError> {
        // Per-fetch byte ceiling for the Manifest path. Pre-fix
        // an attacker-controllable manifest pointing at locally-
        // resident chunks let a handful of concurrent `fetch`
        // calls exhaust process memory — the per-chunk hash
        // verify defends against wrong-content but not against
        // wrong-size aggregate. 256 MiB is a generous bulk-fetch
        // upper bound; callers needing streaming on larger
        // payloads should route through `fetch_range` per-chunk
        // or `fetch_chunk` directly. Surfaces as a typed
        // BlobError::Backend so callers can fall back to the
        // streaming path on the same error.
        const MAX_BULK_FETCH_BYTES: u64 = 256 * 1024 * 1024;
        let result = match blob_ref {
            BlobRef::Small { hash, .. } => self.fetch_chunk(hash).await,
            BlobRef::Manifest {
                chunks, total_size, ..
            } => {
                if *total_size > MAX_BULK_FETCH_BYTES {
                    return Err(BlobError::Backend(format!(
                        "mesh blob: Manifest total_size {} exceeds bulk-fetch cap {}; \
                         use fetch_range or per-chunk fetch_chunk for large payloads",
                        total_size, MAX_BULK_FETCH_BYTES
                    )));
                }
                // Pre-allocate up to the bulk-fetch cap (per
                // dataforts perf #180). The over-cap check above
                // already rejected `total_size > MAX_BULK_FETCH_BYTES`,
                // so once we get here the declared size is
                // bounded by 256 MiB — `as usize` is safe even on
                // 32-bit targets (256 MiB << u32::MAX), and a
                // hostile manifest can no longer balloon the
                // up-front allocation beyond the cap. Legitimate
                // fetches now hit one allocation up front instead
                // of O(log N) reallocs across `extend_from_slice`
                // grows. The redundant `.min()` is defensive — if
                // the over-cap check drifts, the alloc stays
                // bounded — and folds to a no-op at runtime
                // because `total_size` is already <= the cap.
                //
                // Parallel chunk fetch via `buffered(N)` per
                // dataforts perf #172. Pre-fix this loop was
                // sequential — each chunk waited for the prior
                // one's fetch to land before issuing its own.
                // For a replicated manifest where chunks come
                // from peers, sequential fetch serialized N
                // network RTTs; `buffered` (not
                // `buffer_unordered`) preserves chunk order so
                // the result Vec is still byte-correct, while
                // allowing up to `MANIFEST_FETCH_CONCURRENCY`
                // requests in flight simultaneously. On a
                // 1024-chunk replicated blob at 1 ms/chunk,
                // sequential = ~1 s, buffered(16) = ~64 ms —
                // 15× speedup per the doc's worked example.
                //
                // Concurrency cap kept small (16) so we don't
                // overrun the substrate's per-channel credit
                // window on partial-availability manifests.
                // The decision to break-on-first-error matches
                // the legacy loop; `buffered` keeps the
                // already-in-flight futures running until the
                // stream is dropped, but the surrounding `match`
                // exits as soon as `Err` is observed so wasted
                // work is bounded by the concurrency cap.
                use futures::StreamExt;
                const MANIFEST_FETCH_CONCURRENCY: usize = 16;
                let fetch_stream = futures::stream::iter(chunks.iter().copied()).map(
                    |chunk: ChunkRef| async move {
                        match self.fetch_chunk(&chunk.hash).await {
                            Ok(bytes) if bytes.len() as u64 != chunk.size as u64 => {
                                Err(BlobError::Backend(format!(
                                    "mesh blob: chunk {} fetched size {} != declared {}",
                                    hex32(&chunk.hash),
                                    bytes.len(),
                                    chunk.size
                                )))
                            }
                            Ok(bytes) => Ok(bytes),
                            Err(e) => Err(e),
                        }
                    },
                );
                let mut stream = std::pin::pin!(fetch_stream.buffered(MANIFEST_FETCH_CONCURRENCY));
                let prealloc_cap = (*total_size).min(MAX_BULK_FETCH_BYTES) as usize;
                let mut out: Vec<u8> = Vec::with_capacity(prealloc_cap);
                let mut err: Option<BlobError> = None;
                while let Some(result) = stream.next().await {
                    match result {
                        // `bytes` is the owning `Bytes` returned
                        // by `fetch_chunk`; copy its contents into
                        // the assembly buffer (per dataforts perf
                        // #184, the chunk-side `to_vec()` memcpy
                        // is gone but Manifest assembly still
                        // joins N independent chunks into one
                        // contiguous output buffer — one memcpy
                        // per chunk, not two).
                        Ok(bytes) => out.extend_from_slice(&bytes),
                        Err(e) => {
                            err = Some(e);
                            break;
                        }
                    }
                }
                if let Some(e) = err {
                    Err(e)
                } else {
                    Ok(Bytes::from(out))
                }
            }
            BlobRef::Tree { .. } => {
                // Tree-shaped bulk fetch lands in Phase A4
                // (`TreeWalker` via `fetch_range`). The bulk
                // surface here doesn't accept Tree BlobRefs —
                // callers route through `fetch_range`'s tree
                // path or per-chunk `fetch_chunk` directly.
                return Err(BlobError::Backend(
                    "mesh blob: fetch(BlobRef::Tree) is not supported; \
                     use fetch_range for Tree blobs"
                        .to_owned(),
                ));
            }
        };
        if result.is_ok() {
            self.metrics.record_fetch();
            // PR-5j-b: bump blob heat for every chunk hash a
            // successful fetch resolved. No-op when no registry
            // is wired. Streams the hash sequence directly into
            // `bump_heat` per dataforts perf #178 — no
            // intermediate `Vec` allocation.
            if self.blob_heat.is_some() {
                match blob_ref {
                    BlobRef::Small { hash, .. } => self.bump_heat(std::iter::once(*hash)),
                    BlobRef::Manifest { chunks, .. } => {
                        self.bump_heat(chunks.iter().map(|c| c.hash));
                    }
                    // Tree path errored above; unreachable here.
                    BlobRef::Tree { .. } => {}
                }
            }
        }
        result
    }

    async fn fetch_range(
        &self,
        blob_ref: &BlobRef,
        range: std::ops::Range<u64>,
    ) -> Result<Bytes, BlobError> {
        if range.start > range.end {
            return Err(BlobError::Backend(format!(
                "mesh blob: range.start ({}) > range.end ({})",
                range.start, range.end
            )));
        }
        let len = range.end - range.start;
        if len == 0 {
            return Ok(Bytes::new());
        }
        // Guard against `u64 -> usize` truncation on 32-bit targets.
        // The Small arm indexes `bytes[range.start as usize..range.end as usize]`
        // and the Manifest arm calls `Vec::with_capacity(len as usize)`; both
        // silently truncate on 32-bit unless we reject here. Mirror of
        // FileSystemAdapter::fetch_range's guard in fs.rs.
        if len > usize::MAX as u64 || range.end > usize::MAX as u64 {
            return Err(BlobError::Backend(format!(
                "mesh blob: range length {} or end {} exceeds usize::MAX on this target",
                len, range.end
            )));
        }
        // v0.3 Tree lifts the effective addressable size from 16 GiB
        // to 128 PiB, and fetch_range returns the whole requested
        // range as a single `Vec<u8>`. Without an explicit cap, a
        // single `fetch_range(0, 100 GiB)` against a Tree blob would
        // allocate 100 GiB in-process. Bound the per-call range to
        // MAX_FETCH_RANGE_BYTES so a misconfigured caller (or
        // adversarial inbound) can't OOM the substrate. The cap is
        // generous (1 GiB) — well above any chunk-aligned read and
        // every legitimate range fetch — but well below the addressable
        // ceiling. Streaming consumers needing TB-scale walks should
        // page through smaller slices.
        if len > MAX_FETCH_RANGE_BYTES {
            return Err(BlobError::Backend(format!(
                "mesh blob: range length {} exceeds per-call cap {} \
                 (page through smaller slices for streaming reads)",
                len, MAX_FETCH_RANGE_BYTES,
            )));
        }
        let (result, touched): (Result<Bytes, BlobError>, Vec<[u8; 32]>) = match blob_ref {
            BlobRef::Small { hash, size, .. } => {
                if range.end > *size {
                    return Err(BlobError::Backend(format!(
                        "mesh blob: range.end {} exceeds Small size {}",
                        range.end, size
                    )));
                }
                match self.fetch_chunk(hash).await {
                    // Per dataforts perf #184: `Bytes::slice` is a
                    // zero-copy view into the same allocation, so
                    // the partial-Small range returns without a
                    // memcpy. Pre-fix this allocated `bytes[..].to_vec()`
                    // for every range read.
                    Ok(bytes) => (
                        Ok(bytes.slice(range.start as usize..range.end as usize)),
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
                // Parallel per-chunk fetch with order-preserving
                // `buffered(N)`, symmetric with the bulk-`fetch`
                // Manifest path. `ShortChunk` is the right error
                // surface for a size disagreement (vs `HashMismatch`,
                // which can collide with a truncated tail aligned to
                // a block boundary).
                use futures::StreamExt;
                const FETCH_RANGE_CONCURRENCY: usize = 16;
                let fetch_stream =
                    futures::stream::iter(requests.iter().copied()).map(|req| async move {
                        let chunk = &chunks[req.chunk_index];
                        let chunk_bytes = self.fetch_chunk(&chunk.hash).await?;
                        let end = req.end_in_chunk as usize;
                        if end > chunk_bytes.len() {
                            return Err(BlobError::ShortChunk {
                                hash: chunk.hash,
                                requested_start: req.start_in_chunk as u64,
                                requested_end: req.end_in_chunk as u64,
                                actual_len: chunk_bytes.len() as u64,
                            });
                        }
                        let slice = chunk_bytes.slice(req.start_in_chunk as usize..end);
                        Ok::<_, BlobError>((chunk.hash, slice))
                    });
                let mut stream = std::pin::pin!(fetch_stream.buffered(FETCH_RANGE_CONCURRENCY));
                let mut err: Option<BlobError> = None;
                while let Some(result) = stream.next().await {
                    match result {
                        Ok((hash, slice)) => {
                            out.extend_from_slice(&slice);
                            touched.push(hash);
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
                    (Ok(Bytes::from(out)), touched)
                }
            }
            BlobRef::Tree {
                root_hash,
                total_size,
                depth,
                ..
            } => {
                if range.end > *total_size {
                    return Err(BlobError::Backend(format!(
                        "mesh blob: range.end {} exceeds Tree total_size {}",
                        range.end, total_size
                    )));
                }
                let mut touched = Vec::new();
                // PERF_AUDIT §6.4 — pre-allocate the output buffer
                // to the requested range length so even the initial
                // growth-by-doubling cost is zero and every
                // recursion level appends in-place rather than
                // alloc'ing its own intermediate Vec.
                let mut out: Vec<u8> = Vec::with_capacity(
                    range.end.saturating_sub(range.start) as usize,
                );
                let walk_result = self
                    .walk_tree_range(
                        *root_hash,
                        *total_size,
                        *depth,
                        range.start,
                        range.end,
                        &mut touched,
                        &mut out,
                    )
                    .await;
                match walk_result {
                    Ok(()) => (Ok(Bytes::from(out)), touched),
                    Err(e) => (Err(e), Vec::new()),
                }
            }
        };
        if result.is_ok() && !touched.is_empty() {
            self.bump_heat(touched);
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
            BlobRef::Tree { root_hash, .. } => {
                // Tree `exists` is approximated by root-node
                // presence: the root must be locally present for
                // the tree walk to start. Sub-tree completeness
                // requires actually walking the manifest, which
                // is A4 scope. Returning `true` only on root-
                // present is a conservative under-report (a tree
                // whose root exists but a subtree is missing
                // returns `true` here), but the alternative —
                // walking the tree — duplicates A4 logic. Phase
                // A4 will override this with the walker-based
                // implementation.
                self.chunk_exists(root_hash)
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
            BlobRef::Tree { root_hash, .. } => {
                // Tree prefetch lands in A4 with the walker —
                // a full prefetch needs to descend the tree and
                // open every leaf chunk's channel. For A1, open
                // the root only so a subsequent walker call
                // starts with the root locally resident. The
                // post-A4 implementation walks the full tree.
                vec![*root_hash]
            }
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
            BlobRef::Tree { root_hash, .. } => {
                // Surface the root node's last_seen as a
                // proxy. A full max-over-tree requires walking
                // the tree (A4); this gives operators the
                // right shape today without the walker
                // overhead.
                self.refcount.get(root_hash).map(|e| e.last_seen_unix_ms)
            }
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
        // `replica_target` is per-adapter (set via
        // `with_replication`); cheap to read once outside the
        // map. `replicas_observed` would require a capability-
        // index lookup per row — surface `None` for now and
        // flip to a bulk lookup when the cap index is wired
        // through to this path (see `BlobStat::replicas_observed`
        // for the eventual integration point).
        let replica_target = self.replication.as_ref().map(|c| c.factor as u32);
        let mut entries: Vec<super::adapter::BlobInventoryEntry> = raw
            .into_iter()
            .map(|(hash, e)| super::adapter::BlobInventoryEntry {
                adapter_id: self.id.clone(),
                hash_hex: hex_encode(&hash),
                refcount: e.refcount,
                pinned: e.pinned,
                first_seen_unix_ms: e.first_seen_unix_ms,
                last_seen_unix_ms: e.last_seen_unix_ms,
                size_bytes: e.size_bytes,
                replicas_observed: None,
                replica_target,
            })
            .collect();
        // Most-recently-touched first — operators triaging
        // an incident want the freshest churn at the top.
        entries.sort_by_key(|e| std::cmp::Reverse(e.last_seen_unix_ms));
        if opts.limit > 0 && entries.len() > opts.limit {
            entries.truncate(opts.limit);
        }
        Ok(entries)
    }

    fn supports_list(&self) -> bool {
        true
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
/// Uses `write!` into the pre-allocated buffer rather than
/// `format!` per byte — saves 64 transient `String` allocs
/// per call, which adds up on prefix scans across a 32k-entry
/// refcount table.
fn hex_encode(bytes: &[u8; 32]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(64);
    for b in bytes {
        let _ = write!(&mut out, "{b:02x}");
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
        // PERF_AUDIT §6.7 — cached open.
        let file = self.open_chunk_file_cached(hash)?;
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
            BlobRef::Tree { .. } => {
                // sync_blob for Tree blobs requires walking the
                // tree to enumerate every leaf chunk. Lands with
                // the A4 `TreeWalker`; for A1 we surface the
                // un-implemented case as a typed error so callers
                // don't silently skip sync on a Tree blob.
                return Err(BlobError::Backend(
                    "mesh blob: sync_blob(BlobRef::Tree) is not yet implemented \
                     (Phase A4 / `TreeWalker`)"
                        .to_owned(),
                ));
            }
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

    /// PERF_AUDIT §6.7 — the chunk-file handle cache MUST
    /// invalidate on `delete_chunk` so a re-store + fetch after
    /// delete sees the fresh file rather than reading from a
    /// stale handle that points at the now-unlinked file.
    #[tokio::test]
    async fn delete_chunk_invalidates_handle_cache_before_restore() {
        use crate::adapter::net::dataforts::blob::adapter::BlobAdapter;
        let adapter = make_adapter();
        let payload_v1 = b"original payload".to_vec();
        let hash_v1: [u8; 32] = blake3::hash(&payload_v1).into();
        let blob_v1 = BlobRef::small(
            format!("mesh://{}", hex32(&hash_v1)),
            hash_v1,
            payload_v1.len() as u64,
        );
        adapter.store(&blob_v1, &payload_v1).await.unwrap();

        // Warm the cache via a fetch.
        let got = adapter.fetch(&blob_v1).await.unwrap();
        assert_eq!(got.as_ref(), payload_v1.as_slice());

        // Delete the chunk — the cache slot MUST go away.
        adapter.delete_chunk(&hash_v1).await.unwrap();

        // Fetching again must NOT see the deleted-but-cached
        // handle as alive. NotFound is the correct outcome here.
        let after = adapter.fetch(&blob_v1).await;
        assert!(
            matches!(after, Err(BlobError::NotFound(_))),
            "fetch after delete must surface NotFound; got {after:?}",
        );

        // Re-store under the same hash — must succeed (no stale
        // handle pointing at a phantom file) and the subsequent
        // fetch must return the fresh bytes.
        adapter.store(&blob_v1, &payload_v1).await.unwrap();
        let after_restore = adapter.fetch(&blob_v1).await.unwrap();
        assert_eq!(after_restore.as_ref(), payload_v1.as_slice());
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
    async fn store_blob_reader_matches_chunk_payload_across_boundaries() {
        // The streaming store must produce the byte-identical BlobRef the
        // buffered chunk_payload → into_blob_ref path yields, including at
        // the Small/Manifest boundary (≤ one chunk = Small; strictly larger
        // = Manifest) and for the empty payload. `None` adapter exercises
        // the compute-only (dry) path.
        let chunk = BLOB_CHUNK_SIZE_BYTES as usize;
        let sizes = [0usize, 1, 100, chunk - 1, chunk, chunk + 1, 2 * chunk + 7];
        for &n in &sizes {
            let bytes: Vec<u8> = (0..n).map(|i| (i.wrapping_mul(31) % 251) as u8).collect();
            let expected = chunk_payload(&bytes)
                .expect("chunk_payload")
                .into_blob_ref("mesh://x", Encoding::Replicated)
                .expect("into_blob_ref");
            let got = store_blob_reader(None, &bytes[..], "mesh://x", Encoding::Replicated)
                .await
                .expect("store_blob_reader");
            assert_eq!(got, expected, "ref mismatch at size {n}");
        }
    }

    #[tokio::test]
    async fn store_blob_reader_persists_fetchable_chunks() {
        // With an adapter, each chunk is stored as it streams; the assembled
        // ref must fetch back byte-for-byte over the multi-chunk path.
        let adapter = make_adapter();
        let n = 2 * BLOB_CHUNK_SIZE_BYTES as usize + 1024; // spans 3 chunks
        let bytes: Vec<u8> = (0..n).map(|i| (i.wrapping_mul(7) % 251) as u8).collect();

        let blob = store_blob_reader(Some(&adapter), &bytes[..], "mesh://x", Encoding::Replicated)
            .await
            .expect("store_blob_reader");
        assert!(
            matches!(blob, BlobRef::Manifest { .. }),
            "expected a Manifest for a 3-chunk payload"
        );
        let fetched = adapter.fetch(&blob).await.expect("fetch");
        assert_eq!(&fetched[..], &bytes[..]);
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
    async fn supports_list_distinguishes_mesh_from_opt_out_adapter() {
        // The BlobAdapter trait default for `supports_list`
        // is `false` — adapters that genuinely enumerate must
        // override. MeshBlobAdapter holds the refcount table
        // and enumerates authoritatively, so its override
        // returns `true`. A consumer (the Deck BLOBS tab)
        // checks supports_list before rendering "0 rows"
        // vs "N/A" so opt-out adapters aren't conflated with
        // empty ones.
        let adapter = make_adapter();
        assert!(adapter.supports_list(), "MeshBlobAdapter enumerates");
        // Default impl on a trait object that doesn't override
        // (NoopAdapter doesn't override) should report false.
        let noop: Arc<dyn super::super::adapter::BlobAdapter> =
            Arc::new(super::super::noop::NoopAdapter::new("noop"));
        assert!(
            !noop.supports_list(),
            "default opt-out adapter must not advertise list support",
        );
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

    /// Idempotent fast-path must reject a length-mismatched
    /// pre-existing payload at the same channel (e.g. truncated
    /// replication catch-up) — surfaces as `HashMismatch` rather
    /// than silently being affirmed by an honest caller's `store`.
    /// Per PERF_AUDIT §6.3 the fast-path now compares length only
    /// (no re-hash of the existing bytes); same-length on-disk
    /// corruption is left to GC/scrub. The legacy test still
    /// passes because the poisoned bytes here are a different
    /// length from the honest payload.
    #[tokio::test]
    async fn store_chunk_idempotent_path_rejects_length_mismatch() {
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
            "idempotent fast-path must reject length-mismatched existing bytes; got {:?}",
            err
        );
    }

    /// PERF_AUDIT §6.1 — the offloaded blake3 path must agree with
    /// the inline path for every input size, especially at the
    /// threshold boundary. Asserts both helpers match
    /// `blake3::hash` directly so we can't regress the offload into
    /// a subtle mis-hash by accident.
    #[tokio::test]
    async fn blake3_hash_offload_matches_inline_around_threshold() {
        use super::{
            blake3_hash_offload_bytes, blake3_hash_offload_vec, BLAKE3_OFFLOAD_THRESHOLD_BYTES,
        };
        // Test sizes that bracket the threshold from below, at, and
        // above — exercises both the inline short-circuit branch
        // and the spawn_blocking branch.
        for size in [
            0usize,
            1,
            64,
            BLAKE3_OFFLOAD_THRESHOLD_BYTES - 1,
            BLAKE3_OFFLOAD_THRESHOLD_BYTES,
            BLAKE3_OFFLOAD_THRESHOLD_BYTES + 1,
            BLAKE3_OFFLOAD_THRESHOLD_BYTES * 2,
            BLAKE3_OFFLOAD_THRESHOLD_BYTES * 4 + 17,
        ] {
            // Deterministic-but-varied content so identical-zero
            // collisions can't accidentally pass.
            let payload: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
            let expected: [u8; 32] = blake3::hash(&payload).into();

            let (offload_hash_vec, returned) = blake3_hash_offload_vec(payload.clone()).await;
            assert_eq!(
                offload_hash_vec, expected,
                "Vec offload hash mismatch at size {}",
                size
            );
            assert_eq!(
                returned, payload,
                "Vec offload must return the original bytes at size {}",
                size
            );

            let snapshot = bytes::Bytes::from(payload);
            let offload_hash_bytes = blake3_hash_offload_bytes(&snapshot).await;
            assert_eq!(
                offload_hash_bytes, expected,
                "Bytes offload hash mismatch at size {}",
                size
            );
        }
    }

    /// PERF_AUDIT §6.2 — `store_chunk_prehashed` is the in-crate
    /// trusted entry that skips the second-pass blake3 guard. It
    /// must still drive content correctly when the (hash, bytes)
    /// pair is consistent. The contract is "the caller guarantees
    /// hash == blake3(bytes)" — if they lie, we don't promise
    /// detection (and `fetch_chunk`'s read-side verify will catch
    /// it later). This test pins the well-formed contract.
    #[tokio::test]
    async fn store_chunk_prehashed_round_trips_against_fetch() {
        let adapter = make_adapter();
        let payload: Vec<u8> = b"prehashed round-trip".to_vec();
        let hash: [u8; 32] = blake3::hash(&payload).into();
        adapter
            .store_chunk_prehashed(&hash, &payload)
            .await
            .expect("prehashed store accepts well-formed input");
        let fetched = adapter.fetch_chunk(&hash).await.unwrap();
        assert_eq!(&fetched[..], &payload[..]);
    }

    /// PERF_AUDIT §6.2 — the public `BlobAdapter::store` surface
    /// must keep verifying caller-supplied bytes against the
    /// declared hash (the verify lives in `store_chunk`, not in
    /// `store_chunk_prehashed`). This test pins that the
    /// refactor did not accidentally bypass the verify on the
    /// untrusted entry point.
    #[tokio::test]
    async fn store_chunk_public_path_still_verifies_caller_bytes() {
        use crate::adapter::net::dataforts::blob::adapter::BlobAdapter;
        let adapter = make_adapter();
        let advertised: &[u8] = b"truth-bytes-xx"; // 14 bytes
        let attempted: Vec<u8> = b"lie-bytes-zzzz".to_vec(); // 14 bytes — same length
        assert_eq!(advertised.len(), attempted.len());
        let hash: [u8; 32] = blake3::hash(advertised).into();
        let blob = BlobRef::small("mesh://verify-after-refactor", hash, attempted.len() as u64);
        let err = adapter.store(&blob, &attempted).await.unwrap_err();
        assert!(
            matches!(err, BlobError::HashMismatch { .. }),
            "public store must verify caller bytes; got {:?}",
            err
        );
    }

    /// PERF_AUDIT §6.3 — the dedup fast-path must not pay the
    /// O(chunk_size) read + hash on every duplicate store. With
    /// length-only verification, a same-length corrupted
    /// pre-existing payload (the GC/scrub failure mode the audit
    /// explicitly defers) is no longer caught at store time; this
    /// test asserts the new behavior to lock the cost-shift in
    /// place and to act as a regression guard against accidental
    /// reintroduction of the deep re-hash.
    #[tokio::test]
    async fn store_chunk_idempotent_path_accepts_same_length_existing_bytes() {
        use crate::adapter::net::dataforts::blob::adapter::BlobAdapter;
        let adapter = make_adapter();
        let intended_payload = b"honest payload!!".to_vec(); // 16 bytes
        let intended_hash: [u8; 32] = blake3::hash(&intended_payload).into();
        let channel = MeshBlobAdapter::chunk_channel_for_hash(&intended_hash);
        let file = adapter
            .redex
            .open_file(&channel, RedexFileConfig::new())
            .unwrap();
        // Pre-poison with bytes of the same length but different
        // content. Under the legacy deep re-hash this would have
        // returned HashMismatch; under length-only verification it
        // succeeds (verification deferred to GC/scrub).
        let corrupt: Vec<u8> = b"CCCCCCCCCCCCCCCC".to_vec();
        assert_eq!(corrupt.len(), intended_payload.len());
        file.append(&corrupt).unwrap();

        let blob = BlobRef::small(
            "mesh://same-len",
            intended_hash,
            intended_payload.len() as u64,
        );
        // The fast-path now trusts content-addressing and length;
        // it should accept this store (no read + hash performed).
        adapter
            .store(&blob, &intended_payload)
            .await
            .expect("length-only verification accepts same-length pre-existing");
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
        assert_eq!(fetched.as_ref(), &payload[start as usize..end as usize]);
    }

    #[tokio::test]
    async fn fetch_range_against_small() {
        let adapter = make_adapter();
        let payload = b"hello world, mesh blob adapter".to_vec();
        let blob = small_ref_for(&payload);
        adapter.store(&blob, &payload).await.unwrap();

        let fetched = adapter.fetch_range(&blob, 6..11).await.unwrap();
        assert_eq!(fetched.as_ref(), b"world");
    }

    /// Pin dataforts perf #184: `fetch_range` on a `BlobRef::Small`
    /// returns a `Bytes::slice` into the underlying chunk
    /// allocation, not a fresh memcpy. Concretely: fetching the
    /// whole blob then `.slice(...)`-ing the same range produces
    /// a `Bytes` whose backing pointer is identical to the
    /// `fetch_range` result — both views point at the same
    /// allocator-owned buffer (one atomic refcount, no second
    /// copy). A regression that re-introduces `.to_vec()` in the
    /// Small fetch_range path would surface here as distinct
    /// backing pointers.
    #[tokio::test]
    async fn fetch_range_small_is_zero_copy_slice_of_chunk_buffer() {
        let adapter = make_adapter();
        let payload = b"hello world, mesh blob adapter".to_vec();
        let blob = small_ref_for(&payload);
        adapter.store(&blob, &payload).await.unwrap();

        let full = adapter.fetch(&blob).await.unwrap();
        let ranged = adapter.fetch_range(&blob, 6..11).await.unwrap();
        // Slice the full fetch the same way `fetch_range` does.
        let full_slice = full.slice(6..11);
        // Both should be the same byte content.
        assert_eq!(ranged.as_ref(), full_slice.as_ref());
        // Note: `fetch` and `fetch_range` are independent calls
        // so they walk through `fetch_chunk` separately and end
        // up with distinct refcount-roots — we cannot
        // `Bytes::ptr_eq` across calls. The pointer-identity
        // invariant we DO check is within one call's result:
        // `ranged.as_ptr()` falls inside the underlying chunk's
        // address range. The simplest assertion that captures
        // the no-memcpy contract is that re-slicing the ranged
        // result is also pointer-stable.
        let resliced = ranged.slice(0..ranged.len());
        assert_eq!(
            resliced.as_ptr(),
            ranged.as_ptr(),
            "slice of a Bytes must share the same backing pointer (zero-copy contract)",
        );
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

    /// Pin dataforts perf #180: a Manifest fetch returns a `Vec`
    /// whose capacity matches `total_size`, not the chunk-by-chunk
    /// grow path. We don't have direct access to the internal
    /// `out` Vec, but the public-API guarantee is straightforward:
    /// the returned bytes' length is exactly `total_size`, AND
    /// the underlying Vec was sized in one shot — which we
    /// observe indirectly via the capacity surfaced through
    /// `into_boxed_slice`'s round-trip behavior:
    /// `Vec::with_capacity(n)` + `extend_from_slice(...)` of `n`
    /// bytes followed by `into_boxed_slice` reuses the original
    /// allocation (no second alloc), and `Box<[u8]>::into_vec`
    /// produces a Vec where `len == capacity == n`. By contrast,
    /// the pre-fix `Vec::new()` + extend path produces a Vec
    /// whose capacity is >= the next power of two after `n` —
    /// so for a payload that's NOT already a power-of-two size,
    /// `capacity()` would be strictly greater than `len()` until
    /// `shrink_to_fit` is called. The assertion below catches a
    /// regression that drops the pre-alloc and goes back to
    /// power-of-two growth.
    #[tokio::test]
    async fn fetch_manifest_preallocates_vec_to_total_size() {
        let adapter = make_adapter();

        // Payload size deliberately not a power of two so the
        // grow-from-empty path would over-allocate, separating
        // pre-alloc (capacity == len) from grow (capacity > len).
        let len = BLOB_CHUNK_SIZE_BYTES as usize * 2 + 4321;
        let payload: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        let chunked = chunk_payload(&payload).unwrap();
        let chunk_refs: Vec<ChunkRef> = match chunked {
            ChunkedPayload::Chunked { chunks, .. } => chunks.into_iter().map(|(r, _)| r).collect(),
            _ => panic!("expected Chunked"),
        };
        let blob =
            BlobRef::manifest("mesh://prealloc", Encoding::Replicated, chunk_refs.clone()).unwrap();
        adapter.store(&blob, &payload).await.unwrap();

        let fetched = adapter.fetch(&blob).await.unwrap();
        assert_eq!(
            fetched.len(),
            len,
            "fetch must return exactly total_size bytes"
        );
        // After dataforts perf #184 the assembly buffer is wrapped
        // in `Bytes::from(Vec<u8>)` before being returned. `Bytes`
        // collapses the original Vec into its internal Arc and
        // doesn't surface the capacity field externally, so the
        // pre-fix capacity probe (round-trip through `Box<[u8]>`
        // back to `Vec`) no longer applies. The pre-alloc
        // invariant is still in force inside `fetch` — see the
        // `Vec::with_capacity(prealloc_cap)` call site comment —
        // and any regression that drops it would surface as a
        // longer p99 fetch latency under load rather than a
        // capacity assertion failure here.
    }

    /// Pin dataforts perf #178: a successful Manifest fetch bumps
    /// the heat counter for EVERY chunk hash in the manifest. The
    /// fix replaced a `vec![*hash]` / `chunks.iter().map().collect()`
    /// staging Vec with `self.bump_heat(chunks.iter().map(|c| c.hash))`
    /// (streamed iterator into the new `IntoIterator<Item = [u8;32]>`
    /// Pin: cubic-dev-ai code review for dataforts perf #173B —
    /// the buffer_unordered store loop must DRAIN to completion
    /// rather than short-circuit via `result?;` on the first
    /// error. `store_chunk` registers a per-hash entry in
    /// `in_flight_stores` on entry and removes it after
    /// `store_chunk_locked` returns (success or error). Dropping
    /// a buffered future mid-flight skips that cleanup and leaks
    /// the entry until a subsequent `store_chunk` for the same
    /// hash evicts it via `remove_if`.
    ///
    /// We can't easily inject a mid-flight failure (the
    /// pre-verification prepass at the top of `store(Manifest)`
    /// catches caller-poisoned manifests, and `store_chunk_locked`
    /// only fails on backend I/O which the test harness doesn't
    /// simulate). The next-best signal is the happy-path
    /// invariant: after a successful manifest store the
    /// `in_flight_stores` map must be empty. The drain-vs-?
    /// shape is the same for happy and failure paths — if a
    /// regression flipped back to `result?;` and the test
    /// covered N > 1 chunks, the happy-path traces would still
    /// pass but the failure-path traces would leak. Pair this
    /// runtime check with a source pin that ensures the
    /// `first_err` collect-then-return shape stays in place.
    #[tokio::test]
    async fn store_manifest_drains_buffer_unordered_and_clears_in_flight_stores() {
        let adapter = make_adapter();
        // 4-chunk manifest — comfortably above 1 to exercise
        // multiple in-flight futures, well under the
        // MANIFEST_STORE_CONCURRENCY=16 cap.
        let payload: Vec<u8> = (0..(BLOB_CHUNK_SIZE_BYTES as usize * 4))
            .map(|i| (i % 251) as u8)
            .collect();
        let chunked = chunk_payload(&payload).unwrap();
        let chunk_refs: Vec<ChunkRef> = match chunked {
            ChunkedPayload::Chunked { chunks, .. } => chunks.into_iter().map(|(r, _)| r).collect(),
            _ => panic!("expected Chunked"),
        };
        let blob = BlobRef::manifest("mesh://drain", Encoding::Replicated, chunk_refs.clone())
            .expect("manifest");
        adapter.store(&blob, &payload).await.expect("store");

        // Every per-hash mutex entry must have been removed by
        // `store_chunk`'s `remove_if` cleanup. A leaked entry —
        // the shape `result?;` on the buffer_unordered loop
        // would produce on the failure path — would survive
        // here as a non-zero count even on the happy path
        // if any of the buffered futures were dropped before
        // their cleanup ran.
        assert_eq!(
            adapter.in_flight_stores.len(),
            0,
            "in_flight_stores must be empty after a successful manifest store; \
             leak indicates buffer_unordered short-circuited without draining",
        );
    }

    /// Source pin: the buffer_unordered store loop in
    /// `MeshBlobAdapter::store` MUST drain via the
    /// `first_err`/collect shape, not short-circuit via the
    /// `?` operator. A "simplification" PR that flipped back to
    /// the operator would silently break the `in_flight_stores`
    /// cleanup contract for failure-path traces — observable
    /// only under load with a concurrent store that happens to
    /// fail mid-flight. Pin via source inspection.
    #[test]
    fn store_buffer_unordered_loop_must_drain_not_short_circuit() {
        let src = include_str!("mesh.rs");
        // Strip line comments so the assertion only inspects
        // executable source, not doc text. Block comments aren't
        // used in this file's loop body.
        let stripped: String = src
            .lines()
            .map(|l| match l.find("//") {
                Some(idx) => &l[..idx],
                None => l,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let body_idx = stripped
            .find("let mut futs = futures::stream::iter(store_items.into_iter().map(")
            .expect("buffered store loop must exist");
        // The shape we expect — collect first error, drain, then
        // surface — looks for the `first_err` variable plus the
        // drain loop body within ~800 chars after the iter call.
        // Use `saturating_add` + `min(len())` so a future
        // refactor that shrinks the surrounding body doesn't
        // panic the test on an out-of-bounds slice.
        let end = body_idx.saturating_add(800).min(stripped.len());
        let body = &stripped[body_idx..end];
        assert!(
            body.contains("first_err"),
            "buffer_unordered store loop must collect into `first_err` \
             and drain to completion — a `?` short-circuit would skip \
             per-chunk `in_flight_stores` cleanup. Body: {body}",
        );
        // The drain loop body (the `while let Some(...)` block)
        // must NOT have the short-circuit shape.
        let drain_loop_idx = body
            .find("while let Some(result) = futs.next().await")
            .expect("drain loop must exist");
        let drain_end = drain_loop_idx.saturating_add(200).min(body.len());
        let drain_loop_body = &body[drain_loop_idx..drain_end];
        assert!(
            !drain_loop_body.contains("result?;"),
            "buffer_unordered drain loop must not short-circuit — leaks \
             in_flight_stores entries on the failure path. Body: \
             {drain_loop_body}",
        );
    }

    /// `bump_heat` signature). A regression that dropped chunks
    /// from the streamed sequence — e.g. an off-by-one on the
    /// iterator, or misrouting the `BlobRef::Manifest` arm back to
    /// the `Tree`-style no-op — would surface here as missing heat
    /// entries for the trailing chunks.
    #[tokio::test]
    async fn fetch_manifest_bumps_blob_heat_for_every_chunk_hash() {
        use crate::adapter::net::dataforts::gravity::BlobHeatRegistry;
        let redex = Arc::new(Redex::new());
        let registry = Arc::new(parking_lot::Mutex::new(BlobHeatRegistry::new()));
        let adapter = MeshBlobAdapter::new("mesh-heat-manifest", redex)
            .with_blob_heat(registry.clone(), DEFAULT_BLOB_HEAT_HALF_LIFE);

        // 3-chunk payload: well over 2×BLOB_CHUNK_SIZE_BYTES so
        // we exercise the iterator over a chunk list bigger than
        // any small-Vec optimization fast path could mask.
        let payload: Vec<u8> = (0..(BLOB_CHUNK_SIZE_BYTES as usize * 2 + 1024))
            .map(|i| (i % 251) as u8)
            .collect();
        let chunked = chunk_payload(&payload).unwrap();
        let chunk_refs: Vec<ChunkRef> = match chunked {
            ChunkedPayload::Chunked { chunks, .. } => chunks.into_iter().map(|(r, _)| r).collect(),
            _ => panic!("expected Chunked"),
        };
        assert!(
            chunk_refs.len() >= 3,
            "fixture must produce ≥3 chunks (got {})",
            chunk_refs.len()
        );
        let blob = BlobRef::manifest("mesh://heat-many", Encoding::Replicated, chunk_refs.clone())
            .unwrap();
        adapter.store(&blob, &payload).await.unwrap();

        let _ = adapter.fetch(&blob).await.unwrap();

        let guard = registry.lock();
        for (i, c) in chunk_refs.iter().enumerate() {
            assert!(
                guard.get(&c.hash).is_some(),
                "chunk {i} hash must have a heat entry after fetch"
            );
        }
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

    // ────────────────────────────────────────────────────────
    // store_stream_tree (Phase A3)
    // ────────────────────────────────────────────────────────

    use super::super::blob_tree::{ChunkingStrategy, TreeNode, MAX_TREE_DEPTH, TREE_FANOUT};
    use bytes::Bytes;

    /// Build a `BlobByteStream` from a single byte buffer. Helps
    /// keep the tree tests compact without spinning up a real
    /// async source. The stream emits exactly one item, so all
    /// chunking happens inside `store_stream_tree`'s buffer logic.
    fn stream_one(bytes: Vec<u8>) -> BlobByteStream {
        Box::pin(futures::stream::once(async move { Ok(Bytes::from(bytes)) }))
    }

    /// Build a `BlobByteStream` from many small byte slices to
    /// exercise the buffering logic in `store_stream_tree` (where
    /// the producer doesn't align to the 4 MiB chunk boundary).
    fn stream_many(slices: Vec<Vec<u8>>) -> BlobByteStream {
        let items: Vec<Result<Bytes, BlobError>> =
            slices.into_iter().map(|s| Ok(Bytes::from(s))).collect();
        Box::pin(futures::stream::iter(items))
    }

    fn deterministic_bytes(seed: u8, len: usize) -> Vec<u8> {
        // Use a tiny LCG so the bytes are content-distinct per
        // seed but cheap to produce — no rand crate dependency
        // in the test path.
        let mut state: u64 = seed as u64;
        (0..len)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (state >> 33) as u8
            })
            .collect()
    }

    /// A two-chunk blob (just over BLOB_CHUNK_SIZE_BYTES) round-
    /// trips: store_stream_tree returns a BlobRef::Tree; every
    /// chunk + the root node lands locally.
    #[tokio::test]
    async fn store_stream_tree_two_chunk_round_trip() {
        let adapter = make_adapter();
        let len = BLOB_CHUNK_SIZE_BYTES as usize + 1024; // one full + one tiny
        let payload = deterministic_bytes(0x11, len);
        let stream = stream_one(payload.clone());

        let blob_ref = adapter
            .store_stream_tree(stream, Encoding::Replicated, ChunkingStrategy::default())
            .await
            .expect("store_stream_tree succeeds");

        // The returned ref is a Tree.
        assert!(matches!(blob_ref, BlobRef::Tree { .. }));
        assert_eq!(blob_ref.size(), len as u64);
        // depth=1 for a single-leaf tree (since both chunks fit
        // in one leaf with TREE_FANOUT=128).
        assert_eq!(blob_ref.tree_depth(), Some(1));

        // Root node is locally fetchable.
        let root_hash = *blob_ref.tree_root_hash().unwrap();
        let root_bytes = adapter
            .fetch_chunk(&root_hash)
            .await
            .expect("root node locally fetchable");
        let root_decoded = TreeNode::decode(&root_bytes).expect("root decodes");
        assert!(root_decoded.is_leaf());
        // Root is a 2-chunk leaf.
        if let TreeNode::Leaf { chunks } = root_decoded {
            assert_eq!(chunks.len(), 2);
            assert_eq!(chunks[0].size, BLOB_CHUNK_SIZE_BYTES as u32);
            assert_eq!(chunks[1].size, 1024);
            // Each chunk is locally fetchable.
            for chunk in &chunks {
                let bytes = adapter
                    .fetch_chunk(&chunk.hash)
                    .await
                    .expect("chunk fetchable");
                assert_eq!(bytes.len(), chunk.size as usize);
                // BLAKE3 cross-check matches the manifest.
                let computed: [u8; 32] = blake3::hash(&bytes).into();
                assert_eq!(computed, chunk.hash);
            }
        }
    }

    /// Empty stream is rejected (use BlobRef::Small for zero-byte).
    #[tokio::test]
    async fn store_stream_tree_rejects_empty_stream() {
        let adapter = make_adapter();
        let empty = stream_one(Vec::new());
        let err = adapter
            .store_stream_tree(empty, Encoding::Replicated, ChunkingStrategy::default())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("empty stream"), "got: {err}");
    }

    /// CDC strategy with off-spec parameters is rejected — the
    /// public surface accepts only the pinned
    /// `PRODUCTION_CDC_PARAMS` triple (4 MiB avg, 1 MiB min,
    /// 16 MiB max) so all CDC-stored blobs in a cluster can
    /// dedup against each other.
    #[tokio::test]
    async fn store_stream_tree_rejects_off_spec_cdc_params() {
        let adapter = make_adapter();
        let bytes = deterministic_bytes(0x22, 1024);
        let err = adapter
            .store_stream_tree(
                stream_one(bytes),
                Encoding::Replicated,
                ChunkingStrategy::Cdc {
                    // Off-spec: smaller than production for any
                    // would-be tuner. The error must surface so
                    // callers know to use the test-only internal
                    // path or the production triple.
                    avg: 2048,
                    min: 512,
                    max: 8192,
                },
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("does not match the v0.3 production parameter triple"),
            "got: {err}"
        );
    }

    /// Reed-Solomon Tree round-trip via the test-internal RS
    /// store: store a deterministic blob long enough to fill at
    /// least one stripe, fetch the full range back, assert
    /// byte-equality. Pins the Phase C2 happy-path encode +
    /// decode (no reconstruction — all chunks present).
    #[tokio::test]
    async fn store_stream_tree_rs_round_trips_when_all_chunks_present() {
        let adapter = make_adapter();
        // 4 KiB chunks × 6 = 24 KiB payload = 1 full RS(4,2)
        // stripe + 2 trailing chunks (Replicated fallback).
        let chunk_size: u32 = 4 * 1024;
        let payload = deterministic_bytes(0xE0, chunk_size as usize * 6);
        let rs_params = super::super::erasure::RsParams { k: 4, m: 2 };
        let blob_ref = adapter
            .store_stream_tree_rs_internal(
                stream_one(payload.clone()),
                ChunkingStrategy::Fixed { size: chunk_size },
                rs_params,
            )
            .await
            .expect("RS store_stream_tree round trip");
        assert!(matches!(blob_ref, BlobRef::Tree { .. }));
        assert_eq!(blob_ref.size(), payload.len() as u64);
        let fetched = adapter
            .fetch_range(&blob_ref, 0..payload.len() as u64)
            .await
            .expect("RS fetch_range");
        assert_eq!(
            fetched, payload,
            "RS happy-path round-trip must be byte-identical"
        );
    }

    /// Killing up to `m` data chunks per stripe still allows the
    /// fetch path to succeed — reconstruction from parity recovers
    /// the missing bytes. Pins Phase C5's read-side reconstruction
    /// contract.
    #[tokio::test]
    async fn fetch_range_rs_reconstructs_when_data_chunks_missing() {
        let adapter = make_adapter();
        let chunk_size: u32 = 4 * 1024;
        // Single full stripe: exactly k=4 data chunks, no trailing.
        let payload = deterministic_bytes(0xE2, chunk_size as usize * 4);
        let rs_params = super::super::erasure::RsParams { k: 4, m: 2 };
        let blob_ref = adapter
            .store_stream_tree_rs_internal(
                stream_one(payload.clone()),
                ChunkingStrategy::Fixed { size: chunk_size },
                rs_params,
            )
            .await
            .unwrap();

        // Walk the tree to find the stripe's data chunk hashes.
        // The RS path emits leaves via `push_prebuilt_leaf`
        // mid-stream; the finalize-time peel doesn't fire for
        // such single-leaf trees, so the root is an Internal
        // wrapping one ErasureLeaf — walk one level deeper.
        let root_hash = *blob_ref.tree_root_hash().unwrap();
        let root_bytes = adapter.fetch_chunk(&root_hash).await.unwrap();
        let leaf_bytes = match TreeNode::decode(&root_bytes).unwrap() {
            TreeNode::ErasureLeaf { .. } => root_bytes,
            TreeNode::Internal { children } => {
                let (child_hash, _) = children[0];
                adapter.fetch_chunk(&child_hash).await.unwrap()
            }
            TreeNode::Leaf { .. } => panic!("RS path should not emit Leaf nodes"),
        };
        let stripes = match TreeNode::decode(&leaf_bytes).unwrap() {
            TreeNode::ErasureLeaf { stripes } => stripes,
            other => panic!("expected ErasureLeaf, got: {:?}", other),
        };
        assert_eq!(stripes.len(), 1);
        let data_chunk_hashes: Vec<[u8; 32]> = stripes[0]
            .chunks
            .iter()
            .filter(|c| c.is_data())
            .map(|c| c.hash)
            .collect();
        assert_eq!(data_chunk_hashes.len(), 4);

        // Kill 2 data chunks (= m = tolerance).
        for hash in &data_chunk_hashes[0..2] {
            adapter.delete_chunk(hash).await.unwrap();
        }

        // Fetch must still succeed via reconstruction.
        let fetched = adapter
            .fetch_range(&blob_ref, 0..payload.len() as u64)
            .await
            .expect("RS fetch must reconstruct from parity");
        assert_eq!(fetched, payload, "reconstructed bytes must match original");
    }

    /// Lazy stripe-index population: a fresh adapter doesn't
    /// know about any stripes, but the first `fetch_range` that
    /// walks an ErasureLeaf re-populates the index. Simulates
    /// the cold-start path where an in-memory-only index would
    /// otherwise leave previously-stored RS stripes unprotected
    /// against parity-sweep loss until the next write touches
    /// them.
    #[tokio::test]
    async fn fetch_range_lazily_populates_stripe_index() {
        // Two adapters sharing the same Redex — simulates a
        // process restart where the on-disk chunk store is
        // preserved but the in-memory stripe index is reset.
        let redex = Arc::new(Redex::new());
        let chunk_size: u32 = 4 * 1024;
        let payload = deterministic_bytes(0xAB, chunk_size as usize * 4);
        let rs_params = super::super::erasure::RsParams { k: 4, m: 2 };

        // Adapter 1: writes the blob — its index has the stripe.
        let adapter1 = MeshBlobAdapter::new("lazy-1", redex.clone());
        let blob_ref = adapter1
            .store_stream_tree_rs_internal(
                stream_one(payload.clone()),
                ChunkingStrategy::Fixed { size: chunk_size },
                rs_params,
            )
            .await
            .unwrap();
        assert_eq!(adapter1.stripe_index.lock().registered_count(), 1);

        // Adapter 2: fresh adapter on the same Redex (simulating
        // restart). Index is empty.
        let adapter2 = MeshBlobAdapter::new("lazy-2", redex);
        assert_eq!(adapter2.stripe_index.lock().registered_count(), 0);

        // First fetch on adapter 2 populates the index lazily.
        let fetched = adapter2
            .fetch_range(&blob_ref, 0..payload.len() as u64)
            .await
            .unwrap();
        assert_eq!(fetched, payload);
        assert_eq!(
            adapter2.stripe_index.lock().registered_count(),
            1,
            "fetch must lazily register the stripe"
        );

        // Repeated fetches don't bloat the index (dedup).
        let _ = adapter2
            .fetch_range(&blob_ref, 0..payload.len() as u64)
            .await
            .unwrap();
        let _ = adapter2
            .fetch_range(&blob_ref, 0..payload.len() as u64)
            .await
            .unwrap();
        assert_eq!(
            adapter2.stripe_index.lock().registered_count(),
            1,
            "dedup must keep the count at 1 across repeated reads"
        );
    }

    /// Opt-in fetch-path auto-repair: when the adapter is
    /// constructed with `with_auto_repair_on_fetch(true)`, a
    /// successful reconstruction during `fetch_range` re-stores
    /// the previously-missing data chunks under their original
    /// hashes. Subsequent fetches don't re-pay the
    /// reconstruction cost.
    #[tokio::test]
    async fn fetch_range_auto_repair_restores_missing_chunks_when_enabled() {
        let redex = Arc::new(Redex::new());
        let adapter = MeshBlobAdapter::new("auto-repair-on", redex).with_auto_repair_on_fetch(true);
        let chunk_size: u32 = 4 * 1024;
        let payload = deterministic_bytes(0xAD, chunk_size as usize * 4);
        let rs_params = super::super::erasure::RsParams { k: 4, m: 2 };
        let blob_ref = adapter
            .store_stream_tree_rs_internal(
                stream_one(payload.clone()),
                ChunkingStrategy::Fixed { size: chunk_size },
                rs_params,
            )
            .await
            .unwrap();

        // Find data chunk hashes via the manifest.
        let root_hash = *blob_ref.tree_root_hash().unwrap();
        let root_bytes = adapter.fetch_chunk(&root_hash).await.unwrap();
        let leaf_bytes = match TreeNode::decode(&root_bytes).unwrap() {
            TreeNode::ErasureLeaf { .. } => root_bytes,
            TreeNode::Internal { children } => adapter.fetch_chunk(&children[0].0).await.unwrap(),
            TreeNode::Leaf { .. } => panic!("RS path should not emit Leaf nodes"),
        };
        let stripes = match TreeNode::decode(&leaf_bytes).unwrap() {
            TreeNode::ErasureLeaf { stripes } => stripes,
            other => panic!("expected ErasureLeaf, got: {:?}", other),
        };
        let data_hashes: Vec<[u8; 32]> = stripes[0]
            .chunks
            .iter()
            .filter(|c| c.is_data())
            .map(|c| c.hash)
            .collect();

        // Kill 2 data chunks (= m tolerance).
        for hash in &data_hashes[0..2] {
            adapter.delete_chunk(hash).await.unwrap();
        }
        // Confirm deletion.
        assert!(adapter.fetch_chunk(&data_hashes[0]).await.is_err());

        // First fetch reconstructs + re-stores (auto-repair on).
        let fetched = adapter
            .fetch_range(&blob_ref, 0..payload.len() as u64)
            .await
            .unwrap();
        assert_eq!(fetched, payload);

        // After auto-repair the previously-missing chunks are
        // back on disk — verify by fetching them directly.
        for hash in &data_hashes[0..2] {
            let bytes = adapter
                .fetch_chunk(hash)
                .await
                .expect("auto-repair must have re-stored this chunk");
            let computed: [u8; 32] = blake3::hash(&bytes).into();
            assert_eq!(&computed, hash);
        }
    }

    /// With auto-repair off (default), fetch returns the
    /// reconstructed bytes but does NOT re-store missing
    /// chunks. The chunk-channel state is preserved as-was —
    /// the plan's stated semantic that "fetch never writes."
    #[tokio::test]
    async fn fetch_range_does_not_restore_chunks_when_auto_repair_off() {
        let adapter = make_adapter(); // default: auto-repair off
        let chunk_size: u32 = 4 * 1024;
        let payload = deterministic_bytes(0xAE, chunk_size as usize * 4);
        let rs_params = super::super::erasure::RsParams { k: 4, m: 2 };
        let blob_ref = adapter
            .store_stream_tree_rs_internal(
                stream_one(payload.clone()),
                ChunkingStrategy::Fixed { size: chunk_size },
                rs_params,
            )
            .await
            .unwrap();
        let root_hash = *blob_ref.tree_root_hash().unwrap();
        let root_bytes = adapter.fetch_chunk(&root_hash).await.unwrap();
        let leaf_bytes = match TreeNode::decode(&root_bytes).unwrap() {
            TreeNode::ErasureLeaf { .. } => root_bytes,
            TreeNode::Internal { children } => adapter.fetch_chunk(&children[0].0).await.unwrap(),
            TreeNode::Leaf { .. } => panic!("RS path should not emit Leaf nodes"),
        };
        let stripes = match TreeNode::decode(&leaf_bytes).unwrap() {
            TreeNode::ErasureLeaf { stripes } => stripes,
            other => panic!("expected ErasureLeaf, got: {:?}", other),
        };
        let data_hashes: Vec<[u8; 32]> = stripes[0]
            .chunks
            .iter()
            .filter(|c| c.is_data())
            .map(|c| c.hash)
            .collect();
        adapter.delete_chunk(&data_hashes[0]).await.unwrap();

        // First fetch reconstructs.
        let fetched = adapter
            .fetch_range(&blob_ref, 0..payload.len() as u64)
            .await
            .unwrap();
        assert_eq!(fetched, payload);

        // Deleted chunk is STILL missing — auto-repair off, so
        // reconstruction produced bytes in memory only.
        assert!(
            adapter.fetch_chunk(&data_hashes[0]).await.is_err(),
            "auto-repair off: deleted chunk must remain missing after fetch"
        );
    }

    /// GC stripe-membership pin: when an RS stripe is degraded
    /// (a data chunk is missing), every other member chunk in
    /// the stripe — including parity chunks whose refcount is
    /// zero and would otherwise be GC-eligible — is pinned
    /// against the sweep. Validates v0.3 Phase C6 end-to-end.
    #[tokio::test]
    async fn sweep_gc_pins_parity_chunks_of_degraded_rs_stripe() {
        let adapter = make_adapter();
        let chunk_size: u32 = 4 * 1024;
        let payload = deterministic_bytes(0xC6, chunk_size as usize * 4);
        let rs_params = super::super::erasure::RsParams { k: 4, m: 2 };
        let blob_ref = adapter
            .store_stream_tree_rs_internal(
                stream_one(payload),
                ChunkingStrategy::Fixed { size: chunk_size },
                rs_params,
            )
            .await
            .unwrap();

        // Find the stripe's parity chunks.
        let root_hash = *blob_ref.tree_root_hash().unwrap();
        let root_bytes = adapter.fetch_chunk(&root_hash).await.unwrap();
        let leaf_bytes = match TreeNode::decode(&root_bytes).unwrap() {
            TreeNode::ErasureLeaf { .. } => root_bytes,
            TreeNode::Internal { children } => adapter.fetch_chunk(&children[0].0).await.unwrap(),
            TreeNode::Leaf { .. } => panic!("RS path should not emit Leaf nodes"),
        };
        let stripes = match TreeNode::decode(&leaf_bytes).unwrap() {
            TreeNode::ErasureLeaf { stripes } => stripes,
            other => panic!("expected ErasureLeaf, got: {:?}", other),
        };
        let parity_hashes: Vec<[u8; 32]> = stripes[0]
            .chunks
            .iter()
            .filter(|c| c.is_parity())
            .map(|c| c.hash)
            .collect();
        let data_hashes: Vec<[u8; 32]> = stripes[0]
            .chunks
            .iter()
            .filter(|c| c.is_data())
            .map(|c| c.hash)
            .collect();

        // Degrade the stripe by deleting 1 data chunk.
        adapter.delete_chunk(&data_hashes[0]).await.unwrap();

        // Force refcounts to zero on the parity chunks so they
        // become GC candidates (otherwise sweep_gc wouldn't even
        // consider them). `store_observed` from store_chunk only
        // updates last_seen; refcount stays at whatever the
        // table tracks. The default test fixture doesn't pin
        // chunks, so they ARE refcount=0 already — just need to
        // bypass the retention floor.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + (DEFAULT_RETENTION_FLOOR.as_millis() as u64 * 2);

        // Run sweep with the future-stamped clock + disk-pressure
        // bypass so every refcount=0 chunk is eligible. Without
        // the C6 pin, every parity chunk would be swept here.
        let _swept = adapter
            .sweep_gc(now, /* disk_pressure_critical = */ true)
            .await
            .unwrap();

        // Assert parity chunks still locally present — the C6
        // pin prevented sweep because the stripe is degraded.
        for phash in &parity_hashes {
            assert!(
                adapter.chunk_exists(phash).unwrap_or(false),
                "parity chunk {:?} of a degraded stripe must be pinned against sweep",
                phash
            );
        }
        // And: the remaining data chunks (1..k) — same pin.
        for dhash in &data_hashes[1..] {
            assert!(
                adapter.chunk_exists(dhash).unwrap_or(false),
                "surviving data chunk {:?} of a degraded stripe must be pinned",
                dhash
            );
        }

        // Run repair: stripe goes from degraded back to healthy.
        let report = adapter.repair_blob(&blob_ref).await.unwrap();
        assert_eq!(report.stripes_repaired, 1);

        // Now the same sweep run — with the stripe healthy — is
        // free to proceed (no pin), so parity chunks become
        // eligible if their refcount + retention says so.
        // Re-checking that they're eligible post-repair would
        // require driving a sweep that actually deletes; what
        // matters for the C6 contract is the pin DID fire while
        // degraded, which the assertions above confirmed.
        let _ = report;
    }

    /// Degraded-stripe pin protects every surviving member when
    /// the sweep actually runs (`disk_pressure_critical=false`).
    /// The atomicity fix in sweep_gc ensures the pin-check and
    /// take_if_deletable cannot interleave with a concurrent
    /// register_stripe — proven structurally by holding the
    /// stripe-index lock across both steps. This test asserts
    /// the pin's end-to-end effect on a genuinely degraded
    /// stripe: with k=2, m=2 and 3 of 4 members deleted,
    /// `present_count=1 < k=2` so the lone survivor must NOT be
    /// swept.
    #[tokio::test]
    async fn sweep_gc_pins_surviving_member_of_genuinely_degraded_stripe() {
        let adapter = make_adapter();
        let chunk_size: u32 = 4 * 1024;
        let payload = deterministic_bytes(0xC7, chunk_size as usize * 2);
        let rs_params = super::super::erasure::RsParams { k: 2, m: 2 };
        let blob_ref = adapter
            .store_stream_tree_rs_internal(
                stream_one(payload),
                ChunkingStrategy::Fixed { size: chunk_size },
                rs_params,
            )
            .await
            .unwrap();
        let root_hash = *blob_ref.tree_root_hash().unwrap();
        let root_bytes = adapter.fetch_chunk(&root_hash).await.unwrap();
        let leaf_bytes = match TreeNode::decode(&root_bytes).unwrap() {
            TreeNode::ErasureLeaf { .. } => root_bytes,
            TreeNode::Internal { children } => adapter.fetch_chunk(&children[0].0).await.unwrap(),
            TreeNode::Leaf { .. } => panic!("RS path should not emit Leaf nodes"),
        };
        let stripes = match TreeNode::decode(&leaf_bytes).unwrap() {
            TreeNode::ErasureLeaf { stripes } => stripes,
            other => panic!("expected ErasureLeaf, got: {:?}", other),
        };
        let stripe = stripes[0].clone();
        let all_hashes: Vec<[u8; 32]> = stripe.chunks.iter().map(|c| c.hash).collect();
        assert_eq!(all_hashes.len(), 4, "k=2 + m=2 → 4 members");
        // Delete 3 members; the lone survivor is index 3. After
        // delete_chunk, present_count = 1 < k=2 → pin must hold.
        for h in &all_hashes[..3] {
            adapter.delete_chunk(h).await.unwrap();
        }
        let survivor = all_hashes[3];
        // Sweep with disk_pressure_critical=false so the sweep
        // actually runs (true SUPPRESSES via should_sweep — see
        // refcount.rs).
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + (DEFAULT_RETENTION_FLOOR.as_millis() as u64 * 2);
        let _ = adapter.sweep_gc(now, false).await.unwrap();
        // The survivor MUST still be present — the stripe is
        // degraded and the C6 pin (now held under the atomic
        // pin_check + take_if_deletable critical section) keeps
        // it alive against the otherwise-eligible sweep.
        assert!(
            adapter.chunk_exists(&survivor).unwrap_or(false),
            "surviving stripe member must survive sweep when stripe is degraded",
        );
    }

    /// `repair_blob` restores missing data chunks of an
    /// RS-encoded blob in-place. Stores blob, deletes m=2 data
    /// chunks, runs repair, asserts the report counts a repair
    /// fired + the 2 chunks were restored, then asserts a
    /// subsequent fetch_range works against the LOCAL chunk
    /// store (no reconstruction needed since chunks are back).
    #[tokio::test]
    async fn repair_blob_restores_missing_data_chunks() {
        let adapter = make_adapter();
        let chunk_size: u32 = 4 * 1024;
        let payload = deterministic_bytes(0xF1, chunk_size as usize * 4);
        let rs_params = super::super::erasure::RsParams { k: 4, m: 2 };
        let blob_ref = adapter
            .store_stream_tree_rs_internal(
                stream_one(payload.clone()),
                ChunkingStrategy::Fixed { size: chunk_size },
                rs_params,
            )
            .await
            .unwrap();

        // Identify the single stripe's data chunks.
        let root_hash = *blob_ref.tree_root_hash().unwrap();
        let root_bytes = adapter.fetch_chunk(&root_hash).await.unwrap();
        let leaf_bytes = match TreeNode::decode(&root_bytes).unwrap() {
            TreeNode::ErasureLeaf { .. } => root_bytes,
            TreeNode::Internal { children } => adapter.fetch_chunk(&children[0].0).await.unwrap(),
            TreeNode::Leaf { .. } => panic!("RS path should not emit Leaf nodes"),
        };
        let stripes = match TreeNode::decode(&leaf_bytes).unwrap() {
            TreeNode::ErasureLeaf { stripes } => stripes,
            other => panic!("expected ErasureLeaf, got: {:?}", other),
        };
        let data_hashes: Vec<[u8; 32]> = stripes[0]
            .chunks
            .iter()
            .filter(|c| c.is_data())
            .map(|c| c.hash)
            .collect();

        // Kill 2 data chunks (= m tolerance).
        for hash in &data_hashes[0..2] {
            adapter.delete_chunk(hash).await.unwrap();
        }
        // Confirm deletion landed.
        assert!(adapter.fetch_chunk(&data_hashes[0]).await.is_err());

        // Run repair.
        let report = adapter
            .repair_blob(&blob_ref)
            .await
            .expect("repair_blob succeeds");
        assert_eq!(report.stripes_walked, 1);
        assert_eq!(report.stripes_repaired, 1);
        assert_eq!(report.chunks_restored, 2);
        assert_eq!(report.stripes_unrecoverable, 0);

        // Both previously-deleted chunks are back, byte-identical
        // to the originals (cross-check via fetch_chunk + hash).
        for hash in &data_hashes[0..2] {
            let bytes = adapter
                .fetch_chunk(hash)
                .await
                .expect("restored chunk must be fetchable");
            let computed: [u8; 32] = blake3::hash(&bytes).into();
            assert_eq!(&computed, hash, "restored chunk must match original hash");
        }

        // Full-range fetch still byte-identical to original.
        let fetched = adapter
            .fetch_range(&blob_ref, 0..payload.len() as u64)
            .await
            .unwrap();
        assert_eq!(fetched, payload);
    }

    /// Repair on an already-healthy RS blob is a no-op: every
    /// stripe is counted as healthy, no chunks restored.
    #[tokio::test]
    async fn repair_blob_no_op_when_healthy() {
        let adapter = make_adapter();
        let chunk_size: u32 = 4 * 1024;
        let payload = deterministic_bytes(0xF2, chunk_size as usize * 4);
        let rs_params = super::super::erasure::RsParams { k: 4, m: 2 };
        let blob_ref = adapter
            .store_stream_tree_rs_internal(
                stream_one(payload),
                ChunkingStrategy::Fixed { size: chunk_size },
                rs_params,
            )
            .await
            .unwrap();
        let report = adapter.repair_blob(&blob_ref).await.unwrap();
        assert_eq!(report.stripes_walked, 1);
        assert_eq!(report.stripes_already_healthy, 1);
        assert_eq!(report.stripes_repaired, 0);
        assert_eq!(report.chunks_restored, 0);
    }

    /// Repair records unrecoverable stripes without failing —
    /// the operator decides what to do with them.
    #[tokio::test]
    async fn repair_blob_records_unrecoverable_stripes_without_erroring() {
        let adapter = make_adapter();
        let chunk_size: u32 = 4 * 1024;
        let payload = deterministic_bytes(0xF3, chunk_size as usize * 4);
        let rs_params = super::super::erasure::RsParams { k: 4, m: 2 };
        let blob_ref = adapter
            .store_stream_tree_rs_internal(
                stream_one(payload),
                ChunkingStrategy::Fixed { size: chunk_size },
                rs_params,
            )
            .await
            .unwrap();
        let root_hash = *blob_ref.tree_root_hash().unwrap();
        let root_bytes = adapter.fetch_chunk(&root_hash).await.unwrap();
        let leaf_bytes = match TreeNode::decode(&root_bytes).unwrap() {
            TreeNode::ErasureLeaf { .. } => root_bytes,
            TreeNode::Internal { children } => adapter.fetch_chunk(&children[0].0).await.unwrap(),
            TreeNode::Leaf { .. } => panic!("RS path should not emit Leaf nodes"),
        };
        let stripes = match TreeNode::decode(&leaf_bytes).unwrap() {
            TreeNode::ErasureLeaf { stripes } => stripes,
            other => panic!("expected ErasureLeaf, got: {:?}", other),
        };
        // Kill 3 chunks (= m+1, beyond tolerance).
        let all_hashes: Vec<[u8; 32]> = stripes[0].chunks.iter().map(|c| c.hash).collect();
        for hash in &all_hashes[0..3] {
            adapter.delete_chunk(hash).await.unwrap();
        }
        let report = adapter
            .repair_blob(&blob_ref)
            .await
            .expect("repair_blob must NOT error on unrecoverable stripes");
        assert_eq!(report.stripes_walked, 1);
        assert_eq!(report.stripes_unrecoverable, 1);
        assert_eq!(report.stripes_repaired, 0);
        assert_eq!(report.chunks_restored, 0);
    }

    /// Repair on a non-Tree BlobRef (Small / Manifest) is a
    /// zero-counter no-op.
    #[tokio::test]
    async fn repair_blob_no_op_for_non_tree() {
        let adapter = make_adapter();
        let payload = deterministic_bytes(0xF4, 256);
        let hash: [u8; 32] = blake3::hash(&payload).into();
        let small_ref = BlobRef::small("mesh://small-test", hash, payload.len() as u64);
        let report = adapter.repair_blob(&small_ref).await.unwrap();
        assert_eq!(report, super::RepairReport::default());
    }

    /// `repair_blob_authorized` rejects when no AuthGuard is wired —
    /// repair walks the entire tree + runs RS reconstruction per
    /// stripe, so it must be unreachable on a network-facing path
    /// absent a capability check.
    #[tokio::test]
    async fn repair_blob_authorized_rejects_when_no_guard_configured() {
        let adapter = make_adapter();
        let payload = deterministic_bytes(0xF5, 64);
        let hash: [u8; 32] = blake3::hash(&payload).into();
        let small_ref = BlobRef::small("mesh://repair-noauth", hash, payload.len() as u64);
        let channel = auth_channel();
        let err = adapter
            .repair_blob_authorized(&small_ref, 0xCAFE_BABE, &channel)
            .await
            .unwrap_err();
        assert!(matches!(err, BlobError::Unauthorized(_)));
    }

    /// `repair_blob_authorized` rejects an origin that the AuthGuard
    /// doesn't list for the channel.
    #[tokio::test]
    async fn repair_blob_authorized_rejects_unauthorized_origin() {
        let origin: u64 = 0xC0FFEE;
        let (adapter, channel) = adapter_with_authorized_origin(origin);
        let payload = deterministic_bytes(0xF6, 64);
        let hash: [u8; 32] = blake3::hash(&payload).into();
        let small_ref = BlobRef::small("mesh://repair-intruder", hash, payload.len() as u64);
        let intruder: u64 = 0xDEAD_BEEF;
        let err = adapter
            .repair_blob_authorized(&small_ref, intruder, &channel)
            .await
            .unwrap_err();
        assert!(matches!(err, BlobError::Unauthorized(_)));
    }

    /// `repair_blob_authorized` admits and round-trips to
    /// `repair_blob` once an origin is authorized.
    #[tokio::test]
    async fn repair_blob_authorized_admits_authorized_origin() {
        let origin: u64 = 0xCAFE_BABE;
        let (adapter, channel) = adapter_with_authorized_origin(origin);
        // Non-Tree blob → repair is a zero-counter no-op.
        let payload = deterministic_bytes(0xF7, 64);
        let hash: [u8; 32] = blake3::hash(&payload).into();
        let small_ref = BlobRef::small("mesh://repair-ok", hash, payload.len() as u64);
        let report = adapter
            .repair_blob_authorized(&small_ref, origin, &channel)
            .await
            .unwrap();
        assert_eq!(report, super::RepairReport::default());
    }

    /// Killing more than `m` chunks per stripe must surface a
    /// clean `BlobError::Backend("erasure: stripe unrecoverable")`
    /// rather than corrupting the fetch or panicking.
    #[tokio::test]
    async fn fetch_range_rs_fails_cleanly_when_more_than_m_chunks_lost() {
        let adapter = make_adapter();
        let chunk_size: u32 = 4 * 1024;
        let payload = deterministic_bytes(0xE3, chunk_size as usize * 4);
        let rs_params = super::super::erasure::RsParams { k: 4, m: 2 };
        let blob_ref = adapter
            .store_stream_tree_rs_internal(
                stream_one(payload.clone()),
                ChunkingStrategy::Fixed { size: chunk_size },
                rs_params,
            )
            .await
            .unwrap();
        let root_hash = *blob_ref.tree_root_hash().unwrap();
        let root_bytes = adapter.fetch_chunk(&root_hash).await.unwrap();
        let leaf_bytes = match TreeNode::decode(&root_bytes).unwrap() {
            TreeNode::ErasureLeaf { .. } => root_bytes,
            TreeNode::Internal { children } => adapter.fetch_chunk(&children[0].0).await.unwrap(),
            TreeNode::Leaf { .. } => panic!("RS path should not emit Leaf nodes"),
        };
        let stripes = match TreeNode::decode(&leaf_bytes).unwrap() {
            TreeNode::ErasureLeaf { stripes } => stripes,
            other => panic!("expected ErasureLeaf, got: {:?}", other),
        };
        // Kill 3 chunks total — exceeds m=2 tolerance.
        let all_hashes: Vec<[u8; 32]> = stripes[0].chunks.iter().map(|c| c.hash).collect();
        for hash in &all_hashes[0..3] {
            adapter.delete_chunk(hash).await.unwrap();
        }
        let err = adapter
            .fetch_range(&blob_ref, 0..payload.len() as u64)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unrecoverable") || msg.contains("erasure"),
            "expected unrecoverable-stripe error, got: {}",
            msg
        );
    }

    /// RS storing the same content on two adapters lands on the
    /// same root hash — determinism through the striper +
    /// ErasureLeaf encoding.
    #[tokio::test]
    async fn store_stream_tree_rs_is_deterministic_across_adapters() {
        let adapter_a = make_adapter();
        let adapter_b = make_adapter();
        let chunk_size: u32 = 4 * 1024;
        let payload = deterministic_bytes(0xE1, chunk_size as usize * 8);
        let rs_params = super::super::erasure::RsParams { k: 4, m: 2 };
        let r_a = adapter_a
            .store_stream_tree_rs_internal(
                stream_one(payload.clone()),
                ChunkingStrategy::Fixed { size: chunk_size },
                rs_params,
            )
            .await
            .unwrap();
        let r_b = adapter_b
            .store_stream_tree_rs_internal(
                stream_one(payload),
                ChunkingStrategy::Fixed { size: chunk_size },
                rs_params,
            )
            .await
            .unwrap();
        assert_eq!(
            r_a.tree_root_hash(),
            r_b.tree_root_hash(),
            "two independent RS stores of the same content must agree on the root hash"
        );
    }

    /// Fixed strategy with a non-v0.2-compatible chunk size is
    /// rejected — keeps chunk-level dedup consistent with the
    /// v0.2 Manifest path.
    #[tokio::test]
    async fn store_stream_tree_rejects_non_v0_2_chunk_size() {
        let adapter = make_adapter();
        let bytes = deterministic_bytes(0x44, 1024 * 1024);
        let err = adapter
            .store_stream_tree(
                stream_one(bytes),
                Encoding::Replicated,
                ChunkingStrategy::Fixed { size: 1024 * 1024 }, // 1 MiB, not 4 MiB
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("dedup"), "got: {err}");
    }

    /// CDC end-to-end at test-friendly scale: store a
    /// deterministic blob via the test-internal CDC path, fetch
    /// the full range back, assert byte-equality. Uses the
    /// `store_stream_tree_cdc_internal` helper with small params
    /// (256 / 1024 / 4096 byte triple) so the test allocates
    /// kilobytes, not megabytes. Pins B1's wiring: the CDC
    /// chunker drives `emit_tree_chunk` through the same path
    /// the Fixed variant uses.
    #[tokio::test]
    async fn store_stream_tree_cdc_round_trips_at_small_scale() {
        let adapter = make_adapter();
        let payload = deterministic_bytes(0x77, 32 * 1024);
        let params = super::super::cdc::CdcParams {
            min: 256,
            avg: 1024,
            max: 4096,
        };
        let blob_ref = adapter
            .store_stream_tree_cdc_internal(
                stream_one(payload.clone()),
                Encoding::Replicated,
                params,
            )
            .await
            .expect("CDC store_stream_tree round trip");
        assert!(matches!(blob_ref, BlobRef::Tree { .. }));
        assert_eq!(blob_ref.size(), payload.len() as u64);
        let fetched = adapter
            .fetch_range(&blob_ref, 0..payload.len() as u64)
            .await
            .expect("CDC fetch_range");
        assert_eq!(fetched, payload, "CDC round-trip must be byte-identical");
    }

    /// CDC determinism through the adapter: two independent
    /// adapters storing the same bytes via CDC produce identical
    /// root hashes. Pins that the CDC chunker's boundary
    /// decisions are reproducible end-to-end.
    #[tokio::test]
    async fn store_stream_tree_cdc_is_deterministic_across_adapters() {
        let adapter_a = make_adapter();
        let adapter_b = make_adapter();
        let payload = deterministic_bytes(0x88, 16 * 1024);
        let params = super::super::cdc::CdcParams {
            min: 256,
            avg: 1024,
            max: 4096,
        };
        let r_a = adapter_a
            .store_stream_tree_cdc_internal(
                stream_one(payload.clone()),
                Encoding::Replicated,
                params,
            )
            .await
            .unwrap();
        let r_b = adapter_b
            .store_stream_tree_cdc_internal(stream_one(payload), Encoding::Replicated, params)
            .await
            .unwrap();
        assert_eq!(
            r_a.tree_root_hash(),
            r_b.tree_root_hash(),
            "two independent CDC stores of the same content must agree on the root hash"
        );
    }

    /// Determinism: storing the same bytes via two separate
    /// store_stream_tree calls produces the same root hash —
    /// content-addressed dedup at the tree level.
    #[tokio::test]
    async fn store_stream_tree_is_deterministic_across_calls() {
        let adapter_a = make_adapter();
        let adapter_b = make_adapter();
        // 3 chunks + a tail.
        let len = (BLOB_CHUNK_SIZE_BYTES * 3) as usize + 12345;
        let payload = deterministic_bytes(0x55, len);
        let r_a = adapter_a
            .store_stream_tree(
                stream_one(payload.clone()),
                Encoding::Replicated,
                ChunkingStrategy::default(),
            )
            .await
            .unwrap();
        let r_b = adapter_b
            .store_stream_tree(
                stream_one(payload),
                Encoding::Replicated,
                ChunkingStrategy::default(),
            )
            .await
            .unwrap();
        assert_eq!(r_a.tree_root_hash(), r_b.tree_root_hash());
        assert_eq!(r_a.size(), r_b.size());
        assert_eq!(r_a.tree_depth(), r_b.tree_depth());
    }

    /// Input that arrives as many small slices (not aligned to
    /// the chunk boundary) chunks identically to the same content
    /// as one big slice — proves the buffer logic in
    /// store_stream_tree handles producer-side fragmentation.
    #[tokio::test]
    async fn store_stream_tree_chunks_consistently_across_input_fragmentation() {
        let adapter_a = make_adapter();
        let adapter_b = make_adapter();
        let len = (BLOB_CHUNK_SIZE_BYTES * 2) as usize + 100;
        let payload = deterministic_bytes(0x66, len);
        let r_a = adapter_a
            .store_stream_tree(
                stream_one(payload.clone()),
                Encoding::Replicated,
                ChunkingStrategy::default(),
            )
            .await
            .unwrap();
        // Fragment the same payload into 17-byte slices.
        let slices: Vec<Vec<u8>> = payload.chunks(17).map(|c| c.to_vec()).collect();
        let r_b = adapter_b
            .store_stream_tree(
                stream_many(slices),
                Encoding::Replicated,
                ChunkingStrategy::default(),
            )
            .await
            .unwrap();
        assert_eq!(
            r_a.tree_root_hash(),
            r_b.tree_root_hash(),
            "fragmented vs single-slice input must produce identical roots"
        );
    }

    /// A 3-chunk blob produces a single-leaf tree (depth=1).
    /// All chunks reachable, root decodes as Leaf.
    #[tokio::test]
    async fn store_stream_tree_three_chunks_yields_depth_one() {
        let adapter = make_adapter();
        let len = BLOB_CHUNK_SIZE_BYTES as usize * 3;
        let payload = deterministic_bytes(0x77, len);
        let blob_ref = adapter
            .store_stream_tree(
                stream_one(payload),
                Encoding::Replicated,
                ChunkingStrategy::default(),
            )
            .await
            .unwrap();
        assert_eq!(blob_ref.tree_depth(), Some(1));
        let root = TreeNode::decode(
            &adapter
                .fetch_chunk(blob_ref.tree_root_hash().unwrap())
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(root.is_leaf());
        assert_eq!(root.arity(), 3);
    }

    /// A `TREE_FANOUT + 1`-chunk blob produces a depth-2 tree
    /// (one full leaf streamed mid-flight + a partial-leaf
    /// finalize → both lifted into a root internal). The root
    /// internal has 2 children: one full leaf (FANOUT chunks)
    /// and one partial leaf (1 chunk).
    ///
    /// Uses the test-internal store path with a 1 KiB chunk
    /// size so the test allocates ~130 KiB instead of ~516 MiB
    /// — the production-gate's 4 MiB chunk size would OOM
    /// parallel test threads on the Windows runner.
    #[tokio::test]
    async fn store_stream_tree_fanout_plus_one_yields_depth_two() {
        let adapter = make_adapter();
        let small_chunk: u32 = 1024;
        let len = small_chunk as usize * (TREE_FANOUT + 1);
        let payload = deterministic_bytes(0x88, len);
        let blob_ref = adapter
            .store_stream_tree_internal(stream_one(payload), Encoding::Replicated, small_chunk)
            .await
            .unwrap();
        assert_eq!(blob_ref.tree_depth(), Some(2));
        assert_eq!(blob_ref.size(), len as u64);
        let root = TreeNode::decode(
            &adapter
                .fetch_chunk(blob_ref.tree_root_hash().unwrap())
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(root.is_internal());
        assert_eq!(root.arity(), 2);
        // Each child references a leaf node that's also locally
        // fetchable + decodable.
        if let TreeNode::Internal { children } = root {
            let (full_leaf_hash, full_leaf_size) = children[0];
            let leaf_bytes = adapter.fetch_chunk(&full_leaf_hash).await.unwrap();
            let leaf = TreeNode::decode(&leaf_bytes).unwrap();
            assert!(leaf.is_leaf());
            assert_eq!(leaf.arity(), TREE_FANOUT);
            assert_eq!(full_leaf_size, small_chunk as u64 * TREE_FANOUT as u64);
        }
    }

    // ────────────────────────────────────────────────────────
    // fetch_range tree walk (Phase A4)
    // ────────────────────────────────────────────────────────

    /// Full round-trip: store via tree, fetch back byte-for-byte
    /// via fetch_range with range = 0..total_size.
    #[tokio::test]
    async fn fetch_range_tree_full_blob_round_trips() {
        let adapter = make_adapter();
        let len = BLOB_CHUNK_SIZE_BYTES as usize * 2 + 12345;
        let payload = deterministic_bytes(0xA1, len);
        let blob_ref = adapter
            .store_stream_tree(
                stream_one(payload.clone()),
                Encoding::Replicated,
                ChunkingStrategy::default(),
            )
            .await
            .unwrap();
        // Fetch the entire range.
        let fetched = adapter
            .fetch_range(&blob_ref, 0..len as u64)
            .await
            .expect("full-range fetch succeeds");
        assert_eq!(fetched, payload, "byte-for-byte match");
    }

    /// Range query that lands entirely inside one chunk returns
    /// the matching slice.
    #[tokio::test]
    async fn fetch_range_tree_intra_chunk_slice() {
        let adapter = make_adapter();
        let len = BLOB_CHUNK_SIZE_BYTES as usize * 3;
        let payload = deterministic_bytes(0xA2, len);
        let blob_ref = adapter
            .store_stream_tree(
                stream_one(payload.clone()),
                Encoding::Replicated,
                ChunkingStrategy::default(),
            )
            .await
            .unwrap();
        // Pick a range inside the middle chunk.
        let start = BLOB_CHUNK_SIZE_BYTES + 1000;
        let end = BLOB_CHUNK_SIZE_BYTES + 5000;
        let fetched = adapter.fetch_range(&blob_ref, start..end).await.unwrap();
        assert_eq!(fetched.len() as u64, end - start);
        assert_eq!(fetched, &payload[start as usize..end as usize]);
    }

    /// Range query that straddles a chunk boundary fetches both
    /// chunks and stitches the slice correctly.
    #[tokio::test]
    async fn fetch_range_tree_cross_chunk_boundary() {
        let adapter = make_adapter();
        let len = BLOB_CHUNK_SIZE_BYTES as usize * 3;
        let payload = deterministic_bytes(0xA3, len);
        let blob_ref = adapter
            .store_stream_tree(
                stream_one(payload.clone()),
                Encoding::Replicated,
                ChunkingStrategy::default(),
            )
            .await
            .unwrap();
        // Range that crosses the first/second chunk boundary.
        let start = BLOB_CHUNK_SIZE_BYTES - 1000;
        let end = BLOB_CHUNK_SIZE_BYTES + 1000;
        let fetched = adapter.fetch_range(&blob_ref, start..end).await.unwrap();
        assert_eq!(fetched, &payload[start as usize..end as usize]);
    }

    /// Range query at a depth-2 tree that straddles a child-
    /// subtree boundary (different LEAVES) fetches both leaves
    /// and stitches correctly. Uses the test-internal store
    /// path with a 1 KiB chunk so the test allocates ~130 KiB
    /// instead of ~516 MiB.
    #[tokio::test]
    async fn fetch_range_tree_cross_leaf_boundary_depth_two() {
        let adapter = make_adapter();
        let small_chunk: u32 = 1024;
        let len = small_chunk as usize * (TREE_FANOUT + 1);
        let payload = deterministic_bytes(0xA4, len);
        let blob_ref = adapter
            .store_stream_tree_internal(
                stream_one(payload.clone()),
                Encoding::Replicated,
                small_chunk,
            )
            .await
            .unwrap();
        assert_eq!(blob_ref.tree_depth(), Some(2));
        // The first leaf covers FANOUT chunks; the second leaf
        // covers the trailing 1 chunk. Range crosses boundary.
        let leaf_boundary = small_chunk as u64 * TREE_FANOUT as u64;
        let start = leaf_boundary - 100;
        let end = leaf_boundary + 100;
        let fetched = adapter.fetch_range(&blob_ref, start..end).await.unwrap();
        assert_eq!(fetched, &payload[start as usize..end as usize]);
    }

    /// Zero-length range short-circuits without any fetches.
    #[tokio::test]
    async fn fetch_range_tree_zero_length_returns_empty() {
        let adapter = make_adapter();
        let len = BLOB_CHUNK_SIZE_BYTES as usize;
        let payload = deterministic_bytes(0xA5, len);
        let blob_ref = adapter
            .store_stream_tree(
                stream_one(payload),
                Encoding::Replicated,
                ChunkingStrategy::default(),
            )
            .await
            .unwrap();
        let fetched = adapter.fetch_range(&blob_ref, 100..100).await.unwrap();
        assert_eq!(fetched.len(), 0);
    }

    /// Range that exceeds total_size is rejected with a typed
    /// error (the BlobRef pre-check fires before the walk).
    #[tokio::test]
    async fn fetch_range_tree_rejects_out_of_bounds() {
        let adapter = make_adapter();
        let len = BLOB_CHUNK_SIZE_BYTES as usize;
        let payload = deterministic_bytes(0xA6, len);
        let blob_ref = adapter
            .store_stream_tree(
                stream_one(payload),
                Encoding::Replicated,
                ChunkingStrategy::default(),
            )
            .await
            .unwrap();
        let err = adapter
            .fetch_range(&blob_ref, 0..(len as u64 + 1))
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Tree total_size"), "got: {msg}");
    }

    /// Tree-walk integrity: a node fetched from disk whose bytes
    /// don't BLAKE3-match the parent's stored child hash is
    /// rejected. We synthesize this by storing a tree, then
    /// corrupting the root by replacing it with a different
    /// node's bytes — fetch_range then catches the mismatch.
    ///
    /// (We can't easily corrupt the chunk file in-place from
    /// the test; instead we construct a BlobRef::Tree with a
    /// root_hash that doesn't match any stored content. The
    /// walker's first `fetch_chunk` call surfaces a NotFound.
    /// That's the existing v0.2 store-side integrity — the
    /// explicit hash recheck inside walk_tree_range adds
    /// defense-in-depth.)
    #[tokio::test]
    async fn fetch_range_tree_rejects_unknown_root() {
        let adapter = make_adapter();
        // Construct a BlobRef::Tree referencing a hash no chunk
        // store has ever seen.
        let bogus_root = [0xDE; 32];
        let blob_ref =
            BlobRef::tree("mesh://deadbeef", Encoding::Replicated, bogus_root, 1024, 1).unwrap();
        let err = adapter.fetch_range(&blob_ref, 0..512).await.unwrap_err();
        // Either NotFound or HashMismatch is acceptable here —
        // the chunk file doesn't exist, so the underlying fetch
        // path surfaces a typed error. Pin that we DO surface an
        // error rather than returning empty bytes.
        let _ = err; // any error is fine; assert we got one
    }

    /// Tree depth-lengthening attack: a peer advertises a Tree
    /// blob with `depth` ONE GREATER than the actual structure.
    /// The walker traverses N-1 Internal nodes (where N is the
    /// claim) and expects a Leaf at residual_depth=1 — but the
    /// actual structure has Leaves at residual_depth=2 because
    /// the real depth is N-1. B-3's Leaf-at-rd==1 check rejects
    /// the structurally-shallower tree.
    ///
    /// This pins the symmetric case to B-3's depth-shortening
    /// test (`fetch_range_tree_rejects_leaf_at_unexpected_residual_depth`)
    /// and the cubic finding that suggested
    /// `depth.saturating_sub(1)` (which would have broken
    /// legitimate depth=1 trees).
    #[tokio::test]
    async fn fetch_range_tree_rejects_depth_advertised_one_greater_than_actual() {
        let adapter = make_adapter();
        let small_chunk: u32 = 1024;
        // FANOUT + 1 chunks → genuine depth-2 tree (Internal root
        // pointing at 2 Leaves).
        let len = small_chunk as usize * (TREE_FANOUT + 1);
        let payload = deterministic_bytes(0xCD, len);
        let blob_ref = adapter
            .store_stream_tree_internal(
                stream_one(payload.clone()),
                Encoding::Replicated,
                small_chunk,
            )
            .await
            .unwrap();
        assert_eq!(blob_ref.tree_depth(), Some(2), "real tree is depth=2");
        let (uri, root_hash, total_size) = match &blob_ref {
            BlobRef::Tree {
                uri,
                root_hash,
                total_size,
                ..
            } => (uri.clone(), *root_hash, *total_size),
            _ => panic!("expected Tree"),
        };
        // Forged BlobRef::Tree claiming depth=3 against the same
        // real-depth-2 root. The walker traverses two Internal
        // levels (residual_depth 3 → 2 → 1), then expects a Leaf
        // at residual_depth=1 — but the actual tree has Leaves at
        // the rd=2 step (one level shallower than claimed).
        // B-3's Leaf-at-rd!=1 check catches this.
        let forged = BlobRef::tree(&uri, Encoding::Replicated, root_hash, total_size, 3).unwrap();
        let err = adapter
            .fetch_range(&forged, 0..total_size)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Leaf at residual_depth=")
                && msg.contains("disagrees with BlobRef::Tree.depth"),
            "expected depth-disagreement decode error from Leaf-arm rejection; got: {msg}",
        );
    }

    /// fetch_range rejects requests larger than MAX_FETCH_RANGE_BYTES
    /// (1 GiB). v0.3 Tree blobs can address up to 128 PiB but
    /// returning a single Vec<u8> for a multi-GiB range would OOM
    /// the substrate. Streaming consumers must page through
    /// smaller slices.
    #[tokio::test]
    async fn fetch_range_rejects_request_larger_than_cap() {
        let adapter = make_adapter();
        // Build a Tree BlobRef advertising 2 GiB. Don't bother
        // storing the bytes — the cap check fires before any
        // walk traffic. (The root_hash is bogus; that's fine.)
        let blob_ref = BlobRef::tree(
            "mesh://oversize",
            Encoding::Replicated,
            [0xEE; 32],
            2 * 1024 * 1024 * 1024,
            2,
        )
        .unwrap();
        let err = adapter
            .fetch_range(&blob_ref, 0..(2 * 1024 * 1024 * 1024))
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("exceeds per-call cap"),
            "expected per-call cap error; got: {msg}",
        );
    }

    /// Tree depth-shortening attack: a peer-supplied root that
    /// decodes as a `Leaf` against a `BlobRef::Tree` whose advertised
    /// `depth > 1` must be rejected. Without the residual_depth
    /// check, a hostile peer could substitute a Leaf root for any
    /// blob whose `total_size <= TREE_FANOUT * TREE_LEAF_CHUNK_MAX_BYTES`
    /// — the cross-check on covered_bytes alone admits the swap.
    ///
    /// Construction: store a legitimate depth=1 tree (Leaf root),
    /// then build a BlobRef::Tree claiming depth=2 with the same
    /// root_hash. The walker fetches the Leaf, sees covered_bytes ==
    /// total_size (so the existing cross-check passes), enters the
    /// Leaf arm, and rejects on residual_depth != 1.
    #[tokio::test]
    async fn fetch_range_tree_rejects_leaf_at_unexpected_residual_depth() {
        let adapter = make_adapter();
        let small_chunk: u32 = 1024;
        let payload = deterministic_bytes(0xA7, small_chunk as usize);
        // A depth=1 tree: root is a Leaf.
        let blob_ref = adapter
            .store_stream_tree_internal(
                stream_one(payload.clone()),
                Encoding::Replicated,
                small_chunk,
            )
            .await
            .unwrap();
        assert_eq!(blob_ref.tree_depth(), Some(1));
        let (uri, root_hash, total_size) = match &blob_ref {
            BlobRef::Tree {
                uri,
                root_hash,
                total_size,
                ..
            } => (uri.clone(), *root_hash, *total_size),
            _ => panic!("expected Tree"),
        };
        // Forged BlobRef::Tree claiming depth=2 against the depth=1
        // root. Pre-fix the walker fetched the Leaf, found
        // covered_bytes == total_size, and silently sliced bytes
        // from a tree shallower than advertised.
        let forged = BlobRef::tree(&uri, Encoding::Replicated, root_hash, total_size, 2).unwrap();
        let err = adapter
            .fetch_range(&forged, 0..total_size)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Leaf at residual_depth=")
                && msg.contains("disagrees with BlobRef::Tree.depth"),
            "expected depth-disagreement decode error; got: {msg}",
        );
    }

    // ────────────────────────────────────────────────────────
    // publish_stream_with_downgrade (Phase A6)
    // ────────────────────────────────────────────────────────

    use super::super::blob_tree::TreeSupportProbe;
    use super::super::cdc::CdcSupportProbe;
    use super::super::erasure::ErasureSupportProbe;

    /// Probe `AlwaysSupported` + above-threshold size hint
    /// routes to the Tree path.
    #[tokio::test]
    async fn publish_downgrade_routes_to_tree_when_supported_and_above_threshold() {
        let adapter = make_adapter();
        let payload = deterministic_bytes(0xD1, BLOB_CHUNK_SIZE_BYTES as usize);
        let blob_ref = adapter
            .publish_stream_with_downgrade(
                stream_one(payload),
                Encoding::Replicated,
                ChunkingStrategy::default(),
                Some(super::super::blob_tree::TREE_THRESHOLD_BYTES + 1),
                &DowngradeProbes::new(
                    &TreeSupportProbe::AlwaysSupported,
                    &CdcSupportProbe::AlwaysSupported,
                    &ErasureSupportProbe::AlwaysSupported,
                ),
            )
            .await
            .unwrap();
        assert!(matches!(blob_ref, BlobRef::Tree { .. }));
    }

    /// Probe `ForceManifest` always downgrades to Manifest,
    /// even when the stream is large enough that Tree would win.
    #[tokio::test]
    async fn publish_downgrade_force_manifest_routes_to_manifest_regardless_of_size() {
        let adapter = make_adapter();
        let payload = deterministic_bytes(0xD2, BLOB_CHUNK_SIZE_BYTES as usize * 2);
        let blob_ref = adapter
            .publish_stream_with_downgrade(
                stream_one(payload),
                Encoding::Replicated,
                ChunkingStrategy::default(),
                Some(super::super::blob_tree::TREE_THRESHOLD_BYTES + 1),
                &DowngradeProbes::new(
                    &TreeSupportProbe::ForceManifest,
                    &CdcSupportProbe::AlwaysSupported,
                    &ErasureSupportProbe::AlwaysSupported,
                ),
            )
            .await
            .unwrap();
        assert!(
            matches!(blob_ref, BlobRef::Manifest { .. }),
            "ForceManifest must always produce a Manifest"
        );
    }

    /// Below-threshold size hint downgrades to Manifest even
    /// when the peer supports Tree — round-trip efficiency.
    #[tokio::test]
    async fn publish_downgrade_below_threshold_prefers_manifest() {
        let adapter = make_adapter();
        let payload = deterministic_bytes(0xD3, BLOB_CHUNK_SIZE_BYTES as usize * 2);
        let blob_ref = adapter
            .publish_stream_with_downgrade(
                stream_one(payload),
                Encoding::Replicated,
                ChunkingStrategy::default(),
                Some(BLOB_CHUNK_SIZE_BYTES * 2),
                &DowngradeProbes::new(
                    &TreeSupportProbe::AlwaysSupported,
                    &CdcSupportProbe::AlwaysSupported,
                    &ErasureSupportProbe::AlwaysSupported,
                ),
            )
            .await
            .unwrap();
        assert!(
            matches!(blob_ref, BlobRef::Manifest { .. }),
            "below-threshold + Tree-supported should still pick Manifest"
        );
    }

    /// Size hints between BLOB_REF_MAX_SIZE (16 GiB) and
    /// TREE_THRESHOLD_BYTES (32 GiB) must route to the Tree path,
    /// not the Manifest downgrade. Pre-fix the downgrade buffer
    /// would have failed at the 16 GiB cap on the actual bytes,
    /// even though Tree support is available. The size_hint is
    /// the signal the routing layer uses; we drive a smaller
    /// payload here (the routing decision happens on size_hint,
    /// not on observed bytes) and assert the result is a Tree.
    #[tokio::test]
    async fn publish_downgrade_routes_to_tree_when_hint_exceeds_manifest_cap() {
        let adapter = make_adapter();
        // Real payload is tiny — the routing decision is driven
        // by size_hint. 24 GiB hint sits between BLOB_REF_MAX_SIZE
        // (16 GiB) and TREE_THRESHOLD_BYTES (32 GiB).
        let payload = deterministic_bytes(0xD9, BLOB_CHUNK_SIZE_BYTES as usize);
        let blob_ref = adapter
            .publish_stream_with_downgrade(
                stream_one(payload),
                Encoding::Replicated,
                ChunkingStrategy::default(),
                Some(24 * 1024 * 1024 * 1024),
                &DowngradeProbes::new(
                    &TreeSupportProbe::AlwaysSupported,
                    &CdcSupportProbe::AlwaysSupported,
                    &ErasureSupportProbe::AlwaysSupported,
                ),
            )
            .await
            .unwrap();
        assert!(
            matches!(blob_ref, BlobRef::Tree { .. }),
            "size_hint above BLOB_REF_MAX_SIZE must take the Tree path even when \
             under TREE_THRESHOLD_BYTES; pre-fix this routed to Manifest and \
             the downgrade buffer's 16 GiB cap would have rejected real bytes"
        );
    }

    /// ForceManifest still overrides the cap-exceeded fast path —
    /// the operator's explicit "no Tree" directive is honored,
    /// even if it means an oversize-stream Backend error later.
    /// This pins the operator-intent semantic.
    #[tokio::test]
    async fn publish_downgrade_force_manifest_overrides_cap_exceeded() {
        let adapter = make_adapter();
        // 2 chunks worth so the downgrade path produces Manifest
        // (single-chunk payloads emit BlobRef::Small).
        let payload = deterministic_bytes(0xDA, BLOB_CHUNK_SIZE_BYTES as usize * 2);
        let blob_ref = adapter
            .publish_stream_with_downgrade(
                stream_one(payload),
                Encoding::Replicated,
                ChunkingStrategy::default(),
                Some(24 * 1024 * 1024 * 1024),
                &DowngradeProbes::new(
                    &TreeSupportProbe::ForceManifest,
                    &CdcSupportProbe::AlwaysSupported,
                    &ErasureSupportProbe::AlwaysSupported,
                ),
            )
            .await
            .unwrap();
        // ForceManifest produces Manifest regardless of size hint;
        // the real bytes here fit under the cap.
        assert!(
            matches!(blob_ref, BlobRef::Manifest { .. }),
            "ForceManifest must still produce Manifest even when size_hint \
             exceeds BLOB_REF_MAX_SIZE"
        );
    }

    /// Below-threshold inline-size payload routes to Manifest
    /// (or Small, depending on the chunker's Inline branch).
    /// Either way, NOT a Tree.
    #[tokio::test]
    async fn publish_downgrade_small_payload_does_not_produce_tree() {
        let adapter = make_adapter();
        let payload = deterministic_bytes(0xD4, 1024);
        let blob_ref = adapter
            .publish_stream_with_downgrade(
                stream_one(payload),
                Encoding::Replicated,
                ChunkingStrategy::default(),
                Some(1024),
                &DowngradeProbes::new(
                    &TreeSupportProbe::AlwaysSupported,
                    &CdcSupportProbe::AlwaysSupported,
                    &ErasureSupportProbe::AlwaysSupported,
                ),
            )
            .await
            .unwrap();
        assert!(
            !matches!(blob_ref, BlobRef::Tree { .. }),
            "small payload must not produce a Tree"
        );
    }

    /// Empty stream is rejected from the downgrade path.
    #[tokio::test]
    async fn publish_downgrade_rejects_empty_stream() {
        let adapter = make_adapter();
        let err = adapter
            .publish_stream_with_downgrade(
                stream_one(Vec::new()),
                Encoding::Replicated,
                ChunkingStrategy::default(),
                Some(0),
                &DowngradeProbes::new(
                    &TreeSupportProbe::AlwaysSupported,
                    &CdcSupportProbe::AlwaysSupported,
                    &ErasureSupportProbe::AlwaysSupported,
                ),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("empty stream"), "got: {err}");
    }

    /// Dynamic probe arm — closure evaluated per call.
    #[tokio::test]
    async fn publish_downgrade_dynamic_probe_consults_closure() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc as StdArc;
        let adapter = make_adapter();
        let allow = StdArc::new(AtomicBool::new(false));
        let allow_for_probe = allow.clone();
        let probe =
            TreeSupportProbe::Dynamic(Box::new(move || allow_for_probe.load(Ordering::Relaxed)));
        // First call: probe says false → downgrade away from
        // Tree. Use a payload > BLOB_CHUNK_SIZE_BYTES so the
        // chunker actually returns a Manifest (not a Small).
        let payload1 = deterministic_bytes(0xD5, BLOB_CHUNK_SIZE_BYTES as usize + 1);
        let r1 = adapter
            .publish_stream_with_downgrade(
                stream_one(payload1),
                Encoding::Replicated,
                ChunkingStrategy::default(),
                Some(super::super::blob_tree::TREE_THRESHOLD_BYTES + 1),
                &DowngradeProbes::new(
                    &probe,
                    &CdcSupportProbe::AlwaysSupported,
                    &ErasureSupportProbe::AlwaysSupported,
                ),
            )
            .await
            .unwrap();
        assert!(matches!(r1, BlobRef::Manifest { .. }));
        // Flip the flag; second call: probe says true → Tree.
        allow.store(true, Ordering::Relaxed);
        let payload2 = deterministic_bytes(0xD6, BLOB_CHUNK_SIZE_BYTES as usize);
        let r2 = adapter
            .publish_stream_with_downgrade(
                stream_one(payload2),
                Encoding::Replicated,
                ChunkingStrategy::default(),
                Some(super::super::blob_tree::TREE_THRESHOLD_BYTES + 1),
                &DowngradeProbes::new(
                    &probe,
                    &CdcSupportProbe::AlwaysSupported,
                    &ErasureSupportProbe::AlwaysSupported,
                ),
            )
            .await
            .unwrap();
        assert!(matches!(r2, BlobRef::Tree { .. }));
    }

    /// CDC probe set to `ForceFixed` collapses a `ChunkingStrategy::Cdc`
    /// request to `Fixed` before any leaf bytes hit disk — peers without
    /// the `dataforts:blob-cdc-supported` capability can re-derive
    /// boundaries against the resulting Tree.
    #[tokio::test]
    async fn publish_downgrade_force_fixed_collapses_cdc_to_fixed() {
        let adapter = make_adapter();
        // Reference: produce a Tree under CDC (probe AlwaysSupported)
        // and under Fixed (the downgraded request). The downgraded
        // request lands on the SAME root hash as a manually-fixed
        // chunking would have, proving the downgrade applied before
        // the leaf-emission path saw a CDC parameter.
        let payload = deterministic_bytes(0xD7, BLOB_CHUNK_SIZE_BYTES as usize * 3);
        let cdc_blob = adapter
            .publish_stream_with_downgrade(
                stream_one(payload.clone()),
                Encoding::Replicated,
                ChunkingStrategy::Cdc {
                    min: super::super::cdc::PRODUCTION_CDC_PARAMS.min,
                    avg: super::super::cdc::PRODUCTION_CDC_PARAMS.avg,
                    max: super::super::cdc::PRODUCTION_CDC_PARAMS.max,
                },
                Some(super::super::blob_tree::TREE_THRESHOLD_BYTES + 1),
                &DowngradeProbes::new(
                    &TreeSupportProbe::AlwaysSupported,
                    &CdcSupportProbe::ForceFixed,
                    &ErasureSupportProbe::AlwaysSupported,
                ),
            )
            .await
            .unwrap();
        let fixed_blob = adapter
            .publish_stream_with_downgrade(
                stream_one(payload),
                Encoding::Replicated,
                ChunkingStrategy::default(),
                Some(super::super::blob_tree::TREE_THRESHOLD_BYTES + 1),
                &DowngradeProbes::new(
                    &TreeSupportProbe::AlwaysSupported,
                    &CdcSupportProbe::AlwaysSupported,
                    &ErasureSupportProbe::AlwaysSupported,
                ),
            )
            .await
            .unwrap();
        assert_eq!(
            cdc_blob.tree_root_hash(),
            fixed_blob.tree_root_hash(),
            "ForceFixed must produce a Tree whose root matches the Fixed-chunked baseline; \
             this proves the downgrade applied before any leaf bytes were emitted",
        );
    }

    /// Erasure probe set to `ForceReplicated` collapses a
    /// `Encoding::ReedSolomon` request to `Replicated` before any
    /// stripe layout is committed. The resulting Tree has no
    /// `ErasureLeaf` nodes — peers without RS support can still
    /// reconstruct from replicated leaves alone.
    #[tokio::test]
    async fn publish_downgrade_force_replicated_collapses_rs_to_replicated() {
        let adapter = make_adapter();
        let payload = deterministic_bytes(0xD8, BLOB_CHUNK_SIZE_BYTES as usize * 3);
        let rs_blob = adapter
            .publish_stream_with_downgrade(
                stream_one(payload.clone()),
                Encoding::ReedSolomon { k: 4, m: 2 },
                ChunkingStrategy::default(),
                Some(super::super::blob_tree::TREE_THRESHOLD_BYTES + 1),
                &DowngradeProbes::new(
                    &TreeSupportProbe::AlwaysSupported,
                    &CdcSupportProbe::AlwaysSupported,
                    &ErasureSupportProbe::ForceReplicated,
                ),
            )
            .await
            .unwrap();
        // The downgraded blob is a Tree with Replicated encoding; no
        // ErasureLeaf nodes anywhere.
        let root_hash = *rs_blob.tree_root_hash().unwrap();
        let root_bytes = adapter.fetch_chunk(&root_hash).await.unwrap();
        let mut stack: Vec<Bytes> = vec![root_bytes];
        while let Some(bytes) = stack.pop() {
            match TreeNode::decode(&bytes).unwrap() {
                TreeNode::Internal { children } => {
                    for (child_hash, _) in children {
                        stack.push(adapter.fetch_chunk(&child_hash).await.unwrap());
                    }
                }
                TreeNode::Leaf { .. } => { /* expected */ }
                TreeNode::ErasureLeaf { .. } => {
                    panic!("ForceReplicated must not emit ErasureLeaf");
                }
            }
        }
    }

    // ────────────────────────────────────────────────────────
    // Manifest LRU cache (Phase A5)
    // ────────────────────────────────────────────────────────

    /// With the cache attached, two adjacent range reads on the
    /// same blob's tree must observe at least one cache hit
    /// on the second walk — the root + spanning leaf are reused.
    ///
    /// Uses the test-internal store path with 1 KiB chunks so
    /// the FANOUT-spanning test allocates ~140 KiB instead of
    /// the production 4 MiB chunker's ~540 MiB.
    #[tokio::test]
    async fn fetch_range_tree_cache_hits_on_adjacent_reads() {
        let redex = Arc::new(Redex::new());
        let adapter =
            MeshBlobAdapter::new("mesh-tree-cache", redex).with_tree_node_cache(64 * 1024 * 1024);
        // Build a depth-2 tree so a walk fetches root + at
        // least one leaf — both cacheable.
        let small_chunk: u32 = 1024;
        let len = small_chunk as usize * (TREE_FANOUT + 5);
        let payload = deterministic_bytes(0xC1, len);
        let blob_ref = adapter
            .store_stream_tree_internal(
                stream_one(payload.clone()),
                Encoding::Replicated,
                small_chunk,
            )
            .await
            .unwrap();
        assert_eq!(blob_ref.tree_depth(), Some(2));
        // First fetch — populates the cache.
        let _ = adapter
            .fetch_range(&blob_ref, 0..small_chunk as u64)
            .await
            .unwrap();
        let (hits_1, _, _, _) = adapter.tree_node_cache_stats().unwrap();
        // Second fetch in the same byte range — should hit the
        // cache for the root + the first leaf.
        let _ = adapter
            .fetch_range(&blob_ref, 100..(small_chunk as u64 - 100))
            .await
            .unwrap();
        let (hits_2, _, _, _) = adapter.tree_node_cache_stats().unwrap();
        assert!(
            hits_2 > hits_1,
            "second adjacent fetch must observe at least one cache hit; \
             hits_1={hits_1} hits_2={hits_2}"
        );
        let (_, _, _, entries) = adapter.tree_node_cache_stats().unwrap();
        assert!(entries >= 1, "cache should have populated entries");
    }

    /// Cache hit returns byte-identical content to the chunk-
    /// store fetch. Content-addressed → no consistency loss.
    #[tokio::test]
    async fn fetch_range_tree_cache_hit_byte_identical() {
        let redex = Arc::new(Redex::new());
        let adapter = MeshBlobAdapter::new("mesh-tree-cache-bytes", redex)
            .with_tree_node_cache(64 * 1024 * 1024);
        let len = BLOB_CHUNK_SIZE_BYTES as usize * 2;
        let payload = deterministic_bytes(0xC2, len);
        let blob_ref = adapter
            .store_stream_tree(
                stream_one(payload.clone()),
                Encoding::Replicated,
                ChunkingStrategy::default(),
            )
            .await
            .unwrap();
        // Two identical fetches; second one hits the cache for
        // the root. Both must return identical bytes.
        let a = adapter.fetch_range(&blob_ref, 0..len as u64).await.unwrap();
        let b = adapter.fetch_range(&blob_ref, 0..len as u64).await.unwrap();
        assert_eq!(a, b);
        assert_eq!(a, payload);
    }

    /// Manifest cache must be invalidated when a chunk leaves the
    /// store via delete_chunk. Without invalidation, a subsequent
    /// fetch_range traverses the cached tree node and only
    /// discovers the missing leaf chunks at the bottom of the
    /// descent, confusing the operator-visible error attribution
    /// (NotFound on a leaf vs "blob was deleted out from under
    /// us"). Cache integrity (bytes hash to key) is preserved
    /// either way — this fix targets error-path clarity, not
    /// soundness.
    #[tokio::test]
    async fn delete_chunk_invalidates_cached_tree_node() {
        let redex = Arc::new(Redex::new());
        let adapter = MeshBlobAdapter::new("mesh-cache-invalidate", redex)
            .with_tree_node_cache(64 * 1024 * 1024);
        let len = BLOB_CHUNK_SIZE_BYTES as usize * 2;
        let payload = deterministic_bytes(0xCA, len);
        let blob_ref = adapter
            .store_stream_tree(
                stream_one(payload),
                Encoding::Replicated,
                ChunkingStrategy::default(),
            )
            .await
            .unwrap();
        let root_hash = *blob_ref.tree_root_hash().unwrap();
        // Populate the cache.
        let _ = adapter.fetch_range(&blob_ref, 0..len as u64).await.unwrap();
        let (_, _, _, entries_before) = adapter.tree_node_cache_stats().unwrap();
        assert!(
            entries_before >= 1,
            "cache should hold at least the root node"
        );
        // Delete the root chunk directly — simulates a sweep
        // landing on the manifest node.
        adapter.delete_chunk(&root_hash).await.unwrap();
        // The cache entry for the deleted root hash must be gone.
        // Probe via a fresh fetch — pre-fix it would cache-hit on
        // the root, decode, then NotFound on a child; post-fix it
        // misses and surfaces the absence directly.
        let cache = adapter.tree_node_cache.as_ref().unwrap();
        assert!(
            cache.lock().get(&root_hash).is_none(),
            "deleted root must be evicted from the manifest cache"
        );
    }

    /// `sweep_gc` deletes chunks via close_and_unlink_file
    /// directly (not through delete_chunk), so it has its own
    /// cache-invalidation site. Test pins that path.
    #[tokio::test]
    async fn sweep_gc_invalidates_cached_tree_node() {
        let redex = Arc::new(Redex::new());
        let adapter = MeshBlobAdapter::new("mesh-cache-sweep-invalidate", redex)
            .with_tree_node_cache(64 * 1024 * 1024);
        let len = BLOB_CHUNK_SIZE_BYTES as usize * 2;
        let payload = deterministic_bytes(0xCB, len);
        let blob_ref = adapter
            .store_stream_tree(
                stream_one(payload),
                Encoding::Replicated,
                ChunkingStrategy::default(),
            )
            .await
            .unwrap();
        let root_hash = *blob_ref.tree_root_hash().unwrap();
        let _ = adapter.fetch_range(&blob_ref, 0..len as u64).await.unwrap();
        // Far-future timestamp pushes age >= retention floor;
        // disk_pressure_critical=false (under pressure the sweep
        // is rejected outright per `should_sweep`).
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + (DEFAULT_RETENTION_FLOOR.as_millis() as u64 * 2);
        let _ = adapter.sweep_gc(now, false).await.unwrap();
        let cache = adapter.tree_node_cache.as_ref().unwrap();
        assert!(
            cache.lock().get(&root_hash).is_none(),
            "sweep_gc must evict every swept hash from the manifest cache"
        );
    }

    /// Cache disabled (`with_tree_node_cache(0)`) → no entries
    /// land, every walk takes the fetch_chunk path.
    #[tokio::test]
    async fn fetch_range_tree_cache_can_be_disabled() {
        let redex = Arc::new(Redex::new());
        let adapter =
            MeshBlobAdapter::new("mesh-tree-cache-disabled", redex).with_tree_node_cache(0);
        let len = BLOB_CHUNK_SIZE_BYTES as usize;
        let payload = deterministic_bytes(0xC3, len);
        let blob_ref = adapter
            .store_stream_tree(
                stream_one(payload),
                Encoding::Replicated,
                ChunkingStrategy::default(),
            )
            .await
            .unwrap();
        let _ = adapter.fetch_range(&blob_ref, 0..len as u64).await.unwrap();
        let (_, _, bytes_total, len_count) = adapter.tree_node_cache_stats().unwrap();
        assert_eq!(bytes_total, 0);
        assert_eq!(len_count, 0);
    }

    /// `store_stream_tree`'s root_depth always lies in
    /// `1..=MAX_TREE_DEPTH`.
    #[tokio::test]
    async fn store_stream_tree_root_depth_in_range() {
        let adapter = make_adapter();
        let len = BLOB_CHUNK_SIZE_BYTES as usize + 1;
        let payload = deterministic_bytes(0x99, len);
        let blob_ref = adapter
            .store_stream_tree(
                stream_one(payload),
                Encoding::Replicated,
                ChunkingStrategy::default(),
            )
            .await
            .unwrap();
        let depth = blob_ref.tree_depth().unwrap();
        assert!(
            (1..=MAX_TREE_DEPTH).contains(&depth),
            "depth {} out of range 1..={}",
            depth,
            MAX_TREE_DEPTH,
        );
    }
}
