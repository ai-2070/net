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
use super::replication::ChannelId;
use super::replication_budget::BandwidthBudget;
use super::replication_config::PlacementStrategy;
use super::replication_coordinator::{ChannelIdentity, ReplicationCoordinator};
use super::replication_metrics::ReplicationMetricsRegistry;
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
struct ReplicationWiring {
    mesh: Arc<MeshNode>,
    router: Arc<RedexReplicationRouter>,
    metrics: Arc<ReplicationMetricsRegistry>,
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
            rep.validate().map_err(|e| {
                RedexError::Channel(format!("replication config invalid: {e}"))
            })?;
            if self.replication.read().is_none() {
                return Err(RedexError::Channel(
                    "RedexFileConfig::replication requires Redex::enable_replication(mesh)"
                        .into(),
                ));
            }
        }

        // Lock-free fast path for the common re-open case: avoid taking
        // a shard write entry when the file is already present.
        if let Some(existing) = self.files.get(name) {
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
            Entry::Occupied(e) => return Ok(e.get().clone()),
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
        // NIC peak estimate — plan §6 wires the measured peak from
        // the proximity-graph throughput probe. Until that lands,
        // use a 1 Gbps placeholder (125_000_000 B/s). The fraction
        // arm of `BandwidthBudget::new` scales this down.
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
        let rtt_lookup: super::replication_runtime::RttLookup =
            Arc::new(move |node: crate::adapter::net::behavior::placement::NodeId| {
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
            });

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
        };

        let handle = Arc::new(spawn_replication_runtime(
            inputs,
            coordinator,
            wiring.mesh.clone() as Arc<dyn super::ReplicationDispatcher>,
            budget,
        ));

        // Register on the router; if a prior handle was registered
        // for the same channel (reopen path), the predecessor is
        // returned — `try_dispatch(Shutdown)` triggers a graceful
        // exit on its next inbox poll.
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
                let _ =
                    handle.try_dispatch(super::replication_runtime::Inbound::Shutdown);
            }
        }
        if let Some((_, file)) = self.files.remove(name) {
            file.close()?;
        }
        Ok(())
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
        // had the 16-bit `channel_hash` in its fast-path bloom. A
        // different channel name whose 16-bit hash collided with an
        // authorized one would then grant unauthorized storage
        // access. The fix requires the canonical channel name in
        // the exact ACL, so a fast-path-only authorization is
        // insufficient.
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
        let cfg = RedexFileConfig::default()
            .with_replication(Some(ReplicationConfig::new()));
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
        let cfg = RedexFileConfig::default().with_replication(Some(
            ReplicationConfig::new().with_heartbeat_ms(60_000),
        ));
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
        let cfg = RedexFileConfig::default().with_replication(Some(
            ReplicationConfig::new().with_factor(0),
        ));
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
        let cfg1 = RedexFileConfig::default().with_replication(Some(
            ReplicationConfig::new().with_heartbeat_ms(60_000),
        ));
        let _f1 = r.open_file(&name, cfg1).unwrap();
        assert_eq!(r.replication_runtime_count(), 1);
        // Second open returns the existing file (re-open path) and
        // does NOT spawn a second runtime — the replication slot
        // is only honored on first open per the open_file contract.
        let cfg2 = RedexFileConfig::default().with_replication(Some(
            ReplicationConfig::new().with_heartbeat_ms(60_000),
        ));
        let _f2 = r.open_file(&name, cfg2).unwrap();
        assert_eq!(
            r.replication_runtime_count(),
            1,
            "reopen must not spawn a second runtime"
        );
    }
}
