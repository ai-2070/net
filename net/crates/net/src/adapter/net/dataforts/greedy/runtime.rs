//! Greedy-LRU runtime — the load-bearing async glue.
//!
//! One per-process runtime, installed by
//! `MeshNode::enable_greedy_dataforts(cfg)` (slice 5). Owns:
//!
//! - The [`GreedyCacheRegistry`] holding per-channel `RedexFile`s
//!   and the cluster-wide LRU index.
//! - The [`BandwidthBudget`] gating cache writes against a
//!   configured fraction of measured NIC peak.
//! - The [`GreedyMetricsRegistry`] surfacing `dataforts_greedy_*`
//!   counters.
//! - The local-node [`CapabilitySet`] snapshot, the
//!   [`IntentRegistry`], and the [`PlacementMetadataKeys`] —
//!   inputs to [`should_admit`].
//! - An [`Arc`] to a [`Redex`] for opening per-channel cache
//!   files, and an [`Arc<dyn ChainTagSink>`] for announce /
//!   withdraw.
//!
//! Public entry-point is [`GreedyRuntime::dispatch_event`] —
//! called by the mesh's inbound dispatch hook (slice 5) on every
//! channel event the local node observes. The runtime runs the
//! pure [`should_admit`] decision, then on Admit writes the
//! payload to the per-channel cache file (admitting the channel
//! lazily on first event), enforces the bandwidth budget, fires
//! metrics, and announces the `causal:` chain tag on first cache.
//! Cache writes are best-effort — failures log + drop rather than
//! propagating to the application's tail.

use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use parking_lot::Mutex;

use crate::adapter::net::behavior::capability::CapabilitySet;
use crate::adapter::net::behavior::placement::{IntentRegistry, PlacementMetadataKeys};
use crate::adapter::net::channel::ChannelName;
use crate::adapter::net::redex::{
    BandwidthBudget, ChainTagSink, Redex, RedexFile, RedexFileConfig,
};

use super::admission::{should_admit, AdmissionInputs, AdmissionVerdict};
use super::cache::GreedyCacheRegistry;
use super::config::GreedyConfig;
use super::metrics::{AdmitRejectReason, GreedyMetricsRegistry};
use crate::adapter::net::dataforts::blob::{
    classify_payload, should_pull_blob, EventPayload, PullBlobVerdict,
};

/// Trait the mesh dispatch loop uses to fan inbound events into
/// the greedy runtime. Stays sync — the mesh's `process_local_packet`
/// is itself a synchronous fn, so the trait method spawns whatever
/// async work it needs internally rather than forcing the mesh to
/// `.await`.
///
/// Fire-and-forget: the mesh never inspects the outcome (greedy is
/// best-effort, parallel to the application's tail).
///
/// **Ordering caveat.** The default
/// [`GreedyRuntime::observe_event`] impl spawns one tokio task per
/// inbound event so the mesh hot path stays non-blocking; the
/// per-channel cache file's append calls race concurrently, so
/// the cache may surface events out of publish order. Operators
/// needing strict ordering should use replication (`Redex::open_file`
/// with `RedexFileConfig::replication`) which preserves seq order
/// via the per-channel runtime + `apply_sync_response` monotonicity
/// guard. Greedy is positioned as a speculative observability /
/// data-locality layer, not an ordered-replay primitive.
pub trait GreedyObserver: Send + Sync {
    /// Observe one inbound channel event. The implementation is
    /// responsible for any async work + backpressure.
    ///
    /// `channel_hash` is the wire `u16` hash carried in the
    /// `NetHeader` — the mesh strips channel names on ingress, so
    /// the observer maps `channel_hash` to a cache-side
    /// [`ChannelName`] via [`synthesize_cache_channel_name`]. The
    /// data-plane greedy cache deliberately keys on the wire `u16`
    /// (not the canonical
    /// [`ChannelHash`](crate::adapter::net::channel::ChannelHash))
    /// because that's the identifier carried by the packet that
    /// triggered the observe call; ACL / storage / config decisions
    /// key on the canonical `u32` elsewhere in the stack and are not
    /// weakened by this data-plane choice.
    fn observe_event(
        &self,
        channel_hash: u16,
        origin_hash: u64,
        chain_caps: Arc<CapabilitySet>,
        payload: Bytes,
    );
}

/// Synthesize a stable cache-side [`ChannelName`] from the wire
/// `u16` channel hash carried by inbound packets. Wire-bucket
/// collisions are routine at scale and cause two real channels to
/// share a cache file — a small mix-up at the data-plane cache
/// layer; ACL and storage decisions key on the canonical
/// [`ChannelHash`](crate::adapter::net::channel::ChannelHash)
/// (`u32`) and are not affected.
///
/// Naming convention `dataforts/greedy/<hex>` reserves a
/// channel-namespace prefix that won't collide with application
/// channels (`/` separators + reserved-prefix discipline).
pub fn synthesize_cache_channel_name(channel_hash: u16) -> ChannelName {
    ChannelName::new(&format!("dataforts/greedy/{:04x}", channel_hash))
        .expect("hex-formatted name with reserved prefix is always valid")
}

// 1 Gbps placeholder for the measured NIC peak. The replication
// runtime uses the same placeholder until the plan §6 proximity-
// graph throughput probe lands; reuse the same number here so the
// `replication_budget_fraction` and `bandwidth_budget_fraction`
// configurations share a denominator. Operators with > 1 Gbps
// links should set [`GreedyConfig::nic_peak_bytes_per_s`]
// explicitly until the measured-NIC-peak probe ships.
//
// TODO(plan-§6): wire the measured-NIC-peak probe through here so
// the explicit override becomes opt-out rather than opt-in.

/// Outcome of a single [`GreedyRuntime::dispatch_event`] call.
/// Returned for testability and operator-trace inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchOutcome {
    /// Event admitted to the cache and successfully appended.
    Cached,
    /// Admission gate rejected the event. The runtime bumped the
    /// corresponding `dataforts_greedy_admit_rejected_total`
    /// counter for the reason.
    RejectedByAdmission(AdmitRejectReason),
    /// Admission passed but the bandwidth budget refused — try
    /// later. The runtime bumped the `capacity` reject counter.
    BandwidthExhausted,
    /// Append into the per-channel cache failed (typically the
    /// disk-tier rejected the write). Greedy is best-effort; the
    /// runtime logs + drops. The application's tail is unaffected
    /// (this is a parallel write).
    AppendFailed,
}

/// The greedy-LRU runtime handle. Cheap to clone (`Arc`-backed
/// internals); pass clones into the inbound dispatch hook.
#[derive(Clone)]
pub struct GreedyRuntime {
    inner: Arc<GreedyRuntimeInner>,
}

struct GreedyRuntimeInner {
    config: GreedyConfig,
    redex: Arc<Redex>,
    sink: Arc<dyn ChainTagSink>,
    cache: Mutex<GreedyCacheRegistry>,
    budget: Mutex<BandwidthBudget>,
    metrics: Arc<GreedyMetricsRegistry>,
    intent_registry: IntentRegistry,
    metadata_keys: PlacementMetadataKeys,
    /// Local node's advertised capability set. Snapshotted at
    /// install time; refreshable via [`GreedyRuntime::set_local_caps`]
    /// when the node's caps change.
    local_caps: Mutex<Arc<CapabilitySet>>,
    /// Optional data-gravity state (Phase 4). Interior-mutable
    /// so operators can flip gravity on / off after the greedy
    /// runtime is already shared via Arc clones (the Arc count
    /// stays > 1 after `Redex::enable_greedy_dataforts`, so
    /// `try_unwrap` for a build-then-replace pattern would
    /// always fail). RwLock chosen over Mutex because reads
    /// (note_read, gravity_tick) dominate writes (enable /
    /// disable) once gravity is installed.
    #[cfg(feature = "dataforts")]
    gravity: parking_lot::RwLock<Option<GravityState>>,
    /// Optional [`BlobRefcountTable`] handle used as a chain-fold
    /// refcount source: every event admitted into the greedy
    /// cache whose payload decodes to a `BlobRef` bumps the
    /// referenced hash's refcount; the corresponding decrement
    /// fires when the cache evicts the channel under LRU
    /// pressure. Without this wiring the refcount table sees
    /// only the passive `store_observed` stamps from
    /// `MeshBlobAdapter::store_chunk`, so GC's `deletable_hashes`
    /// path can't distinguish "still referenced by a cached
    /// chain" from "orphaned chunk past retention". Operator
    /// installs via [`GreedyRuntime::set_blob_refcount_table`].
    #[cfg(feature = "dataforts")]
    blob_refcount: parking_lot::RwLock<Option<super::super::blob::BlobRefcountTable>>,
    /// Optional [`BlobAdapter`](super::super::blob::BlobAdapter)
    /// handle used by the G-1 admit path to kick off a best-effort
    /// `prefetch` of the referenced blob (PR-5i). When wired,
    /// every admit verdict spawns one tokio task that calls
    /// `adapter.prefetch(blob_ref)` so the chunk channels open
    /// against the local Redex handle and the replication runtime
    /// begins pulling from peers carrying the `causal:<hex>` tag.
    /// Operator installs via
    /// [`GreedyRuntime::set_blob_adapter`].
    #[cfg(feature = "dataforts")]
    blob_adapter: parking_lot::RwLock<Option<Arc<dyn super::super::blob::BlobAdapter>>>,
    /// Per-channel set of `BlobRef` hashes the runtime has
    /// admitted into the cache. Drives the matching decrement on
    /// channel eviction so the refcount source is balanced.
    /// `Mutex` because writes happen on the (hot) admit path and
    /// on the (rarer) eviction sweep — RwLock's reader path
    /// doesn't help here since every dispatch is a write.
    #[cfg(feature = "dataforts")]
    chain_blob_refs:
        Mutex<std::collections::HashMap<ChannelName, std::collections::HashSet<[u8; 32]>>>,
    /// Bounds the number of in-flight `observe_event` spawn tasks.
    /// `observe_event` is the mesh hot-path entry; without a bound
    /// a flooding peer creates one outstanding task per event
    /// before the per-event admission lock serializes them, and
    /// the per-task `Bytes` + `Arc<CapabilitySet>` clones pile up.
    /// `try_acquire_owned`-shaped: on saturation drop the event
    /// and bump a counter rather than blocking the mesh.
    observer_inflight: Arc<tokio::sync::Semaphore>,
}

/// Per-runtime data-gravity state. Behind the same gate as the
/// public `with_gravity` builder + `gravity_tick` method.
#[cfg(feature = "dataforts")]
struct GravityState {
    /// Heat-tag emission policy. Immutable after install;
    /// reconfigure by re-enabling greedy.
    policy: super::super::gravity::DataGravityPolicy,
    /// Per-chain heat registry. Bumped on `note_read`; ticked
    /// by `gravity_tick`.
    heat: Mutex<super::super::gravity::HeatRegistry>,
    /// Wire-side sink for `announce_heat` / `withdraw_heat`.
    /// In production this is `Arc<MeshNode>`.
    sink: Arc<dyn super::super::gravity::HeatSink>,
}

impl std::fmt::Debug for GreedyRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let cache = self.inner.cache.lock();
        f.debug_struct("GreedyRuntime")
            .field("cached_channels", &cache.len())
            .field("cache_bytes", &cache.total_bytes())
            .field("metrics_channels", &self.inner.metrics.len())
            .finish_non_exhaustive()
    }
}

impl GreedyRuntime {
    /// Construct a runtime. Caller has already validated the
    /// config and built the inputs:
    ///
    /// - `redex` — the local `Redex` to open per-channel cache
    ///   files against. Same handle the application uses;
    ///   greedy's cache files coexist with application channels.
    /// - `sink` — chain-tag announce / withdraw. In production
    ///   wiring this is `Arc<MeshNode>`.
    /// - `local_caps` — the node's advertised capability set;
    ///   used by the intent / colocation admission gates.
    /// - `intent_registry` — typically `IntentRegistry::defaults()`
    ///   augmented with application-registered intents.
    pub fn new(
        config: GreedyConfig,
        redex: Arc<Redex>,
        sink: Arc<dyn ChainTagSink>,
        local_caps: Arc<CapabilitySet>,
        intent_registry: IntentRegistry,
    ) -> Self {
        let now = Instant::now();
        let nic_peak = config.effective_nic_peak_bytes_per_s();
        let budget = BandwidthBudget::new(config.bandwidth_budget_fraction, nic_peak, now);
        let cache = GreedyCacheRegistry::new(config.total_cap_bytes);
        let observer_inflight = Arc::new(tokio::sync::Semaphore::new(config.observer_inflight_cap));
        Self {
            inner: Arc::new(GreedyRuntimeInner {
                config,
                redex,
                sink,
                cache: Mutex::new(cache),
                budget: Mutex::new(budget),
                metrics: Arc::new(GreedyMetricsRegistry::new()),
                intent_registry,
                metadata_keys: PlacementMetadataKeys::default(),
                local_caps: Mutex::new(local_caps),
                #[cfg(feature = "dataforts")]
                gravity: parking_lot::RwLock::new(None),
                #[cfg(feature = "dataforts")]
                blob_refcount: parking_lot::RwLock::new(None),
                #[cfg(feature = "dataforts")]
                blob_adapter: parking_lot::RwLock::new(None),
                #[cfg(feature = "dataforts")]
                chain_blob_refs: Mutex::new(std::collections::HashMap::new()),
                observer_inflight,
            }),
        }
    }

    /// Enable data-gravity heat-counter emission on this runtime.
    /// Callable at any time — flips the gravity slot regardless
    /// of how many Arc clones exist on the runtime. Operators
    /// pair this with a periodic [`Self::gravity_tick`] task.
    ///
    /// Idempotent — replacing an already-installed gravity state
    /// (e.g. to reconfigure the policy) is permitted; the heat
    /// registry resets on each call so the new policy starts
    /// from a clean slate.
    #[cfg(feature = "dataforts")]
    pub fn set_gravity(
        &self,
        policy: super::super::gravity::DataGravityPolicy,
        heat_sink: Arc<dyn super::super::gravity::HeatSink>,
    ) {
        *self.inner.gravity.write() = Some(GravityState {
            policy,
            heat: Mutex::new(super::super::gravity::HeatRegistry::new()),
            sink: heat_sink,
        });
    }

    /// Disable data-gravity. The heat registry drops; subsequent
    /// `note_read` calls won't touch heat; `gravity_tick` becomes
    /// a no-op. Idempotent.
    #[cfg(feature = "dataforts")]
    pub fn clear_gravity(&self) {
        *self.inner.gravity.write() = None;
    }

    /// True iff this runtime has data gravity installed.
    #[cfg(feature = "dataforts")]
    pub fn gravity_enabled(&self) -> bool {
        self.inner.gravity.read().is_some()
    }

    /// Wire a [`BlobRefcountTable`](super::super::blob::BlobRefcountTable)
    /// so this runtime acts as a chain-fold refcount source: every
    /// blob-ref-shaped event admitted into the cache bumps the
    /// referenced hash's refcount, and the matching decrement
    /// fires when the channel is evicted from the cache. Operators
    /// typically pass `mesh_blob_adapter.refcount_table().clone()`
    /// so the same table feeds the adapter's GC sweep.
    ///
    /// Idempotent — replacing an already-installed handle is
    /// allowed; the per-channel `chain_blob_refs` shadow set is
    /// preserved so in-flight admits/evictions stay balanced.
    #[cfg(feature = "dataforts")]
    pub fn set_blob_refcount_table(&self, table: super::super::blob::BlobRefcountTable) {
        *self.inner.blob_refcount.write() = Some(table);
    }

    /// Disable the chain-fold refcount source. The shadow set is
    /// drained so a future re-install starts from a clean slate.
    /// Idempotent.
    #[cfg(feature = "dataforts")]
    pub fn clear_blob_refcount_table(&self) {
        *self.inner.blob_refcount.write() = None;
        self.inner.chain_blob_refs.lock().clear();
    }

    /// True iff this runtime is wired as a blob refcount source.
    #[cfg(feature = "dataforts")]
    pub fn blob_refcount_enabled(&self) -> bool {
        self.inner.blob_refcount.read().is_some()
    }

    /// Wire a [`BlobAdapter`](super::super::blob::BlobAdapter) so
    /// the G-1 admit verdict actually kicks off a best-effort
    /// prefetch — the runtime spawns one tokio task per admit
    /// calling `adapter.prefetch(blob_ref)`. Without this wiring
    /// G-1 stays decision-only (PR-5c semantics): the verdict
    /// bumps the admitted counter but no fetch happens.
    ///
    /// Idempotent — replacing an already-installed adapter is
    /// permitted; in-flight prefetch tasks finish against the
    /// previous handle.
    #[cfg(feature = "dataforts")]
    pub fn set_blob_adapter(&self, adapter: Arc<dyn super::super::blob::BlobAdapter>) {
        *self.inner.blob_adapter.write() = Some(adapter);
    }

    /// Disable the prefetch path. Subsequent admits stay
    /// decision-only. Idempotent.
    #[cfg(feature = "dataforts")]
    pub fn clear_blob_adapter(&self) {
        *self.inner.blob_adapter.write() = None;
    }

    /// True iff this runtime is wired to act on G-1 admits.
    #[cfg(feature = "dataforts")]
    pub fn blob_adapter_enabled(&self) -> bool {
        self.inner.blob_adapter.read().is_some()
    }

    /// Borrow the metrics registry. Cheap clone of the inner Arc.
    pub fn metrics(&self) -> Arc<GreedyMetricsRegistry> {
        self.inner.metrics.clone()
    }

    /// Replace the local capability snapshot. Use when the node's
    /// advertised caps change so subsequent admission decisions
    /// see the new shape.
    pub fn set_local_caps(&self, caps: Arc<CapabilitySet>) {
        *self.inner.local_caps.lock() = caps;
    }

    /// Number of channels currently in the greedy cache.
    pub fn cached_channel_count(&self) -> usize {
        self.inner.cache.lock().len()
    }

    /// Total bytes resident across every cached channel. Upper
    /// bound on disk usage — see [`GreedyCacheRegistry`].
    pub fn cached_bytes(&self) -> u64 {
        self.inner.cache.lock().total_bytes()
    }

    /// Resync the cache's per-entry byte counts against the
    /// substrate's authoritative `RedexFile::retained_bytes` view.
    /// Operators call this periodically (e.g. from a heartbeat-
    /// aligned task) so the registry's monotonic counter doesn't
    /// drift arbitrarily above what's actually on disk under hot,
    /// retention-trimmed channels — without resync, cluster-cap
    /// admission can false-reject indefinitely. O(n) over cached
    /// channels; not for the hot path.
    pub fn resync_cache_bytes(&self) {
        self.inner.cache.lock().resync_bytes_from_files();
    }

    /// True iff the local cache currently holds `channel`.
    pub fn contains(&self, channel: &ChannelName) -> bool {
        self.inner.cache.lock().contains(channel)
    }

    /// Borrow the per-channel cache file if greedy is holding
    /// `channel`. Returns a `RedexFile` clone (Arc-backed; cheap).
    /// Does NOT bump the read-recency LRU position — pair with
    /// [`Self::note_read`] when serving from the file so the
    /// promote-on-read semantic fires.
    ///
    /// Operator-facing read-path integration: a caller wanting
    /// "give me chain X's local cache" passes the synthesized
    /// channel name (or uses [`Redex::greedy_cache_for`] which
    /// does the synthesis + read-recency bump in one call).
    pub fn cache_file(&self, channel: &ChannelName) -> Option<RedexFile> {
        self.inner.cache.lock().get(channel).map(|e| e.file.clone())
    }

    /// Bump the read-path LRU position for `channel`. Wire into
    /// the substrate's read path so reads served from the cache
    /// promote the channel against eviction.
    ///
    /// When data gravity is enabled, this also bumps the per-
    /// chain heat counter. Heat tag emission happens on the
    /// throttled [`Self::gravity_tick`] cycle (the bump itself
    /// is free; emission is rate-limited per
    /// [`super::super::gravity::DataGravityPolicy::emit_threshold_ratio`]).
    pub fn note_read(&self, channel: &ChannelName) {
        let now = Instant::now();
        let origin_hash = {
            let mut cache = self.inner.cache.lock();
            cache.touch(channel, now);
            cache.get(channel).map(|e| e.origin_hash)
        };
        let m = self.inner.metrics.for_channel(channel.as_str());
        m.incr_serve();

        #[cfg(feature = "dataforts")]
        {
            let gravity = self.inner.gravity.read();
            if let Some(gravity) = gravity.as_ref() {
                if let Some(origin_hash) = origin_hash {
                    // Skip heat tracking when origin_hash == 0
                    // (publisher hasn't stamped identity). All
                    // chains on a node with default publishers
                    // would otherwise collapse into one bucket,
                    // collapsing per-chain heat into an aggregate
                    // "global temperature." The skip is counted
                    // in dataforts_greedy_gravity_heat_unattributed_total
                    // so operators see the signal and configure
                    // their publishers to stamp origins.
                    if origin_hash == 0 {
                        self.inner
                            .metrics
                            .cluster()
                            .incr_gravity_heat_unattributed();
                    } else {
                        let mut heat = gravity.heat.lock();
                        heat.entry_mut(origin_hash, gravity.policy.decay_half_life, now)
                            .bump(now);
                    }
                }
            }
        }
        #[cfg(not(feature = "dataforts"))]
        let _ = origin_hash;
    }

    /// Apply decay through `now` and emit heat tags for chains
    /// whose rate has crossed the configured threshold (per
    /// [`super::super::gravity::should_emit_heat`]). Withdrawals
    /// for chains that decayed to zero fire on the same call.
    ///
    /// Async because the heat sink's `announce_heat` /
    /// `withdraw_heat` calls hit the mesh transport. Each
    /// per-chain emission is fire-and-forget — a failed sink
    /// call logs + drops; the next tick retries.
    ///
    /// Operators schedule this via a periodic tokio interval
    /// (typically `heartbeat_ms`-aligned) so heat propagation
    /// piggybacks on the existing capability-announcement
    /// cadence.
    #[cfg(feature = "dataforts")]
    pub async fn gravity_tick(&self) {
        // Snapshot the sink + emissions list under the read lock,
        // then drop the lock before awaiting the sink. Holding
        // the gravity lock across an .await would block any
        // concurrent set_gravity / clear_gravity for the duration
        // of the wire emission.
        let (sink, emissions) = {
            let gravity = self.inner.gravity.read();
            let Some(gravity) = gravity.as_ref() else {
                return;
            };
            let now = Instant::now();
            let emissions = gravity.heat.lock().tick(&gravity.policy, now);
            (gravity.sink.clone(), emissions)
        };
        // Coalesce the per-chain emissions into one
        // (origin_hash, Option<rate>) vector and submit through
        // the sink's batch path. The sink's default impl falls
        // back to per-chain calls; the MeshNode impl rewrites the
        // full capability set once and rebroadcasts once.
        //
        // Snapshot the policy reference so the normalization
        // formula uses the operator-configured scale rather than a
        // hard-coded saturating curve.
        let policy = {
            let gravity = self.inner.gravity.read();
            gravity.as_ref().map(|g| g.policy.clone())
        };
        let mut batch: Vec<(u64, Option<f64>)> = Vec::new();
        for (origin_hash, emission) in emissions {
            match emission {
                super::super::gravity::HeatEmission::Suppress => {}
                super::super::gravity::HeatEmission::Emit { rate } => {
                    // Log-scale normalize unbounded rate to
                    // [0.0, 1.0] using the policy's reference rate.
                    // The previous `rate / (rate + 1)` form
                    // saturated at the top end (every "warm"
                    // chain looked identical to "blazing"); the
                    // log form stretches the wire range across
                    // useful operating rates.
                    let normalized = policy
                        .as_ref()
                        .map(|p| p.normalize_rate_for_wire(rate))
                        .unwrap_or_else(|| (rate / (rate + 1.0)).min(1.0));
                    batch.push((origin_hash, Some(normalized)));
                }
                super::super::gravity::HeatEmission::Withdraw => {
                    batch.push((origin_hash, None));
                }
            }
        }
        if !batch.is_empty() {
            if let Err(e) = sink.announce_heat_batch(&batch).await {
                tracing::trace!(
                    error = ?e,
                    batch_len = batch.len(),
                    "gravity: announce_heat_batch failed"
                );
            }
        }
    }

    /// Dispatch an inbound channel event through the greedy
    /// admission + cache-write path.
    ///
    /// `chain_caps` is the capability set the chain advertises —
    /// typically the publisher's announcement carried alongside
    /// the channel publish. `origin_hash` identifies the chain
    /// for the `causal:` announcement on first cache.
    ///
    /// Returns the [`DispatchOutcome`] for the call. Best-effort
    /// — never panics, never propagates errors to the caller.
    pub async fn dispatch_event(
        &self,
        channel: &ChannelName,
        origin_hash: u64,
        chain_caps: &CapabilitySet,
        payload: &[u8],
    ) -> DispatchOutcome {
        let now = Instant::now();
        let local_caps = self.inner.local_caps.lock().clone();

        // Resolve the colocation hint against the local cache so
        // SoftPreference + StrictRequired both have something to
        // gate against. The hint values are 16-char hex origin
        // hashes (`MeshNode::chain_hex`). `None` means "no hint
        // applies" — the admission code treats that as "target
        // not held" for fail-closed StrictRequired.
        let colocation_target_held = {
            let meta = &chain_caps.metadata;
            let hex = meta
                .get(&self.inner.metadata_keys.colocate_with_strict)
                .or_else(|| meta.get(&self.inner.metadata_keys.colocate_with));
            hex.and_then(|h| u64::from_str_radix(h, 16).ok())
                .map(|target| self.inner.cache.lock().contains_origin(target))
        };

        // 1. Admission decision (pure).
        let verdict = should_admit(&AdmissionInputs {
            chain_caps,
            local_caps: &local_caps,
            config: &self.inner.config,
            intent_registry: &self.inner.intent_registry,
            metadata_keys: &self.inner.metadata_keys,
            colocation_target_held,
        });
        match verdict {
            AdmissionVerdict::Admit => {}
            AdmissionVerdict::RejectScope => {
                self.inner
                    .metrics
                    .cluster()
                    .incr_admit_rejected(AdmitRejectReason::Scope);
                return DispatchOutcome::RejectedByAdmission(AdmitRejectReason::Scope);
            }
            AdmissionVerdict::RejectIntent => {
                self.inner
                    .metrics
                    .cluster()
                    .incr_admit_rejected(AdmitRejectReason::Intent);
                return DispatchOutcome::RejectedByAdmission(AdmitRejectReason::Intent);
            }
            AdmissionVerdict::RejectColocation => {
                self.inner
                    .metrics
                    .cluster()
                    .incr_admit_rejected(AdmitRejectReason::Colocation);
                return DispatchOutcome::RejectedByAdmission(AdmitRejectReason::Colocation);
            }
        }

        // 2. Bandwidth budget. Bumps a distinct `bandwidth` axis
        // on the admit_rejected counter so operators on
        // faster-than-gigabit NICs can tell bandwidth throttling
        // apart from real cluster-cap exhaustion (the bandwidth
        // budget is computed against `nic_peak_bytes_per_s`, which
        // defaults to 1 Gbps — see `GreedyConfig`).
        let payload_bytes = payload.len() as u64;
        let admitted_by_budget = {
            let mut budget = self.inner.budget.lock();
            budget.try_consume(payload_bytes, now)
        };
        if !admitted_by_budget {
            self.inner
                .metrics
                .cluster()
                .incr_admit_rejected(AdmitRejectReason::Bandwidth);
            return DispatchOutcome::BandwidthExhausted;
        }

        // 3. Lazy admission — open a per-channel cache file if we
        //    don't have one yet, then append. Steady-state (already-
        //    cached channel) costs ONE cache.lock() acquisition; the
        //    new-channel path costs two (the second one re-checks
        //    under-lock after the file open, since opening is I/O
        //    and can't be held under the cache lock). Two concurrent
        //    dispatch_event calls for the same new channel both pass
        //    the outer get() = None branch, both open a file, and
        //    only one upsert lands — the loser's file is harmless to
        //    drop (RedexFile is Arc-internal, reopen is idempotent).
        let (file, is_new_channel) = {
            let cache = self.inner.cache.lock();
            if let Some(entry) = cache.get(channel) {
                let file = entry.file.clone();
                drop(cache);
                (file, false)
            } else {
                drop(cache);
                let cfg = RedexFileConfig::default()
                    .with_retention_max_bytes(self.inner.config.per_channel_cap_bytes);
                let opened = match self.inner.redex.open_file(channel, cfg) {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::trace!(
                            channel = channel.as_str(),
                            error = ?e,
                            "greedy: failed to open cache file for new channel"
                        );
                        return DispatchOutcome::AppendFailed;
                    }
                };
                let mut cache = self.inner.cache.lock();
                if let Some(entry) = cache.get(channel) {
                    // Lost the race; use the winner's handle.
                    (entry.file.clone(), false)
                } else {
                    cache.upsert(channel.clone(), opened.clone(), now);
                    cache.set_origin_hash(channel, origin_hash);
                    (opened, true)
                }
            }
        };

        if let Err(e) = file.append(payload) {
            tracing::trace!(
                channel = channel.as_str(),
                error = ?e,
                "greedy: cache append failed; greedy is best-effort"
            );
            return DispatchOutcome::AppendFailed;
        }

        // 4. Byte accounting + cluster-cap eviction. One lock
        // acquisition covers both note_appended and the per-channel
        // bytes_resident gauge read; the metrics set runs after
        // dropping the lock so the contention window stays tight.
        let (sweep, resident_bytes) = {
            let mut cache = self.inner.cache.lock();
            let sweep = cache.note_appended(channel, payload_bytes, now);
            let resident = cache.get(channel).map(|e| e.bytes).unwrap_or(0);
            (sweep, resident)
        };
        self.inner
            .metrics
            .for_channel(channel.as_str())
            .set_bytes_resident(resident_bytes);

        // 4b. G-1 blob-pull verdict + chain-fold refcount source.
        // The chain event was admitted + cached, so this is the
        // moment to consider whether the local node should
        // *additionally* pull any blob the event payload
        // references. Decision-only in this PR — counters surface
        // the verdict; the actual fetch path lands when remote
        // blob fetch wires up. See
        // `dataforts/blob/admission.rs::should_pull_blob` for the
        // decision rule + the plan's § G-1 for the full contract.
        //
        // When a refcount table is wired (PR-5h), the BlobRef hash
        // is also folded into the table as a live reference. The
        // matching decrement fires below in step 6 when the
        // channel is evicted from the cache.
        if let Ok(EventPayload::Blob(blob_ref)) = classify_payload(payload) {
            match should_pull_blob(&local_caps, chain_caps) {
                PullBlobVerdict::Admit => {
                    self.inner.metrics.cluster().incr_blob_pull_admitted();
                    // PR-5i: act on the admit when an adapter is
                    // wired. Spawn so the hot dispatch path stays
                    // non-blocking — actual chunk arrival is
                    // asynchronous via the per-chunk replication
                    // runtime spawned inside `prefetch`.
                    self.spawn_blob_prefetch(blob_ref.clone());
                }
                PullBlobVerdict::Reject(reason) => {
                    self.inner.metrics.cluster().incr_blob_pull_rejected(reason);
                }
            }
            self.record_blob_ref(channel, &blob_ref);
        }

        let sink = self.inner.sink.clone();

        // 5. First-cache chain announcement.
        if is_new_channel {
            // Best-effort — log on failure but don't propagate.
            // Heartbeat / re-announcement upstream will retry.
            if let Err(e) = sink.announce_chain(origin_hash, 0).await {
                tracing::trace!(
                    channel = channel.as_str(),
                    error = ?e,
                    "greedy: chain announce failed"
                );
            }
        }

        // 6. Withdrawal announcements for evicted channels. The
        // cache surfaces (channel, origin_hash) pairs in the sweep
        // so we can withdraw without a follow-up lookup. Skip the
        // withdraw when origin_hash == 0 — that means no event
        // landed before eviction, so nothing was announced.
        if !sweep.is_empty() {
            // Drop any gravity heat counters that the evicted
            // chains owned. Without this the HeatRegistry grows
            // unboundedly across LRU churn (when gravity is wired
            // but greedy isn't doing the bounding; D-10 + N-2 fix).
            //
            // LOCK-ORDERING INVARIANT: the heat lock + gravity
            // read guard are taken in a tightly scoped block that
            // ends BEFORE any `.await` on the chain sink. The
            // sink call below (`withdraw_chain`) may re-enter the
            // runtime via the capability-index path; holding the
            // heat lock across that would create a lock-ordering
            // hazard the next time the order is exercised. The
            // explicit inner-block `{}` enforces release; the
            // explicit `drop(heat)` + `drop(gravity)` make the
            // release visible to readers of this code so a future
            // refactor can't quietly flatten the two loops.
            #[cfg(feature = "dataforts")]
            {
                let gravity = self.inner.gravity.read();
                if let Some(gravity) = gravity.as_ref() {
                    let mut heat = gravity.heat.lock();
                    for evicted in &sweep.evicted {
                        if evicted.origin_hash != 0 {
                            heat.remove(&evicted.origin_hash);
                        }
                    }
                    drop(heat);
                }
                drop(gravity);
            }

            // Decrement refcounts for every BlobRef hash the
            // evicted channels had recorded. Balances the
            // increment at step 4b so the refcount table tracks
            // the "live references" semantics the GC sweep relies
            // on. Drained under the inner lock so a concurrent
            // dispatch on the same channel (post-eviction) starts
            // from a clean slate.
            #[cfg(feature = "dataforts")]
            self.release_blob_refs_for_evicted(&sweep.evicted);

            for evicted in &sweep.evicted {
                self.inner
                    .metrics
                    .for_channel(evicted.channel.as_str())
                    .incr_eviction();
                self.inner
                    .metrics
                    .for_channel(evicted.channel.as_str())
                    .set_bytes_resident(0);
                if evicted.origin_hash != 0 {
                    if let Err(e) = sink.withdraw_chain(evicted.origin_hash).await {
                        tracing::trace!(
                            channel = evicted.channel.as_str(),
                            origin_hash = evicted.origin_hash,
                            error = ?e,
                            "greedy: chain withdraw failed"
                        );
                    }
                }
            }
        }

        DispatchOutcome::Cached
    }

    /// Record one BlobRef-shaped event payload against the
    /// per-channel shadow set + bump the refcount on the
    /// underlying hash(es). No-op when no refcount table is
    /// wired or when the channel has already recorded this
    /// exact hash (set semantics — duplicate observations of the
    /// same blob from the same channel don't double-count).
    /// For `BlobRef::Manifest`, every constituent chunk hash is
    /// recorded — the manifest body itself doesn't have its own
    /// hash on the wire, so the per-chunk projection is the only
    /// surface the refcount table can hold.
    #[cfg(feature = "dataforts")]
    fn record_blob_ref(&self, channel: &ChannelName, blob_ref: &super::super::blob::BlobRef) {
        let table = match self.inner.blob_refcount.read().clone() {
            Some(t) => t,
            None => return,
        };
        let hashes: Vec<[u8; 32]> = match blob_ref {
            super::super::blob::BlobRef::Small { hash, .. } => vec![*hash],
            super::super::blob::BlobRef::Manifest { chunks, .. } => {
                chunks.iter().map(|c| c.hash).collect()
            }
        };
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let mut seen = self.inner.chain_blob_refs.lock();
        let bucket = seen.entry(channel.clone()).or_default();
        for hash in hashes {
            // Deduplicate per-channel: a chain re-publishing the
            // same BlobRef across multiple events shouldn't
            // accumulate refcount. The matching decrement at
            // eviction also fires once per (channel, hash) pair.
            if bucket.insert(hash) {
                table.incr(hash, now_ms);
            }
        }
    }

    /// Spawn a best-effort prefetch task against the wired
    /// [`BlobAdapter`](super::super::blob::BlobAdapter). No-op
    /// when no adapter is wired (G-1 stays decision-only — the
    /// PR-5c behavior). The task counts success / error into the
    /// cluster metric registry; the dispatch hot path never
    /// waits on completion.
    #[cfg(feature = "dataforts")]
    fn spawn_blob_prefetch(&self, blob_ref: super::super::blob::BlobRef) {
        let adapter = match self.inner.blob_adapter.read().clone() {
            Some(a) => a,
            None => return,
        };
        let metrics = self.inner.metrics.clone();
        tokio::spawn(async move {
            match adapter.prefetch(&blob_ref).await {
                Ok(()) => metrics.cluster().incr_blob_prefetch_ok(),
                Err(e) => {
                    tracing::trace!(
                        error = ?e,
                        "greedy: blob prefetch failed; G-1 admit recorded, fetch best-effort"
                    );
                    metrics.cluster().incr_blob_prefetch_err();
                }
            }
        });
    }

    /// On channel eviction, drain the shadow set and decrement
    /// each recorded BlobRef hash. Pairs with
    /// [`Self::record_blob_ref`] to keep the refcount table
    /// balanced — every admit that incremented is followed by
    /// exactly one decrement when the holding channel is evicted.
    #[cfg(feature = "dataforts")]
    fn release_blob_refs_for_evicted(&self, evicted: &[super::cache::EvictedEntry]) {
        let table = match self.inner.blob_refcount.read().clone() {
            Some(t) => t,
            None => {
                // Drain without decrementing if the table is
                // disabled, so a future re-install doesn't see
                // stale shadow entries.
                let mut seen = self.inner.chain_blob_refs.lock();
                for e in evicted {
                    seen.remove(&e.channel);
                }
                return;
            }
        };
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let mut seen = self.inner.chain_blob_refs.lock();
        for e in evicted {
            if let Some(hashes) = seen.remove(&e.channel) {
                for hash in hashes {
                    table.decr(hash, now_ms);
                }
            }
        }
    }
}

impl GreedyObserver for GreedyRuntime {
    fn observe_event(
        &self,
        channel_hash: u16,
        origin_hash: u64,
        chain_caps: Arc<CapabilitySet>,
        payload: Bytes,
    ) {
        // Bound the spawn fan-out: a flooding peer can otherwise
        // create one outstanding tokio task per event before the
        // per-event admission lock serializes them. The per-task
        // payload + cap clones pile up. On saturation drop the
        // event and bump the metric — the mesh hot path stays
        // non-blocking either way.
        let permit = match self.inner.observer_inflight.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                self.inner
                    .metrics
                    .cluster()
                    .incr_observer_dropped_overloaded();
                return;
            }
        };
        let runtime = self.clone();
        let channel = synthesize_cache_channel_name(channel_hash);
        tokio::spawn(async move {
            let _ = runtime
                .dispatch_event(&channel, origin_hash, &chain_caps, &payload)
                .await;
            drop(permit);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::tag::Tag;
    use crate::error::AdapterError;

    /// Recorder sink — captures every announce / withdraw call so
    /// tests can assert on the observed sequence. Mirror of
    /// `RecorderSink` under replication_coordinator.
    #[derive(Default)]
    struct RecorderSink {
        announces: Mutex<Vec<(u64, u64)>>,
        withdraws: Mutex<Vec<u64>>,
    }

    #[async_trait::async_trait]
    impl ChainTagSink for RecorderSink {
        async fn announce_chain(&self, origin_hash: u64, tip_seq: u64) -> Result<(), AdapterError> {
            self.announces.lock().push((origin_hash, tip_seq));
            Ok(())
        }
        async fn withdraw_chain(&self, origin_hash: u64) -> Result<(), AdapterError> {
            self.withdraws.lock().push(origin_hash);
            Ok(())
        }
    }

    /// Parallel recorder for the data-gravity heat sink. Lets a
    /// test pin the heat announce/withdraw sequence without
    /// going through a real MeshNode.
    #[derive(Default)]
    struct RecorderHeatSink {
        announces: Mutex<Vec<(u64, f64)>>,
        withdraws: Mutex<Vec<u64>>,
    }

    #[async_trait::async_trait]
    impl crate::adapter::net::dataforts::gravity::HeatSink for RecorderHeatSink {
        async fn announce_heat(&self, origin_hash: u64, rate: f64) -> Result<(), AdapterError> {
            self.announces.lock().push((origin_hash, rate));
            Ok(())
        }
        async fn withdraw_heat(&self, origin_hash: u64) -> Result<(), AdapterError> {
            self.withdraws.lock().push(origin_hash);
            Ok(())
        }
    }

    fn cn(s: &str) -> ChannelName {
        ChannelName::new(s).unwrap()
    }

    fn build_runtime(cfg: GreedyConfig) -> (GreedyRuntime, Arc<RecorderSink>) {
        let redex = Arc::new(Redex::new());
        let sink = Arc::new(RecorderSink::default());
        let local_caps = Arc::new(CapabilitySet::default());
        let intent_registry = IntentRegistry::new();
        let rt = GreedyRuntime::new(
            cfg,
            redex,
            sink.clone() as Arc<dyn ChainTagSink>,
            local_caps,
            intent_registry,
        );
        (rt, sink)
    }

    fn chain_caps_with_scope(scope: &str) -> CapabilitySet {
        let mut caps = CapabilitySet::default();
        caps.tags.insert(Tag::Reserved {
            prefix: "scope:".to_string(),
            body: scope.to_string(),
        });
        caps
    }

    #[tokio::test]
    async fn admitted_event_caches_and_announces_chain() {
        let cfg =
            GreedyConfig::default().with_intent_match(super::super::IntentMatchPolicy::Disabled);
        let (rt, sink) = build_runtime(cfg);
        let chain = chain_caps_with_scope("any");
        let outcome = rt
            .dispatch_event(&cn("test/cached"), 0xDEAD_BEEF, &chain, b"hello")
            .await;
        assert_eq!(outcome, DispatchOutcome::Cached);
        assert!(rt.contains(&cn("test/cached")));
        assert_eq!(rt.cached_bytes(), 5);
        let announces = sink.announces.lock().clone();
        assert_eq!(announces, vec![(0xDEAD_BEEF, 0)]);
    }

    #[tokio::test]
    async fn rejected_by_scope_does_not_cache() {
        use super::super::ScopeLabel;
        let cfg = GreedyConfig::default()
            .with_scopes(vec![ScopeLabel::new("industrial")])
            .with_intent_match(super::super::IntentMatchPolicy::Disabled);
        let (rt, sink) = build_runtime(cfg);
        let chain = chain_caps_with_scope("webcam");
        let outcome = rt
            .dispatch_event(&cn("test/scope-miss"), 1, &chain, b"x")
            .await;
        assert_eq!(
            outcome,
            DispatchOutcome::RejectedByAdmission(AdmitRejectReason::Scope)
        );
        assert!(!rt.contains(&cn("test/scope-miss")));
        assert!(sink.announces.lock().is_empty());
        let snap = rt.metrics().snapshot();
        assert_eq!(snap.cluster.admit_rejected_scope_total, 1);
    }

    #[tokio::test]
    async fn second_event_does_not_re_announce_chain() {
        let cfg =
            GreedyConfig::default().with_intent_match(super::super::IntentMatchPolicy::Disabled);
        let (rt, sink) = build_runtime(cfg);
        let chain = chain_caps_with_scope("any");
        rt.dispatch_event(&cn("test/a"), 1, &chain, b"first").await;
        rt.dispatch_event(&cn("test/a"), 1, &chain, b"second").await;
        let announces = sink.announces.lock().clone();
        assert_eq!(announces.len(), 1, "announce only on first cache");
    }

    #[tokio::test]
    async fn bandwidth_budget_blocks_oversize_burst() {
        // 1 Gbps placeholder * 1e-6 fraction = 125 B/s refill,
        // 125-byte capacity. Two consecutive 4 KiB payloads:
        //
        // - First fires the oversize-escape-hatch (4096 > 125 and
        //   bucket is at full credit) — admits, drains to 0.
        // - Second arrives microseconds later; the bucket has
        //   refilled by less than one byte, so the
        //   oversize-escape-hatch (needs full credit) doesn't
        //   fire AND the available tokens fall short of 4096 →
        //   BandwidthExhausted.
        let cfg = GreedyConfig::default()
            .with_intent_match(super::super::IntentMatchPolicy::Disabled)
            .with_bandwidth_budget_fraction(0.000001);
        let (rt, _sink) = build_runtime(cfg);
        let chain = chain_caps_with_scope("any");
        let big = vec![0u8; 4096];
        // First call drains the bucket via the oversize hatch.
        let first = rt.dispatch_event(&cn("a"), 1, &chain, &big).await;
        assert_eq!(first, DispatchOutcome::Cached);
        // Second call exhausts the budget.
        let second = rt.dispatch_event(&cn("a"), 1, &chain, &big).await;
        assert_eq!(second, DispatchOutcome::BandwidthExhausted);
        let snap = rt.metrics().snapshot();
        // Bandwidth-throttling is its own axis on admit_rejected_total
        // so operators can dashboard it apart from real capacity
        // exhaustion (cluster-cap eviction).
        assert_eq!(snap.cluster.admit_rejected_bandwidth_total, 1);
        assert_eq!(snap.cluster.admit_rejected_capacity_total, 0);
    }

    #[tokio::test]
    async fn nic_peak_override_widens_bandwidth_budget() {
        // Same fraction (1e-6) but with a 1000× larger NIC peak →
        // 1000× larger token bucket. The same 4 KiB payload now
        // fits twice without exhausting the budget.
        let cfg = GreedyConfig::default()
            .with_intent_match(super::super::IntentMatchPolicy::Disabled)
            .with_bandwidth_budget_fraction(0.000001)
            .with_nic_peak_bytes_per_s(Some(125_000_000_000));
        let (rt, _sink) = build_runtime(cfg);
        let chain = chain_caps_with_scope("any");
        let big = vec![0u8; 4096];
        let first = rt.dispatch_event(&cn("a"), 1, &chain, &big).await;
        assert_eq!(first, DispatchOutcome::Cached);
        let second = rt.dispatch_event(&cn("a"), 1, &chain, &big).await;
        assert_eq!(
            second,
            DispatchOutcome::Cached,
            "wider NIC-peak override must keep the second event within budget"
        );
    }

    #[tokio::test]
    async fn note_read_bumps_serve_count_metric() {
        let cfg =
            GreedyConfig::default().with_intent_match(super::super::IntentMatchPolicy::Disabled);
        let (rt, _sink) = build_runtime(cfg);
        let chain = chain_caps_with_scope("any");
        rt.dispatch_event(&cn("test/a"), 1, &chain, b"x").await;
        rt.note_read(&cn("test/a"));
        rt.note_read(&cn("test/a"));
        let snap = rt.metrics().snapshot();
        let c = snap
            .channels
            .iter()
            .find(|c| c.channel == "test/a")
            .unwrap();
        assert_eq!(c.serve_count_total, 2);
    }

    #[tokio::test]
    async fn set_local_caps_updates_intent_evaluation() {
        // Without caps the intent gate (Strict) admits no chains
        // with a declared intent the local node can't satisfy.
        // Update local_caps to include the required tag and
        // admission flips to Admit.
        use crate::adapter::net::behavior::tag::TaxonomyAxis;
        let registry = IntentRegistry::defaults();
        let cfg =
            GreedyConfig::default().with_intent_match(super::super::IntentMatchPolicy::Strict);
        let redex = Arc::new(Redex::new());
        let sink = Arc::new(RecorderSink::default());
        let initial_caps = Arc::new(CapabilitySet::default());
        let rt = GreedyRuntime::new(
            cfg,
            redex,
            sink as Arc<dyn ChainTagSink>,
            initial_caps,
            registry,
        );
        let mut chain = CapabilitySet::default();
        chain
            .metadata
            .insert("intent".to_string(), "cpu-bound".to_string());
        let outcome = rt.dispatch_event(&cn("a"), 1, &chain, b"x").await;
        assert_eq!(
            outcome,
            DispatchOutcome::RejectedByAdmission(AdmitRejectReason::Intent)
        );
        // Refresh local caps with cpu_cores=8 — satisfies
        // cpu-bound intent.
        let mut upgraded = CapabilitySet::default();
        upgraded.tags.insert(Tag::AxisValue {
            axis: TaxonomyAxis::Hardware,
            key: "cpu_cores".to_string(),
            value: "8".to_string(),
            separator: crate::adapter::net::behavior::tag::AxisSeparator::Eq,
        });
        rt.set_local_caps(Arc::new(upgraded));
        let outcome2 = rt.dispatch_event(&cn("a"), 1, &chain, b"x").await;
        assert_eq!(outcome2, DispatchOutcome::Cached);
    }

    // ────────────────────────────────────────────────────────
    // Data-gravity integration (Phase 4)
    // ────────────────────────────────────────────────────────

    /// `note_read` bumps the heat counter; `gravity_tick` emits
    /// a `heat:` tag via the sink on first-rate observation.
    /// A second tick suppresses (rate hasn't moved) — pins the
    /// emission-throttle path.
    #[tokio::test]
    async fn gravity_tick_emits_then_suppresses() {
        use crate::adapter::net::dataforts::gravity::DataGravityPolicy;

        let cfg =
            GreedyConfig::default().with_intent_match(super::super::IntentMatchPolicy::Disabled);
        let redex = Arc::new(Redex::new());
        let sink = Arc::new(RecorderSink::default());
        let heat_sink = Arc::new(RecorderHeatSink::default());
        let rt = GreedyRuntime::new(
            cfg,
            redex,
            sink as Arc<dyn ChainTagSink>,
            Arc::new(CapabilitySet::default()),
            IntentRegistry::new(),
        );
        rt.set_gravity(
            DataGravityPolicy::default(),
            heat_sink.clone() as Arc<dyn crate::adapter::net::dataforts::gravity::HeatSink>,
        );
        assert!(rt.gravity_enabled());

        // Dispatch + read so the cache entry exists with
        // origin_hash + the heat counter bumps.
        let chain = CapabilitySet::default();
        rt.dispatch_event(&cn("test/heat-channel"), 0xCAFE, &chain, b"x")
            .await;
        rt.note_read(&cn("test/heat-channel"));

        // First tick — emit (no prior emission).
        rt.gravity_tick().await;
        let emissions = heat_sink.announces.lock().clone();
        assert_eq!(emissions.len(), 1, "first tick must emit");
        assert_eq!(emissions[0].0, 0xCAFE);
        assert!(emissions[0].1 > 0.0);

        // Second tick — suppress (rate hasn't moved).
        rt.gravity_tick().await;
        assert_eq!(
            heat_sink.announces.lock().len(),
            1,
            "second tick must suppress when rate is unchanged"
        );
    }

    /// HeatRegistry must drop entries for evicted chains. Without
    /// the wire-up, long-running nodes accumulate counters
    /// forever and tick() walks stale state.
    #[tokio::test]
    async fn cluster_cap_eviction_drops_heat_counter() {
        use crate::adapter::net::dataforts::gravity::DataGravityPolicy;
        let cfg = GreedyConfig::default()
            .with_intent_match(super::super::IntentMatchPolicy::Disabled)
            .with_per_channel_cap_bytes(super::super::config::MIN_PER_CHANNEL_CAP_BYTES)
            .with_total_cap_bytes(super::super::config::MIN_PER_CHANNEL_CAP_BYTES + 512 * 1024);
        let redex = Arc::new(Redex::new());
        let sink = Arc::new(RecorderSink::default());
        let heat_sink = Arc::new(RecorderHeatSink::default());
        let rt = GreedyRuntime::new(
            cfg,
            redex,
            sink as Arc<dyn ChainTagSink>,
            Arc::new(CapabilitySet::default()),
            IntentRegistry::new(),
        );
        rt.set_gravity(
            DataGravityPolicy::default(),
            heat_sink as Arc<dyn crate::adapter::net::dataforts::gravity::HeatSink>,
        );

        let chain = chain_caps_with_scope("any");
        let big = vec![0u8; 1024 * 1024];
        // Channel A — drive a heat bump via note_read.
        rt.dispatch_event(&cn("test/heat-a"), 0xAAAA_1111, &chain, &big)
            .await;
        rt.note_read(&cn("test/heat-a"));
        // Channel B — large enough to evict A by cluster cap.
        rt.dispatch_event(&cn("test/heat-b"), 0xBBBB_2222, &chain, &big)
            .await;
        // The gravity heat registry must no longer hold A.
        let gravity = rt.inner.gravity.read();
        let g = gravity.as_ref().unwrap();
        let heat = g.heat.lock();
        assert!(
            heat.get(&0xAAAA_1111).is_none(),
            "heat counter for evicted chain must be dropped"
        );
    }

    /// note_read with origin_hash == 0 must NOT enter the heat
    /// registry — otherwise every chain on a node with default
    /// publishers collapses into one shared heat counter. The
    /// skip is reflected in
    /// `gravity_heat_unattributed_total` so operators see the
    /// signal and configure their publishers to stamp identity.
    #[tokio::test]
    async fn gravity_skips_unattributed_origin_zero() {
        use crate::adapter::net::dataforts::gravity::DataGravityPolicy;
        let cfg =
            GreedyConfig::default().with_intent_match(super::super::IntentMatchPolicy::Disabled);
        let redex = Arc::new(Redex::new());
        let sink = Arc::new(RecorderSink::default());
        let heat_sink = Arc::new(RecorderHeatSink::default());
        let rt = GreedyRuntime::new(
            cfg,
            redex,
            sink as Arc<dyn ChainTagSink>,
            Arc::new(CapabilitySet::default()),
            IntentRegistry::new(),
        );
        rt.set_gravity(
            DataGravityPolicy::default(),
            heat_sink.clone() as Arc<dyn crate::adapter::net::dataforts::gravity::HeatSink>,
        );
        let chain = CapabilitySet::default();
        // origin_hash = 0 — default publisher path.
        rt.dispatch_event(&cn("test/unstamped"), 0, &chain, b"x")
            .await;
        rt.note_read(&cn("test/unstamped"));
        rt.note_read(&cn("test/unstamped"));
        rt.gravity_tick().await;
        // Heat sink saw nothing — the bucket was never populated.
        assert!(
            heat_sink.announces.lock().is_empty(),
            "origin_hash=0 must not produce heat emissions"
        );
        // Operator-facing signal is the unattributed counter.
        let snap = rt.metrics().snapshot();
        assert_eq!(snap.cluster.gravity_heat_unattributed_total, 2);
    }

    /// Colocation hints (`metadata.colocate-with-strict`) must be
    /// resolved against the local cache: a chain pointing at an
    /// already-cached origin admits under StrictRequired, while a
    /// chain pointing at an origin we don't hold rejects.
    #[tokio::test]
    async fn colocation_strict_resolves_against_cache() {
        use crate::adapter::net::behavior::tag::Tag;
        let cfg = GreedyConfig::default()
            .with_intent_match(super::super::IntentMatchPolicy::Disabled)
            .with_colocation_policy(super::super::ColocationPolicy::StrictRequired);
        let (rt, _sink) = build_runtime(cfg);

        // First, prime the cache with origin 0xCAFE.
        let mut anchor = CapabilitySet::default();
        anchor.tags.insert(Tag::Reserved {
            prefix: "scope:".to_string(),
            body: "any".to_string(),
        });
        rt.dispatch_event(&cn("test/anchor"), 0xCAFE, &anchor, b"hello")
            .await;
        assert!(rt.contains(&cn("test/anchor")));

        // Now a follower chain that hints colocate-with-strict on
        // 0xCAFE — must admit because the cache holds 0xCAFE.
        let target_hex = format!("{:016x}", 0xCAFEu64);
        let mut follower_yes = CapabilitySet::default();
        follower_yes.tags.insert(Tag::Reserved {
            prefix: "scope:".to_string(),
            body: "any".to_string(),
        });
        follower_yes
            .metadata
            .insert("colocate-with-strict".to_string(), target_hex);
        let outcome = rt
            .dispatch_event(&cn("test/follower-yes"), 0xF000, &follower_yes, b"hi")
            .await;
        assert_eq!(outcome, DispatchOutcome::Cached);

        // A follower pointing at an origin we don't hold rejects.
        let unknown_hex = format!("{:016x}", 0xDEAD_BEEFu64);
        let mut follower_no = CapabilitySet::default();
        follower_no.tags.insert(Tag::Reserved {
            prefix: "scope:".to_string(),
            body: "any".to_string(),
        });
        follower_no
            .metadata
            .insert("colocate-with-strict".to_string(), unknown_hex);
        let outcome = rt
            .dispatch_event(&cn("test/follower-no"), 0xF001, &follower_no, b"hi")
            .await;
        assert_eq!(
            outcome,
            DispatchOutcome::RejectedByAdmission(AdmitRejectReason::Colocation)
        );
    }

    /// observe_event must bound its spawn fan-out: a flood of
    /// inbound events with the runtime stuck (admission lock
    /// held) must drop events past the cap rather than spawning
    /// unbounded.
    #[tokio::test]
    async fn observe_event_drops_under_inflight_cap() {
        let cfg = GreedyConfig::default()
            .with_intent_match(super::super::IntentMatchPolicy::Disabled)
            .with_observer_inflight_cap(2);
        let (rt, _sink) = build_runtime(cfg);
        let chain = Arc::new(chain_caps_with_scope("any"));

        // Block dispatch indefinitely by holding the cache lock
        // (parking_lot::Mutex — synchronous). The first two
        // observe_event spawns acquire permits and wait on the
        // lock; everything past the cap drops.
        let _guard = rt.inner.cache.lock();

        let n = 10u64;
        for i in 0..n {
            rt.observe_event(0, i, chain.clone(), Bytes::from_static(b"x"));
        }
        // Drop counter must reflect the surplus (n - cap = 8).
        let snap = rt.metrics().snapshot();
        assert_eq!(snap.cluster.observer_dropped_overloaded_total, 8);
    }

    /// Two concurrent dispatch_event calls for the same new
    /// channel must converge on one announce — without the
    /// double-checked insert, both callers see is_new_channel=true
    /// (separate contains() lock acquisitions) and both call
    /// announce_chain, leaving one orphaned RedexFile in the
    /// process.
    #[tokio::test]
    async fn concurrent_new_channel_dispatch_announces_once() {
        let cfg =
            GreedyConfig::default().with_intent_match(super::super::IntentMatchPolicy::Disabled);
        let (rt, sink) = build_runtime(cfg);
        let chain = chain_caps_with_scope("any");
        let chain = Arc::new(chain);

        // Fire N concurrent dispatch_event calls against the same
        // brand-new channel and wait for them all to finish.
        let n = 8usize;
        let mut handles = Vec::with_capacity(n);
        for _ in 0..n {
            let rt = rt.clone();
            let chain = chain.clone();
            handles.push(tokio::spawn(async move {
                let payload = vec![b'x'; 4];
                rt.dispatch_event(&cn("test/race"), 0xDEAD, &chain, &payload)
                    .await;
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        // Exactly one announce — every concurrent caller after the
        // first must see the channel already present.
        let announces = sink.announces.lock().clone();
        assert_eq!(
            announces.len(),
            1,
            "concurrent dispatch must produce one announce, got {announces:?}"
        );
    }

    /// Cluster-cap eviction must call `withdraw_chain` for each
    /// evicted channel so reads route to other holders instead of
    /// the now-empty cache file. Skip channels whose `origin_hash`
    /// is zero — those never announced anything to withdraw.
    #[tokio::test]
    async fn cluster_cap_eviction_calls_withdraw() {
        // per-channel cap = 1 MiB (validator floor), total cap =
        // 1.5 MiB. One 1-MiB payload fits per channel; two 1-MiB
        // payloads together exceed the cluster cap → A evicts.
        let cfg = GreedyConfig::default()
            .with_intent_match(super::super::IntentMatchPolicy::Disabled)
            .with_per_channel_cap_bytes(super::super::config::MIN_PER_CHANNEL_CAP_BYTES)
            .with_total_cap_bytes(super::super::config::MIN_PER_CHANNEL_CAP_BYTES + 512 * 1024);
        let (rt, sink) = build_runtime(cfg);
        let chain = chain_caps_with_scope("any");
        let big = vec![0u8; 1024 * 1024];
        rt.dispatch_event(&cn("test/cap-a"), 0xAAAA_1111, &chain, &big)
            .await;
        rt.dispatch_event(&cn("test/cap-b"), 0xBBBB_2222, &chain, &big)
            .await;
        // Both announces should have landed.
        assert_eq!(sink.announces.lock().len(), 2);
        // A was oldest by LRU → A withdraws.
        let withdraws = sink.withdraws.lock().clone();
        assert_eq!(
            withdraws,
            vec![0xAAAA_1111],
            "evicted channel's origin_hash must be withdrawn"
        );
        assert!(!rt.contains(&cn("test/cap-a")));
        assert!(rt.contains(&cn("test/cap-b")));
    }

    /// `note_read` is a no-op for heat when gravity isn't
    /// enabled — pins the "Phase 1 still works without Phase 4"
    /// invariant.
    #[tokio::test]
    async fn note_read_without_gravity_does_not_touch_heat() {
        let cfg =
            GreedyConfig::default().with_intent_match(super::super::IntentMatchPolicy::Disabled);
        let (rt, _sink) = build_runtime(cfg);
        assert!(!rt.gravity_enabled());

        let chain = CapabilitySet::default();
        rt.dispatch_event(&cn("test/no-gravity"), 0xBEEF, &chain, b"x")
            .await;
        rt.note_read(&cn("test/no-gravity"));
        // No heat-sink to assert on; the test passes by not
        // panicking and by leaving gravity_enabled false.
        assert!(!rt.gravity_enabled());
    }

    // --- G-1 blob-pull verdict wiring (PR-5c) ---

    /// Encode `payload` as a `BlobRef::Small`-bearing event payload
    /// so the dispatch hook's `classify_payload` sees the magic +
    /// version + body and decodes a real BlobRef. Mirrors the
    /// production wire shape that `publish_blob` produces.
    fn encode_blob_payload(payload: &[u8]) -> Vec<u8> {
        use crate::adapter::net::dataforts::blob::BlobRef;
        let hash: [u8; 32] = blake3::hash(payload).into();
        BlobRef::small("mesh://test", hash, payload.len() as u64).encode()
    }

    /// `dataforts.blob.storage` + `dataforts.greedy.enabled` +
    /// proximity tag set — qualifies as a participating local node
    /// under `should_pull_blob`.
    fn participating_blob_caps() -> CapabilitySet {
        CapabilitySet::new()
            .add_tag("dataforts.blob.storage")
            .add_tag("dataforts.blob.disk_total_gb=100")
            .add_tag("dataforts.blob.disk_free_gb=50")
            .add_tag("dataforts.greedy.enabled")
            .add_tag("dataforts.greedy.scope=mesh")
            .add_tag("dataforts.greedy.proximity=128")
    }

    fn publisher_mesh_scope_caps() -> CapabilitySet {
        CapabilitySet::new()
            .add_tag("dataforts.greedy.scope=mesh")
            .add_tag("dataforts.gravity.scope=mesh")
    }

    #[tokio::test]
    async fn inline_payload_does_not_bump_blob_pull_counters() {
        let cfg =
            GreedyConfig::default().with_intent_match(super::super::IntentMatchPolicy::Disabled);
        let (rt, _sink) = build_runtime(cfg);
        rt.set_local_caps(Arc::new(participating_blob_caps()));
        let chain = publisher_mesh_scope_caps();

        // Plain bytes — no BlobRef discriminator → classify_payload
        // reports Inline → G-1 hook is a no-op.
        let outcome = rt
            .dispatch_event(&cn("test/g1-inline"), 0xAA, &chain, b"plain payload")
            .await;
        assert_eq!(outcome, DispatchOutcome::Cached);

        let snap = rt.metrics().snapshot();
        assert_eq!(snap.cluster.blob_pulls_admitted_total, 0);
        assert_eq!(snap.cluster.blob_pulls_rejected_no_storage_total, 0);
        assert_eq!(snap.cluster.blob_pulls_rejected_greedy_disabled_total, 0);
        assert_eq!(snap.cluster.blob_pulls_rejected_proximity_zero_total, 0);
        assert_eq!(snap.cluster.blob_pulls_rejected_unhealthy_total, 0);
        assert_eq!(snap.cluster.blob_pulls_rejected_scope_mismatch_total, 0);
    }

    #[tokio::test]
    async fn blobref_payload_with_participating_local_bumps_admitted() {
        let cfg =
            GreedyConfig::default().with_intent_match(super::super::IntentMatchPolicy::Disabled);
        let (rt, _sink) = build_runtime(cfg);
        rt.set_local_caps(Arc::new(participating_blob_caps()));
        let chain = publisher_mesh_scope_caps();
        let encoded = encode_blob_payload(b"out of band content");

        let outcome = rt
            .dispatch_event(&cn("test/g1-admit"), 0xBB, &chain, &encoded)
            .await;
        assert_eq!(outcome, DispatchOutcome::Cached);

        let snap = rt.metrics().snapshot();
        assert_eq!(snap.cluster.blob_pulls_admitted_total, 1);
        assert_eq!(snap.cluster.blob_pulls_rejected_no_storage_total, 0);
    }

    #[tokio::test]
    async fn blobref_payload_without_storage_cap_bumps_no_storage() {
        let cfg =
            GreedyConfig::default().with_intent_match(super::super::IntentMatchPolicy::Disabled);
        let (rt, _sink) = build_runtime(cfg);
        // Local lacks `dataforts.blob.storage` → G-1 vetoes with
        // NoStorageCap as soon as it sees the BlobRef.
        let local = CapabilitySet::new()
            .add_tag("dataforts.greedy.enabled")
            .add_tag("dataforts.greedy.scope=mesh")
            .add_tag("dataforts.greedy.proximity=128");
        rt.set_local_caps(Arc::new(local));
        let chain = publisher_mesh_scope_caps();
        let encoded = encode_blob_payload(b"out of band content");

        rt.dispatch_event(&cn("test/g1-nostorage"), 0xCC, &chain, &encoded)
            .await;

        let snap = rt.metrics().snapshot();
        assert_eq!(snap.cluster.blob_pulls_admitted_total, 0);
        assert_eq!(snap.cluster.blob_pulls_rejected_no_storage_total, 1);
    }

    #[tokio::test]
    async fn blobref_payload_with_greedy_disabled_bumps_greedy_disabled() {
        let cfg =
            GreedyConfig::default().with_intent_match(super::super::IntentMatchPolicy::Disabled);
        let (rt, _sink) = build_runtime(cfg);
        // Storage cap present, but greedy is not enabled on the
        // local node → G-1 vetoes with GreedyDisabled.
        let local = CapabilitySet::new()
            .add_tag("dataforts.blob.storage")
            .add_tag("dataforts.blob.disk_total_gb=100")
            .add_tag("dataforts.blob.disk_free_gb=50");
        rt.set_local_caps(Arc::new(local));
        let chain = publisher_mesh_scope_caps();
        let encoded = encode_blob_payload(b"out of band content");

        rt.dispatch_event(&cn("test/g1-greedy-off"), 0xDD, &chain, &encoded)
            .await;

        let snap = rt.metrics().snapshot();
        assert_eq!(snap.cluster.blob_pulls_admitted_total, 0);
        assert_eq!(snap.cluster.blob_pulls_rejected_greedy_disabled_total, 1);
    }

    #[tokio::test]
    async fn blobref_payload_with_proximity_zero_bumps_proximity_zero() {
        let cfg =
            GreedyConfig::default().with_intent_match(super::super::IntentMatchPolicy::Disabled);
        let (rt, _sink) = build_runtime(cfg);
        // Storage + greedy enabled, but proximity tag explicitly
        // zero → operator-driven veto without flipping the master
        // flag.
        let local = CapabilitySet::new()
            .add_tag("dataforts.blob.storage")
            .add_tag("dataforts.blob.disk_total_gb=100")
            .add_tag("dataforts.blob.disk_free_gb=50")
            .add_tag("dataforts.greedy.enabled")
            .add_tag("dataforts.greedy.scope=mesh")
            .add_tag("dataforts.greedy.proximity=0");
        rt.set_local_caps(Arc::new(local));
        let chain = publisher_mesh_scope_caps();
        let encoded = encode_blob_payload(b"out of band content");

        rt.dispatch_event(&cn("test/g1-prox-zero"), 0xEE, &chain, &encoded)
            .await;

        let snap = rt.metrics().snapshot();
        assert_eq!(snap.cluster.blob_pulls_admitted_total, 0);
        assert_eq!(snap.cluster.blob_pulls_rejected_proximity_zero_total, 1);
    }

    #[tokio::test]
    async fn admit_rejected_chain_does_not_evaluate_blob_pull() {
        // When the chain admission stage rejects, dispatch_event
        // returns early and never reaches the G-1 hook. Even an
        // otherwise-pullable BlobRef payload must not bump the
        // blob counters when the chain itself was rejected.
        use super::super::ScopeLabel;
        let cfg = GreedyConfig::default()
            .with_scopes(vec![ScopeLabel::new("industrial")])
            .with_intent_match(super::super::IntentMatchPolicy::Disabled);
        let (rt, _sink) = build_runtime(cfg);
        rt.set_local_caps(Arc::new(participating_blob_caps()));
        let chain = chain_caps_with_scope("webcam");
        let encoded = encode_blob_payload(b"would pull");

        let outcome = rt
            .dispatch_event(&cn("test/g1-chain-rejected"), 0xFF, &chain, &encoded)
            .await;
        assert!(matches!(
            outcome,
            DispatchOutcome::RejectedByAdmission(AdmitRejectReason::Scope)
        ));

        let snap = rt.metrics().snapshot();
        assert_eq!(snap.cluster.admit_rejected_scope_total, 1);
        // Critically: zero blob-pull verdicts because dispatch_event
        // returned before the G-1 hook ran.
        assert_eq!(snap.cluster.blob_pulls_admitted_total, 0);
        assert_eq!(snap.cluster.blob_pulls_rejected_no_storage_total, 0);
        assert_eq!(snap.cluster.blob_pulls_rejected_greedy_disabled_total, 0);
        assert_eq!(snap.cluster.blob_pulls_rejected_proximity_zero_total, 0);
        assert_eq!(snap.cluster.blob_pulls_rejected_unhealthy_total, 0);
        assert_eq!(snap.cluster.blob_pulls_rejected_scope_mismatch_total, 0);
    }

    // --- Chain-fold refcount source (PR-5h) ---

    fn encode_blob_payload_with_hash(payload: &[u8]) -> (Vec<u8>, [u8; 32]) {
        use crate::adapter::net::dataforts::blob::BlobRef;
        let hash: [u8; 32] = blake3::hash(payload).into();
        let bytes = BlobRef::small("mesh://test", hash, payload.len() as u64).encode();
        (bytes, hash)
    }

    #[tokio::test]
    async fn blobref_event_with_refcount_table_increments_hash() {
        use crate::adapter::net::dataforts::blob::BlobRefcountTable;
        let cfg =
            GreedyConfig::default().with_intent_match(super::super::IntentMatchPolicy::Disabled);
        let (rt, _sink) = build_runtime(cfg);
        rt.set_local_caps(Arc::new(participating_blob_caps()));
        let table = BlobRefcountTable::new();
        rt.set_blob_refcount_table(table.clone());
        assert!(rt.blob_refcount_enabled());

        let chain = publisher_mesh_scope_caps();
        let (encoded, hash) = encode_blob_payload_with_hash(b"refcount source");
        rt.dispatch_event(&cn("test/refcount-incr"), 0xAA, &chain, &encoded)
            .await;

        let entry = table.get(&hash).expect("hash must be in refcount table");
        assert_eq!(entry.refcount, 1, "first observation bumps refcount to 1");
    }

    #[tokio::test]
    async fn duplicate_blobref_within_channel_does_not_double_count() {
        use crate::adapter::net::dataforts::blob::BlobRefcountTable;
        let cfg =
            GreedyConfig::default().with_intent_match(super::super::IntentMatchPolicy::Disabled);
        let (rt, _sink) = build_runtime(cfg);
        rt.set_local_caps(Arc::new(participating_blob_caps()));
        let table = BlobRefcountTable::new();
        rt.set_blob_refcount_table(table.clone());

        let chain = publisher_mesh_scope_caps();
        let (encoded, hash) = encode_blob_payload_with_hash(b"dedup me");
        let channel = cn("test/refcount-dedup");
        rt.dispatch_event(&channel, 0xBB, &chain, &encoded).await;
        rt.dispatch_event(&channel, 0xBB, &chain, &encoded).await;
        rt.dispatch_event(&channel, 0xBB, &chain, &encoded).await;

        let entry = table.get(&hash).expect("hash in refcount table");
        assert_eq!(
            entry.refcount, 1,
            "duplicate observations on the same channel must not stack the refcount"
        );
    }

    #[tokio::test]
    async fn inline_payload_with_refcount_table_does_not_increment() {
        use crate::adapter::net::dataforts::blob::BlobRefcountTable;
        let cfg =
            GreedyConfig::default().with_intent_match(super::super::IntentMatchPolicy::Disabled);
        let (rt, _sink) = build_runtime(cfg);
        rt.set_local_caps(Arc::new(participating_blob_caps()));
        let table = BlobRefcountTable::new();
        rt.set_blob_refcount_table(table.clone());

        let chain = publisher_mesh_scope_caps();
        rt.dispatch_event(&cn("test/refcount-inline"), 0xCC, &chain, b"plain")
            .await;

        // No BlobRef discriminator → no hash to record.
        assert_eq!(
            table.zero_refcount_count(),
            0,
            "inline payloads must not bump refcount entries"
        );
    }

    #[tokio::test]
    async fn eviction_decrements_refcounts_for_evicted_channel() {
        use crate::adapter::net::dataforts::blob::BlobRefcountTable;
        // Match the shape from `cluster_cap_eviction_calls_withdraw`:
        // per-channel cap = 1 MiB (validator floor); total cap = 1.5 MiB.
        // One 1-MiB payload fits per channel; two 1-MiB payloads
        // together exceed the cluster cap → channel A evicts when
        // channel B is admitted.
        let cfg = GreedyConfig::default()
            .with_intent_match(super::super::IntentMatchPolicy::Disabled)
            .with_per_channel_cap_bytes(super::super::config::MIN_PER_CHANNEL_CAP_BYTES)
            .with_total_cap_bytes(super::super::config::MIN_PER_CHANNEL_CAP_BYTES + 512 * 1024);
        let (rt, _sink) = build_runtime(cfg);
        rt.set_local_caps(Arc::new(participating_blob_caps()));
        let table = BlobRefcountTable::new();
        rt.set_blob_refcount_table(table.clone());

        let chain = publisher_mesh_scope_caps();
        let (encoded_a, hash_a) = encode_blob_payload_with_hash(b"channel A blob");

        // Channel A: small BlobRef event first (bumps refcount[hash_a]
        // to 1) — followed by a 1-MiB inline padding event that
        // fills the per-channel cap so the cluster cap is near
        // saturation. The BlobRef event itself is tiny (~40 bytes
        // wire); the padding is what gets channel A to ~1 MiB.
        let channel_a = cn("test/refcount-evict-a");
        rt.dispatch_event(&channel_a, 0xAA, &chain, &encoded_a)
            .await;
        assert_eq!(
            table.get(&hash_a).map(|e| e.refcount),
            Some(1),
            "hash_a bumped on admit"
        );
        let big = vec![0u8; 1024 * 1024];
        rt.dispatch_event(&channel_a, 0xAA, &chain, &big).await;
        assert!(rt.contains(&channel_a), "channel A must be cached");

        // Channel B: 1-MiB inline payload pushes total bytes past
        // the cluster cap → channel A (LRU oldest) evicts.
        rt.dispatch_event(&cn("test/refcount-evict-b"), 0xBB, &chain, &big)
            .await;

        // hash_a's refcount must be back at 0 — eviction released it.
        assert!(!rt.contains(&channel_a), "channel A must have evicted");
        assert_eq!(
            table.get(&hash_a).map(|e| e.refcount),
            Some(0),
            "hash_a refcount must drop on channel-A eviction (got {:?})",
            table.get(&hash_a),
        );
    }

    #[tokio::test]
    async fn refcount_source_disabled_when_no_table_wired() {
        let cfg =
            GreedyConfig::default().with_intent_match(super::super::IntentMatchPolicy::Disabled);
        let (rt, _sink) = build_runtime(cfg);
        assert!(!rt.blob_refcount_enabled());
        // No panic, no table — dispatch_event with a BlobRef still
        // works; refcount path is a silent no-op.
        rt.set_local_caps(Arc::new(participating_blob_caps()));
        let chain = publisher_mesh_scope_caps();
        let (encoded, _) = encode_blob_payload_with_hash(b"no table wired");
        rt.dispatch_event(&cn("test/refcount-disabled"), 0xDD, &chain, &encoded)
            .await;
    }

    // --- Remote blob prefetch wiring (PR-5i) ---

    /// Recorder adapter — counts prefetch calls + lets a test
    /// pick admit vs reject outcomes per-call. Mirrors the
    /// `AdversarialAdapter` shape from `dispatch.rs` tests but
    /// for the prefetch path.
    struct RecorderAdapter {
        prefetch_calls: Arc<std::sync::atomic::AtomicU64>,
        prefetch_fail: bool,
    }

    impl RecorderAdapter {
        fn new() -> Self {
            Self {
                prefetch_calls: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                prefetch_fail: false,
            }
        }
        fn with_failing_prefetch() -> Self {
            Self {
                prefetch_calls: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                prefetch_fail: true,
            }
        }
    }

    #[async_trait::async_trait]
    impl crate::adapter::net::dataforts::blob::BlobAdapter for RecorderAdapter {
        fn adapter_id(&self) -> &str {
            "test-recorder"
        }
        async fn store(
            &self,
            _: &crate::adapter::net::dataforts::blob::BlobRef,
            _: &[u8],
        ) -> Result<(), crate::adapter::net::dataforts::blob::BlobError> {
            unreachable!("test recorder only exercises prefetch")
        }
        async fn fetch(
            &self,
            _: &crate::adapter::net::dataforts::blob::BlobRef,
        ) -> Result<Vec<u8>, crate::adapter::net::dataforts::blob::BlobError> {
            unreachable!()
        }
        async fn fetch_range(
            &self,
            _: &crate::adapter::net::dataforts::blob::BlobRef,
            _: std::ops::Range<u64>,
        ) -> Result<Vec<u8>, crate::adapter::net::dataforts::blob::BlobError> {
            unreachable!()
        }
        async fn exists(
            &self,
            _: &crate::adapter::net::dataforts::blob::BlobRef,
        ) -> Result<bool, crate::adapter::net::dataforts::blob::BlobError> {
            unreachable!()
        }
        async fn prefetch(
            &self,
            _: &crate::adapter::net::dataforts::blob::BlobRef,
        ) -> Result<(), crate::adapter::net::dataforts::blob::BlobError> {
            self.prefetch_calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if self.prefetch_fail {
                Err(crate::adapter::net::dataforts::blob::BlobError::Backend(
                    "test failure".into(),
                ))
            } else {
                Ok(())
            }
        }
    }

    #[tokio::test]
    async fn g1_admit_with_adapter_wired_calls_prefetch() {
        let cfg =
            GreedyConfig::default().with_intent_match(super::super::IntentMatchPolicy::Disabled);
        let (rt, _sink) = build_runtime(cfg);
        rt.set_local_caps(Arc::new(participating_blob_caps()));
        let adapter = Arc::new(RecorderAdapter::new());
        let calls = adapter.prefetch_calls.clone();
        rt.set_blob_adapter(adapter);
        assert!(rt.blob_adapter_enabled());

        let chain = publisher_mesh_scope_caps();
        let (encoded, _) = encode_blob_payload_with_hash(b"prefetch me");
        rt.dispatch_event(&cn("test/prefetch-admit"), 0xAA, &chain, &encoded)
            .await;

        // Wait for the spawned prefetch task to land (best-effort
        // async — give it a bounded window).
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            if calls.load(std::sync::atomic::Ordering::Relaxed) >= 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "prefetch must be called exactly once per admit"
        );

        // Wait for the ok counter to land (the spawn writes the
        // counter after the prefetch await).
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            let snap = rt.metrics().snapshot();
            if snap.cluster.blob_prefetches_ok_total >= 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let snap = rt.metrics().snapshot();
        assert_eq!(snap.cluster.blob_prefetches_ok_total, 1);
        assert_eq!(snap.cluster.blob_prefetches_err_total, 0);
    }

    #[tokio::test]
    async fn g1_reject_does_not_call_prefetch() {
        let cfg =
            GreedyConfig::default().with_intent_match(super::super::IntentMatchPolicy::Disabled);
        let (rt, _sink) = build_runtime(cfg);
        // Compute-only local — should_pull_blob vetoes on
        // NoStorageCap so the prefetch must NOT fire.
        let compute_only = CapabilitySet::new()
            .add_tag("dataforts.greedy.enabled")
            .add_tag("dataforts.greedy.scope=mesh")
            .add_tag("dataforts.greedy.proximity=128");
        rt.set_local_caps(Arc::new(compute_only));
        let adapter = Arc::new(RecorderAdapter::new());
        let calls = adapter.prefetch_calls.clone();
        rt.set_blob_adapter(adapter);

        let chain = publisher_mesh_scope_caps();
        let (encoded, _) = encode_blob_payload_with_hash(b"vetoed");
        rt.dispatch_event(&cn("test/prefetch-reject"), 0xBB, &chain, &encoded)
            .await;

        // Brief settle window — confirm no prefetch task spawned.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "veto must short-circuit before the prefetch spawn"
        );
        let snap = rt.metrics().snapshot();
        assert_eq!(snap.cluster.blob_pulls_rejected_no_storage_total, 1);
        assert_eq!(snap.cluster.blob_prefetches_ok_total, 0);
    }

    #[tokio::test]
    async fn prefetch_failure_bumps_err_counter() {
        let cfg =
            GreedyConfig::default().with_intent_match(super::super::IntentMatchPolicy::Disabled);
        let (rt, _sink) = build_runtime(cfg);
        rt.set_local_caps(Arc::new(participating_blob_caps()));
        let adapter = Arc::new(RecorderAdapter::with_failing_prefetch());
        rt.set_blob_adapter(adapter);

        let chain = publisher_mesh_scope_caps();
        let (encoded, _) = encode_blob_payload_with_hash(b"will fail");
        rt.dispatch_event(&cn("test/prefetch-err"), 0xCC, &chain, &encoded)
            .await;

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            let snap = rt.metrics().snapshot();
            if snap.cluster.blob_prefetches_err_total >= 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let snap = rt.metrics().snapshot();
        assert_eq!(snap.cluster.blob_prefetches_err_total, 1);
        assert_eq!(snap.cluster.blob_prefetches_ok_total, 0);
    }

    #[tokio::test]
    async fn g1_admit_without_adapter_does_not_panic_or_count_prefetch() {
        let cfg =
            GreedyConfig::default().with_intent_match(super::super::IntentMatchPolicy::Disabled);
        let (rt, _sink) = build_runtime(cfg);
        rt.set_local_caps(Arc::new(participating_blob_caps()));
        assert!(!rt.blob_adapter_enabled());

        let chain = publisher_mesh_scope_caps();
        let (encoded, _) = encode_blob_payload_with_hash(b"admit but no adapter");
        rt.dispatch_event(&cn("test/prefetch-noadapter"), 0xDD, &chain, &encoded)
            .await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let snap = rt.metrics().snapshot();
        // Admit still counts (decision-only path stays correct).
        assert_eq!(snap.cluster.blob_pulls_admitted_total, 1);
        // No prefetch counters move when no adapter is wired.
        assert_eq!(snap.cluster.blob_prefetches_ok_total, 0);
        assert_eq!(snap.cluster.blob_prefetches_err_total, 0);
    }
}
