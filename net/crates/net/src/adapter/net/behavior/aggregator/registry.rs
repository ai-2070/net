//! [`AggregatorRegistry`] — process-level index of live
//! [`LifecycleGroup<AggregatorDaemon>`]s, keyed by operator-
//! chosen name.
//!
//! Direction B / step 3 of
//! `docs/plans/AGGREGATOR_LIFECYCLE_DEFERRED_2026_05_23.md`.
//! The registry is the substrate primitive operator CLI verbs
//! (`net aggregator spawn / ls / scale`) operate against. A
//! group's entry carries:
//!
//! - Operator-chosen `name` (the lookup key).
//! - 32-byte `group_seed` for deterministic replica identity.
//! - Typed `Arc<AggregatorDaemon>` per replica (no downcast).
//! - Recorded `PlacementDecision`s when the group was created
//!   via [`LifecycleGroup::spawn_with_placement`] — empty
//!   otherwise.
//! - Internal handles parked under a mutex so `unregister` can
//!   take ownership for shutdown without conflicting with
//!   shared-ref reads.
//!
//! # Threading
//!
//! The registry uses [`parking_lot::RwLock`] for the outer
//! map + a [`parking_lot::Mutex`] inside each entry to guard
//! handle take/replace. Reads (`get`, `names`) are wait-free
//! against writes; writes (`register`, `unregister`) serialize.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};

use super::daemon::AggregatorDaemon;
use crate::adapter::net::behavior::lifecycle::LifecycleHandle;
use crate::adapter::net::compute::PlacementDecision;

/// A live aggregator group plus the metadata operator tooling
/// reads. Held under an `Arc` so multiple readers share without
/// blocking writers.
pub struct AggregatorGroupEntry {
    /// Operator-chosen group name (the registry key).
    pub name: String,
    /// 32-byte seed used to derive per-replica `EntityKeypair`s.
    pub group_seed: [u8; 32],
    /// Typed handles to each replica in declaration order.
    pub replicas: Vec<Arc<AggregatorDaemon>>,
    /// Per-replica placement decisions from
    /// [`LifecycleGroup::spawn_with_placement`]. Empty when the
    /// group was created via the placement-free
    /// [`LifecycleGroup::spawn`].
    pub placements: Vec<PlacementDecision>,
    /// Lifecycle handles, parked under a `Mutex` so
    /// [`AggregatorRegistry::unregister`] can `take` ownership
    /// and drive `stop()` without racing reads.
    /// `Some` until `unregister` consumes the entry; `None`
    /// afterwards (the entry is also removed from the map at
    /// that point, so this state is transient).
    handles: Mutex<Option<Vec<LifecycleHandle>>>,
}

impl AggregatorGroupEntry {
    /// Number of live replicas at construction.
    pub fn replica_count(&self) -> usize {
        self.replicas.len()
    }
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
}

impl std::fmt::Display for AggregatorRegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateName(n) => write!(f, "aggregator group already registered: {n}"),
            Self::NotFound(n) => write!(f, "aggregator group not found: {n}"),
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

    /// Register a live group. Returns `DuplicateName` if a
    /// group with the same name already exists — callers should
    /// either pick a unique name or `unregister` the prior
    /// group first.
    pub fn register(
        &self,
        name: impl Into<String>,
        group_seed: [u8; 32],
        replicas: Vec<Arc<AggregatorDaemon>>,
        placements: Vec<PlacementDecision>,
        handles: Vec<LifecycleHandle>,
    ) -> Result<Arc<AggregatorGroupEntry>, AggregatorRegistryError> {
        let name = name.into();
        let mut groups = self.groups.write();
        if groups.contains_key(&name) {
            return Err(AggregatorRegistryError::DuplicateName(name));
        }
        let entry = Arc::new(AggregatorGroupEntry {
            name: name.clone(),
            group_seed,
            replicas,
            placements,
            handles: Mutex::new(Some(handles)),
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

    /// Remove a group and return its lifecycle handles so the
    /// caller can stop them. The Arc<AggregatorGroupEntry>
    /// remains valid as long as someone else holds it (its
    /// replicas / placements survive), but the handles are
    /// taken out of the entry.
    ///
    /// Returns `NotFound` if no group is registered under the
    /// name. Returns `Some` handles on first call; later calls
    /// against the same already-unregistered name short-circuit
    /// to `NotFound` because the entry is removed from the map.
    pub fn unregister(
        &self,
        name: &str,
    ) -> Result<Vec<LifecycleHandle>, AggregatorRegistryError> {
        let entry = {
            let mut groups = self.groups.write();
            groups
                .remove(name)
                .ok_or_else(|| AggregatorRegistryError::NotFound(name.to_string()))?
        };
        // `entry` is now the last Arc inside the map. Take the
        // handles out of the entry's mutex. The Arc itself may
        // still outlive this call if a caller held it — the
        // handles slot is `None` after this.
        let handles = entry
            .handles
            .lock()
            .take()
            .unwrap_or_default();
        Ok(handles)
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

    async fn spawn_group(name: &str) -> (LifecycleGroup<AggregatorDaemon>, Arc<MeshNode>) {
        let mesh = build_mesh().await;
        let cfg = AggregatorConfig::new(SubnetId::GLOBAL)
            .with_fold_kind(CapabilityFold::KIND_ID)
            .with_interval(Duration::from_millis(50));
        let cfg_clone = cfg.clone();
        let mesh_clone = mesh.clone();
        let group = LifecycleGroup::<AggregatorDaemon>::spawn(2, [0xCDu8; 32], move |_idx| {
            Arc::new(
                AggregatorDaemon::new(cfg_clone.clone(), mesh_clone.clone()).expect("new"),
            )
        })
        .await
        .expect("spawn group");
        let _ = name;
        (group, mesh)
    }

    #[tokio::test]
    async fn register_and_get_round_trip_by_name() {
        let (group, _mesh) = spawn_group("a").await;
        let registry = AggregatorRegistry::new();
        // We need to split the group's parts into the registry
        // surface: replicas, placements, handles.
        let replicas = group.replicas();
        let placements = group.placements().to_vec();
        let group_seed = *group.group_seed();
        // Take the handles out by destructuring via `into_parts`?
        // The current API doesn't expose that — call stop() on
        // unregister instead. For now, drop the LifecycleGroup
        // to release its handles, then re-acquire by calling
        // LifecycleGroup::spawn again wouldn't preserve the
        // identity. So we just use the typed `replicas` for
        // metadata-only registry tests; handles is empty.
        drop(group);
        // The registry accepts an empty handles vec — typical
        // for "test this without taking ownership of the
        // group's lifecycle" tests.
        let entry = registry
            .register("agg-a", group_seed, replicas.clone(), placements, Vec::new())
            .expect("register");
        assert_eq!(entry.name, "agg-a");
        assert_eq!(entry.replica_count(), 2);
        let got = registry.get("agg-a").expect("get");
        assert!(Arc::ptr_eq(&entry, &got));
        assert!(registry.get("not-there").is_none());
        assert_eq!(registry.names(), vec!["agg-a"]);
        assert_eq!(registry.len(), 1);
    }

    #[tokio::test]
    async fn register_rejects_duplicate_names() {
        let (group, _mesh) = spawn_group("a").await;
        let registry = AggregatorRegistry::new();
        let seed = *group.group_seed();
        let replicas = group.replicas();
        drop(group);
        registry
            .register("dup", seed, replicas.clone(), Vec::new(), Vec::new())
            .expect("first register");
        match registry.register("dup", seed, replicas, Vec::new(), Vec::new()) {
            Err(AggregatorRegistryError::DuplicateName(n)) => assert_eq!(n, "dup"),
            Err(other) => panic!("expected DuplicateName, got {other:?}"),
            Ok(_) => panic!("expected DuplicateName, got Ok"),
        }
    }

    #[tokio::test]
    async fn unregister_returns_handles_and_removes_entry() {
        let (group, _mesh) = spawn_group("a").await;
        let registry = AggregatorRegistry::new();
        let seed = *group.group_seed();
        let replicas = group.replicas();
        // Pull handles out of the LifecycleGroup. The current
        // group API doesn't expose this directly; for the
        // registry test we re-build via the trait by stopping +
        // re-spawning — but that breaks identity. Instead, just
        // stub the registry with empty handles to test the
        // registry's own bookkeeping; the integration test
        // covers the handle-takeover path.
        drop(group);
        registry
            .register("agg", seed, replicas, Vec::new(), Vec::new())
            .expect("register");
        assert_eq!(registry.len(), 1);
        let handles = registry.unregister("agg").expect("unregister");
        assert!(handles.is_empty());
        assert_eq!(registry.len(), 0);
        match registry.unregister("agg") {
            Err(AggregatorRegistryError::NotFound(n)) => assert_eq!(n, "agg"),
            Err(other) => panic!("expected NotFound, got {other:?}"),
            Ok(_) => panic!("expected NotFound, got Ok"),
        }
    }

    #[tokio::test]
    async fn end_to_end_register_then_unregister_drives_real_handle_shutdown() {
        // Spawn a real LifecycleGroup, surrender its parts via
        // `into_parts`, register them, then unregister and stop
        // the returned handles. The replicas' on_stop must
        // observe the shutdown.
        let mesh = build_mesh().await;
        let cfg = AggregatorConfig::new(SubnetId::GLOBAL)
            .with_fold_kind(CapabilityFold::KIND_ID)
            .with_interval(Duration::from_millis(30));
        let cfg_clone = cfg.clone();
        let mesh_clone = mesh.clone();
        let group = LifecycleGroup::<AggregatorDaemon>::spawn(
            2,
            [0xCDu8; 32],
            move |_idx| {
                Arc::new(
                    AggregatorDaemon::new(cfg_clone.clone(), mesh_clone.clone()).expect("new"),
                )
            },
        )
        .await
        .expect("spawn");
        let (replicas, placements, handles, group_seed) = group.into_parts();
        let registry = AggregatorRegistry::new();
        registry
            .register("e2e", group_seed, replicas.clone(), placements, handles)
            .expect("register");
        // Let the loops tick a few times.
        tokio::time::sleep(Duration::from_millis(80)).await;
        let gen_before_stop: Vec<u64> = replicas.iter().map(|d| d.generation()).collect();
        for g in &gen_before_stop {
            assert!(*g >= 1, "each replica should have ticked");
        }
        // Unregister + stop handles to halt the loops.
        let handles_out = registry.unregister("e2e").expect("unregister");
        for h in handles_out {
            h.stop().await;
        }
        // Capture and wait; ticks must not advance further.
        let after_stop: Vec<u64> = replicas.iter().map(|d| d.generation()).collect();
        tokio::time::sleep(Duration::from_millis(60)).await;
        let after_wait: Vec<u64> = replicas.iter().map(|d| d.generation()).collect();
        assert_eq!(after_stop, after_wait, "loops must halt after unregister/stop");
        assert!(registry.is_empty());
    }

    #[tokio::test]
    async fn entries_are_sorted_by_name_for_deterministic_cli_output() {
        let (group_a, _mesh) = spawn_group("a").await;
        let registry = AggregatorRegistry::new();
        let seed = *group_a.group_seed();
        let replicas = group_a.replicas();
        drop(group_a);
        registry
            .register("zulu", seed, replicas.clone(), Vec::new(), Vec::new())
            .expect("register zulu");
        registry
            .register("alpha", seed, replicas.clone(), Vec::new(), Vec::new())
            .expect("register alpha");
        registry
            .register("mike", seed, replicas, Vec::new(), Vec::new())
            .expect("register mike");
        let names: Vec<String> = registry.entries().iter().map(|e| e.name.clone()).collect();
        assert_eq!(names, vec!["alpha", "mike", "zulu"]);
    }
}
