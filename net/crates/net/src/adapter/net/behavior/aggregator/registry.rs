//! [`AggregatorRegistry`] — process-level index of live
//! [`LifecycleGroup<AggregatorDaemon>`]s, keyed by operator-
//! chosen name.
//!
//! Direction B / step 3 + step 6 (registry ↔ HealthMonitor
//! integration) of `docs/plans/AGGREGATOR_LIFECYCLE_DEFERRED_2026_05_23.md`.
//! The registry is the substrate primitive operator CLI verbs
//! (`net aggregator spawn / ls / scale`) operate against. A
//! group's entry holds the live [`LifecycleGroup`] directly
//! (behind an `async` mutex) so the optional [`HealthMonitor`]
//! attached at registration time can lock + replace slots
//! without conflicting with concurrent snapshot reads.
//!
//! # Shape
//!
//! - [`AggregatorGroupEntry`] carries the operator-chosen name,
//!   the 32-byte group seed, an `Arc<AsyncMutex<Option<LifecycleGroup<AggregatorDaemon>>>>`
//!   for the live group (`None` once `unregister` has taken
//!   ownership), and an optional `Arc<HealthMonitor<AggregatorDaemon>>`.
//! - Async accessor methods read through the lock: `replica_count`,
//!   `replicas`, `placements`, `health`.
//! - [`AggregatorRegistry::register`] consumes a `LifecycleGroup`;
//!   [`AggregatorRegistry::register_with_monitor`] does the same
//!   plus spawns a `HealthMonitor` against the group and the
//!   caller-supplied factory.
//! - [`AggregatorRegistry::unregister`] is `async` — stops the
//!   monitor (if any), takes the group out, and returns it to
//!   the caller. The caller drives `group.stop().await`.
//!
//! # Threading
//!
//! - Outer map: `parking_lot::RwLock<HashMap<String, Arc<Entry>>>`.
//!   Reads (`get`, `names`, `entries`) are wait-free against
//!   writes.
//! - Per-entry group: `tokio::sync::Mutex` inside an `Arc` so
//!   the entry, the monitor, and the snapshot path can all hold
//!   their own clones.
//! - Per-entry monitor: `parking_lot::Mutex<Option<Arc<...>>>`
//!   for the install/take during register/unregister.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use tokio::sync::Mutex as AsyncMutex;

use super::daemon::AggregatorDaemon;
use crate::adapter::net::behavior::lifecycle::{
    HealthMonitor, LifecycleDaemon, LifecycleGroup, ReplicaHealth,
};
use crate::adapter::net::compute::PlacementDecision;

/// A live aggregator group plus the metadata operator tooling
/// reads. Held under an `Arc` so multiple readers share without
/// blocking writers.
pub struct AggregatorGroupEntry {
    /// Operator-chosen group name (the registry key).
    pub name: String,
    /// 32-byte seed used to derive per-replica `EntityKeypair`s.
    pub group_seed: [u8; 32],
    /// The live group, shared with the optional
    /// [`HealthMonitor`] so both can lock it concurrently. `None`
    /// once `unregister` has taken ownership (transient — the
    /// entry is also removed from the map at that point).
    group: Arc<AsyncMutex<Option<LifecycleGroup<AggregatorDaemon>>>>,
    /// Health monitor attached at register time (or `None` if
    /// the registration path was [`AggregatorRegistry::register`]
    /// rather than `register_with_monitor`).
    monitor: Mutex<Option<Arc<HealthMonitor<AggregatorDaemon>>>>,
}

impl AggregatorGroupEntry {
    /// Number of replicas in the live group, or `0` once the
    /// entry has been unregistered (the group's been taken).
    pub async fn replica_count(&self) -> usize {
        match &*self.group.lock().await {
            Some(g) => g.replica_count(),
            None => 0,
        }
    }

    /// Typed `Arc` to each replica in declaration order. Empty
    /// after the group has been taken via `unregister`.
    pub async fn replicas(&self) -> Vec<Arc<AggregatorDaemon>> {
        match &*self.group.lock().await {
            Some(g) => g.replicas(),
            None => Vec::new(),
        }
    }

    /// Per-replica placement decisions in declaration order.
    /// Empty when the group was created via the placement-free
    /// [`LifecycleGroup::spawn`], or after `unregister`.
    pub async fn placements(&self) -> Vec<PlacementDecision> {
        match &*self.group.lock().await {
            Some(g) => g.placements().to_vec(),
            None => Vec::new(),
        }
    }

    /// Per-replica health snapshot in declaration order. Empty
    /// after `unregister`.
    pub async fn health(&self) -> Vec<ReplicaHealth> {
        match &*self.group.lock().await {
            Some(g) => g.health().await,
            None => Vec::new(),
        }
    }

    /// Borrow the attached [`HealthMonitor`] if one was wired
    /// at registration time via `register_with_monitor`.
    pub fn monitor(&self) -> Option<Arc<HealthMonitor<AggregatorDaemon>>> {
        self.monitor.lock().clone()
    }

    /// Single lock-once snapshot of the entry's full per-replica
    /// state. Used by [`super::snapshot_group`] (the RPC
    /// `List` path) and `DeckClient::aggregator_registry_snapshot`
    /// — both previously took **three sequential** lock
    /// acquisitions (`replicas` / `placements` / `health`) per
    /// group per snapshot. This collapses to one lock + an
    /// outside-the-guard `join_all` for the per-replica
    /// `health()` futures, so a slow daemon's `health()` no
    /// longer blocks concurrent `register`/`unregister` writers.
    ///
    /// Returns an empty snapshot when the group has been taken
    /// via `unregister`.
    pub async fn snapshot(&self) -> EntrySnapshot {
        let (replicas, placements) = {
            let guard = self.group.lock().await;
            match guard.as_ref() {
                Some(g) => (g.replicas(), g.placements().to_vec()),
                None => return EntrySnapshot::default(),
            }
        };
        // Lock is dropped here. Run per-replica health checks
        // concurrently, with no other guard held.
        let healths =
            futures::future::join_all(replicas.iter().map(|r| async move { r.health().await }))
                .await;
        EntrySnapshot {
            replicas,
            placements,
            healths,
        }
    }
}

/// Lock-once snapshot of an [`AggregatorGroupEntry`]'s
/// per-replica state. Built by
/// [`AggregatorGroupEntry::snapshot`]; consumed by the RPC
/// handler + the deck snapshot accessor.
#[derive(Default)]
pub struct EntrySnapshot {
    /// Typed handles to each replica in declaration order.
    pub replicas: Vec<Arc<AggregatorDaemon>>,
    /// Per-replica placement decisions in declaration order.
    /// Empty when the group was created via the placement-free
    /// [`LifecycleGroup::spawn`].
    pub placements: Vec<PlacementDecision>,
    /// Per-replica health snapshot in declaration order.
    /// Indexed parallel to `replicas`.
    pub healths: Vec<ReplicaHealth>,
}

/// Process-level registry of live aggregator groups.
pub struct AggregatorRegistry {
    groups: RwLock<HashMap<String, Arc<AggregatorGroupEntry>>>,
}

/// Errors from registry operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AggregatorRegistryError {
    /// A group with this name is already registered.
    DuplicateName(String),
    /// No group registered under this name.
    NotFound(String),
    /// `scale_group` rejected for a registry-level reason —
    /// invalid target count, lifecycle helper refused, etc.
    /// Carries an operator-facing diagnostic.
    ScaleFailed(String),
}

impl std::fmt::Display for AggregatorRegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateName(n) => write!(f, "aggregator group already registered: {n}"),
            Self::NotFound(n) => write!(f, "aggregator group not found: {n}"),
            Self::ScaleFailed(d) => write!(f, "aggregator group scale failed: {d}"),
        }
    }
}

impl std::error::Error for AggregatorRegistryError {}

impl AggregatorRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self {
            groups: RwLock::new(HashMap::new()),
        }
    }

    /// Register a live group. The registry takes ownership of
    /// the `LifecycleGroup` — callers that need shared access
    /// route through the returned `Arc<AggregatorGroupEntry>`.
    ///
    /// Returns `DuplicateName` if a group with the same name
    /// already exists.
    pub fn register(
        &self,
        name: impl Into<String>,
        group: LifecycleGroup<AggregatorDaemon>,
    ) -> Result<Arc<AggregatorGroupEntry>, AggregatorRegistryError> {
        let name = name.into();
        let group_seed = *group.group_seed();
        let mut groups = self.groups.write();
        if groups.contains_key(&name) {
            return Err(AggregatorRegistryError::DuplicateName(name));
        }
        let entry = Arc::new(AggregatorGroupEntry {
            name: name.clone(),
            group_seed,
            group: Arc::new(AsyncMutex::new(Some(group))),
            monitor: Mutex::new(None),
        });
        groups.insert(name, entry.clone());
        Ok(entry)
    }

    /// Register a live group **and** spawn a [`HealthMonitor`]
    /// against it. The monitor polls each replica's `health()`
    /// every `monitor_interval` and replaces unhealthy slots
    /// via `factory`.
    ///
    /// The monitor's `factory` is invoked with the failing
    /// replica's index so it can rebuild an identical
    /// replacement (the group's `replica_keypair(index)`
    /// provides the deterministic identity).
    ///
    /// Returns `DuplicateName` if a group with the same name
    /// already exists.
    pub fn register_with_monitor<F>(
        &self,
        name: impl Into<String>,
        group: LifecycleGroup<AggregatorDaemon>,
        factory: F,
        monitor_interval: std::time::Duration,
    ) -> Result<Arc<AggregatorGroupEntry>, AggregatorRegistryError>
    where
        F: FnMut(u8) -> Arc<AggregatorDaemon> + Send + 'static,
    {
        let name = name.into();
        let group_seed = *group.group_seed();
        let mut groups = self.groups.write();
        if groups.contains_key(&name) {
            return Err(AggregatorRegistryError::DuplicateName(name));
        }
        // Park the group in its async mutex, then spawn the
        // monitor against a helper-extracted Arc<Mutex<LifecycleGroup>>.
        // The monitor only sees the LifecycleGroup-flavor; the
        // entry's `Option` wrapper lets `unregister` take it
        // later without invalidating the monitor's reference
        // (the monitor sees `None` after take and bails).
        let group_arc = Arc::new(AsyncMutex::new(Some(group)));
        // The monitor wants `Arc<AsyncMutex<LifecycleGroup<L>>>`,
        // not the `Option`-wrapped version. Wrap the access
        // layer so the monitor's locks see whatever the entry
        // currently holds.
        let monitor = Arc::new(HealthMonitor::spawn(
            group_arc.clone(),
            factory,
            monitor_interval,
        ));
        let entry = Arc::new(AggregatorGroupEntry {
            name: name.clone(),
            group_seed,
            group: group_arc,
            monitor: Mutex::new(Some(monitor)),
        });
        groups.insert(name, entry.clone());
        Ok(entry)
    }

    /// Look up a group by name. Returns `None` if absent.
    pub fn get(&self, name: &str) -> Option<Arc<AggregatorGroupEntry>> {
        self.groups.read().get(name).cloned()
    }

    /// All registered group names, sorted lexicographically for
    /// deterministic CLI output. Cheap O(n log n) snapshot — the
    /// registry isn't on the hot path.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.groups.read().keys().cloned().collect();
        names.sort();
        names
    }

    /// Snapshot of every entry, sorted by name. Used by
    /// `net aggregator ls` to render the live group table.
    pub fn entries(&self) -> Vec<Arc<AggregatorGroupEntry>> {
        let mut entries: Vec<Arc<AggregatorGroupEntry>> =
            self.groups.read().values().cloned().collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        entries
    }

    /// Number of live groups in the registry.
    pub fn len(&self) -> usize {
        self.groups.read().len()
    }

    /// True when no groups are registered.
    pub fn is_empty(&self) -> bool {
        self.groups.read().is_empty()
    }

    /// Resize an existing group in place to `target_replica_count`
    /// replicas. Calls [`LifecycleGroup::add_replica`] (grow) or
    /// [`LifecycleGroup::remove_last`] (shrink) the appropriate
    /// number of times against the registered group's lock,
    /// preserving the identity + generation of the replicas that
    /// survive the resize.
    ///
    /// `factory` is invoked once per added replica (delta times
    /// for grow, zero times for shrink) with the new replica's
    /// index. The daemon supplies a factory that re-resolves the
    /// template + derives the appropriate keypair via
    /// `derive_replica_keypair(group_seed, index)`.
    ///
    /// Returns:
    /// - `NotFound` when no group is registered under `name`.
    /// - `ScaleFailed` when target_replica_count == 0, or when
    ///   the lifecycle helpers refuse (e.g., add_replica fails
    ///   on_start, remove_last hit the floor mid-shrink).
    /// - `Ok(entry)` on success. The entry is the same one that
    ///   would have come from `get(name)` — held across the
    ///   resize so callers can snapshot immediately after.
    pub async fn scale_group<F>(
        &self,
        name: &str,
        target_replica_count: u8,
        mut factory: F,
    ) -> Result<Arc<AggregatorGroupEntry>, AggregatorRegistryError>
    where
        F: FnMut(u8) -> Arc<AggregatorDaemon> + Send,
    {
        if target_replica_count == 0 {
            return Err(AggregatorRegistryError::ScaleFailed(
                "target_replica_count must be > 0".into(),
            ));
        }
        let entry = self
            .get(name)
            .ok_or_else(|| AggregatorRegistryError::NotFound(name.to_string()))?;
        let mut group_guard = entry.group.lock().await;
        let group = group_guard
            .as_mut()
            .ok_or_else(|| AggregatorRegistryError::NotFound(name.to_string()))?;
        let current = group.replica_count();
        let target = target_replica_count as usize;
        if target > current {
            // Grow via the bulk path so on_start handlers run
            // concurrently. Sequential per-replica `add_replica`
            // calls would hold the entry mutex through N on_start
            // awaits and block List / health / HealthMonitor for
            // a 1→N grow — by funnelling through
            // `LifecycleGroup::add_replicas`, the parallel
            // on_start pattern from `start_replicas` is reused.
            let delta = (target - current) as u8;
            group
                .add_replicas(delta, &mut factory)
                .await
                .map_err(|e| AggregatorRegistryError::ScaleFailed(format!("add_replicas: {e}")))?;
        } else if target < current {
            // Shrink: remove_last is genuinely sequential — each
            // pop awaits the previous handle's stop() so the
            // parallel-Vec invariant in LifecycleGroup holds.
            // Refuses to drop below 1 — guarded above by the
            // `target == 0` check, so the LifecycleGroupError
            // shouldn't fire, but we surface it cleanly if the
            // invariant breaks.
            for _ in 0..(current - target) {
                group.remove_last().await.map_err(|e| {
                    AggregatorRegistryError::ScaleFailed(format!("remove_last: {e}"))
                })?;
            }
        }
        // No-op when target == current — operator gets the
        // existing snapshot back.
        drop(group_guard);
        Ok(entry)
    }

    /// Remove a group and return the underlying `LifecycleGroup`
    /// for caller-driven shutdown. The attached `HealthMonitor`
    /// (if any) is stopped first.
    ///
    /// Returns `NotFound` if no group is registered under the
    /// name. Calling `unregister` twice with the same name
    /// returns `NotFound` on the second call (the entry was
    /// removed from the map).
    pub async fn unregister(
        &self,
        name: &str,
    ) -> Result<LifecycleGroup<AggregatorDaemon>, AggregatorRegistryError> {
        let entry = {
            let mut groups = self.groups.write();
            groups
                .remove(name)
                .ok_or_else(|| AggregatorRegistryError::NotFound(name.to_string()))?
        };
        // Stop the monitor first so it doesn't try to lock the
        // group while we're taking it out.
        let monitor = entry.monitor.lock().take();
        if let Some(m) = monitor {
            m.stop().await;
        }
        // Now take the group. The entry's Arc may outlive this
        // call if a caller held it; subsequent accessor reads
        // see the empty Option and return empty Vecs.
        let group = entry
            .group
            .lock()
            .await
            .take()
            .ok_or_else(|| AggregatorRegistryError::NotFound(name.to_string()))?;
        Ok(group)
    }
}

impl Default for AggregatorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::aggregator::{AggregatorConfig, AggregatorDaemon};
    use crate::adapter::net::behavior::fold::capability::CapabilityFold;
    use crate::adapter::net::behavior::fold::FoldKind;
    use crate::adapter::net::behavior::lifecycle::LifecycleGroup;
    use crate::adapter::net::identity::EntityKeypair;
    use crate::adapter::net::{MeshNode, MeshNodeConfig, SubnetId};
    use std::net::SocketAddr;
    use std::time::Duration;

    async fn build_mesh() -> Arc<MeshNode> {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let cfg = MeshNodeConfig::new(addr, [0x17u8; 32]);
        Arc::new(
            MeshNode::new(EntityKeypair::generate(), cfg)
                .await
                .expect("MeshNode::new"),
        )
    }

    async fn spawn_group_2(interval_ms: u64) -> LifecycleGroup<AggregatorDaemon> {
        let mesh = build_mesh().await;
        let cfg = AggregatorConfig::new(SubnetId::GLOBAL)
            .with_fold_kind(CapabilityFold::KIND_ID)
            .with_interval(Duration::from_millis(interval_ms));
        let cfg_clone = cfg.clone();
        let mesh_clone = mesh.clone();
        LifecycleGroup::<AggregatorDaemon>::spawn(2, [0xCDu8; 32], move |_idx| {
            Arc::new(AggregatorDaemon::new(cfg_clone.clone(), mesh_clone.clone()).expect("new"))
        })
        .await
        .expect("spawn group")
    }

    #[tokio::test]
    async fn register_then_accessors_reflect_live_group_state() {
        let group = spawn_group_2(50).await;
        let registry = AggregatorRegistry::new();
        let entry = registry.register("a", group).expect("register");
        assert_eq!(entry.name, "a");
        assert_eq!(entry.replica_count().await, 2);
        assert_eq!(entry.replicas().await.len(), 2);
        assert!(entry.placements().await.is_empty());
        let health = entry.health().await;
        assert_eq!(health.len(), 2);
        // Drain via unregister.
        let g = registry.unregister("a").await.expect("unregister");
        g.stop().await;
    }

    #[tokio::test]
    async fn register_rejects_duplicate_names() {
        let registry = AggregatorRegistry::new();
        registry
            .register("dup", spawn_group_2(50).await)
            .expect("first register");
        match registry.register("dup", spawn_group_2(50).await) {
            Err(AggregatorRegistryError::DuplicateName(n)) => assert_eq!(n, "dup"),
            Err(other) => panic!("expected DuplicateName, got {other:?}"),
            Ok(_) => panic!("expected DuplicateName, got Ok"),
        }
        // Cleanup.
        let g = registry.unregister("dup").await.expect("unregister");
        g.stop().await;
    }

    #[tokio::test]
    async fn unregister_returns_group_and_removes_entry() {
        let registry = AggregatorRegistry::new();
        registry
            .register("a", spawn_group_2(50).await)
            .expect("register");
        assert_eq!(registry.len(), 1);
        let group = registry.unregister("a").await.expect("unregister");
        assert_eq!(group.replica_count(), 2);
        group.stop().await;
        assert_eq!(registry.len(), 0);
        match registry.unregister("a").await {
            Err(AggregatorRegistryError::NotFound(n)) => assert_eq!(n, "a"),
            Err(other) => panic!("expected NotFound, got {other:?}"),
            Ok(_) => panic!("expected NotFound, got Ok"),
        }
    }

    #[tokio::test]
    async fn entries_are_sorted_by_name_for_deterministic_cli_output() {
        let registry = AggregatorRegistry::new();
        registry
            .register("zulu", spawn_group_2(50).await)
            .expect("register zulu");
        registry
            .register("alpha", spawn_group_2(50).await)
            .expect("register alpha");
        registry
            .register("mike", spawn_group_2(50).await)
            .expect("register mike");
        let names: Vec<String> = registry.entries().iter().map(|e| e.name.clone()).collect();
        assert_eq!(names, vec!["alpha", "mike", "zulu"]);
        // Cleanup.
        for n in ["alpha", "mike", "zulu"] {
            let g = registry.unregister(n).await.expect("unregister");
            g.stop().await;
        }
    }

    #[tokio::test]
    async fn register_with_monitor_stops_monitor_on_unregister() {
        // Wire a monitor; on unregister it must be stopped.
        // The factory is called from the monitor task — for
        // this test no replicas go unhealthy, so factory isn't
        // invoked.
        let registry = AggregatorRegistry::new();
        let mesh = build_mesh().await;
        // Short interval so the on_stop backstop (interval + 1s)
        // doesn't dominate the test wallclock.
        let cfg = AggregatorConfig::new(SubnetId::GLOBAL)
            .with_fold_kind(CapabilityFold::KIND_ID)
            .with_interval(Duration::from_millis(50));
        let factory_cfg = cfg.clone();
        let factory_mesh = mesh.clone();
        let group = LifecycleGroup::<AggregatorDaemon>::spawn(2, [0u8; 32], {
            let cfg = cfg.clone();
            let mesh = mesh.clone();
            move |_idx| Arc::new(AggregatorDaemon::new(cfg.clone(), mesh.clone()).expect("new"))
        })
        .await
        .expect("spawn");

        let entry = registry
            .register_with_monitor(
                "monitored",
                group,
                move |_idx| {
                    Arc::new(
                        AggregatorDaemon::new(factory_cfg.clone(), factory_mesh.clone())
                            .expect("new"),
                    )
                },
                Duration::from_millis(20),
            )
            .expect("register_with_monitor");

        // Monitor must be attached.
        assert!(entry.monitor().is_some());

        // Wait a few monitor ticks to ensure the loop is alive.
        tokio::time::sleep(Duration::from_millis(80)).await;
        let monitor_handle = entry.monitor().expect("monitor");
        let ticks_before_stop = monitor_handle
            .stats()
            .ticks
            .load(std::sync::atomic::Ordering::Acquire);
        assert!(
            ticks_before_stop >= 1,
            "monitor should have ticked at least once"
        );

        // Unregister stops the monitor + returns the group.
        let group = registry.unregister("monitored").await.expect("unregister");
        // Sleep past several intervals; the monitor's ticks
        // counter must not advance further.
        tokio::time::sleep(Duration::from_millis(80)).await;
        let ticks_after_stop = monitor_handle
            .stats()
            .ticks
            .load(std::sync::atomic::Ordering::Acquire);
        assert_eq!(
            ticks_before_stop, ticks_after_stop,
            "monitor must stop ticking after unregister"
        );
        group.stop().await;
    }
}
