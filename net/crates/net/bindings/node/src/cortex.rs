//! Node.js bindings for the CortEX adapter slice — tasks + memories.
//!
//! Feature-gated behind `cortex` in this crate (which turns on the
//! core's `cortex` feature). Exposes:
//!
//! - [`Redex`] — local RedEX manager handle
//! - [`TasksAdapter`] / [`MemoriesAdapter`] — typed adapters with CRUD
//!   plus a synchronous `list*(filter)` snapshot query
//!
//! u64 fields (ids, timestamps, RedEX sequences) cross the napi
//! boundary as `BigInt` to preserve full 64-bit precision.
//!
//! Watch / `AsyncIterator` is deliberately deferred — the JS async
//! iterator glue lands in a follow-up session.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures::stream::BoxStream;
use futures::StreamExt;
use napi::bindgen_prelude::*;
use napi_derive::napi;
use tokio::sync::{Mutex as TokioMutex, Notify};

use ::net::adapter::net::channel::ChannelName;
use ::net::adapter::net::cortex::memories::{
    MemoriesAdapter as InnerMemoriesAdapter, Memory as InnerMemory, OrderBy as InnerMemoriesOrderBy,
};
use ::net::adapter::net::cortex::tasks::{
    OrderBy as InnerTasksOrderBy, Task as InnerTask, TaskStatus as InnerTaskStatus,
    TasksAdapter as InnerTasksAdapter,
};
use ::net::adapter::net::cortex::WaitForTokenError as InnerWaitForTokenError;
use ::net::adapter::net::redex::{
    FsyncPolicy as InnerFsyncPolicy, PlacementStrategy as InnerPlacementStrategy,
    Redex as InnerRedex, RedexError as InnerRedexError, RedexEvent as InnerRedexEvent,
    RedexFile as InnerRedexFile, RedexFileConfig, ReplicationConfig as InnerReplicationConfig,
    UnderCapacity as InnerUnderCapacity, WriteToken as InnerWriteToken,
};
use bytes::Bytes;

// =========================================================================
// Error-class prefix contract
// =========================================================================
//
// Stable prefixes the `@ai2070/net/errors` wrapper inspects to re-throw
// typed `CortexError` / `NetDbError` instances. Keep these strings
// byte-stable — they are part of the SDK's public contract.

pub(crate) const ERR_CORTEX_PREFIX: &str = "cortex:";
pub(crate) const ERR_NETDB_PREFIX: &str = "netdb:";
/// R-29: stable prefix on every RedEX-originated error. JS-side
/// callers should `e.message.startsWith("redex:")` to discriminate
/// (no dedicated `RedexError` JS class — napi-rs only emits plain
/// `Error` and the prefix is the contract). The full shape is
/// `"redex: <context>: <detail>"` where `<context>` is a stable
/// identifier the SDK can pattern-match on (e.g. `"open_file"`,
/// `"replication.factor"`, `"enable_replication"`).
pub(crate) const ERR_REDEX_PREFIX: &str = "redex:";

#[inline]
pub(crate) fn cortex_err(context: &str, detail: impl std::fmt::Display) -> Error {
    Error::from_reason(format!("{} {}: {}", ERR_CORTEX_PREFIX, context, detail))
}

#[inline]
pub(crate) fn netdb_err(context: &str, detail: impl std::fmt::Display) -> Error {
    Error::from_reason(format!("{} {}: {}", ERR_NETDB_PREFIX, context, detail))
}

#[inline]
pub(crate) fn redex_err(context: &str, detail: impl std::fmt::Display) -> Error {
    Error::from_reason(format!("{} {}: {}", ERR_REDEX_PREFIX, context, detail))
}

// =========================================================================
// Shared helpers
// =========================================================================

use crate::common::bigint_u64;

/// A captured CortEX adapter state snapshot, suitable for
/// `openFromSnapshot`. Callers persist both fields together.
#[napi(object)]
pub struct CortexSnapshot {
    /// bincode-serialized materialized state.
    pub state_bytes: Buffer,
    /// Highest RedEX sequence folded into `state_bytes`. `null` if
    /// no event had been folded at snapshot time.
    pub last_seq: Option<BigInt>,
}

fn redex_config_from_persistent(persistent: Option<bool>) -> RedexFileConfig {
    if persistent.unwrap_or(false) {
        RedexFileConfig::default().with_persistent(true)
    } else {
        RedexFileConfig::default()
    }
}

// =========================================================================
// WriteToken — typed handle to a specific write, returned by ingest
// paths and consumed by read-your-writes wait primitives.
// =========================================================================

/// Address of a single write. Pair with a typed adapter's
/// `waitForToken(token, deadlineMs)` to make sure the local fold has
/// caught up to the write before reading state. `originHash` is the
/// 64-bit chain identifier; `seq` is the per-chain monotonic
/// sequence assigned by `RedexFile.append`.
#[napi]
#[derive(Clone, Copy)]
pub struct WriteToken {
    inner: InnerWriteToken,
}

#[napi]
impl WriteToken {
    #[napi(constructor)]
    pub fn new(origin_hash: BigInt, seq: BigInt) -> Result<Self> {
        Ok(Self {
            inner: InnerWriteToken::new(bigint_u64(origin_hash)?, bigint_u64(seq)?),
        })
    }

    #[napi(getter)]
    pub fn origin_hash(&self) -> BigInt {
        BigInt::from(self.inner.origin_hash)
    }

    #[napi(getter)]
    pub fn seq(&self) -> BigInt {
        BigInt::from(self.inner.seq)
    }

    /// Parse a token from its `<16-hex-origin>:<seq>` string form.
    #[napi(factory)]
    pub fn from_string(s: String) -> Result<Self> {
        s.parse::<InnerWriteToken>()
            .map(|inner| Self { inner })
            .map_err(|e| Error::from_reason(format!("WriteToken parse: {}", e)))
    }

    #[napi(js_name = "toString")]
    #[allow(clippy::inherent_to_string, clippy::wrong_self_convention)]
    pub fn to_string(&self) -> String {
        self.inner.to_string()
    }
}

impl WriteToken {
    fn as_inner(&self) -> InnerWriteToken {
        self.inner
    }
}

fn wait_for_token_err(e: InnerWaitForTokenError) -> Error {
    cortex_err("wait_for_token", e)
}

// =========================================================================
// Redex manager
// =========================================================================

/// Local RedEX manager. Holds the set of open files on this node.
///
/// Cheap to share — methods take `&self`.
#[napi]
pub struct Redex {
    inner: Arc<InnerRedex>,
}

#[napi]
impl Redex {
    /// Open a new Redex manager.
    ///
    /// `persistentDir`: if provided, files opened through adapters
    /// with `persistent: true` write to `<persistentDir>/<channel_path>/{idx,dat}`
    /// and replay from those files on reopen. Heap-only when omitted.
    #[napi(constructor)]
    #[allow(clippy::new_without_default)]
    pub fn new(persistent_dir: Option<String>) -> Self {
        let inner = match persistent_dir {
            Some(dir) => InnerRedex::new().with_persistent_dir(dir),
            None => InnerRedex::new(),
        };
        Self {
            inner: Arc::new(inner),
        }
    }

    /// Install cross-node replication wiring rooted at `mesh`. After
    /// this returns, `openFile` calls with
    /// `config.replication = { ... }` spawn per-channel replication
    /// runtimes. Idempotent — repeated calls leave the existing
    /// wiring in place (the second installation would orphan every
    /// per-channel runtime registered under the first).
    ///
    /// Gated on the `net` feature: replication requires a
    /// `NetMesh`, which only ships when `net` is enabled. Build the
    /// Node binding with both `--features cortex` and `--features
    /// net` (or any superset) to expose this method.
    ///
    /// R-42: `index.d.ts` is regenerated by napi with the full
    /// build matrix and therefore emits `enableReplication` even
    /// when the runtime binary was built without `net`. The
    /// `cfg(not(feature = "net"))` stub at the bottom of this
    /// `impl` block returns a typed `redex:` error so consumers
    /// of a non-`net` build see the same error class, just with
    /// a "rebuild with --features net" hint.
    ///
    /// See `CONFIG_REPLICATION.md` for the full operator surface.
    #[cfg(feature = "net")]
    #[napi]
    pub fn enable_replication(&self, mesh: &crate::NetMesh) -> Result<()> {
        let arc = mesh.node_arc_clone()?;
        self.inner.enable_replication(arc);
        Ok(())
    }

    /// R-7: when this binding is built without the `net` feature
    /// (cortex-only build), surface a typed `redex:` error
    /// naming the missing feature instead of
    /// `TypeError: redex.enableReplication is not a function`.
    /// Takes `napi::Unknown` so a JS call site with any
    /// arg shape still reaches the typed error.
    #[cfg(not(feature = "net"))]
    #[napi]
    pub fn enable_replication(&self, _mesh: napi::Unknown) -> Result<()> {
        Err(redex_err(
            "enable_replication",
            "binding built without `net` feature; rebuild with --features net",
        ))
    }

    /// Count of per-channel replication runtimes currently registered
    /// on this manager. `0` when replication isn't enabled. Useful
    /// for tests + operator observability.
    #[napi]
    pub fn replication_runtime_count(&self) -> u32 {
        // Safe — count is bounded by MAX_TRACKED_CHANNELS (1024)
        // plus reasonable channel counts in practice.
        self.inner.replication_runtime_count() as u32
    }

    /// Render the replication metrics as Prometheus text. Returns
    /// the empty string when replication isn't enabled —
    /// convenient for piping into an HTTP scrape endpoint without
    /// branching.
    ///
    /// Covers the seven per-channel shapes from
    /// `CONFIG_REPLICATION.md`: `*_lag_seconds{role}`,
    /// `*_sync_bytes_total`, `*_leader_changes_total`,
    /// `*_under_capacity_total`, `*_skip_ahead_total`,
    /// `*_election_thrash_total`, `*_witness_withdrawals_total`.
    #[napi]
    pub fn replication_prometheus_text(&self) -> String {
        self.inner.replication_prometheus_text()
    }

    /// Install greedy-LRU dataforts wiring rooted at `mesh`. The
    /// runtime opens per-channel cache files against this Redex
    /// and announces chains via the mesh's `ChainTagSink` impl.
    ///
    /// Idempotent — a second call with greedy already enabled is
    /// a no-op (use `disableGreedyDataforts` to reconfigure).
    ///
    /// Locked Phase-1 defaults from `DATAFORTS_PLAN.md`:
    /// scopes empty (admit any), 200 ms proximity bound,
    /// 100 MiB per channel, 10 GiB total, 0.25 NIC fraction,
    /// `any_of_local_capabilities` intent, `soft_preference`
    /// colocation.
    ///
    /// `intentMatch` values: `'disabled'` / `'any_of_local_capabilities'` / `'strict'`.
    /// `colocationPolicy` values: `'ignore'` / `'soft_preference'` / `'strict_required'`.
    #[cfg(all(feature = "net", feature = "dataforts"))]
    #[napi]
    pub fn enable_greedy_dataforts(
        &self,
        mesh: &crate::NetMesh,
        config: Option<GreedyConfigJs>,
    ) -> Result<()> {
        use net::adapter::net::dataforts::{
            ColocationPolicy, GreedyConfig, IntentMatchPolicy, ScopeLabel,
        };
        let cfg_js = config.unwrap_or_default();
        let mut cfg = GreedyConfig::new();
        if let Some(scopes) = cfg_js.scopes {
            cfg = cfg.with_scopes(scopes.into_iter().map(ScopeLabel::new).collect());
        }
        if let Some(ms) = cfg_js.proximity_max_rtt_ms {
            cfg = cfg.with_proximity_max_rtt(std::time::Duration::from_millis(ms as u64));
        }
        if let Some(b) = cfg_js.per_channel_cap_bytes {
            let bytes = redex_bigint_u64("greedy.per_channel_cap_bytes", b)?;
            cfg = cfg.with_per_channel_cap_bytes(bytes);
        }
        if let Some(b) = cfg_js.total_cap_bytes {
            let bytes = redex_bigint_u64("greedy.total_cap_bytes", b)?;
            cfg = cfg.with_total_cap_bytes(bytes);
        }
        if let Some(f) = cfg_js.bandwidth_budget_fraction {
            cfg = cfg.with_bandwidth_budget_fraction(f as f32);
        }
        if let Some(peak) = cfg_js.nic_peak_bytes_per_s {
            let peak_u64 = redex_bigint_u64("greedy.nic_peak_bytes_per_s", peak)?;
            cfg = cfg.with_nic_peak_bytes_per_s(Some(peak_u64));
        }
        if let Some(cap) = cfg_js.observer_inflight_cap {
            cfg = cfg.with_observer_inflight_cap(cap as usize);
        }
        if let Some(policy) = cfg_js.intent_match {
            let parsed = match policy.as_str() {
                "disabled" => IntentMatchPolicy::Disabled,
                "any_of_local_capabilities" => IntentMatchPolicy::AnyOfLocalCapabilities,
                "strict" => IntentMatchPolicy::Strict,
                other => {
                    return Err(redex_err(
                        "enable_greedy_dataforts",
                        format!("intentMatch {other:?} unknown"),
                    ))
                }
            };
            cfg = cfg.with_intent_match(parsed);
        }
        if let Some(policy) = cfg_js.colocation_policy {
            let parsed = match policy.as_str() {
                "ignore" => ColocationPolicy::Ignore,
                "soft_preference" => ColocationPolicy::SoftPreference,
                "strict_required" => ColocationPolicy::StrictRequired,
                other => {
                    return Err(redex_err(
                        "enable_greedy_dataforts",
                        format!("colocationPolicy {other:?} unknown"),
                    ))
                }
            };
            cfg = cfg.with_colocation_policy(parsed);
        }
        let arc = mesh.node_arc_clone()?;
        let local_caps =
            Arc::new(net::adapter::net::behavior::capability::CapabilitySet::default());
        let registry = net::adapter::net::behavior::placement::IntentRegistry::defaults();
        self.inner
            .enable_greedy_dataforts(arc, cfg, local_caps, registry)
            .map_err(|e| redex_err("enable_greedy_dataforts", e))
    }

    /// Stub for builds without the `dataforts` feature.
    /// Surfaces a typed `redex:` error rather than the napi
    /// `TypeError: ...is not a function`.
    #[cfg(not(all(feature = "net", feature = "dataforts")))]
    #[napi]
    pub fn enable_greedy_dataforts(
        &self,
        _mesh: napi::Unknown,
        _config: napi::Unknown,
    ) -> Result<()> {
        Err(redex_err(
            "enable_greedy_dataforts",
            "binding built without `dataforts` feature; rebuild with --features dataforts",
        ))
    }

    /// Uninstall greedy wiring. Idempotent — no-op when not
    /// enabled.
    #[cfg(feature = "dataforts")]
    #[napi]
    pub fn disable_greedy_dataforts(&self) {
        self.inner.disable_greedy_dataforts();
    }

    /// Stub for builds without the `dataforts` feature. No-op so
    /// JS callers don't get a `TypeError: ...is not a function`.
    #[cfg(not(feature = "dataforts"))]
    #[napi]
    pub fn disable_greedy_dataforts(&self) {}

    /// Number of channels currently in the greedy cache. `0` when
    /// greedy isn't enabled.
    #[cfg(feature = "dataforts")]
    #[napi]
    pub fn greedy_cached_channel_count(&self) -> u32 {
        self.inner
            .greedy_runtime()
            .map(|r| r.cached_channel_count() as u32)
            .unwrap_or(0)
    }

    /// Stub for builds without the `dataforts` feature. Reports
    /// `0` so dashboards can treat absence as "greedy not enabled".
    #[cfg(not(feature = "dataforts"))]
    #[napi]
    pub fn greedy_cached_channel_count(&self) -> u32 {
        0
    }

    /// Render the greedy metrics as Prometheus text. Empty string
    /// when greedy isn't enabled.
    #[cfg(feature = "dataforts")]
    #[napi]
    pub fn greedy_prometheus_text(&self) -> String {
        self.inner
            .greedy_runtime()
            .map(|r| r.metrics().snapshot().prometheus_text())
            .unwrap_or_default()
    }

    /// Stub for builds without the `dataforts` feature. Returns
    /// an empty string so a scrape integration sees no greedy
    /// metrics.
    #[cfg(not(feature = "dataforts"))]
    #[napi]
    pub fn greedy_prometheus_text(&self) -> String {
        String::new()
    }

    /// Install data-gravity heat-counter emission on the
    /// already-installed greedy runtime. Validates the policy
    /// and spawns a tokio task that fires `gravityTick` on
    /// `tickIntervalMs` cadence. Requires greedy to be enabled
    /// first; throws a typed `redex:` error otherwise.
    ///
    /// Locked Phase-4 defaults from `DATAFORTS_PLAN.md`:
    /// emitThresholdRatio = 2.0, decayHalfLifeSecs = 1800.
    #[cfg(all(feature = "net", feature = "dataforts"))]
    #[napi]
    pub fn enable_gravity_for_greedy(
        &self,
        mesh: &crate::NetMesh,
        config: Option<DataGravityConfigJs>,
    ) -> Result<()> {
        use net::adapter::net::dataforts::DataGravityPolicy;
        let cfg_js = config.unwrap_or_default();
        let mut policy = DataGravityPolicy::new().with_enabled(cfg_js.enabled.unwrap_or(true));
        if let Some(r) = cfg_js.emit_threshold_ratio {
            policy = policy.with_emit_threshold_ratio(r as f32);
        }
        if let Some(secs) = cfg_js.decay_half_life_secs {
            let secs_u64 = bigint_u64(secs)?;
            policy = policy.with_decay_half_life(std::time::Duration::from_secs(secs_u64));
        }
        let tick = match cfg_js.tick_interval_ms {
            Some(v) => std::time::Duration::from_millis(bigint_u64(v)?),
            None => std::time::Duration::from_millis(500),
        };
        let arc = mesh.node_arc_clone()?;
        self.inner
            .enable_gravity_for_greedy(arc, policy, tick)
            .map_err(|e| redex_err("enable_gravity_for_greedy", e))
    }

    /// Stub for builds without `dataforts`.
    #[cfg(not(all(feature = "net", feature = "dataforts")))]
    #[napi]
    pub fn enable_gravity_for_greedy(
        &self,
        _mesh: napi::Unknown,
        _config: napi::Unknown,
    ) -> Result<()> {
        Err(redex_err(
            "enable_gravity_for_greedy",
            "binding built without `dataforts` feature; rebuild with --features dataforts",
        ))
    }

    /// Uninstall the gravity layer. Greedy stays running.
    /// Idempotent.
    #[cfg(feature = "dataforts")]
    #[napi]
    pub fn disable_gravity_for_greedy(&self) {
        self.inner.disable_gravity_for_greedy();
    }

    /// Stub for builds without the `dataforts` feature.
    #[cfg(not(feature = "dataforts"))]
    #[napi]
    pub fn disable_gravity_for_greedy(&self) {}

    /// Open (or get) a raw RedEX file bound to `channelName`. Returns
    /// a handle for append / tail / read operations without going
    /// through the CortEX adapter layer.
    ///
    /// Re-opening an existing name returns the live handle; the
    /// `config` argument is honored only on first open.
    ///
    /// With `config.persistent = true`, this manager must have been
    /// constructed with a `persistentDir`. Otherwise the call fails
    /// with a `redex:` error.
    #[napi]
    pub fn open_file(
        &self,
        channel_name: String,
        config: Option<RedexFileConfigJs>,
    ) -> Result<RedexFile> {
        let name =
            ChannelName::new(&channel_name).map_err(|e| redex_err("invalid channel name", e))?;
        let cfg = resolve_redex_file_config(config)?;
        let file = self
            .inner
            .open_file(&name, cfg)
            .map_err(|e| redex_err("open_file", e))?;
        Ok(RedexFile {
            inner: Arc::new(file),
        })
    }
}

// =========================================================================
// Raw RedEX file — domain-agnostic event log
// =========================================================================

/// Configuration for [`Redex::open_file`]. Mirrors the core
/// `RedexFileConfig` but flattens `FsyncPolicy` into two mutually
/// exclusive optional fields. Leave both unset for the default
/// `Never` policy.
#[napi(object)]
pub struct RedexFileConfigJs {
    /// Disk-backed storage. Requires `Redex` to have been constructed
    /// with `persistentDir`. Default: `false` (heap only).
    pub persistent: Option<bool>,
    /// Fsync after every N appends (`1` fsyncs on every append).
    /// Mutually exclusive with `fsync_interval_ms`. Ignored unless
    /// `persistent: true`. `0` is rejected.
    pub fsync_every_n: Option<BigInt>,
    /// Fsync on a timer (milliseconds). Mutually exclusive with
    /// `fsync_every_n`. Ignored unless `persistent: true`. `0` is
    /// rejected.
    pub fsync_interval_ms: Option<u32>,
    /// Retain at most N events.
    pub retention_max_events: Option<BigInt>,
    /// Retain at most N bytes of payload.
    pub retention_max_bytes: Option<BigInt>,
    /// Drop entries older than this many milliseconds at the next
    /// retention sweep.
    pub retention_max_age_ms: Option<BigInt>,
    /// Opt the channel into cross-node replication. When set, the
    /// owning `Redex` must have called `enableReplication(mesh)`
    /// first — otherwise `openFile` rejects with a typed
    /// `redex:` error. Leave unset (or `null`) for single-node
    /// behavior. See `CONFIG_REPLICATION.md` for field semantics.
    pub replication: Option<ReplicationConfigJs>,
}

/// Opt-in cross-node replication settings for a RedEX channel.
/// Mirrors `ReplicationConfig` from the core. All fields are
/// optional — omitted ones fall back to the core's defaults
/// (`factor=3`, `heartbeat_ms=500`, `placement=Standard`,
/// `on_under_capacity=Withdraw`, `replication_budget_fraction=0.5`).
#[napi(object)]
pub struct ReplicationConfigJs {
    /// Replication factor (replicas including the leader). Range
    /// `[1, 16]`. Defaults to `3` when omitted.
    pub factor: Option<u32>,
    /// Heartbeat cadence between leader → replicas in milliseconds.
    /// Minimum `100`. Defaults to `500` when omitted.
    pub heartbeat_ms: Option<BigInt>,
    /// Placement strategy. One of `"standard"` (default), `"pinned"`,
    /// or `"colocation-strict"`. With `"pinned"` the `pinned_nodes`
    /// field is required and pins the effective replication factor
    /// to the list length.
    pub placement: Option<String>,
    /// Pinned `NodeId` list, required when `placement = "pinned"`.
    /// Ignored otherwise.
    pub pinned_nodes: Option<Vec<BigInt>>,
    /// Pin the leader to a specific `NodeId`. Optional; the
    /// deterministic election picks the lowest-RTT healthy replica
    /// when omitted.
    pub leader_pinned: Option<BigInt>,
    /// Behavior on disk-pressure. `"withdraw"` (default) drops the
    /// replica role; `"evict-oldest"` calls retention sweep and
    /// retries.
    pub on_under_capacity: Option<String>,
    /// Bandwidth budget for replication-sync I/O as a fraction of
    /// measured NIC peak. Range `(0.0, 1.0]`. Defaults to `0.5`
    /// when omitted.
    pub replication_budget_fraction: Option<f64>,
}

/// JS-side config for `Redex.enableGravityForGreedy`. Locked
/// Phase-4 defaults — `DATAFORTS_PLAN.md` § Phase 4. All fields
/// optional; omit any to keep the substrate default.
#[cfg(feature = "dataforts")]
#[napi(object)]
#[derive(Default)]
pub struct DataGravityConfigJs {
    /// Whether the counter + emission cycle is active. Default
    /// `true`; pass `false` to keep the policy carried but
    /// suppress emissions.
    pub enabled: Option<bool>,
    /// Re-emission threshold ratio. Range `[1.01, 10.0]`. Default
    /// `2.0`.
    pub emit_threshold_ratio: Option<f64>,
    /// Decay half-life in seconds. Default `1800` (30 min).
    /// `BigInt` so values match the Python `u64` / Go `uint64`
    /// shape; u32 here would overflow at ~136 years which is
    /// silly but the cross-binding parity matters more.
    pub decay_half_life_secs: Option<BigInt>,
    /// Tick interval for the gravity tick task, in milliseconds.
    /// Default `500`. `BigInt` for parity with Python u64 / Go
    /// uint64; a u32 ms field overflows at ~49 days.
    pub tick_interval_ms: Option<BigInt>,
}

/// JS-side config for `Redex.enableGreedyDataforts`. Locked
/// Phase-1 defaults — `DATAFORTS_PLAN.md` § Phase 1. All fields
/// optional; omit any to keep the substrate default.
#[cfg(feature = "dataforts")]
#[napi(object)]
#[derive(Default)]
pub struct GreedyConfigJs {
    /// Scope filter — chains whose `scope:` tag matches any of
    /// these admit. Empty / omitted admits regardless of scope.
    pub scopes: Option<Vec<String>>,
    /// Maximum acceptable RTT to the chain's home node, in
    /// milliseconds. Default `200`.
    pub proximity_max_rtt_ms: Option<u32>,
    /// Per-channel byte cap on the cache substrate. Default
    /// `100 * 1024 * 1024` (100 MiB). Floor `1 * 1024 * 1024`.
    pub per_channel_cap_bytes: Option<BigInt>,
    /// Total byte cap across every channel. LRU eviction drives
    /// toward this bound. Default `10 * 1024 * 1024 * 1024` (10
    /// GiB).
    pub total_cap_bytes: Option<BigInt>,
    /// I/O budget as a fraction of measured NIC peak.
    /// Range `(0.0, 1.0]`. Default `0.25`.
    pub bandwidth_budget_fraction: Option<f64>,
    /// Override for the NIC peak (bytes/sec) the bandwidth budget
    /// computes against. Default falls back to 1 Gbps. Deployments
    /// on faster NICs should set this explicitly to avoid the
    /// `bandwidth` reason saturating the admit_rejected counter
    /// under normal load.
    pub nic_peak_bytes_per_s: Option<BigInt>,
    /// Maximum in-flight `observe_event` tasks. Default `1024`.
    /// Floor 1. Past this many spawned tasks the observer drops
    /// events with a metric increment rather than blocking the
    /// mesh inbound hot path.
    pub observer_inflight_cap: Option<u32>,
    /// Intent-match policy. One of `"disabled"` /
    /// `"any_of_local_capabilities"` (default) / `"strict"`.
    pub intent_match: Option<String>,
    /// Colocation policy. One of `"ignore"` / `"soft_preference"`
    /// (default) / `"strict_required"`.
    pub colocation_policy: Option<String>,
}

fn resolve_placement_strategy(
    placement: Option<String>,
    pinned_nodes: Option<Vec<BigInt>>,
    leader_pinned: Option<&BigInt>,
) -> Result<InnerPlacementStrategy> {
    match placement.as_deref() {
        None | Some("standard") => Ok(InnerPlacementStrategy::Standard),
        Some("colocation-strict") => Ok(InnerPlacementStrategy::ColocationStrict),
        Some("pinned") => {
            let nodes = pinned_nodes.ok_or_else(|| {
                redex_err(
                    "replication.pinned_nodes",
                    "required when placement = 'pinned'",
                )
            })?;
            // R-27: reject empty pinned_nodes at the binding
            // layer for a clearer error than the core
            // validator's PinnedSetEmpty.
            if nodes.is_empty() {
                return Err(redex_err(
                    "replication.pinned_nodes",
                    "must be non-empty when placement = 'pinned'",
                ));
            }
            let mut out = Vec::with_capacity(nodes.len());
            for (i, n) in nodes.into_iter().enumerate() {
                out.push(redex_bigint_u64(
                    &format!("replication.pinned_nodes[{i}]"),
                    n,
                )?);
            }
            // R-28: cross-check leader_pinned membership at
            // the binding layer for a clearer error than the
            // core's LeaderPinnedNotInPinnedSet.
            if let Some(lp_big) = leader_pinned {
                let lp = redex_bigint_u64("replication.leader_pinned", lp_big.clone())?;
                if !out.contains(&lp) {
                    return Err(redex_err(
                        "replication.leader_pinned",
                        format!("{lp} is not in replication.pinned_nodes"),
                    ));
                }
            }
            Ok(InnerPlacementStrategy::Pinned(out))
        }
        Some(other) => Err(redex_err(
            "replication.placement",
            format!(
                "unknown strategy {other:?}; expected 'standard', 'pinned', or 'colocation-strict'"
            ),
        )),
    }
}

fn resolve_under_capacity(s: Option<String>) -> Result<InnerUnderCapacity> {
    match s.as_deref() {
        None | Some("withdraw") => Ok(InnerUnderCapacity::Withdraw),
        Some("evict-oldest") => Ok(InnerUnderCapacity::EvictOldest),
        Some(other) => Err(redex_err(
            "replication.on_under_capacity",
            format!("unknown policy {other:?}; expected 'withdraw' or 'evict-oldest'"),
        )),
    }
}

fn resolve_replication_config(cfg: ReplicationConfigJs) -> Result<InnerReplicationConfig> {
    let mut out = InnerReplicationConfig::new();
    if let Some(f) = cfg.factor {
        // `factor` rides as `u32` (BigInt would be overkill for a
        // u8 range). Reject anything that doesn't fit in u8 here
        // rather than silently truncating.
        if f > u8::MAX as u32 {
            return Err(redex_err(
                "replication.factor",
                format!("must fit in u8 (got {f})"),
            ));
        }
        out = out.with_factor(f as u8);
    }
    if let Some(hb) = cfg.heartbeat_ms {
        out = out.with_heartbeat_ms(redex_bigint_u64("replication.heartbeat_ms", hb)?);
    }
    out = out.with_placement(resolve_placement_strategy(
        cfg.placement,
        cfg.pinned_nodes,
        cfg.leader_pinned.as_ref(),
    )?);
    if let Some(leader) = cfg.leader_pinned {
        out = out.with_leader_pinned(Some(redex_bigint_u64("replication.leader_pinned", leader)?));
    }
    out = out.with_on_under_capacity(resolve_under_capacity(cfg.on_under_capacity)?);
    if let Some(fraction) = cfg.replication_budget_fraction {
        out = out.with_replication_budget_fraction(fraction as f32);
    }
    // Validate fail-fast so a malformed config can't reach
    // `open_file`. The core revalidates there too, but the
    // binding-side error gives a cleaner stack trace.
    out.validate()
        .map_err(|e| redex_err("replication config invalid", e))?;
    Ok(out)
}

/// Validate a `BigInt` config field while preserving the `redex:`
/// error-message prefix so the SDK can classify it as `RedexError`.
/// The shared `common::bigint_u64` emits prefix-less errors; rethrow
/// with the RedEX prefix tacked on.
fn redex_bigint_u64(field: &str, b: BigInt) -> Result<u64> {
    bigint_u64(b).map_err(|e| redex_err(&format!("config.{}", field), e.reason.clone()))
}

fn resolve_redex_file_config(cfg: Option<RedexFileConfigJs>) -> Result<RedexFileConfig> {
    let Some(c) = cfg else {
        return Ok(RedexFileConfig::default());
    };
    let mut out = RedexFileConfig::default();
    if let Some(p) = c.persistent {
        out.persistent = p;
    }
    match (c.fsync_every_n, c.fsync_interval_ms) {
        (Some(_), Some(_)) => {
            return Err(redex_err(
                "config",
                "fsync_every_n and fsync_interval_ms are mutually exclusive",
            ));
        }
        (Some(n), None) => {
            let n = redex_bigint_u64("fsync_every_n", n)?;
            if n == 0 {
                return Err(redex_err("config", "fsync_every_n must be > 0"));
            }
            out.fsync_policy = InnerFsyncPolicy::EveryN(n);
        }
        (None, Some(ms)) => {
            if ms == 0 {
                return Err(redex_err("config", "fsync_interval_ms must be > 0"));
            }
            out.fsync_policy =
                InnerFsyncPolicy::Interval(std::time::Duration::from_millis(ms as u64));
        }
        (None, None) => {}
    }
    if let Some(n) = c.retention_max_events {
        out.retention_max_events = Some(redex_bigint_u64("retention_max_events", n)?);
    }
    if let Some(b) = c.retention_max_bytes {
        out.retention_max_bytes = Some(redex_bigint_u64("retention_max_bytes", b)?);
    }
    if let Some(ms) = c.retention_max_age_ms {
        let ms = redex_bigint_u64("retention_max_age_ms", ms)?;
        out.retention_max_age_ns = Some(ms.saturating_mul(1_000_000));
    }
    if let Some(rep) = c.replication {
        out.replication = Some(resolve_replication_config(rep)?);
    }
    Ok(out)
}

/// A materialized RedEX event: `seq` + `payload`.
#[napi(object)]
pub struct RedexEventJs {
    pub seq: BigInt,
    pub payload: Buffer,
    /// Low-28-bit xxh3 truncation of the payload, stamped at append
    /// time. Use to detect storage corruption.
    pub checksum: u32,
    /// True if the 8-byte payload was stored inline in the entry
    /// record rather than in the payload segment.
    pub is_inline: bool,
}

impl From<InnerRedexEvent> for RedexEventJs {
    fn from(ev: InnerRedexEvent) -> Self {
        RedexEventJs {
            seq: BigInt::from(ev.entry.seq),
            payload: Buffer::from(ev.payload.as_ref()),
            checksum: ev.entry.checksum(),
            is_inline: ev.entry.is_inline(),
        }
    }
}

/// Raw RedEX file handle. Append / tail / read without the CortEX
/// adapter layer. Cheap to clone (internal `Arc`).
#[napi]
pub struct RedexFile {
    inner: Arc<InnerRedexFile>,
}

#[napi]
impl RedexFile {
    /// Append one payload. Returns the assigned sequence number.
    #[napi]
    pub fn append(&self, payload: Buffer) -> Result<BigInt> {
        let seq = self
            .inner
            .append(payload.as_ref())
            .map_err(|e| redex_err("append", e))?;
        Ok(BigInt::from(seq))
    }

    /// Append a batch of payloads atomically. Returns the sequence
    /// number of the FIRST appended event, or `null` if `payloads`
    /// was empty (no events appended). Callers deduce subsequent
    /// seqs as `first + 0, first + 1, ...`.
    ///
    /// The underlying `RedexFile::append_batch`
    /// returns `Result<Option<u64>>` so callers can distinguish
    /// "wrote zero" from "wrote one with seq N". The TypeScript
    /// signature mirrors that — `BigInt | null`.
    #[napi]
    pub fn append_batch(&self, payloads: Vec<Buffer>) -> Result<Option<BigInt>> {
        let bytes: Vec<Bytes> = payloads
            .into_iter()
            .map(|b| Bytes::copy_from_slice(b.as_ref()))
            .collect();
        let seq = self
            .inner
            .append_batch(&bytes)
            .map_err(|e| redex_err("append_batch", e))?;
        Ok(seq.map(BigInt::from))
    }

    /// Read the half-open range `[start, end)` from the in-memory
    /// index. Returns only entries still retained — any seq in the
    /// range that has been evicted is silently skipped.
    #[napi]
    pub fn read_range(&self, start: BigInt, end: BigInt) -> Result<Vec<RedexEventJs>> {
        let s = redex_bigint_u64("start", start)?;
        let e = redex_bigint_u64("end", end)?;
        Ok(self
            .inner
            .read_range(s, e)
            .into_iter()
            .map(RedexEventJs::from)
            .collect())
    }

    /// Number of retained events (post-retention eviction). Returned
    /// as `BigInt` so event counts above `u32::MAX` (~4.3 B) don't
    /// silently truncate.
    #[napi]
    pub fn len(&self) -> BigInt {
        BigInt::from(self.inner.len() as u64)
    }

    /// Open a live tail over this file. The iterator yields every
    /// event with `seq >= fromSeq` (default `0`), atomically
    /// backfilling the existing retained range and then streaming
    /// subsequent appends. Terminate early with `.close()` or by
    /// breaking out of `for await` — breaking triggers `return()`,
    /// which the SDK wrapper routes to `close()`.
    ///
    /// Declared `async` so the underlying `UnboundedReceiverStream`
    /// lives inside the napi tokio runtime.
    #[napi]
    pub async fn tail(&self, from_seq: Option<BigInt>) -> Result<RedexTailIter> {
        let from = match from_seq {
            Some(s) => redex_bigint_u64("from_seq", s)?,
            None => 0,
        };
        let stream = self.inner.tail(from);
        use futures::StreamExt;
        let boxed: BoxStream<'static, std::result::Result<InnerRedexEvent, InnerRedexError>> =
            stream.boxed();
        Ok(RedexTailIter {
            inner: Arc::new(RedexTailIterInner {
                stream: TokioMutex::new(Some(boxed)),
                shutdown: Notify::new(),
                is_shutdown: AtomicBool::new(false),
            }),
        })
    }

    /// Explicit fsync. Always fsyncs regardless of configured
    /// `fsyncPolicy`. No-op on heap-only files.
    ///
    /// Declared `async` so the disk flush runs on a napi worker
    /// thread rather than the JavaScript event-loop thread —
    /// without this, a `persistent: true` channel's fsync stalls
    /// every other JS callback until the kernel completes the
    /// write.
    #[napi]
    pub async fn sync(&self) -> Result<()> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || inner.sync())
            .await
            .map_err(|e| napi::Error::from_reason(format!("redex: sync join: {e}")))?
            .map_err(|e| redex_err("sync", e))
    }

    /// Close the file. Outstanding tail iterators resolve with a
    /// `redex:` error on their next `.next()` call.
    ///
    /// Declared `async` for the same reason as `sync` — close
    /// flushes pending writes on persistent files.
    #[napi]
    pub async fn close(&self) -> Result<()> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || inner.close())
            .await
            .map_err(|e| napi::Error::from_reason(format!("redex: close join: {e}")))?
            .map_err(|e| redex_err("close", e))
    }
}

struct RedexTailIterInner {
    stream: TokioMutex<
        Option<BoxStream<'static, std::result::Result<InnerRedexEvent, InnerRedexError>>>,
    >,
    shutdown: Notify,
    is_shutdown: AtomicBool,
}

/// Async iterator over a live `RedexFile::tail`.
#[napi]
pub struct RedexTailIter {
    inner: Arc<RedexTailIterInner>,
}

#[napi]
impl RedexTailIter {
    /// Wait for the next event. Returns `null` when the iterator has
    /// been closed or the underlying file was closed. Throws a
    /// `redex:` error if the backing stream yielded an error item.
    #[napi]
    pub async fn next(&self) -> Result<Option<RedexEventJs>> {
        if self.inner.is_shutdown.load(Ordering::Acquire) {
            return Ok(None);
        }
        let mut guard = self.inner.stream.lock().await;
        let stream = match guard.as_mut() {
            Some(s) => s,
            None => return Ok(None),
        };

        let shutdown_fut = self.inner.shutdown.notified();
        tokio::pin!(shutdown_fut);
        shutdown_fut.as_mut().enable();

        if self.inner.is_shutdown.load(Ordering::Acquire) {
            *guard = None;
            return Ok(None);
        }

        tokio::select! {
            biased;
            _ = shutdown_fut => {
                *guard = None;
                Ok(None)
            }
            msg = stream.next() => match msg {
                Some(Ok(event)) => Ok(Some(RedexEventJs::from(event))),
                Some(Err(e)) => {
                    // The tail stream surfaces RedexError::Closed when
                    // the owning file is closed; map that to a normal
                    // stream-end so for-await loops terminate cleanly.
                    *guard = None;
                    if matches!(e, InnerRedexError::Closed) {
                        Ok(None)
                    } else {
                        Err(redex_err("tail", e))
                    }
                }
                None => {
                    *guard = None;
                    Ok(None)
                }
            }
        }
    }

    /// Terminate the iterator. Idempotent.
    #[napi]
    pub fn close(&self) {
        self.inner.is_shutdown.store(true, Ordering::Release);
        self.inner.shutdown.notify_waiters();
    }
}

// =========================================================================
// Tasks
// =========================================================================

/// Task lifecycle status.
#[napi(string_enum)]
#[derive(Clone)]
pub enum TaskStatus {
    Pending,
    Completed,
}

impl From<InnerTaskStatus> for TaskStatus {
    fn from(s: InnerTaskStatus) -> Self {
        match s {
            InnerTaskStatus::Pending => TaskStatus::Pending,
            InnerTaskStatus::Completed => TaskStatus::Completed,
        }
    }
}

impl From<TaskStatus> for InnerTaskStatus {
    fn from(s: TaskStatus) -> Self {
        match s {
            TaskStatus::Pending => InnerTaskStatus::Pending,
            TaskStatus::Completed => InnerTaskStatus::Completed,
        }
    }
}

/// Ordering for task queries.
#[napi(string_enum)]
pub enum TasksOrderBy {
    IdAsc,
    IdDesc,
    CreatedAsc,
    CreatedDesc,
    UpdatedAsc,
    UpdatedDesc,
}

impl From<TasksOrderBy> for InnerTasksOrderBy {
    fn from(o: TasksOrderBy) -> Self {
        match o {
            TasksOrderBy::IdAsc => InnerTasksOrderBy::IdAsc,
            TasksOrderBy::IdDesc => InnerTasksOrderBy::IdDesc,
            TasksOrderBy::CreatedAsc => InnerTasksOrderBy::CreatedAsc,
            TasksOrderBy::CreatedDesc => InnerTasksOrderBy::CreatedDesc,
            TasksOrderBy::UpdatedAsc => InnerTasksOrderBy::UpdatedAsc,
            TasksOrderBy::UpdatedDesc => InnerTasksOrderBy::UpdatedDesc,
        }
    }
}

/// A materialized task record.
#[napi(object)]
#[derive(Clone)]
pub struct Task {
    pub id: BigInt,
    pub title: String,
    pub status: TaskStatus,
    pub created_ns: BigInt,
    pub updated_ns: BigInt,
}

impl From<InnerTask> for Task {
    fn from(t: InnerTask) -> Self {
        Task {
            id: BigInt::from(t.id),
            title: t.title,
            status: t.status.into(),
            created_ns: BigInt::from(t.created_ns),
            updated_ns: BigInt::from(t.updated_ns),
        }
    }
}

/// Filter for [`TasksAdapter::list_tasks`] and
/// [`TasksAdapter::watch_tasks`].
#[napi(object)]
pub struct TaskFilter {
    pub status: Option<TaskStatus>,
    pub title_contains: Option<String>,
    pub created_after_ns: Option<BigInt>,
    pub created_before_ns: Option<BigInt>,
    pub updated_after_ns: Option<BigInt>,
    pub updated_before_ns: Option<BigInt>,
    pub order_by: Option<TasksOrderBy>,
    pub limit: Option<u32>,
}

// =========================================================================
// Task watch iterator (napi)
// =========================================================================

struct TaskWatchIterInner {
    stream: TokioMutex<Option<BoxStream<'static, Vec<InnerTask>>>>,
    shutdown: Notify,
    /// Set by `close()` before notifying. `next()` pre-checks this
    /// flag so a close that races ahead of `next()` is still observed
    /// (raw `Notify::notify_waiters` only wakes currently-registered
    /// waiters).
    is_shutdown: AtomicBool,
}

/// Async iterator over a live task filter.
///
/// Rust returns `null` from [`Self::next`] when the underlying
/// watcher ends; JS should treat that as `done: true`. Paired with
/// the JS helper in the test suite below, this cleanly wraps into a
/// `for await (const tasks of ...)` loop.
#[napi]
pub struct TaskWatchIter {
    inner: Arc<TaskWatchIterInner>,
}

#[napi]
impl TaskWatchIter {
    /// Wait for the next filter result. Returns `null` when the
    /// iterator has been closed or the underlying stream has ended.
    #[napi]
    pub async fn next(&self) -> Option<Vec<Task>> {
        task_watch_next(&self.inner).await
    }

    /// Terminate the iterator early. Any pending `next()` call
    /// resolves to `null`. Subsequent `next()` calls also return
    /// `null`. Idempotent.
    #[napi]
    pub fn close(&self) {
        task_watch_close(&self.inner);
    }
}

/// Typed tasks adapter handle.
#[napi]
pub struct TasksAdapter {
    inner: Arc<InnerTasksAdapter>,
}

impl Clone for TasksAdapter {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

#[napi]
impl TasksAdapter {
    /// Open the tasks adapter against a Redex manager.
    ///
    /// `persistent` — when `true`, the file writes to disk under the
    /// Redex's configured persistent directory and replays from disk
    /// on reopen. Requires the Redex to have been constructed with
    /// `persistentDir`; otherwise `open()` errors.
    ///
    /// Declared `async` so napi-rs runs it with its tokio runtime
    /// active — the underlying `CortexAdapter::open` spawns the
    /// fold task via `tokio::spawn` and needs a live reactor.
    #[napi(factory)]
    pub async fn open(
        redex: &Redex,
        origin_hash: BigInt,
        persistent: Option<bool>,
    ) -> Result<Self> {
        let cfg = redex_config_from_persistent(persistent);
        let origin = bigint_u64(origin_hash)?;
        let inner = InnerTasksAdapter::open_with_config(&redex.inner, origin, cfg)
            .await
            .map_err(|e| cortex_err("TasksAdapter open failed", e))?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Open from a snapshot captured via [`Self::snapshot`]. Skips
    /// replay of events `[0, lastSeq]` on the underlying file.
    #[napi(factory)]
    pub async fn open_from_snapshot(
        redex: &Redex,
        origin_hash: BigInt,
        state_bytes: Buffer,
        last_seq: Option<BigInt>,
        persistent: Option<bool>,
    ) -> Result<Self> {
        let cfg = redex_config_from_persistent(persistent);
        let origin = bigint_u64(origin_hash)?;
        let last = last_seq.map(bigint_u64).transpose()?;
        let inner = InnerTasksAdapter::open_from_snapshot_with_config(
            &redex.inner,
            origin,
            cfg,
            state_bytes.as_ref(),
            last,
        )
        .await
        .map_err(|e| cortex_err("TasksAdapter open_from_snapshot failed", e))?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Capture a state snapshot. Persist both fields together; restore
    /// via [`Self::open_from_snapshot`].
    #[napi]
    pub fn snapshot(&self) -> Result<CortexSnapshot> {
        let (bytes, last_seq) = self
            .inner
            .snapshot()
            .map_err(|e| cortex_err("snapshot failed", e))?;
        Ok(CortexSnapshot {
            state_bytes: Buffer::from(bytes),
            last_seq: last_seq.map(BigInt::from),
        })
    }

    /// Create a new task. Returns the RedEX sequence of the append.
    #[napi]
    pub fn create(&self, id: BigInt, title: String, now_ns: BigInt) -> Result<BigInt> {
        self.inner
            .create(bigint_u64(id)?, title, bigint_u64(now_ns)?)
            .map(BigInt::from)
            .map_err(|e| cortex_err("create failed", e))
    }

    /// Rename an existing task. No-op at fold time if `id` is unknown.
    #[napi]
    pub fn rename(&self, id: BigInt, new_title: String, now_ns: BigInt) -> Result<BigInt> {
        self.inner
            .rename(bigint_u64(id)?, new_title, bigint_u64(now_ns)?)
            .map(BigInt::from)
            .map_err(|e| cortex_err("rename failed", e))
    }

    /// Mark a task completed. No-op at fold time if `id` is unknown.
    #[napi]
    pub fn complete(&self, id: BigInt, now_ns: BigInt) -> Result<BigInt> {
        self.inner
            .complete(bigint_u64(id)?, bigint_u64(now_ns)?)
            .map(BigInt::from)
            .map_err(|e| cortex_err("complete failed", e))
    }

    /// Delete a task.
    #[napi]
    pub fn delete(&self, id: BigInt) -> Result<BigInt> {
        self.inner
            .delete(bigint_u64(id)?)
            .map(BigInt::from)
            .map_err(|e| cortex_err("delete failed", e))
    }

    /// Block until every event up through `seq` has been folded into
    /// state. Use as a read-after-write barrier.
    #[napi]
    pub async fn wait_for_seq(&self, seq: BigInt) -> Result<()> {
        self.inner.wait_for_seq(bigint_u64(seq)?).await;
        Ok(())
    }

    /// Read-your-writes wait. Blocks until this adapter's fold has
    /// applied through `token.seq`, or `deadlineMs` elapses. Rejects
    /// a wrong-origin token or a saturated wait queue (default cap
    /// 1024) immediately. All three failure variants surface as a
    /// `cortex:` prefixed napi `Error`.
    #[napi]
    pub async fn wait_for_token(&self, token: &WriteToken, deadline_ms: u32) -> Result<()> {
        let inner_token = token.as_inner();
        self.inner
            .wait_for_token(
                inner_token,
                std::time::Duration::from_millis(deadline_ms as u64),
            )
            .await
            .map_err(wait_for_token_err)
    }

    /// Close the adapter. Idempotent.
    #[napi]
    pub fn close(&self) -> Result<()> {
        self.inner
            .close()
            .map_err(|e| cortex_err("close failed", e))
    }

    /// True if the fold task is currently running.
    #[napi]
    pub fn is_running(&self) -> bool {
        self.inner.is_running()
    }

    /// Snapshot query over current state. Clones out matching tasks
    /// as a Vec. Pass `null` / `undefined` for no filter (returns all).
    #[napi]
    pub fn list_tasks(&self, filter: Option<TaskFilter>) -> Result<Vec<Task>> {
        let state_handle = self.inner.state();
        let state = state_handle.read();
        let mut q = state.query();
        if let Some(f) = filter {
            if let Some(s) = f.status {
                q = q.where_status(s.into());
            }
            if let Some(s) = f.title_contains {
                q = q.title_contains(s);
            }
            if let Some(ns) = f.created_after_ns {
                q = q.created_after(bigint_u64(ns)?);
            }
            if let Some(ns) = f.created_before_ns {
                q = q.created_before(bigint_u64(ns)?);
            }
            if let Some(ns) = f.updated_after_ns {
                q = q.updated_after(bigint_u64(ns)?);
            }
            if let Some(ns) = f.updated_before_ns {
                q = q.updated_before(bigint_u64(ns)?);
            }
            if let Some(o) = f.order_by {
                q = q.order_by(o.into());
            }
            if let Some(l) = f.limit {
                q = q.limit(l as usize);
            }
        }
        Ok(q.collect().into_iter().map(Task::from).collect())
    }

    /// Total task count in current state (ignores any filter).
    #[napi]
    pub fn count(&self) -> u32 {
        self.inner.state().read().len() as u32
    }

    /// Open a reactive watcher over the filter. Returns an iterator
    /// whose `.next()` yields the current filter result on first
    /// call, then yields again whenever a fold tick produces a
    /// different filter result (deduplicated).
    ///
    /// Declared `async` so the underlying watcher's `tokio::spawn`
    /// fold-forwarding task runs inside napi's tokio runtime.
    #[napi]
    pub async fn watch_tasks(&self, filter: Option<TaskFilter>) -> Result<TaskWatchIter> {
        let w = task_watcher_with_filter(&self.inner, filter)?;
        let stream: BoxStream<'static, Vec<InnerTask>> = w.stream().boxed();
        Ok(TaskWatchIter {
            inner: Arc::new(TaskWatchIterInner {
                stream: TokioMutex::new(Some(stream)),
                shutdown: Notify::new(),
                is_shutdown: AtomicBool::new(false),
            }),
        })
    }

    /// Atomic "paint what's here now, then react to changes" primitive.
    /// Computes the current filter result AND hands back an iterator
    /// over subsequent deltas in one call so the caller can't race a
    /// mutation that lands between a separate `listTasks` and
    /// `watchTasks`. The iterator drops only leading emissions equal
    /// to the returned snapshot; if a change lands during construction,
    /// the watcher's first emission is forwarded through instead of
    /// being silently dropped.
    ///
    /// Declared `async` for the same tokio-runtime reason as
    /// [`Self::watch_tasks`].
    #[napi]
    pub async fn snapshot_and_watch_tasks(
        &self,
        filter: Option<TaskFilter>,
    ) -> Result<TasksSnapshotAndWatch> {
        let w = task_watcher_with_filter(&self.inner, filter)?;
        let (snapshot, stream) = self.inner.snapshot_and_watch(w);
        let stream: BoxStream<'static, Vec<InnerTask>> = stream;
        Ok(TasksSnapshotAndWatch {
            snapshot: snapshot.into_iter().map(Task::from).collect(),
            inner: Arc::new(TaskWatchIterInner {
                stream: TokioMutex::new(Some(stream)),
                shutdown: Notify::new(),
                is_shutdown: AtomicBool::new(false),
            }),
        })
    }
}

fn task_watcher_with_filter(
    adapter: &InnerTasksAdapter,
    filter: Option<TaskFilter>,
) -> Result<::net::adapter::net::cortex::tasks::TasksWatcher> {
    let mut w = adapter.watch();
    if let Some(f) = filter {
        if let Some(s) = f.status {
            w = w.where_status(s.into());
        }
        if let Some(s) = f.title_contains {
            w = w.title_contains(s);
        }
        if let Some(ns) = f.created_after_ns {
            w = w.created_after(bigint_u64(ns)?);
        }
        if let Some(ns) = f.created_before_ns {
            w = w.created_before(bigint_u64(ns)?);
        }
        if let Some(ns) = f.updated_after_ns {
            w = w.updated_after(bigint_u64(ns)?);
        }
        if let Some(ns) = f.updated_before_ns {
            w = w.updated_before(bigint_u64(ns)?);
        }
        if let Some(o) = f.order_by {
            w = w.order_by(o.into());
        }
        if let Some(l) = f.limit {
            w = w.limit(l as usize);
        }
    }
    Ok(w)
}

/// Result of [`TasksAdapter::snapshot_and_watch_tasks`]. The snapshot
/// reflects the filter result at the moment of the call; `next()` /
/// `close()` drive the delta iterator for subsequent changes.
#[napi]
pub struct TasksSnapshotAndWatch {
    snapshot: Vec<Task>,
    inner: Arc<TaskWatchIterInner>,
}

#[napi]
impl TasksSnapshotAndWatch {
    /// The initial filter result, captured atomically with the
    /// watcher. Clone-on-read; safe to call from JS without
    /// invalidating the iterator.
    #[napi(getter)]
    pub fn snapshot(&self) -> Vec<Task> {
        self.snapshot.clone()
    }

    /// Wait for the next delta. Returns `null` when the iterator has
    /// been closed or the underlying stream has ended. Mirrors
    /// [`TaskWatchIter::next`].
    #[napi]
    pub async fn next(&self) -> Option<Vec<Task>> {
        task_watch_next(&self.inner).await
    }

    /// Terminate the iterator early. Idempotent.
    #[napi]
    pub fn close(&self) {
        task_watch_close(&self.inner);
    }
}

async fn task_watch_next(inner: &Arc<TaskWatchIterInner>) -> Option<Vec<Task>> {
    if inner.is_shutdown.load(Ordering::Acquire) {
        return None;
    }
    let mut guard = inner.stream.lock().await;
    let stream = match guard.as_mut() {
        Some(s) => s,
        None => return None,
    };

    let shutdown_fut = inner.shutdown.notified();
    tokio::pin!(shutdown_fut);
    shutdown_fut.as_mut().enable();

    if inner.is_shutdown.load(Ordering::Acquire) {
        *guard = None;
        return None;
    }

    tokio::select! {
        biased;
        _ = shutdown_fut => {
            *guard = None;
            None
        }
        msg = stream.next() => match msg {
            Some(items) => Some(items.into_iter().map(Task::from).collect()),
            None => {
                *guard = None;
                None
            }
        }
    }
}

fn task_watch_close(inner: &Arc<TaskWatchIterInner>) {
    inner.is_shutdown.store(true, Ordering::Release);
    inner.shutdown.notify_waiters();
}

// =========================================================================
// Memories
// =========================================================================

/// Ordering for memory queries.
#[napi(string_enum)]
pub enum MemoriesOrderBy {
    IdAsc,
    IdDesc,
    CreatedAsc,
    CreatedDesc,
    UpdatedAsc,
    UpdatedDesc,
}

impl From<MemoriesOrderBy> for InnerMemoriesOrderBy {
    fn from(o: MemoriesOrderBy) -> Self {
        match o {
            MemoriesOrderBy::IdAsc => InnerMemoriesOrderBy::IdAsc,
            MemoriesOrderBy::IdDesc => InnerMemoriesOrderBy::IdDesc,
            MemoriesOrderBy::CreatedAsc => InnerMemoriesOrderBy::CreatedAsc,
            MemoriesOrderBy::CreatedDesc => InnerMemoriesOrderBy::CreatedDesc,
            MemoriesOrderBy::UpdatedAsc => InnerMemoriesOrderBy::UpdatedAsc,
            MemoriesOrderBy::UpdatedDesc => InnerMemoriesOrderBy::UpdatedDesc,
        }
    }
}

/// A materialized memory record.
#[napi(object)]
#[derive(Clone)]
pub struct Memory {
    pub id: BigInt,
    pub content: String,
    pub tags: Vec<String>,
    pub source: String,
    pub created_ns: BigInt,
    pub updated_ns: BigInt,
    pub pinned: bool,
}

impl From<InnerMemory> for Memory {
    fn from(m: InnerMemory) -> Self {
        Memory {
            id: BigInt::from(m.id),
            content: m.content,
            tags: m.tags,
            source: m.source,
            created_ns: BigInt::from(m.created_ns),
            updated_ns: BigInt::from(m.updated_ns),
            pinned: m.pinned,
        }
    }
}

/// Filter for [`MemoriesAdapter::list_memories`] and
/// [`MemoriesAdapter::watch_memories`]. Tag predicates:
///
/// - `tag` — must include this exact tag.
/// - `any_tag` — must include at least one tag from the array.
/// - `all_tags` — must include every tag in the array.
#[napi(object)]
pub struct MemoryFilter {
    pub source: Option<String>,
    pub content_contains: Option<String>,
    pub tag: Option<String>,
    pub any_tag: Option<Vec<String>>,
    pub all_tags: Option<Vec<String>>,
    pub pinned: Option<bool>,
    pub created_after_ns: Option<BigInt>,
    pub created_before_ns: Option<BigInt>,
    pub updated_after_ns: Option<BigInt>,
    pub updated_before_ns: Option<BigInt>,
    pub order_by: Option<MemoriesOrderBy>,
    pub limit: Option<u32>,
}

// =========================================================================
// Memory watch iterator (napi)
// =========================================================================

struct MemoryWatchIterInner {
    stream: TokioMutex<Option<BoxStream<'static, Vec<InnerMemory>>>>,
    shutdown: Notify,
    is_shutdown: AtomicBool,
}

/// Async iterator over a live memory filter.
#[napi]
pub struct MemoryWatchIter {
    inner: Arc<MemoryWatchIterInner>,
}

#[napi]
impl MemoryWatchIter {
    /// Wait for the next filter result. Returns `null` when the
    /// iterator has been closed or the underlying stream has ended.
    #[napi]
    pub async fn next(&self) -> Option<Vec<Memory>> {
        memory_watch_next(&self.inner).await
    }

    /// Terminate the iterator early. Idempotent.
    #[napi]
    pub fn close(&self) {
        memory_watch_close(&self.inner);
    }
}

async fn memory_watch_next(inner: &Arc<MemoryWatchIterInner>) -> Option<Vec<Memory>> {
    if inner.is_shutdown.load(Ordering::Acquire) {
        return None;
    }
    let mut guard = inner.stream.lock().await;
    let stream = match guard.as_mut() {
        Some(s) => s,
        None => return None,
    };

    let shutdown_fut = inner.shutdown.notified();
    tokio::pin!(shutdown_fut);
    shutdown_fut.as_mut().enable();

    if inner.is_shutdown.load(Ordering::Acquire) {
        *guard = None;
        return None;
    }

    tokio::select! {
        biased;
        _ = shutdown_fut => {
            *guard = None;
            None
        }
        msg = stream.next() => match msg {
            Some(items) => Some(items.into_iter().map(Memory::from).collect()),
            None => {
                *guard = None;
                None
            }
        }
    }
}

fn memory_watch_close(inner: &Arc<MemoryWatchIterInner>) {
    inner.is_shutdown.store(true, Ordering::Release);
    inner.shutdown.notify_waiters();
}

/// Typed memories adapter handle.
#[napi]
pub struct MemoriesAdapter {
    inner: Arc<InnerMemoriesAdapter>,
}

impl Clone for MemoriesAdapter {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

#[napi]
impl MemoriesAdapter {
    /// Open the memories adapter against a Redex manager. See
    /// [`TasksAdapter::open`] for `persistent` semantics.
    #[napi(factory)]
    pub async fn open(
        redex: &Redex,
        origin_hash: BigInt,
        persistent: Option<bool>,
    ) -> Result<Self> {
        let cfg = redex_config_from_persistent(persistent);
        let origin = bigint_u64(origin_hash)?;
        let inner = InnerMemoriesAdapter::open_with_config(&redex.inner, origin, cfg)
            .await
            .map_err(|e| cortex_err("MemoriesAdapter open failed", e))?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Open from a snapshot captured via [`Self::snapshot`].
    #[napi(factory)]
    pub async fn open_from_snapshot(
        redex: &Redex,
        origin_hash: BigInt,
        state_bytes: Buffer,
        last_seq: Option<BigInt>,
        persistent: Option<bool>,
    ) -> Result<Self> {
        let cfg = redex_config_from_persistent(persistent);
        let origin = bigint_u64(origin_hash)?;
        let last = last_seq.map(bigint_u64).transpose()?;
        let inner = InnerMemoriesAdapter::open_from_snapshot_with_config(
            &redex.inner,
            origin,
            cfg,
            state_bytes.as_ref(),
            last,
        )
        .await
        .map_err(|e| cortex_err("MemoriesAdapter open_from_snapshot failed", e))?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Capture a state snapshot for restore via
    /// [`Self::open_from_snapshot`].
    #[napi]
    pub fn snapshot(&self) -> Result<CortexSnapshot> {
        let (bytes, last_seq) = self
            .inner
            .snapshot()
            .map_err(|e| cortex_err("snapshot failed", e))?;
        Ok(CortexSnapshot {
            state_bytes: Buffer::from(bytes),
            last_seq: last_seq.map(BigInt::from),
        })
    }

    /// Store a new memory.
    #[napi]
    pub fn store(
        &self,
        id: BigInt,
        content: String,
        tags: Vec<String>,
        source: String,
        now_ns: BigInt,
    ) -> Result<BigInt> {
        self.inner
            .store(bigint_u64(id)?, content, tags, source, bigint_u64(now_ns)?)
            .map(BigInt::from)
            .map_err(|e| cortex_err("store failed", e))
    }

    /// Replace the tag set on an existing memory. No-op at fold time
    /// if `id` is unknown.
    #[napi]
    pub fn retag(&self, id: BigInt, tags: Vec<String>, now_ns: BigInt) -> Result<BigInt> {
        self.inner
            .retag(bigint_u64(id)?, tags, bigint_u64(now_ns)?)
            .map(BigInt::from)
            .map_err(|e| cortex_err("retag failed", e))
    }

    /// Pin a memory.
    #[napi]
    pub fn pin(&self, id: BigInt, now_ns: BigInt) -> Result<BigInt> {
        self.inner
            .pin(bigint_u64(id)?, bigint_u64(now_ns)?)
            .map(BigInt::from)
            .map_err(|e| cortex_err("pin failed", e))
    }

    /// Unpin a memory.
    #[napi]
    pub fn unpin(&self, id: BigInt, now_ns: BigInt) -> Result<BigInt> {
        self.inner
            .unpin(bigint_u64(id)?, bigint_u64(now_ns)?)
            .map(BigInt::from)
            .map_err(|e| cortex_err("unpin failed", e))
    }

    /// Delete a memory.
    #[napi]
    pub fn delete(&self, id: BigInt) -> Result<BigInt> {
        self.inner
            .delete(bigint_u64(id)?)
            .map(BigInt::from)
            .map_err(|e| cortex_err("delete failed", e))
    }

    /// Block until every event up through `seq` has been folded.
    #[napi]
    pub async fn wait_for_seq(&self, seq: BigInt) -> Result<()> {
        self.inner.wait_for_seq(bigint_u64(seq)?).await;
        Ok(())
    }

    /// Read-your-writes wait. See `TasksAdapter.waitForToken` for
    /// the full contract.
    #[napi]
    pub async fn wait_for_token(&self, token: &WriteToken, deadline_ms: u32) -> Result<()> {
        let inner_token = token.as_inner();
        self.inner
            .wait_for_token(
                inner_token,
                std::time::Duration::from_millis(deadline_ms as u64),
            )
            .await
            .map_err(wait_for_token_err)
    }

    /// Close the adapter. Idempotent.
    #[napi]
    pub fn close(&self) -> Result<()> {
        self.inner
            .close()
            .map_err(|e| cortex_err("close failed", e))
    }

    /// True if the fold task is currently running.
    #[napi]
    pub fn is_running(&self) -> bool {
        self.inner.is_running()
    }

    /// Snapshot query. See [`MemoryFilter`] for available predicates.
    #[napi]
    pub fn list_memories(&self, filter: Option<MemoryFilter>) -> Result<Vec<Memory>> {
        let state_handle = self.inner.state();
        let state = state_handle.read();
        let mut q = state.query();
        if let Some(f) = filter {
            if let Some(s) = f.source {
                q = q.where_source(s);
            }
            if let Some(s) = f.content_contains {
                q = q.content_contains(s);
            }
            if let Some(tag) = f.tag {
                q = q.where_tag(tag);
            }
            if let Some(tags) = f.any_tag {
                q = q.where_any_tag(tags);
            }
            if let Some(tags) = f.all_tags {
                q = q.where_all_tags(tags);
            }
            if let Some(pinned) = f.pinned {
                q = q.where_pinned(pinned);
            }
            if let Some(ns) = f.created_after_ns {
                q = q.created_after(bigint_u64(ns)?);
            }
            if let Some(ns) = f.created_before_ns {
                q = q.created_before(bigint_u64(ns)?);
            }
            if let Some(ns) = f.updated_after_ns {
                q = q.updated_after(bigint_u64(ns)?);
            }
            if let Some(ns) = f.updated_before_ns {
                q = q.updated_before(bigint_u64(ns)?);
            }
            if let Some(o) = f.order_by {
                q = q.order_by(o.into());
            }
            if let Some(l) = f.limit {
                q = q.limit(l as usize);
            }
        }
        Ok(q.collect().into_iter().map(Memory::from).collect())
    }

    /// Total memory count in current state (ignores any filter).
    #[napi]
    pub fn count(&self) -> u32 {
        self.inner.state().read().len() as u32
    }

    /// Open a reactive watcher over the filter. See
    /// [`TasksAdapter::watch_tasks`] for emission semantics.
    #[napi]
    pub async fn watch_memories(&self, filter: Option<MemoryFilter>) -> Result<MemoryWatchIter> {
        let w = memory_watcher_with_filter(&self.inner, filter)?;
        let stream: BoxStream<'static, Vec<InnerMemory>> = w.stream().boxed();
        Ok(MemoryWatchIter {
            inner: Arc::new(MemoryWatchIterInner {
                stream: TokioMutex::new(Some(stream)),
                shutdown: Notify::new(),
                is_shutdown: AtomicBool::new(false),
            }),
        })
    }

    /// Atomic "paint + react" primitive. Mirrors
    /// [`TasksAdapter::snapshot_and_watch_tasks`] for memories.
    #[napi]
    pub async fn snapshot_and_watch_memories(
        &self,
        filter: Option<MemoryFilter>,
    ) -> Result<MemoriesSnapshotAndWatch> {
        let w = memory_watcher_with_filter(&self.inner, filter)?;
        let (snapshot, stream) = self.inner.snapshot_and_watch(w);
        let stream: BoxStream<'static, Vec<InnerMemory>> = stream;
        Ok(MemoriesSnapshotAndWatch {
            snapshot: snapshot.into_iter().map(Memory::from).collect(),
            inner: Arc::new(MemoryWatchIterInner {
                stream: TokioMutex::new(Some(stream)),
                shutdown: Notify::new(),
                is_shutdown: AtomicBool::new(false),
            }),
        })
    }
}

fn memory_watcher_with_filter(
    adapter: &InnerMemoriesAdapter,
    filter: Option<MemoryFilter>,
) -> Result<::net::adapter::net::cortex::memories::MemoriesWatcher> {
    let mut w = adapter.watch();
    if let Some(f) = filter {
        if let Some(s) = f.source {
            w = w.where_source(s);
        }
        if let Some(s) = f.content_contains {
            w = w.content_contains(s);
        }
        if let Some(tag) = f.tag {
            w = w.where_tag(tag);
        }
        if let Some(tags) = f.any_tag {
            w = w.where_any_tag(tags);
        }
        if let Some(tags) = f.all_tags {
            w = w.where_all_tags(tags);
        }
        if let Some(pinned) = f.pinned {
            w = w.where_pinned(pinned);
        }
        if let Some(ns) = f.created_after_ns {
            w = w.created_after(bigint_u64(ns)?);
        }
        if let Some(ns) = f.created_before_ns {
            w = w.created_before(bigint_u64(ns)?);
        }
        if let Some(ns) = f.updated_after_ns {
            w = w.updated_after(bigint_u64(ns)?);
        }
        if let Some(ns) = f.updated_before_ns {
            w = w.updated_before(bigint_u64(ns)?);
        }
        if let Some(o) = f.order_by {
            w = w.order_by(o.into());
        }
        if let Some(l) = f.limit {
            w = w.limit(l as usize);
        }
    }
    Ok(w)
}

/// Result of [`MemoriesAdapter::snapshot_and_watch_memories`].
#[napi]
pub struct MemoriesSnapshotAndWatch {
    snapshot: Vec<Memory>,
    inner: Arc<MemoryWatchIterInner>,
}

#[napi]
impl MemoriesSnapshotAndWatch {
    /// Initial filter result captured atomically with the watcher.
    #[napi(getter)]
    pub fn snapshot(&self) -> Vec<Memory> {
        self.snapshot.clone()
    }

    /// Wait for the next delta. `null` when closed / ended.
    #[napi]
    pub async fn next(&self) -> Option<Vec<Memory>> {
        memory_watch_next(&self.inner).await
    }

    /// Terminate the iterator early. Idempotent.
    #[napi]
    pub fn close(&self) {
        memory_watch_close(&self.inner);
    }
}

// =========================================================================
// NetDB — unified query façade over tasks + memories
// =========================================================================

use ::net::adapter::net::netdb::NetDbSnapshot as InnerNetDbSnapshot;

/// Options for [`NetDb::open`] / [`NetDb::open_from_snapshot`].
#[napi(object)]
pub struct NetDbOpenConfig {
    /// Optional persistent base directory. When set, adapters opened
    /// with `persistent: true` write to `<dir>/<channel_path>/{idx,dat}`.
    pub persistent_dir: Option<String>,
    /// Producer origin hash stamped on every `EventMeta`.
    pub origin_hash: BigInt,
    /// Open enabled adapters with `persistent: true`. Requires
    /// `persistentDir`.
    pub persistent: Option<bool>,
    /// Include the tasks model.
    pub with_tasks: Option<bool>,
    /// Include the memories model.
    pub with_memories: Option<bool>,
}

/// Serialized NetDB snapshot bundle returned by [`NetDb::snapshot`]
/// and consumed by [`NetDb::open_from_snapshot`].
#[napi(object)]
pub struct NetDbBundle {
    /// Bincode-encoded [`InnerNetDbSnapshot`] — opaque to callers.
    pub state_bytes: Buffer,
}

/// Unified NetDB handle. Bundles `TasksAdapter` + `MemoriesAdapter`
/// under one object; access them via `.tasks` / `.memories` getters.
///
/// NetDB is the recommended entry point for callers that want a
/// database-like surface. For raw event / stream access, drop down
/// to the individual adapters.
#[napi]
pub struct NetDb {
    tasks: Option<TasksAdapter>,
    memories: Option<MemoriesAdapter>,
}

impl NetDb {
    fn build_redex(config: &NetDbOpenConfig) -> InnerRedex {
        match &config.persistent_dir {
            Some(dir) => InnerRedex::new().with_persistent_dir(dir),
            None => InnerRedex::new(),
        }
    }

    fn cfg(config: &NetDbOpenConfig) -> RedexFileConfig {
        if config.persistent.unwrap_or(false) {
            RedexFileConfig::default().with_persistent(true)
        } else {
            RedexFileConfig::default()
        }
    }
}

#[napi]
impl NetDb {
    /// Open a NetDB with the requested models. Each enabled model
    /// spawns its own CortEX fold task.
    #[napi(factory)]
    pub async fn open(config: NetDbOpenConfig) -> Result<Self> {
        let redex = Self::build_redex(&config);
        let cfg = Self::cfg(&config);
        let origin = bigint_u64(config.origin_hash)?;
        let tasks = if config.with_tasks.unwrap_or(false) {
            Some(TasksAdapter {
                inner: Arc::new(
                    InnerTasksAdapter::open_with_config(&redex, origin, cfg.clone())
                        .await
                        .map_err(|e| cortex_err("NetDb open tasks", e))?,
                ),
            })
        } else {
            None
        };
        let memories = if config.with_memories.unwrap_or(false) {
            Some(MemoriesAdapter {
                inner: Arc::new(
                    InnerMemoriesAdapter::open_with_config(&redex, origin, cfg)
                        .await
                        .map_err(|e| cortex_err("NetDb open memories", e))?,
                ),
            })
        } else {
            None
        };
        Ok(Self { tasks, memories })
    }

    /// Open a NetDB and restore each enabled model's state from the
    /// bundle. Models whose bundle entry is `None` are opened from
    /// scratch (equivalent to [`Self::open`] for that model).
    #[napi(factory)]
    pub async fn open_from_snapshot(config: NetDbOpenConfig, bundle: NetDbBundle) -> Result<Self> {
        let snapshot = InnerNetDbSnapshot::decode(bundle.state_bytes.as_ref())
            .map_err(|e| netdb_err("decode snapshot bundle", e))?;
        let redex = Self::build_redex(&config);
        let cfg = Self::cfg(&config);
        let origin = bigint_u64(config.origin_hash)?;

        let tasks = if config.with_tasks.unwrap_or(false) {
            let adapter = match snapshot.tasks {
                Some((bytes, last_seq)) => InnerTasksAdapter::open_from_snapshot_with_config(
                    &redex,
                    origin,
                    cfg.clone(),
                    &bytes,
                    last_seq,
                )
                .await
                .map_err(|e| cortex_err("NetDb restore tasks", e))?,
                None => InnerTasksAdapter::open_with_config(&redex, origin, cfg.clone())
                    .await
                    .map_err(|e| cortex_err("NetDb open tasks", e))?,
            };
            Some(TasksAdapter {
                inner: Arc::new(adapter),
            })
        } else {
            None
        };

        let memories = if config.with_memories.unwrap_or(false) {
            let adapter = match snapshot.memories {
                Some((bytes, last_seq)) => InnerMemoriesAdapter::open_from_snapshot_with_config(
                    &redex,
                    origin,
                    cfg.clone(),
                    &bytes,
                    last_seq,
                )
                .await
                .map_err(|e| cortex_err("NetDb restore memories", e))?,
                None => InnerMemoriesAdapter::open_with_config(&redex, origin, cfg)
                    .await
                    .map_err(|e| cortex_err("NetDb open memories", e))?,
            };
            Some(MemoriesAdapter {
                inner: Arc::new(adapter),
            })
        } else {
            None
        };

        Ok(Self { tasks, memories })
    }

    /// The tasks adapter (or `null` if tasks weren't enabled).
    #[napi(getter)]
    pub fn tasks(&self) -> Option<TasksAdapter> {
        self.tasks.clone()
    }

    /// The memories adapter (or `null` if memories weren't enabled).
    #[napi(getter)]
    pub fn memories(&self) -> Option<MemoriesAdapter> {
        self.memories.clone()
    }

    /// Snapshot every enabled model into one bundle. Persist the
    /// `stateBytes` blob; restore via [`Self::open_from_snapshot`].
    #[napi]
    pub fn snapshot(&self) -> Result<NetDbBundle> {
        let tasks = match &self.tasks {
            Some(t) => Some(
                t.inner
                    .snapshot()
                    .map_err(|e| cortex_err("snapshot tasks", e))?,
            ),
            None => None,
        };
        let memories = match &self.memories {
            Some(m) => Some(
                m.inner
                    .snapshot()
                    .map_err(|e| cortex_err("snapshot memories", e))?,
            ),
            None => None,
        };
        let snap = InnerNetDbSnapshot { tasks, memories };
        let bytes = snap.encode().map_err(|e| netdb_err("encode snapshot", e))?;
        Ok(NetDbBundle {
            state_bytes: Buffer::from(bytes),
        })
    }

    /// Close every enabled adapter. Idempotent.
    #[napi]
    pub fn close(&self) -> Result<()> {
        if let Some(t) = &self.tasks {
            t.inner.close().map_err(|e| cortex_err("close tasks", e))?;
        }
        if let Some(m) = &self.memories {
            m.inner
                .close()
                .map_err(|e| cortex_err("close memories", e))?;
        }
        Ok(())
    }
}
