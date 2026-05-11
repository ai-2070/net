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
    /// `channel_hash` is the 16-bit wire-form hash carried in the
    /// Net header — the mesh strips channel names on ingress, so
    /// the observer maps `channel_hash` to a cache-side
    /// [`ChannelName`] via [`synthesize_cache_channel_name`].
    fn observe_event(
        &self,
        channel_hash: u16,
        origin_hash: u64,
        chain_caps: Arc<CapabilitySet>,
        payload: Bytes,
    );
}

/// Synthesize a stable cache-side [`ChannelName`] from a 16-bit
/// channel hash. Hash-collision risk is bounded — different real
/// channels with the same hash share a cache file, which behaves
/// as a small mix-up at the cache layer (events from both channels
/// land in the same per-channel-hash retention bucket). Operators
/// running greedy across high-churn channel spaces should monitor
/// hash collisions via the substrate's existing observability.
///
/// Naming convention `dataforts/greedy/<hex>` reserves a
/// channel-namespace prefix that won't collide with application
/// channels (`/` separators + reserved-prefix discipline).
pub fn synthesize_cache_channel_name(channel_hash: u16) -> ChannelName {
    ChannelName::new(&format!("dataforts/greedy/{:04x}", channel_hash))
        .expect("hex-formatted name with reserved prefix is always valid")
}

/// 1 Gbps placeholder for the measured NIC peak. The replication
/// runtime uses the same placeholder until the plan §6 proximity-
/// graph throughput probe lands; reuse the same number here so the
/// `replication_budget_fraction` and `bandwidth_budget_fraction`
/// configurations share a denominator. Operators with > 1 Gbps
/// links see proportional under-utilization until that probe is
/// wired up.
// TODO(plan-§6): wire the measured-NIC-peak probe through here.
const NIC_PEAK_BYTES_PER_S: u64 = 125_000_000;

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
        let budget =
            BandwidthBudget::new(config.bandwidth_budget_fraction, NIC_PEAK_BYTES_PER_S, now);
        let cache = GreedyCacheRegistry::new(config.total_cap_bytes);
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
                    // Bump the heat counter unconditionally —
                    // origin_hash == 0 is a valid bucket (the
                    // standard publish path leaves the header
                    // origin_hash at zero unless the publisher
                    // explicitly stamps its identity, in which
                    // case all chains observed under that
                    // publisher share one heat counter). Real
                    // deployments configure their publishers to
                    // stamp meaningful origin_hashes so the
                    // counter is per-chain; tests against the
                    // default publish path aggregate into the
                    // zero bucket.
                    let mut heat = gravity.heat.lock();
                    heat.entry_mut(origin_hash, gravity.policy.decay_half_life, now)
                        .bump(now);
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
        for (origin_hash, emission) in emissions {
            match emission {
                super::super::gravity::HeatEmission::Suppress => {}
                super::super::gravity::HeatEmission::Emit { rate } => {
                    // Normalize unbounded rate to [0.0, 1.0] for
                    // the wire encoding. Saturate above 1.0; the
                    // substrate clamps anyway but normalize here
                    // so the per-tick value is interpretable.
                    let normalized = (rate / (rate + 1.0)).min(1.0);
                    if let Err(e) = sink.announce_heat(origin_hash, normalized).await {
                        tracing::trace!(
                            origin_hash = origin_hash,
                            error = ?e,
                            "gravity: announce_heat failed"
                        );
                    }
                }
                super::super::gravity::HeatEmission::Withdraw => {
                    if let Err(e) = sink.withdraw_heat(origin_hash).await {
                        tracing::trace!(
                            origin_hash = origin_hash,
                            error = ?e,
                            "gravity: withdraw_heat failed"
                        );
                    }
                }
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

        // 1. Admission decision (pure).
        let verdict = should_admit(&AdmissionInputs {
            chain_caps,
            local_caps: &local_caps,
            config: &self.inner.config,
            intent_registry: &self.inner.intent_registry,
            metadata_keys: &self.inner.metadata_keys,
            colocation_target_held: None,
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

        // 2. Bandwidth budget.
        let payload_bytes = payload.len() as u64;
        let admitted_by_budget = {
            let mut budget = self.inner.budget.lock();
            budget.try_consume(payload_bytes, now)
        };
        if !admitted_by_budget {
            self.inner
                .metrics
                .cluster()
                .incr_admit_rejected(AdmitRejectReason::Capacity);
            return DispatchOutcome::BandwidthExhausted;
        }

        // 3. Lazy admission — open a per-channel cache file if we
        //    don't have one yet, then append.
        let is_new_channel = !self.inner.cache.lock().contains(channel);
        if is_new_channel {
            let cfg = RedexFileConfig::default()
                .with_retention_max_bytes(self.inner.config.per_channel_cap_bytes);
            let file = match self.inner.redex.open_file(channel, cfg) {
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
            cache.upsert(channel.clone(), file, now);
            cache.set_origin_hash(channel, origin_hash);
        }

        // Read the file handle out of the registry under the lock,
        // then drop the lock before the append (which takes the
        // file's own lock — never hold two locks across an I/O).
        let file_for_append = {
            let cache = self.inner.cache.lock();
            cache.get(channel).map(|e| e.file.clone())
        };
        let Some(file) = file_for_append else {
            return DispatchOutcome::AppendFailed;
        };

        if let Err(e) = file.append(payload) {
            tracing::trace!(
                channel = channel.as_str(),
                error = ?e,
                "greedy: cache append failed; greedy is best-effort"
            );
            return DispatchOutcome::AppendFailed;
        }

        // 4. Byte accounting + cluster-cap eviction.
        let sweep = {
            let mut cache = self.inner.cache.lock();
            let sweep = cache.note_appended(channel, payload_bytes, now);
            // Refresh per-channel bytes gauge.
            if let Some(entry) = cache.get(channel) {
                self.inner
                    .metrics
                    .for_channel(channel.as_str())
                    .set_bytes_resident(entry.bytes);
            }
            sweep
        };

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
}

impl GreedyObserver for GreedyRuntime {
    fn observe_event(
        &self,
        channel_hash: u16,
        origin_hash: u64,
        chain_caps: Arc<CapabilitySet>,
        payload: Bytes,
    ) {
        let runtime = self.clone();
        let channel = synthesize_cache_channel_name(channel_hash);
        tokio::spawn(async move {
            let _ = runtime
                .dispatch_event(&channel, origin_hash, &chain_caps, &payload)
                .await;
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
        assert_eq!(snap.cluster.admit_rejected_capacity_total, 1);
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
}
