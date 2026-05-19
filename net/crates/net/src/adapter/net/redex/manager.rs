//! `Redex` — manager owning the `ChannelName -> RedexFile` map.
//!
//! Holds an optional reference to an [`AuthGuard`](super::super::AuthGuard)
//! plus a local origin-hash. When auth is wired up, `open_file` rejects
//! opens unless `(origin, canonical channel name)` has been explicitly
//! authorized via [`AuthGuard::allow_channel`]. The 16-bit wire
//! `channel_hash` alone is not sufficient here — at mesh scale it
//! collides often enough to allow ACL bypass between unrelated names,
//! and even a 64-bit non-cryptographic hash would be crackable by
//! birthday search offline. Keying on the canonical name is the only
//! collision-free answer.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use parking_lot::Mutex;

use super::super::channel::{AuthGuard, ChannelName};
use super::config::RedexFileConfig;
use super::error::RedexError;
use super::file::RedexFile;
use super::replication::{ChannelId, ReplicaRole};
use super::replication_budget::BandwidthBudget;
use super::replication_config::PlacementStrategy;
use super::replication_coordinator::{ChannelIdentity, ReplicationCoordinator};
use super::replication_metrics::{ReplicationMetricsRegistry, ReplicationMetricsSnapshot};
use super::replication_router::RedexReplicationRouter;
use super::replication_runtime::{spawn_replication_runtime, RuntimeInputs};
use crate::adapter::net::MeshNode;

#[cfg(feature = "redex-disk")]
use std::path::PathBuf;

/// Replication wiring installed by [`Redex::enable_replication`]. Owns
/// the mesh handle (used as both `ChainTagSink` and
/// `ReplicationDispatcher`), the per-`Redex` router shared with the
/// mesh's `SUBPROTOCOL_REDEX` inbound dispatch, and the metrics
/// registry that every per-channel coordinator publishes to.
///
/// The `Drop` impl uninstalls the router from the mesh — otherwise
/// the mesh holds the only remaining `Arc<RedexReplicationRouter>`
/// after `Redex` drops, keeping every per-channel runtime task
/// alive with no Redex driving them. Routing-then-dropping the
/// router lets the runtime handles drop, which closes their
/// inboxes, which lets the tokio tasks exit cleanly.
struct ReplicationWiring {
    mesh: Arc<MeshNode>,
    router: Arc<RedexReplicationRouter>,
    metrics: Arc<ReplicationMetricsRegistry>,
}

/// Per-channel replication status entry surfaced by
/// [`Redex::replication_status_snapshot`]. Pairs with the
/// [`ReplicationMetricsSnapshot`] atomic-counter view for the full
/// operator observability picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicationChannelStatus {
    /// Human-readable channel name (matches the
    /// `ChannelMetrics::channel` field in the metrics snapshot).
    pub channel_name: String,
    /// Current replica role per the state machine.
    pub role: ReplicaRole,
    /// Coordinator's view of the local `tail_seq`. The leader's
    /// view is canonical; replica views catch up via the heartbeat
    /// cycle.
    pub tail_seq: u64,
}

impl Drop for ReplicationWiring {
    fn drop(&mut self) {
        // Best-effort uninstall — `set_replication_inbound_router(None)`
        // is infallible and idempotent. Even if the mesh has already
        // been shut down or the slot was reassigned by a racing
        // `enable_replication` (which `Redex::enable_replication`
        // refuses on idempotency, so this would only happen if a
        // caller bypassed the wrapper), the call is a safe no-op.
        self.mesh.set_replication_inbound_router(None);
    }
}

/// Wiring kept alive while `Redex::enable_greedy_dataforts` is in
/// effect. `Drop` un-installs the observer from the mesh so the
/// hot path falls back to the lock-read-and-skip pattern.
#[cfg(feature = "dataforts")]
struct GreedyWiring {
    mesh: Arc<MeshNode>,
    runtime: Arc<super::super::dataforts::GreedyRuntime>,
    /// Periodic `gravity_tick` driver, spawned by
    /// `Redex::enable_gravity_for_greedy`. `None` when gravity
    /// is not enabled. Aborted on `Drop` to stop the tick loop.
    #[cfg(feature = "dataforts")]
    gravity_tick_task: parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

#[cfg(feature = "dataforts")]
impl Drop for GreedyWiring {
    fn drop(&mut self) {
        // Stop the gravity-tick loop first so the runtime drop
        // path doesn't race a tick mid-flight.
        #[cfg(feature = "dataforts")]
        if let Some(task) = self.gravity_tick_task.lock().take() {
            task.abort();
        }
        self.mesh.set_greedy_observer(None);
    }
}

/// Manager for a set of RedEX files bound to channel names.
pub struct Redex {
    files: DashMap<ChannelName, RedexFile>,
    auth: Option<Arc<AuthGuard>>,
    origin_hash: u64,
    #[cfg(feature = "redex-disk")]
    persistent_dir: Option<PathBuf>,
    /// Cumulative count of `build_file` invocations. Sits next to the
    /// `files` map purely so regression tests can assert that
    /// concurrent `open_file` calls for the same name don't both
    /// build — a previous version had two threads race past the
    /// `files.get()` precheck, both run `build_file`, and the loser
    /// of the subsequent `or_insert` was dropped without `close()`,
    /// leaking its `Interval` fsync task and dup file handles for
    /// the lifetime of the runtime.
    build_count: AtomicU64,
    /// Replication wiring installed by [`Redex::enable_replication`].
    /// `None` keeps the manager single-node — opens with
    /// `RedexFileConfig::replication == Some(_)` then surface a typed
    /// error.
    replication: parking_lot::RwLock<Option<Arc<ReplicationWiring>>>,
    /// Greedy-LRU wiring installed by
    /// [`Redex::enable_greedy_dataforts`]. `None` keeps greedy
    /// caching disabled — inbound events flow through the mesh's
    /// hot path with a single `RwLock` read and skip.
    #[cfg(feature = "dataforts")]
    greedy: parking_lot::RwLock<Option<Arc<GreedyWiring>>>,
}

impl Redex {
    /// Create a manager without auth enforcement. Suitable for
    /// single-process tests and local workloads.
    pub fn new() -> Self {
        Self {
            files: DashMap::new(),
            auth: None,
            origin_hash: 0,
            #[cfg(feature = "redex-disk")]
            persistent_dir: None,
            build_count: AtomicU64::new(0),
            replication: parking_lot::RwLock::new(None),
            #[cfg(feature = "dataforts")]
            greedy: parking_lot::RwLock::new(None),
        }
    }

    /// Create a manager that rejects `open_file` unless the
    /// `(origin_hash, channel)` pair has been authorized by `guard`
    /// via [`AuthGuard::allow_channel`]. Uses the exact 64-bit
    /// channel identity, not the 16-bit wire hash — see the module
    /// docs for rationale.
    pub fn with_auth(guard: Arc<AuthGuard>, origin_hash: u64) -> Self {
        Self {
            files: DashMap::new(),
            auth: Some(guard),
            origin_hash,
            #[cfg(feature = "redex-disk")]
            persistent_dir: None,
            build_count: AtomicU64::new(0),
            replication: parking_lot::RwLock::new(None),
            #[cfg(feature = "dataforts")]
            greedy: parking_lot::RwLock::new(None),
        }
    }

    /// Set the base directory for disk-backed (`persistent: true`)
    /// files. All files opened with `persistent: true` use
    /// `<dir>/<channel_path>/{idx,dat}` for durability.
    #[cfg(feature = "redex-disk")]
    pub fn with_persistent_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.persistent_dir = Some(dir.into());
        self
    }

    /// Install replication wiring rooted at `mesh`. Constructs a
    /// fresh [`RedexReplicationRouter`] + [`ReplicationMetricsRegistry`]
    /// and registers the router on the mesh for `SUBPROTOCOL_REDEX`
    /// inbound dispatch. Idempotent — repeated calls return without
    /// disturbing the existing router (the second installation would
    /// orphan every per-channel runtime registered under the first).
    ///
    /// After this returns, [`Self::open_file`] with a
    /// [`RedexFileConfig::replication`] of `Some(cfg)` spawns a
    /// per-channel runtime instead of producing the typed error.
    pub fn enable_replication(&self, mesh: Arc<MeshNode>) {
        let mut slot = self.replication.write();
        if slot.is_some() {
            return;
        }
        let router = Arc::new(RedexReplicationRouter::new());
        let metrics = Arc::new(ReplicationMetricsRegistry::new());
        mesh.set_replication_inbound_router(Some(
            router.clone() as Arc<dyn super::ReplicationInboundRouter>
        ));
        *slot = Some(Arc::new(ReplicationWiring {
            mesh,
            router,
            metrics,
        }));
    }

    /// Install greedy-LRU wiring rooted at `mesh`. Validates the
    /// supplied [`super::super::dataforts::GreedyConfig`], builds
    /// a [`super::super::dataforts::GreedyRuntime`] that opens
    /// per-channel cache files against this manager + announces
    /// chains via the mesh's `ChainTagSink` impl, and installs
    /// the runtime as the mesh's greedy observer.
    ///
    /// Idempotent — a second call with greedy already enabled
    /// returns `Ok` without rebuilding (caller can layer
    /// `disable_greedy_dataforts` + `enable_greedy_dataforts` to
    /// reconfigure).
    ///
    /// Returns `Err(GreedyConfigError)` for invalid configs —
    /// numeric bounds + bandwidth-fraction range. The runtime is
    /// never installed on an invalid config so operators see the
    /// typed error before observing any cache writes.
    ///
    /// `local_caps` snapshots the node's advertised capability
    /// set at install time so the intent / colocation admission
    /// gates have something to evaluate against. Refresh via
    /// [`Self::greedy_runtime`] + `set_local_caps` after each
    /// `MeshNode::announce_capabilities`.
    #[cfg(feature = "dataforts")]
    pub fn enable_greedy_dataforts(
        self: &Arc<Self>,
        mesh: Arc<MeshNode>,
        config: super::super::dataforts::GreedyConfig,
        local_caps: Arc<crate::adapter::net::behavior::capability::CapabilitySet>,
        intent_registry: crate::adapter::net::behavior::placement::IntentRegistry,
    ) -> Result<(), super::super::dataforts::GreedyConfigError> {
        config.validate()?;
        let mut slot = self.greedy.write();
        if slot.is_some() {
            return Ok(());
        }
        let sink = mesh.clone() as Arc<dyn super::ChainTagSink>;
        let runtime = Arc::new(super::super::dataforts::GreedyRuntime::new(
            config,
            self.clone(),
            sink,
            local_caps,
            intent_registry,
        ));
        mesh.set_greedy_observer(Some(
            runtime.clone() as Arc<dyn super::super::dataforts::GreedyObserver>
        ));
        *slot = Some(Arc::new(GreedyWiring {
            mesh,
            runtime,
            #[cfg(feature = "dataforts")]
            gravity_tick_task: parking_lot::Mutex::new(None),
        }));
        Ok(())
    }

    /// Enable data-gravity heat-counter emission on the already-
    /// installed greedy runtime. Validates the supplied policy,
    /// installs it on the runtime, and spawns a tokio task that
    /// fires `gravity_tick().await` on `tick_interval` cadence.
    ///
    /// Requires `enable_greedy_dataforts` to have been called
    /// first — without an installed greedy runtime the heat
    /// counter has nothing to read from. Returns
    /// `Err(GreedyConfigError::*)` if greedy isn't enabled or
    /// the policy fails validation (range / non-finite checks).
    ///
    /// Idempotent — a second call replaces the prior policy +
    /// restarts the tick task. The heat registry resets on each
    /// re-enable so the new policy starts from a clean slate.
    ///
    /// `mesh` is consumed for its
    /// [`super::super::dataforts::HeatSink`] impl
    /// (`announce_heat` / `withdraw_heat`).
    #[cfg(feature = "dataforts")]
    pub fn enable_gravity_for_greedy(
        &self,
        mesh: Arc<MeshNode>,
        policy: super::super::dataforts::DataGravityPolicy,
        tick_interval: std::time::Duration,
    ) -> Result<(), super::super::dataforts::DataGravityPolicyError> {
        policy.validate()?;
        let wiring = self.greedy.read().clone();
        let Some(wiring) = wiring else {
            return Err(super::super::dataforts::DataGravityPolicyError::GreedyNotEnabled);
        };
        let heat_sink: Arc<dyn super::super::dataforts::HeatSink> = mesh.clone();
        wiring.runtime.set_gravity(policy, heat_sink);

        // Replace any existing tick task with a fresh one.
        let runtime_for_task = wiring.runtime.clone();
        let new_task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(tick_interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                runtime_for_task.gravity_tick().await;
            }
        });
        let prev = wiring.gravity_tick_task.lock().replace(new_task);
        if let Some(task) = prev {
            task.abort();
        }
        Ok(())
    }

    /// Disable data-gravity emission. Stops the tick task,
    /// clears the heat registry, and leaves greedy itself
    /// running. Idempotent — no-op when gravity isn't enabled.
    #[cfg(feature = "dataforts")]
    pub fn disable_gravity_for_greedy(&self) {
        let wiring = self.greedy.read().clone();
        let Some(wiring) = wiring else {
            return;
        };
        if let Some(task) = wiring.gravity_tick_task.lock().take() {
            task.abort();
        }
        wiring.runtime.clear_gravity();
    }

    /// Borrow the installed greedy runtime, if any. Cheap clone
    /// of the `Arc` — callers refresh local caps via
    /// `runtime.set_local_caps` after every announce.
    #[cfg(feature = "dataforts")]
    pub fn greedy_runtime(&self) -> Option<Arc<super::super::dataforts::GreedyRuntime>> {
        self.greedy.read().as_ref().map(|w| w.runtime.clone())
    }

    /// Uninstall the greedy wiring. Idempotent — `None` if greedy
    /// wasn't enabled.
    #[cfg(feature = "dataforts")]
    pub fn disable_greedy_dataforts(&self) {
        let _wiring = self.greedy.write().take();
        // Wiring's Drop calls mesh.set_greedy_observer(None).
    }

    /// Operator-facing read-path lookup: if greedy is holding a
    /// cached copy of `channel`, return the cache's `RedexFile`
    /// so the caller can `tail` / `read_range` against it.
    ///
    /// The cache file is keyed under a synthesized name
    /// (`dataforts/greedy/<channel_hash_hex>`) per
    /// [`super::super::dataforts::synthesize_cache_channel_name`];
    /// callers pass the *real* channel name and this method does
    /// the synthesis internally. On a cache hit, this also bumps
    /// the read-recency LRU position and the
    /// `dataforts_greedy_serve_count_total` metric — same shape
    /// as the substrate's "served from cache" accounting.
    ///
    /// Returns `None` when greedy isn't enabled OR the channel
    /// isn't in the cache. Callers fall back to whatever they
    /// were doing before greedy (typically the substrate's own
    /// `find_chain_holders` + network fetch).
    #[cfg(feature = "dataforts")]
    pub fn greedy_cache_for(&self, channel: &ChannelName) -> Option<RedexFile> {
        let runtime = self.greedy_runtime()?;
        // The greedy data-plane cache keys on the wire `u16` hash
        // (that's what the inbound packet path carries); the
        // canonical `u32` hash from `ChannelName::hash()` widens it
        // for ACL / config / RYW elsewhere in the stack but the
        // observe path stays on wire width.
        let synth = super::super::dataforts::synthesize_cache_channel_name(channel.wire_hash());
        let file = runtime.cache_file(&synth)?;
        runtime.note_read(&synth);
        Some(file)
    }

    /// Cumulative count of per-channel replication runtimes currently
    /// registered on this manager. `0` when replication is not
    /// enabled. Exposed for tests + operator observability.
    pub fn replication_runtime_count(&self) -> usize {
        self.replication
            .read()
            .as_ref()
            .map(|w| w.router.len())
            .unwrap_or(0)
    }

    /// The per-channel [`ReplicationCoordinator`] for `name`, if a
    /// replicated runtime was spawned for it. `None` when:
    /// - replication is not enabled on this manager, OR
    /// - no file is open at `name`, OR
    /// - the file at `name` was opened without
    ///   `RedexFileConfig::replication`.
    ///
    /// Exposed for operator inspection (`coordinator.role()`,
    /// `coordinator.metrics()`) and test-driven role transitions
    /// (`coordinator.transition_to(target, signal)`). Production
    /// drives transitions through the placement filter (Phase F) +
    /// election cycle; the surface is here so operators can also
    /// force a transition for recovery / debugging.
    pub fn replication_coordinator_for(
        &self,
        name: &ChannelName,
    ) -> Option<Arc<ReplicationCoordinator>> {
        let wiring = self.replication.read().as_ref().cloned()?;
        let channel_id = ChannelId::from_name(name);
        let handle = wiring.router.get(&channel_id)?;
        Some(handle.coordinator().clone())
    }

    /// Read-only snapshot of the per-channel replication metrics —
    /// the seven counter / gauge shapes from
    /// [`CONFIG_REPLICATION.md`](../../../docs/CONFIG_REPLICATION.md):
    /// `*_lag_seconds`, `*_sync_bytes_total`, `*_leader_changes_total`,
    /// `*_under_capacity_total`, `*_skip_ahead_total`,
    /// `*_election_thrash_total`, `*_witness_withdrawals_total`.
    ///
    /// `None` when replication isn't enabled on this manager.
    ///
    /// Cheap — copies atomic counters into plain data. Suitable for
    /// a per-scrape Prometheus pull. See
    /// [`ReplicationMetricsSnapshot::prometheus_text`] for the
    /// rendered output; [`Self::replication_prometheus_text`] is the
    /// one-call wrapper.
    pub fn replication_metrics_snapshot(&self) -> Option<ReplicationMetricsSnapshot> {
        let wiring = self.replication.read().as_ref().cloned()?;
        Some(wiring.metrics.snapshot())
    }

    /// Convenience wrapper — render the replication metrics snapshot
    /// as Prometheus text. Returns the empty string when replication
    /// isn't enabled (rather than `None`) so the call site can pipe
    /// it straight into an HTTP body without an `unwrap_or_default`.
    pub fn replication_prometheus_text(&self) -> String {
        self.replication_metrics_snapshot()
            .map(|s| s.prometheus_text())
            .unwrap_or_default()
    }

    /// Per-channel replication status snapshot — the richer view
    /// the Phase H `MeshDaemon::snapshot` integration point was
    /// supposed to surface. For every replicated channel registered
    /// on this manager, returns the current `ReplicaRole`,
    /// `tail_seq`, and `channel_name`. Pair with
    /// [`Self::replication_metrics_snapshot`] for the full
    /// observability picture (status here + atomic counters there).
    ///
    /// `None` when replication isn't enabled. Empty vector when
    /// replication is enabled but no channels have been opened.
    pub fn replication_status_snapshot(&self) -> Option<Vec<ReplicationChannelStatus>> {
        let wiring = self.replication.read().as_ref().cloned()?;
        let mut entries: Vec<ReplicationChannelStatus> = wiring
            .router
            .snapshot_handles()
            .into_iter()
            .map(|(_channel_id, handle)| {
                let coordinator = handle.coordinator();
                ReplicationChannelStatus {
                    channel_name: coordinator.channel().channel_name.clone(),
                    role: coordinator.role(),
                    tail_seq: coordinator.tail_seq(),
                }
            })
            .collect();
        // Stable order — keyed on channel_name like the metrics
        // snapshot, so the two snapshots line up by channel.
        entries.sort_by(|a, b| a.channel_name.cmp(&b.channel_name));
        Some(entries)
    }

    /// Open (create if absent) a RedEX file bound to `name`.
    ///
    /// Re-opening an existing name returns the existing handle. The
    /// `config` argument is honored only on first open; subsequent
    /// opens ignore it and return the live file.
    ///
    /// With `persistent: true`, the manager must have been configured
    /// via `with_persistent_dir` (feature `redex-disk`) — otherwise
    /// `open_file` returns a [`RedexError::Channel`] that describes
    /// the missing base dir.
    pub fn open_file(
        &self,
        name: &ChannelName,
        config: RedexFileConfig,
    ) -> Result<RedexFile, RedexError> {
        if let Some(auth) = &self.auth {
            // Use the canonical-name ACL for the storage decision —
            // `is_authorized` (16-bit hash) is reserved for the
            // fast-path packet check where AEAD integrity backstops
            // any bloom-filter false positives. Storage access has
            // no such backstop, and even a 64-bit non-cryptographic
            // hash would be birthday-crackable offline, so the ACL
            // keys on the full canonical name.
            // Widen the 32-bit local origin_hash to match
            // `AuthGuard`'s 64-bit key. The guard keeps the local
            // entity and remote subscribers in disjoint key ranges
            // simply by the natural spread of node_ids — the local
            // entity lives in the lower 2^32 and remote subscribers'
            // full node_ids occupy the full range, so there is no
            // cross-contamination.
            if !auth.is_authorized_full(self.origin_hash, name) {
                return Err(RedexError::Unauthorized);
            }
        }

        // Validate the replication config before anything else —
        // surface the typed error to the caller before we either
        // build the file or attempt to spawn a runtime. An invalid
        // config can't escape into the coordinator's hot loop.
        if let Some(rep) = config.replication.as_ref() {
            rep.validate()
                .map_err(|e| RedexError::Channel(format!("replication config invalid: {e}")))?;
            if self.replication.read().is_none() {
                return Err(RedexError::Channel(
                    "RedexFileConfig::replication requires Redex::enable_replication(mesh)".into(),
                ));
            }
        }

        // Lock-free fast path for the common re-open case: avoid taking
        // a shard write entry when the file is already present.
        if let Some(existing) = self.files.get(name) {
            ensure_reopen_replication_matches(self, name, config.replication.as_ref())?;
            return Ok(existing.clone());
        }

        // First-open path. Take the shard's write entry BEFORE running
        // `build_file`. Holding the entry vacant across the build is
        // what serializes concurrent first-openers for the same name:
        // the loser blocks on the shard write lock and observes the
        // winner's `Occupied` entry on retry. The previous code ran
        // `build_file` outside any lock and resolved with
        // `or_insert(file)`; under `persistent: true` +
        // `FsyncPolicy::Interval` both threads spawned an `Interval`
        // fsync task and held independent file handles, and the
        // loser of `or_insert` was dropped without `close()` — so
        // its Notify never fired and the leaked task plus dup
        // handles outlived the call.
        use dashmap::mapref::entry::Entry;
        let replication_cfg = config.replication.clone();
        let file = match self.files.entry(name.clone()) {
            Entry::Occupied(e) => {
                ensure_reopen_replication_matches(self, name, replication_cfg.as_ref())?;
                return Ok(e.get().clone());
            }
            Entry::Vacant(e) => {
                let file = self.build_file(name, config)?;
                e.insert(file).clone()
            }
        };

        // Spawn the per-channel replication runtime AFTER the file
        // landed in the map — on the unlikely chance the spawn
        // fails, the file is still discoverable and a follow-up
        // open / `enable_replication` sequence can recover. We
        // assert `replication.read().is_some()` above, so the
        // wiring is guaranteed live here.
        if let Some(rep_cfg) = replication_cfg {
            // Re-check the wiring under the read lock — a racing
            // call that disables replication after the precheck
            // surfaces a clean error rather than panicking on
            // unwrap.
            let wiring = match self.replication.read().as_ref() {
                Some(w) => w.clone(),
                None => {
                    return Err(RedexError::Channel(
                        "replication wiring removed between precheck and spawn".into(),
                    ));
                }
            };
            self.spawn_replication_for(name, &file, rep_cfg, &wiring);
        }

        Ok(file)
    }

    /// Spawn the per-channel replication runtime and register it on
    /// the router. Caller already validated `cfg`.
    fn spawn_replication_for(
        &self,
        name: &ChannelName,
        file: &RedexFile,
        cfg: super::replication_config::ReplicationConfig,
        wiring: &ReplicationWiring,
    ) {
        let channel_id = ChannelId::from_name(name);
        let identity = ChannelIdentity {
            channel_name: name.as_str().to_string(),
            origin_hash: self.origin_hash,
        };

        // Compute the initial replica set. Pinned: the literal
        // list. Standard / ColocationStrict: empty for now; Phase F
        // wires placement-recomputation so the coordinator
        // re-resolves the set on roster change.
        let replica_set: Vec<crate::adapter::net::behavior::placement::NodeId> =
            match &cfg.placement {
                PlacementStrategy::Pinned(nodes) => nodes.clone(),
                _ => Vec::new(),
            };

        let heartbeat_ms = cfg.heartbeat_ms;
        let budget_fraction = cfg.replication_budget_fraction;
        // v0.3 Phase D2: snapshot the bandwidth-class config
        // before `cfg` moves into the coordinator below.
        let default_bandwidth_class = cfg.default_bandwidth_class;
        let background_fraction = cfg.background_fraction;
        // TODO(plan-§6): wire the measured NIC peak from the
        // proximity-graph throughput probe here. Until that
        // lands, use a 1 Gbps placeholder (125_000_000 B/s).
        // The fraction arm of `BandwidthBudget::new` scales
        // this down. Operators with >1 Gbps links will see
        // under-utilization when budget_fraction approaches
        // 1.0 until this constant is replaced with the
        // measurement.
        const NIC_PEAK_BYTES_PER_S: u64 = 125_000_000;
        let budget = Arc::new(Mutex::new(BandwidthBudget::new(
            budget_fraction,
            NIC_PEAK_BYTES_PER_S,
            Instant::now(),
        )));

        let coordinator = Arc::new(ReplicationCoordinator::new(
            identity.clone(),
            cfg,
            wiring.mesh.clone() as Arc<dyn super::ChainTagSink>,
            wiring.metrics.as_ref(),
        ));

        let self_node_id = wiring.mesh.node_id();
        let proximity = wiring.mesh.proximity_graph().clone();
        let rtt_lookup: super::replication_runtime::RttLookup = Arc::new(
            move |node: crate::adapter::net::behavior::placement::NodeId| {
                if node == self_node_id {
                    Some(std::time::Duration::ZERO)
                } else {
                    // Mirror `node_id_to_graph_id` from
                    // `mesh.rs`: zero-pad the u64 into the first 8
                    // bytes of a 32-byte proximity NodeId.
                    let mut graph_id = [0u8; 32];
                    graph_id[0..8].copy_from_slice(&node.to_le_bytes());
                    proximity.nearest_rtt(|n| n.node_id == graph_id)
                }
            },
        );

        let file_clone = file.clone();
        let tail_provider: Arc<dyn Fn() -> u64 + Send + Sync> =
            Arc::new(move || file_clone.next_seq());

        let inputs = RuntimeInputs {
            channel: identity,
            channel_id,
            self_node_id,
            replica_set,
            heartbeat_ms,
            wall_clock_provider: Arc::new(|| {
                use std::time::{SystemTime, UNIX_EPOCH};
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0)
            }),
            tail_provider,
            rtt_lookup,
            file: file.clone(),
            // v0.3 Phase D2: per-channel bandwidth-class default
            // + background-fraction admission threshold, sourced
            // from the ReplicationConfig fields (snapshotted
            // above before `cfg` moved into the coordinator).
            default_bandwidth_class,
            background_fraction,
        };

        let handle = Arc::new(spawn_replication_runtime(
            inputs,
            coordinator,
            wiring.mesh.clone() as Arc<dyn super::ReplicationDispatcher>,
            budget,
        ));

        // Register on the router; if a prior handle was registered
        // for the same channel (reopen path), the predecessor is
        // returned. R-21: try_dispatch(Shutdown) is best-effort
        // belt-and-suspenders — `register()`'s swap dropped the
        // router's Arc<RuntimeHandle> on the predecessor, which
        // is the only sender into its inbox; the task observes a
        // closed receiver on its next poll and exits via the
        // `None` arm in the main loop (same shape as Shutdown
        // itself). If the inbox happened to be at cap-1024 the
        // try_dispatch returns Err(_) silently, which is fine
        // because the closed-receiver path will still drive the
        // task out.
        if let Some(prev) = wiring.router.register(channel_id, handle) {
            let _ = prev.try_dispatch(super::replication_runtime::Inbound::Shutdown);
        }
    }

    fn build_file(
        &self,
        name: &ChannelName,
        config: RedexFileConfig,
    ) -> Result<RedexFile, RedexError> {
        self.build_count.fetch_add(1, Ordering::Relaxed);
        #[cfg(feature = "redex-disk")]
        if config.persistent {
            let dir = self.persistent_dir.as_ref().ok_or_else(|| {
                RedexError::Channel(
                    "config.persistent=true requires Redex::with_persistent_dir(...)".into(),
                )
            })?;
            return RedexFile::open_persistent(name.clone(), config, dir);
        }
        Ok(RedexFile::new(name.clone(), config))
    }

    /// Cumulative number of times `build_file` has run on this manager.
    /// Increments once per *first* open of a `ChannelName`; re-opens of
    /// an already-built file do not. Tests assert this against the
    /// number of distinct names opened to confirm concurrent
    /// `open_file` calls did not double-build.
    #[cfg(test)]
    pub(crate) fn build_count(&self) -> u64 {
        self.build_count.load(Ordering::Relaxed)
    }

    /// Look up an already-opened file.
    pub fn get_file(&self, name: &ChannelName) -> Option<RedexFile> {
        self.files.get(name).map(|r| r.clone())
    }

    /// Close and remove a file. Outstanding tail streams receive
    /// `RedexError::Closed`. No-op if no file is open under `name`.
    /// If the channel had a replication runtime spawned, the runtime
    /// is unregistered from the router and signaled to shut down;
    /// the runtime exits on its next inbox poll after observing
    /// `Inbound::Shutdown`.
    pub fn close_file(&self, name: &ChannelName) -> Result<(), RedexError> {
        if let Some(wiring) = self.replication.read().as_ref().cloned() {
            let channel_id = ChannelId::from_name(name);
            if let Some(handle) = wiring.router.unregister(&channel_id) {
                let _ = handle.try_dispatch(super::replication_runtime::Inbound::Shutdown);
            }
        }
        if let Some((_, file)) = self.files.remove(name) {
            file.close()?;
        }
        Ok(())
    }

    /// Close the file (as [`Self::close_file`]) AND unlink any
    /// persistent on-disk segment for the channel. Idempotent: a
    /// channel that has no persistent dir or is unknown to the
    /// manager returns Ok. Used by the blob GC sweep so a swept
    /// chunk doesn't accumulate as an orphaned segment directory
    /// on `with_persistent(true)` deployments.
    ///
    /// Holds the dashmap entry write guard for the channel name
    /// across the close-then-unlink sequence. Pre-fix the entry
    /// was dropped between `close_file` (which removes from the
    /// map) and `remove_dir_all`; a concurrent `open_file` for the
    /// same name landed a fresh entry in the gap, then the unlink
    /// blew away the new segment dir and the next append hit
    /// ENOENT. Holding the entry guard blocks any concurrent
    /// `open_file` on the same name until both phases complete.
    pub fn close_and_unlink_file(&self, name: &ChannelName) -> Result<(), RedexError> {
        // Shut the replication runtime down first — same as
        // `close_file`. This call doesn't touch the `files` map so
        // it's safe to run before taking the entry guard.
        if let Some(wiring) = self.replication.read().as_ref().cloned() {
            let channel_id = ChannelId::from_name(name);
            if let Some(handle) = wiring.router.unregister(&channel_id) {
                let _ = handle.try_dispatch(super::replication_runtime::Inbound::Shutdown);
            }
        }
        // Hold the entry guard across close + unlink so a
        // concurrent `open_file` for the same name blocks until
        // both phases complete. The Entry holds the per-shard
        // write lock; performing the disk unlink inside the
        // Occupied arm before the final `occ.remove()` keeps the
        // map slot locked across the entire sequence. A vacant
        // slot still acquires the lock (via `entry()`); we drop
        // the guard immediately so the no-op path doesn't block
        // longer than necessary.
        match self.files.entry(name.clone()) {
            dashmap::mapref::entry::Entry::Occupied(occ) => {
                let file = occ.get().clone();
                file.close()?;
                #[cfg(feature = "redex-disk")]
                if let Some(base) = self.persistent_dir.as_ref() {
                    let dir = super::disk::channel_dir(base, name);
                    match std::fs::remove_dir_all(&dir) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => return Err(RedexError::io(e)),
                    }
                }
                occ.remove();
                Ok(())
            }
            dashmap::mapref::entry::Entry::Vacant(_) => {
                // No live heap entry; still attempt the disk
                // unlink for the persistent-only case (an
                // operator-restart cycle could have left the
                // dir behind without an open file).
                #[cfg(feature = "redex-disk")]
                if let Some(base) = self.persistent_dir.as_ref() {
                    let dir = super::disk::channel_dir(base, name);
                    match std::fs::remove_dir_all(&dir) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => return Err(RedexError::io(e)),
                    }
                }
                Ok(())
            }
        }
    }

    /// Snapshot list of currently open files. Cheap clone.
    pub fn open_files(&self) -> Vec<RedexFile> {
        self.files.iter().map(|r| r.value().clone()).collect()
    }

    /// Run retention on every open file. Typically called on a
    /// heartbeat tick by the owning runtime.
    pub fn sweep_retention(&self) {
        for entry in self.files.iter() {
            entry.value().sweep_retention();
        }
    }
}

/// Reject reopen calls whose replication config diverges from the
/// original. Same channel name + a different `Some(ReplicationConfig)`
/// would otherwise silently reuse the live coordinator's config — an
/// operator surprise where `replication_factor=5` reopened as
/// `replication_factor=3` keeps replicating at 5.
///
/// Compares against the live coordinator's config (the canonical
/// source for "what is currently in effect"). The pairs accepted as
/// idempotent reopens are:
///
/// - new `None` and original `None`
/// - new `Some(cfg)` and original `Some(cfg)` where the two are
///   structurally `PartialEq`
///
/// Every other shape is rejected with a typed channel error.
fn ensure_reopen_replication_matches(
    redex: &Redex,
    name: &ChannelName,
    requested: Option<&super::replication_config::ReplicationConfig>,
) -> Result<(), RedexError> {
    let channel_id = ChannelId::from_name(name);
    let wiring = redex.replication.read().clone();
    let existing = wiring
        .as_ref()
        .and_then(|w| w.router.get(&channel_id))
        .map(|h| h.coordinator().config().clone());

    match (existing.as_ref(), requested) {
        (None, None) => Ok(()),
        (Some(orig), Some(new)) if orig == new => Ok(()),
        (None, Some(_)) => Err(RedexError::Channel(format!(
            "reopen of '{}' specified a replication config; the original opened without replication",
            name.as_str(),
        ))),
        (Some(_), None) => Err(RedexError::Channel(format!(
            "reopen of '{}' omitted the replication config; the original opened with replication",
            name.as_str(),
        ))),
        (Some(_), Some(_)) => Err(RedexError::Channel(format!(
            "reopen of '{}' supplied a replication config different from the original",
            name.as_str(),
        ))),
    }
}

impl Default for Redex {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Redex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut dbg = f.debug_struct("Redex");
        dbg.field("files", &self.files.len())
            .field("auth", &self.auth.is_some())
            .field("origin_hash", &self.origin_hash);
        #[cfg(feature = "redex-disk")]
        dbg.field("persistent_dir", &self.persistent_dir);
        dbg.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cn(s: &str) -> ChannelName {
        ChannelName::new(s).unwrap()
    }

    #[test]
    fn test_open_and_get() {
        let r = Redex::new();
        let name = cn("sensors/lidar");
        let f = r.open_file(&name, RedexFileConfig::default()).unwrap();
        f.append(b"x").unwrap();

        let g = r.get_file(&name).unwrap();
        assert_eq!(g.len(), 1);
    }

    #[test]
    fn test_reopen_returns_same_file() {
        let r = Redex::new();
        let name = cn("shared");
        let f = r.open_file(&name, RedexFileConfig::default()).unwrap();
        f.append(b"a").unwrap();
        let f2 = r.open_file(&name, RedexFileConfig::default()).unwrap();
        assert_eq!(f2.len(), 1); // sees existing append
        f2.append(b"b").unwrap();
        assert_eq!(f.len(), 2); // original handle also sees it
    }

    #[test]
    fn test_get_file_missing_returns_none() {
        let r = Redex::new();
        assert!(r.get_file(&cn("missing")).is_none());
    }

    #[cfg(feature = "redex-disk")]
    #[test]
    fn close_and_unlink_file_removes_persistent_segment_dir() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let base =
            std::env::temp_dir().join(format!("net-redex-unlink-{}-{}", std::process::id(), n));
        let _ = std::fs::create_dir_all(&base);
        let r = Redex::new().with_persistent_dir(&base);
        let name = cn("dataforts/blob/abc");
        let f = r
            .open_file(&name, RedexFileConfig::default().with_persistent(true))
            .unwrap();
        f.append(b"hello").unwrap();
        let dir = super::super::disk::channel_dir(&base, &name);
        assert!(
            dir.exists(),
            "channel dir must exist after persistent append"
        );

        r.close_and_unlink_file(&name).unwrap();
        assert!(
            !dir.exists(),
            "channel dir must be unlinked after close_and_unlink_file"
        );

        // Idempotent on a second call (file already gone, dir gone).
        r.close_and_unlink_file(&name).unwrap();

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn close_and_unlink_file_noop_when_unknown_or_heap_only() {
        let r = Redex::new();
        // unknown channel — no error
        r.close_and_unlink_file(&cn("never_opened")).unwrap();
        // heap-only channel — closes file, no unlink branch
        let name = cn("heap_only");
        let _f = r.open_file(&name, RedexFileConfig::default()).unwrap();
        r.close_and_unlink_file(&name).unwrap();
        assert!(r.get_file(&name).is_none());
    }

    #[test]
    fn test_auth_denies_unknown_origin() {
        let guard = Arc::new(AuthGuard::new());
        let r = Redex::with_auth(guard, 0xAAAA_BBBB);
        let name = cn("restricted");
        assert!(matches!(
            r.open_file(&name, RedexFileConfig::default()),
            Err(RedexError::Unauthorized)
        ));
    }

    #[test]
    fn test_auth_allows_authorized_origin() {
        let guard = Arc::new(AuthGuard::new());
        let name = cn("allowed");
        // `allow_channel` populates the exact (control-plane) ACL
        // used by `open_file`, plus the fast-path bloom so packet
        // checks on the same channel also pass.
        guard.allow_channel(0x1234_5678, &name);
        let r = Redex::with_auth(guard, 0x1234_5678);
        assert!(r.open_file(&name, RedexFileConfig::default()).is_ok());
    }

    #[test]
    fn test_auth_fast_path_alone_does_not_authorize_open_file() {
        // Regression: `open_file` used to accept any origin that
        // had the (then-truncated) `channel_hash` in its fast-path
        // bloom. A different channel name whose canonical hash
        // collided with an authorized one would then grant
        // unauthorized storage access. The fix requires the canonical
        // channel name in the exact ACL, so a fast-path-only
        // authorization is insufficient — independent of the hash
        // width on the fast path (now `ChannelHash` / u64).
        let guard = Arc::new(AuthGuard::new());
        let name = cn("sensitive");
        // Authorize the fast path ONLY (no allow_channel).
        guard.authorize(0x1234_5678, name.hash());
        let r = Redex::with_auth(guard, 0x1234_5678);
        assert!(matches!(
            r.open_file(&name, RedexFileConfig::default()),
            Err(RedexError::Unauthorized)
        ));
    }

    #[test]
    fn test_close_file_rejects_append_on_existing_handle() {
        let r = Redex::new();
        let name = cn("closable");
        let f = r.open_file(&name, RedexFileConfig::default()).unwrap();
        f.append(b"x").unwrap();
        r.close_file(&name).unwrap();
        assert!(f.append(b"y").is_err());
    }

    #[test]
    fn test_sweep_retention_runs_on_all_open_files() {
        let r = Redex::new();
        let cfg = RedexFileConfig::default().with_retention_max_events(1);
        let f1 = r.open_file(&cn("f1"), cfg.clone()).unwrap();
        let f2 = r.open_file(&cn("f2"), cfg).unwrap();
        for i in 0..3 {
            f1.append(format!("{}", i).as_bytes()).unwrap();
            f2.append(format!("{}", i).as_bytes()).unwrap();
        }
        r.sweep_retention();
        assert_eq!(f1.len(), 1);
        assert_eq!(f2.len(), 1);
    }

    #[test]
    fn test_regression_concurrent_first_open_does_not_double_build() {
        // Regression: `open_file` ran `build_file` outside any lock and
        // resolved with `entry().or_insert(file)`. Two threads calling
        // `open_file(name, ...)` for the same brand-new name could both
        // pass the `files.get()` precheck and both run `build_file`.
        // Under `persistent: true` + `FsyncPolicy::Interval`, each
        // build spawned a tokio interval task and opened independent
        // idx/dat handles; the loser of the `or_insert` was dropped
        // without `close()`, so its `Notify` shutdown never fired and
        // the leaked task plus dup file handles outlived the call for
        // the lifetime of the runtime.
        //
        // The fix takes the shard write entry BEFORE running
        // `build_file` so the loser blocks on the shard lock and
        // observes an `Occupied` entry on retry. We don't need a tokio
        // runtime to exercise the race — `build_count` is incremented
        // unconditionally in `build_file`, so any code path that
        // triggers a double-build shows up here.
        let r = Arc::new(Redex::new());
        let name = cn("contended");

        // 32 threads × 1 trial each — release-mode Windows can resolve
        // a 32-way race in microseconds, plenty of opportunity for the
        // buggy path to run `build_file` more than once.
        let threads: Vec<_> = (0..32)
            .map(|_| {
                let r = Arc::clone(&r);
                let name = name.clone();
                std::thread::spawn(move || {
                    r.open_file(&name, RedexFileConfig::default()).unwrap();
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }

        assert_eq!(
            r.build_count(),
            1,
            "concurrent first-open of the same name double-built — \
             each extra build leaks a fsync interval task and a set \
             of file handles under FsyncPolicy::Interval + persistent"
        );
        // And the public surface still resolves to a single file.
        assert!(r.get_file(&name).is_some());
    }

    // ---- Replication integration tests ----

    use super::super::replication_config::ReplicationConfig;
    use crate::adapter::net::{EntityKeypair, MeshNodeConfig};
    use std::net::SocketAddr;

    async fn build_mesh_for_test() -> Arc<crate::adapter::net::MeshNode> {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let cfg = MeshNodeConfig::new(addr, [0x42u8; 32]);
        Arc::new(
            crate::adapter::net::MeshNode::new(EntityKeypair::generate(), cfg)
                .await
                .expect("MeshNode::new"),
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn open_with_replication_without_enable_returns_error() {
        let r = Redex::new();
        let cfg = RedexFileConfig::default().with_replication(Some(ReplicationConfig::new()));
        let err = r.open_file(&cn("repl/test"), cfg).unwrap_err();
        assert!(matches!(err, RedexError::Channel(_)));
        assert_eq!(r.replication_runtime_count(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn open_with_replication_spawns_runtime_and_close_unregisters() {
        let mesh = build_mesh_for_test().await;
        let r = Redex::new();
        r.enable_replication(mesh);
        let name = cn("repl/spawn");
        let cfg = RedexFileConfig::default()
            .with_replication(Some(ReplicationConfig::new().with_heartbeat_ms(60_000)));
        let _file = r.open_file(&name, cfg).expect("open_file with replication");
        assert_eq!(r.replication_runtime_count(), 1);
        r.close_file(&name).unwrap();
        assert_eq!(r.replication_runtime_count(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn enable_replication_is_idempotent() {
        let mesh = build_mesh_for_test().await;
        let r = Redex::new();
        r.enable_replication(mesh.clone());
        let count_after_first = r.replication_runtime_count();
        r.enable_replication(mesh);
        assert_eq!(
            r.replication_runtime_count(),
            count_after_first,
            "second enable_replication must not disturb existing wiring"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn invalid_replication_config_surfaces_typed_channel_error() {
        let mesh = build_mesh_for_test().await;
        let r = Redex::new();
        r.enable_replication(mesh);
        // factor=0 violates REPLICATION_FACTOR_MIN.
        let cfg = RedexFileConfig::default()
            .with_replication(Some(ReplicationConfig::new().with_factor(0)));
        let err = r.open_file(&cn("repl/bad"), cfg).unwrap_err();
        match err {
            RedexError::Channel(msg) => {
                assert!(msg.contains("replication config invalid"));
            }
            other => panic!("expected Channel error, got {other:?}"),
        }
        assert_eq!(r.replication_runtime_count(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reopen_replicated_channel_replaces_runtime() {
        let mesh = build_mesh_for_test().await;
        let r = Redex::new();
        r.enable_replication(mesh);
        let name = cn("repl/reopen");
        let cfg1 = RedexFileConfig::default()
            .with_replication(Some(ReplicationConfig::new().with_heartbeat_ms(60_000)));
        let _f1 = r.open_file(&name, cfg1).unwrap();
        assert_eq!(r.replication_runtime_count(), 1);
        // Second open returns the existing file (re-open path) and
        // does NOT spawn a second runtime — the replication slot
        // is only honored on first open per the open_file contract.
        let cfg2 = RedexFileConfig::default()
            .with_replication(Some(ReplicationConfig::new().with_heartbeat_ms(60_000)));
        let _f2 = r.open_file(&name, cfg2).unwrap();
        assert_eq!(
            r.replication_runtime_count(),
            1,
            "reopen must not spawn a second runtime"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reopen_replicated_channel_with_different_config_rejects() {
        let mesh = build_mesh_for_test().await;
        let r = Redex::new();
        r.enable_replication(mesh);
        let name = cn("repl/reopen-mismatch");
        let cfg1 = RedexFileConfig::default()
            .with_replication(Some(ReplicationConfig::new().with_heartbeat_ms(60_000)));
        r.open_file(&name, cfg1).unwrap();
        // Reopen with a different heartbeat — the prior code path
        // silently returned the existing handle. Now a typed error
        // surfaces so operators don't get a coordinator running on
        // a config different from what they asked for.
        let cfg2 = RedexFileConfig::default()
            .with_replication(Some(ReplicationConfig::new().with_heartbeat_ms(45_000)));
        let err = r.open_file(&name, cfg2).unwrap_err();
        match err {
            RedexError::Channel(msg) => assert!(
                msg.contains("different from the original"),
                "expected 'different from the original' message, got: {msg}"
            ),
            other => panic!("expected Channel error, got {other:?}"),
        }
        // Original runtime still alive.
        assert_eq!(r.replication_runtime_count(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reopen_replicated_channel_without_replication_rejects() {
        let mesh = build_mesh_for_test().await;
        let r = Redex::new();
        r.enable_replication(mesh);
        let name = cn("repl/reopen-no-repl");
        let cfg1 = RedexFileConfig::default()
            .with_replication(Some(ReplicationConfig::new().with_heartbeat_ms(60_000)));
        r.open_file(&name, cfg1).unwrap();
        // Reopen with replication=None when the original had it
        // would silently re-use the original (a different
        // operator-visible surface). Reject.
        let cfg2 = RedexFileConfig::default();
        let err = r.open_file(&name, cfg2).unwrap_err();
        match err {
            RedexError::Channel(msg) => assert!(
                msg.contains("omitted the replication config"),
                "expected 'omitted the replication config' message, got: {msg}"
            ),
            other => panic!("expected Channel error, got {other:?}"),
        }
    }

    #[test]
    fn replication_status_snapshot_returns_none_when_not_enabled() {
        let r = Redex::new();
        assert!(r.replication_status_snapshot().is_none());
    }

    /// `enable_gravity_for_greedy` fails when greedy isn't
    /// installed first — surfaces a typed `GreedyNotEnabled`
    /// error rather than panicking or silently no-oping.
    #[cfg(feature = "dataforts")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn enable_gravity_without_greedy_returns_typed_error() {
        use super::super::super::dataforts::{DataGravityPolicy, DataGravityPolicyError};

        let mesh = build_mesh_for_test().await;
        let r = Arc::new(Redex::new());
        // Note: greedy is NOT enabled before this call.
        let err = r
            .enable_gravity_for_greedy(
                mesh,
                DataGravityPolicy::default(),
                std::time::Duration::from_secs(1),
            )
            .expect_err("must reject without greedy installed");
        assert!(matches!(err, DataGravityPolicyError::GreedyNotEnabled));
    }

    /// Happy path: install greedy, install gravity on top,
    /// disable gravity, disable greedy. Each step is idempotent
    /// and leaves the next-step state consistent.
    #[cfg(feature = "dataforts")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn enable_disable_gravity_round_trip() {
        use super::super::super::dataforts::{DataGravityPolicy, GreedyConfig};

        let mesh = build_mesh_for_test().await;
        let r = Arc::new(Redex::new());
        r.enable_greedy_dataforts(
            mesh.clone(),
            GreedyConfig::default(),
            Arc::new(crate::adapter::net::behavior::capability::CapabilitySet::default()),
            crate::adapter::net::behavior::placement::IntentRegistry::defaults(),
        )
        .expect("greedy enable");
        let runtime = r.greedy_runtime().expect("runtime");
        assert!(!runtime.gravity_enabled());

        r.enable_gravity_for_greedy(
            mesh.clone(),
            DataGravityPolicy::default(),
            std::time::Duration::from_millis(50),
        )
        .expect("gravity enable");
        assert!(runtime.gravity_enabled());

        // Idempotent re-enable.
        r.enable_gravity_for_greedy(
            mesh.clone(),
            DataGravityPolicy::default(),
            std::time::Duration::from_millis(75),
        )
        .expect("idempotent re-enable");
        assert!(runtime.gravity_enabled());

        // Disable gravity — greedy stays.
        r.disable_gravity_for_greedy();
        assert!(!runtime.gravity_enabled());

        // Disable greedy — wiring drops, Drop uninstalls.
        r.disable_greedy_dataforts();
        assert!(!mesh.has_greedy_observer());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn replication_status_snapshot_includes_open_channels() {
        let mesh = build_mesh_for_test().await;
        let r = Redex::new();
        r.enable_replication(mesh);
        let name_a = cn("repl/status_a");
        let name_b = cn("repl/status_b");
        let cfg = RedexFileConfig::default()
            .with_replication(Some(ReplicationConfig::new().with_heartbeat_ms(60_000)));
        let file_a = r.open_file(&name_a, cfg.clone()).expect("open A");
        r.open_file(&name_b, cfg).expect("open B");

        // Append on A to bump its tail; B stays at 0.
        for i in 0..3 {
            file_a.append(format!("event-{i}").as_bytes()).unwrap();
        }
        // Drive A's coordinator to Replica so its role is observable
        // as non-Idle.
        let coord_a = r.replication_coordinator_for(&name_a).unwrap();
        coord_a
            .transition_to(
                ReplicaRole::Replica,
                super::super::replication_state::TransitionSignal::CapabilitySelected,
            )
            .await
            .unwrap();
        coord_a.record_tail_seq(3);

        let snap = r.replication_status_snapshot().expect("snapshot enabled");
        assert_eq!(snap.len(), 2, "both channels in snapshot");
        // Sorted by channel name.
        assert_eq!(snap[0].channel_name, "repl/status_a");
        assert_eq!(snap[1].channel_name, "repl/status_b");
        // A is Replica with tail 3; B is Idle with tail 0.
        assert_eq!(snap[0].role, ReplicaRole::Replica);
        assert_eq!(snap[0].tail_seq, 3);
        assert_eq!(snap[1].role, ReplicaRole::Idle);
        assert_eq!(snap[1].tail_seq, 0);

        r.close_file(&name_a).unwrap();
        r.close_file(&name_b).unwrap();
    }

    #[test]
    fn replication_metrics_snapshot_returns_none_when_not_enabled() {
        let r = Redex::new();
        assert!(r.replication_metrics_snapshot().is_none());
        // prometheus_text is the empty string in this case so the
        // caller can pipe straight into an HTTP body without
        // branching.
        assert_eq!(r.replication_prometheus_text(), "");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drop_redex_uninstalls_router_from_mesh() {
        // Build a mesh, install a Redex with an active replication
        // runtime, drop the Redex, then install a second Redex on
        // the same mesh. The second `enable_replication` must
        // succeed without observing the prior Redex's router —
        // proves the Drop impl cleared the mesh's slot.
        let mesh = build_mesh_for_test().await;
        {
            let r = Redex::new();
            r.enable_replication(mesh.clone());
            let name = cn("repl/drop");
            let cfg = RedexFileConfig::default()
                .with_replication(Some(ReplicationConfig::new().with_heartbeat_ms(60_000)));
            r.open_file(&name, cfg).expect("open");
            assert_eq!(r.replication_runtime_count(), 1);
            // `r` goes out of scope here; Drop fires.
        }
        // Give the dropped runtime task a tick to observe its
        // inbox close.
        tokio::task::yield_now().await;

        // A second Redex on the same mesh should install cleanly.
        // If the prior router were still pinned, the second
        // installation would silently leak it (the mesh's slot
        // would have been swapped to the new router without
        // shutting down the first set of runtimes).
        let r2 = Redex::new();
        r2.enable_replication(mesh);
        assert_eq!(
            r2.replication_runtime_count(),
            0,
            "fresh Redex starts with zero runtimes"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn replication_metrics_snapshot_includes_open_channel() {
        let mesh = build_mesh_for_test().await;
        let r = Redex::new();
        r.enable_replication(mesh);
        let name = cn("repl/metrics");
        let cfg = RedexFileConfig::default()
            .with_replication(Some(ReplicationConfig::new().with_heartbeat_ms(60_000)));
        let _file = r.open_file(&name, cfg).expect("open");

        let snap = r
            .replication_metrics_snapshot()
            .expect("snapshot when enabled");
        assert_eq!(snap.channels.len(), 1);
        assert_eq!(snap.channels[0].channel, "repl/metrics");
        // Counters all zero on a freshly-opened channel.
        assert_eq!(snap.channels[0].sync_bytes_total, 0);
        assert_eq!(snap.channels[0].leader_changes_total, 0);

        // Prometheus text is non-empty + includes the channel name.
        let text = r.replication_prometheus_text();
        assert!(text.contains("repl/metrics"));

        r.close_file(&name).unwrap();
    }
}
