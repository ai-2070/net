//! Per-file configuration for RedEX.

use std::time::Duration;

use super::replication_config::ReplicationConfig;

/// Disk-side fsync policy for persistent `RedexFile`s.
///
/// Governs **only** the append path on the disk mirror. `close()` and
/// explicit `RedexFile::sync()` calls always fsync regardless of
/// policy — these are the caller's explicit durability barriers.
///
/// | Policy | Process crash | Kernel / power crash |
/// |--------|---------------|---------------------|
/// | `Never` | Loses the tail since last close / `sync()` | Same |
/// | `EveryN(N)` | Loses ≤ (N−1) entries from the last sync point | Same |
/// | `Interval(d)` | Loses ≤ `d` seconds of writes | Same |
/// | `IntervalOrBytes { period, max_bytes }` | Loses ≤ min(`period` of writes, `max_bytes` of writes) | Same |
///
/// Default is [`FsyncPolicy::Never`], matching the pre-`FsyncPolicy`
/// behavior — OS page cache only, fsync on close. Callers that need
/// tighter bounds opt into `EveryN`, `Interval`, or
/// `IntervalOrBytes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FsyncPolicy {
    /// Never fsync on append. `close()` still syncs. Lowest latency;
    /// fine for telemetry / best-effort logs.
    #[default]
    Never,
    /// Fsync after every N successful appends. The fsync runs on a
    /// background task — the appender returns as soon as the bytes
    /// land in the page cache and signals the worker; the worker
    /// runs `fsync_all` off the hot path. Concurrent notifies during
    /// an in-flight fsync coalesce into a single follow-up.
    ///
    /// Worst-case loss bound: (N − 1) entries since the last sync
    /// **point**, plus the bytes from any fsync that was in flight
    /// when the crash interrupted it. `0` and `1` both collapse to
    /// "signal on every append."
    ///
    /// Under heavy concurrent appends the bound loosens slightly:
    /// the threshold check (`fetch_add` then `if cross { reset }`)
    /// is not a CAS, so K appenders racing past the threshold can
    /// each cross before any of them resets the counter. The next
    /// sync covers all K of those entries; the durability contract
    /// still holds (no entry survives unsynced past the next
    /// fsync), but the practical bound becomes
    /// `(N − 1) + (concurrent appenders at threshold)`. Pick a
    /// smaller N if you need a tighter bound under contention.
    EveryN(u64),
    /// Fsync on a timer, independent of append rate. A per-file
    /// background tokio task drives the sync; `close()` cancels it.
    Interval(Duration),
    /// Fsync when **either** `period` elapses **or** `max_bytes` of
    /// writes have accumulated since the last sync, whichever comes
    /// first. The byte threshold counts every byte written to dat,
    /// idx, and ts.
    ///
    /// Use this for bursty workloads where a long `period` would
    /// leave too much data unsynced under load, but a short `period`
    /// would over-fsync when idle.
    ///
    /// Configuration matrix:
    ///
    /// | `period` | `max_bytes` | Behavior |
    /// |----------|-------------|----------|
    /// | `> 0`    | `> 0`       | Full both-arms worker (timer + byte signal) |
    /// | `> 0`    | `0`         | Timer-only worker (equivalent to `Interval(period)`) |
    /// | `0`      | `> 0`       | Byte-only worker (no timer arm); fsyncs when the byte threshold crosses |
    /// | `0`      | `0`         | No worker; equivalent to `Never` |
    ///
    /// The same concurrency caveat as [`Self::EveryN`] applies to
    /// the byte arm: K concurrent appenders can each cross the
    /// threshold before any of them resets the counter, so the
    /// effective bound is
    /// `max_bytes + (concurrent appenders' bytes at threshold)`.
    IntervalOrBytes {
        /// Maximum wall-clock interval between syncs. `0` disables
        /// the timer arm; pair with a non-zero `max_bytes` to get a
        /// byte-only worker.
        period: Duration,
        /// Maximum bytes (across dat + idx + ts) accumulated since
        /// the last sync before the worker is signaled. `0`
        /// disables the byte arm; pair with a non-zero `period` to
        /// get a timer-only worker (equivalent to `Interval`).
        max_bytes: u64,
    },
}

/// Per-file configuration supplied at `Redex::open_file` time.
///
/// Was `Copy` pre-replication. The `replication` field carries a
/// `Vec<NodeId>` when [`PlacementStrategy::Pinned`](super::replication_config::PlacementStrategy::Pinned) is in use, so
/// the type is now `Clone`-only. The struct is small and rarely
/// passed in hot paths; existing callers add a `.clone()` where they
/// previously relied on bit-copy semantics.
#[derive(Debug, Clone)]
pub struct RedexFileConfig {
    /// Heap-only (`false`) vs heap + simple disk segment (`true`).
    ///
    /// `true` requires the `redex-disk` feature **and** a persistent
    /// base directory configured on the owning `Redex` manager via
    /// `Redex::with_persistent_dir`. With no base dir, `open_file`
    /// returns an error.
    ///
    /// With `redex-disk` off, this field is silently ignored — the
    /// file is heap-only regardless.
    pub persistent: bool,

    /// Disk fsync policy for persistent files. Ignored when
    /// `persistent == false`. Defaults to [`FsyncPolicy::Never`].
    pub fsync_policy: FsyncPolicy,

    /// Initial reservation hint for the heap payload segment. Used
    /// only as the capacity passed to the backing `Vec` on open,
    /// capped at 64 MiB internally — the segment grows past this
    /// value on append up to a 3 GB hard limit. **Retention is NOT
    /// driven by this field** in v1; use `retention_max_events`,
    /// `retention_max_bytes`, or `retention_max_age_ns` for that.
    ///
    /// v2's warm-tier rollover will consume this value as the
    /// rollover trigger (see REDEX_V2_PLAN §3).
    pub max_memory_bytes: usize,

    /// Keep only the newest K events. `None` = unbounded.
    pub retention_max_events: Option<u64>,

    /// Keep only the newest M bytes of payload. `None` = unbounded.
    pub retention_max_bytes: Option<u64>,

    /// Drop entries older than this many nanoseconds at the next
    /// [`super::RedexFile::sweep_retention`] tick. Age is measured
    /// against `SystemTime::now()` at append time.
    ///
    /// v2 limitation: per-entry timestamps are in-memory only. On
    /// reopen of a persistent file, all recovered entries get "now"
    /// as their fake timestamp — age retention starts fresh from
    /// the reopen moment. v2 mmap tier will persist timestamps.
    pub retention_max_age_ns: Option<u64>,

    /// Per-subscription buffer depth for `tail()` streams. Caps the
    /// memory a slow subscriber can pin at `tail_buffer_size *
    /// avg_event_size`. Subscribers that can't drain this many
    /// pending events get disconnected with a best-effort
    /// `RedexError::Lagged` signal.
    ///
    /// Tune up for bursty workloads with brief consumer pauses;
    /// tune down to reclaim memory faster from misbehaving
    /// subscribers. Default: 1024.
    pub tail_buffer_size: usize,

    /// Cross-node replication opt-in per
    /// `docs/plans/REDEX_DISTRIBUTED_PLAN.md` §1. `None` (default)
    /// keeps the file single-node; `Some(cfg)` opts the channel
    /// into the `ReplicationCoordinator` lifecycle Phase C wires.
    ///
    /// Validate via `cfg.validate()` before committing to a
    /// `Redex`; Phase C's `Redex::open_file` will surface a typed
    /// `ReplicationConfigError` if the field is `Some(cfg)` with
    /// `cfg.validate().is_err()`.
    pub replication: Option<ReplicationConfig>,

    /// Dataforts Phase 3 — id of the [`BlobAdapter`] this channel's
    /// events resolve against when an event payload's first byte is
    /// the `BlobRef` discriminator. `None` (default) means callers
    /// of `RedexFile::resolve_one` MUST pass an adapter explicitly;
    /// `Some(id)` lets them route through
    /// `global_blob_adapter_registry()` automatically. The field is
    /// advisory metadata at the RedEX layer — substrate reads still
    /// return raw payload bytes; the resolution decision happens at
    /// the convenience read helpers.
    ///
    /// [`BlobAdapter`]: super::super::dataforts::blob::BlobAdapter
    pub blob_adapter_id: Option<String>,

    /// Per-channel override for the blob adapter registry. `None`
    /// (default) routes through `global_blob_adapter_registry()`;
    /// `Some(reg)` looks `blob_adapter_id` up in the supplied
    /// registry instead. Used by multi-tenant binding hosts to
    /// scope adapter ids per tenant — a tenant's
    /// `register_blob_adapter("s3-primary", ...)` lands in its own
    /// registry without colliding with another tenant's same-named
    /// adapter.
    ///
    /// Wrapped in `Arc` so the config is `Clone`-cheap and
    /// multiple channels can share one registry.
    #[cfg(feature = "dataforts")]
    pub blob_adapter_registry:
        Option<std::sync::Arc<super::super::dataforts::blob::BlobAdapterRegistry>>,
}

impl Default for RedexFileConfig {
    fn default() -> Self {
        Self {
            persistent: false,
            fsync_policy: FsyncPolicy::Never,
            max_memory_bytes: 64 * 1024 * 1024, // 64 MiB soft cap
            retention_max_events: None,
            retention_max_bytes: None,
            retention_max_age_ns: None,
            tail_buffer_size: 1024,
            replication: None,
            blob_adapter_id: None,
            #[cfg(feature = "dataforts")]
            blob_adapter_registry: None,
        }
    }
}

impl RedexFileConfig {
    /// Start from defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable persistent (disk-backed) storage.
    pub fn with_persistent(mut self, persistent: bool) -> Self {
        self.persistent = persistent;
        self
    }

    /// Set the disk fsync policy. See [`FsyncPolicy`] for the
    /// durability / latency trade-offs each variant offers.
    pub fn with_fsync_policy(mut self, policy: FsyncPolicy) -> Self {
        self.fsync_policy = policy;
        self
    }

    /// Set the initial reservation size for the heap segment (capped
    /// at 64 MiB internally). Does NOT enforce a retention cap — use
    /// [`Self::with_retention_max_bytes`] for that.
    pub fn with_max_memory_bytes(mut self, bytes: usize) -> Self {
        self.max_memory_bytes = bytes;
        self
    }

    /// Keep at most `events` entries.
    pub fn with_retention_max_events(mut self, events: u64) -> Self {
        self.retention_max_events = Some(events);
        self
    }

    /// Keep at most `bytes` bytes of payload.
    pub fn with_retention_max_bytes(mut self, bytes: u64) -> Self {
        self.retention_max_bytes = Some(bytes);
        self
    }

    /// Drop entries older than `max_age`. Measured in nanoseconds
    /// against `SystemTime::now()` at append time.
    pub fn with_retention_max_age(mut self, max_age: Duration) -> Self {
        self.retention_max_age_ns = Some(max_age.as_nanos() as u64);
        self
    }

    /// Set the per-subscription buffer depth for `tail()` streams.
    /// See the field doc on [`Self::tail_buffer_size`].
    pub fn with_tail_buffer_size(mut self, size: usize) -> Self {
        self.tail_buffer_size = size;
        self
    }

    /// Opt the channel into cross-node replication. Pass `None` to
    /// restore single-node behavior. The supplied
    /// [`ReplicationConfig`] should validate cleanly (see
    /// [`ReplicationConfig::validate`]); Phase C's `Redex::open_file`
    /// surfaces validation errors typed.
    pub fn with_replication(mut self, replication: Option<ReplicationConfig>) -> Self {
        self.replication = replication;
        self
    }

    /// Set the dataforts blob adapter id used by
    /// [`super::RedexFile::resolve_one`]. Pass `None` to clear.
    pub fn with_blob_adapter_id(mut self, id: Option<String>) -> Self {
        self.blob_adapter_id = id;
        self
    }

    /// Bind a specific blob adapter registry for `resolve_one` to
    /// look up `blob_adapter_id` against. `None` (default) falls
    /// back to `global_blob_adapter_registry()`. Multi-tenant
    /// binding hosts construct one registry per tenant and pass
    /// it here to isolate adapter ids across tenants.
    #[cfg(feature = "dataforts")]
    pub fn with_blob_adapter_registry(
        mut self,
        registry: Option<std::sync::Arc<super::super::dataforts::blob::BlobAdapterRegistry>>,
    ) -> Self {
        self.blob_adapter_registry = registry;
        self
    }
}
