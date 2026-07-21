//! Replica groups — N interchangeable copies of a daemon managed as a unit.
//!
//! A `ReplicaGroup` coordinates N instances of the same daemon across
//! different nodes. Each replica has a deterministic identity derived from
//! a group seed — the same index always produces the same keypair, making
//! replacement idempotent. The group provides:
//!
//! - Automatic placement spread across failure domains
//! - Load-balanced event routing to the nearest healthy replica
//! - Group-level health (alive as long as >= 1 replica is healthy)
//! - Dynamic scaling (add/remove replicas)
//! - Auto-replacement on node failure (stateless re-spawn)

use std::collections::HashSet;

use crate::adapter::net::behavior::loadbalance::{RequestContext, Strategy};
use crate::adapter::net::behavior::metadata::NodeId;
use crate::adapter::net::behavior::placement::{Artifact, PlacementFilter, TieBreakContext};
use crate::adapter::net::compute::daemon::{DaemonHostConfig, MeshDaemon};
use crate::adapter::net::compute::group_coord::{
    GroupCoordinator, GroupError, GroupHealth, MemberInfo,
};
use crate::adapter::net::compute::host::DaemonHost;
use crate::adapter::net::compute::registry::DaemonRegistry;
use crate::adapter::net::compute::scheduler::Scheduler;
use crate::adapter::net::identity::EntityKeypair;

/// Subprotocol ID for replica group coordination (reserved, not yet registered).
///
/// Intentionally NOT in `SubprotocolRegistry::with_defaults()`. Groups
/// currently operate as local coordinators — member placement uses the
/// daemon's own `CapabilityFilter`, not a group-specific tag. Register
/// this ID when cross-node group coordination is implemented (distributed
/// membership, remote scale_to, coordinated failover).
pub const SUBPROTOCOL_REPLICA_GROUP: u16 = 0x0900;

// ── Configuration ────────────────────────────────────────────────────────────

/// Configuration for a replica group.
#[derive(Debug, Clone)]
pub struct ReplicaGroupConfig {
    /// Desired number of replicas.
    pub replica_count: u8,
    /// 32-byte seed for deterministic keypair derivation.
    pub group_seed: [u8; 32],
    /// Load balancing strategy for routing events to replicas.
    pub lb_strategy: Strategy,
    /// Daemon host configuration for each replica.
    pub host_config: DaemonHostConfig,
}

// ── Keypair derivation ───────────────────────────────────────────────────────

/// Derive a deterministic keypair for a replica from the group seed.
///
/// Uses BLAKE2s-MAC keyed with `"net-replica-v1"` to derive per-replica
/// secret bytes from `group_seed || index`. This is a cryptographic KDF
/// following the same pattern as `EntityId::blake2s_hash()`.
///
/// Each replica index always produces the same keypair, making the group
/// identity deterministic and reproducible.
#[expect(
    clippy::expect_used,
    reason = "Blake2sMac::new_from_slice rejects only keys longer than 32 bytes; b\"net-replica-v1\" is a 16-byte compile-time-constant label"
)]
pub fn derive_replica_keypair(group_seed: &[u8; 32], index: u8) -> EntityKeypair {
    use blake2::{
        digest::{consts::U32, Mac},
        Blake2sMac,
    };

    let mut input = [0u8; 33];
    input[..32].copy_from_slice(group_seed);
    input[32] = index;

    let mut mac = <Blake2sMac<U32> as Mac>::new_from_slice(b"net-replica-v1")
        .expect("BLAKE2s accepts variable-length keys");
    Mac::update(&mut mac, &input);
    let secret: [u8; 32] = mac.finalize().into_bytes().into();

    EntityKeypair::from_bytes(secret)
}

// ── ReplicaGroup ─────────────────────────────────────────────────────────────

/// Manages N interchangeable copies of a daemon as a logical unit.
///
/// Each replica has a deterministic identity derived from `group_seed + index`.
/// The group does not own the `DaemonHost`s — they live in the
/// `DaemonRegistry` as normal entries. The group is a coordination overlay.
pub struct ReplicaGroup {
    /// Unique group identifier (xxh3 of group_seed).
    group_id: u32,
    /// Configuration.
    config: ReplicaGroupConfig,
    /// Shared coordination (LB, members, health).
    coord: GroupCoordinator,
    /// X-1 epoch — bumped on every recovery-driven re-placement
    /// of a replica slot. See `StandbyGroup::term` for the
    /// fencing intent; the cross-node wire integration is a
    /// separate change.
    term: u64,
}

impl ReplicaGroup {
    /// Create a new replica group, place all replicas, and register them.
    pub fn spawn<F>(
        config: ReplicaGroupConfig,
        daemon_factory: F,
        scheduler: &Scheduler,
        registry: &DaemonRegistry,
    ) -> Result<Self, GroupError>
    where
        F: Fn() -> Box<dyn MeshDaemon>,
    {
        if config.replica_count == 0 {
            return Err(GroupError::InvalidConfig(
                "replica_count must be > 0".into(),
            ));
        }

        let group_id = {
            use xxhash_rust::xxh3::xxh3_64;
            xxh3_64(&config.group_seed) as u32
        };

        let mut coord = GroupCoordinator::new(config.lb_strategy);
        let mut used_nodes: HashSet<u64> = HashSet::new();
        let requirements = daemon_factory().requirements();

        for index in 0..config.replica_count {
            let keypair = derive_replica_keypair(&config.group_seed, index);
            let origin_hash = keypair.origin_hash();
            let entity_id_bytes: NodeId = *keypair.entity_id().as_bytes();

            let placement =
                GroupCoordinator::place_with_spread(scheduler, &requirements, &used_nodes)?;
            let node_id = placement.node_id;
            used_nodes.insert(node_id);

            let daemon = daemon_factory();
            let host = DaemonHost::new(daemon, keypair, config.host_config.clone());
            registry.register(host)?;

            coord.add_member(MemberInfo {
                index,
                origin_hash,
                node_id,
                entity_id_bytes,
                healthy: true,
            });
        }

        Ok(Self {
            group_id,
            config,
            coord,
            term: 1,
        })
    }

    /// Route an inbound event to the best available replica.
    pub fn route_event(&self, ctx: &RequestContext) -> Result<u64, GroupError> {
        self.coord.route_event(ctx)
    }

    /// X-1 epoch counter. Bumped on every successful slot
    /// re-placement via `try_recover` after a node failure.
    /// See `StandbyGroup::term` for the fencing rationale.
    pub fn term(&self) -> u64 {
        self.term
    }

    /// Resize the group to `n` replicas.
    pub fn scale_to<F>(
        &mut self,
        n: u8,
        daemon_factory: F,
        scheduler: &Scheduler,
        registry: &DaemonRegistry,
    ) -> Result<(), GroupError>
    where
        F: Fn() -> Box<dyn MeshDaemon>,
    {
        if n == 0 {
            return Err(GroupError::InvalidConfig(
                "replica_count must be > 0".into(),
            ));
        }

        let current = self.coord.member_count();

        if n > current {
            let requirements = daemon_factory().requirements();
            // `used_nodes` must be `mut` and updated inside the loop.
            // Without this insert, `place_with_spread` sees the same
            // exclusion set every iteration and returns the same
            // first non-excluded node — colocating every new replica
            // on a single node, defeating the spread invariant.
            // `fork_group.rs:185-199` already did this correctly;
            // bring this loop into parity.
            let mut used_nodes: HashSet<u64> =
                self.coord.members().iter().map(|m| m.node_id).collect();

            for index in current..n {
                let keypair = derive_replica_keypair(&self.config.group_seed, index);
                let origin_hash = keypair.origin_hash();
                let entity_id_bytes: NodeId = *keypair.entity_id().as_bytes();

                let placement =
                    GroupCoordinator::place_with_spread(scheduler, &requirements, &used_nodes)?;
                used_nodes.insert(placement.node_id);

                let daemon = daemon_factory();
                let host = DaemonHost::new(daemon, keypair, self.config.host_config.clone());
                registry.register(host)?;

                self.coord.add_member(MemberInfo {
                    index,
                    origin_hash,
                    node_id: placement.node_id,
                    entity_id_bytes,
                    healthy: true,
                });
            }
        } else if n < current {
            while self.coord.member_count() > n {
                let Some(info) = self.coord.remove_last() else {
                    debug_assert!(
                        false,
                        "member_count > n but remove_last is None — coord invariant violation",
                    );
                    break;
                };
                let _ = registry.unregister(info.origin_hash);
            }
        }

        self.config.replica_count = n;
        Ok(())
    }

    /// Handle failure of a node hosting one or more replicas.
    ///
    /// Re-derives the same deterministic keypair and re-spawns on a new node.
    pub fn on_node_failure<F>(
        &mut self,
        failed_node_id: u64,
        daemon_factory: F,
        scheduler: &Scheduler,
        registry: &DaemonRegistry,
    ) -> Result<Vec<u8>, GroupError>
    where
        F: Fn() -> Box<dyn MeshDaemon>,
    {
        let mut replaced = Vec::new();
        let requirements = daemon_factory().requirements();
        let mut exclude: HashSet<u64> = HashSet::new();
        exclude.insert(failed_node_id);

        let affected = self.coord.members_on_node(failed_node_id);

        for index in affected {
            self.coord.mark_unhealthy(index);

            #[expect(
                clippy::unwrap_used,
                reason = "index came from coord.members_on_node above; the matching member is guaranteed to exist"
            )]
            let old_origin_hash = self
                .coord
                .members()
                .iter()
                .find(|m| m.index == index)
                .unwrap()
                .origin_hash;

            // Try `place_with_spread` BEFORE touching the registry.
            // On placement failure, the old slot is still registered
            // (under `old_origin_hash`), so recovery / scale_to can
            // make it healthy again later.
            //
            // On placement success we use `registry.replace` —
            // atomic upsert at the deterministic origin_hash. The
            // older `unregister` → `register` two-step had a
            // window where the second step could fail (concurrent
            // race) and leave the slot orphaned; `replace` collapses
            // the swap into a single map operation, so the slot is
            // never empty between callers.
            let _ = old_origin_hash; // retained as a doc anchor for the comment above.
            let keypair = derive_replica_keypair(&self.config.group_seed, index);
            let entity_id_bytes: NodeId = *keypair.entity_id().as_bytes();

            let placement =
                match GroupCoordinator::place_with_spread(scheduler, &requirements, &exclude) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!(
                            index,
                            error = %e,
                            "ReplicaGroup::on_node_failure: place_with_spread failed; \
                             slot left registered for later recovery (#7)"
                        );
                        continue;
                    }
                };

            let daemon = daemon_factory();
            let host = DaemonHost::new(daemon, keypair, self.config.host_config.clone());
            registry.replace(host);

            self.coord
                .update_member_placement(index, placement.node_id, entity_id_bytes);
            exclude.insert(placement.node_id);
            replaced.push(index);
        }

        Ok(replaced)
    }

    /// Phase G slice 5 — `spawn` with score-based placement. Runs
    /// the same flow as [`Self::spawn`] but routes every per-replica
    /// placement decision through [`GroupCoordinator::place_member`]
    /// (i.e. `Scheduler::select_member_node` + LOCKED §7
    /// tie-breaker), so operators can opt into the v2 path on a
    /// per-call basis without changing the legacy default.
    ///
    /// The artifact passed to `placement` is built per-iteration
    /// from the daemon's `required_capabilities()` /
    /// `optional_capabilities()` plus the deterministic per-replica
    /// entity-id (used as `daemon_id` for stable ordering).
    pub fn spawn_with_placement<F>(
        config: ReplicaGroupConfig,
        daemon_factory: F,
        scheduler: &Scheduler,
        registry: &DaemonRegistry,
        placement: &dyn PlacementFilter,
        tie_break: &TieBreakContext<'_>,
    ) -> Result<Self, GroupError>
    where
        F: Fn() -> Box<dyn MeshDaemon>,
    {
        if config.replica_count == 0 {
            return Err(GroupError::InvalidConfig(
                "replica_count must be > 0".into(),
            ));
        }

        let group_id = {
            use xxhash_rust::xxh3::xxh3_64;
            xxh3_64(&config.group_seed) as u32
        };

        let mut coord = GroupCoordinator::new(config.lb_strategy);
        let mut used_nodes: HashSet<u64> = HashSet::new();

        // Capture the daemon's capability surface once — required /
        // optional are stable across the spawn, so we don't need to
        // re-derive them per replica. `requirements()` (legacy
        // `CapabilityFilter`) still narrows the candidate pool inside
        // `place_member`.
        let prototype = daemon_factory();
        let requirements = prototype.requirements();
        let required = prototype.required_capabilities();
        let optional = prototype.optional_capabilities();
        drop(prototype);

        for index in 0..config.replica_count {
            let keypair = derive_replica_keypair(&config.group_seed, index);
            let origin_hash = keypair.origin_hash();
            let entity_id_bytes: NodeId = *keypair.entity_id().as_bytes();

            let artifact = Artifact::Daemon {
                daemon_id: entity_id_bytes,
                required: &required,
                optional: &optional,
            };

            let decision = GroupCoordinator::place_member(
                scheduler,
                &artifact,
                &requirements,
                &used_nodes,
                placement,
                tie_break,
            )?;
            let node_id = decision.node_id;
            used_nodes.insert(node_id);

            let daemon = daemon_factory();
            let host = DaemonHost::new(daemon, keypair, config.host_config.clone());
            registry.register(host)?;

            coord.add_member(MemberInfo {
                index,
                origin_hash,
                node_id,
                entity_id_bytes,
                healthy: true,
            });
        }

        Ok(Self {
            group_id,
            config,
            coord,
            term: 1,
        })
    }

    /// Phase G slice 5 — `scale_to` with score-based placement.
    /// Routes the additive `current..n` placement loop through
    /// [`GroupCoordinator::place_member`]. Scale-down is unchanged
    /// (no placement decision involved).
    pub fn scale_to_with_placement<F>(
        &mut self,
        n: u8,
        daemon_factory: F,
        scheduler: &Scheduler,
        registry: &DaemonRegistry,
        placement: &dyn PlacementFilter,
        tie_break: &TieBreakContext<'_>,
    ) -> Result<(), GroupError>
    where
        F: Fn() -> Box<dyn MeshDaemon>,
    {
        if n == 0 {
            return Err(GroupError::InvalidConfig(
                "replica_count must be > 0".into(),
            ));
        }

        let current = self.coord.member_count();

        if n > current {
            let prototype = daemon_factory();
            let requirements = prototype.requirements();
            let required = prototype.required_capabilities();
            let optional = prototype.optional_capabilities();
            drop(prototype);

            // `used_nodes` must be `mut` and updated inside the loop —
            // see the regression note in [`Self::scale_to`].
            let mut used_nodes: HashSet<u64> =
                self.coord.members().iter().map(|m| m.node_id).collect();

            for index in current..n {
                let keypair = derive_replica_keypair(&self.config.group_seed, index);
                let origin_hash = keypair.origin_hash();
                let entity_id_bytes: NodeId = *keypair.entity_id().as_bytes();

                let artifact = Artifact::Daemon {
                    daemon_id: entity_id_bytes,
                    required: &required,
                    optional: &optional,
                };

                let decision = GroupCoordinator::place_member(
                    scheduler,
                    &artifact,
                    &requirements,
                    &used_nodes,
                    placement,
                    tie_break,
                )?;
                used_nodes.insert(decision.node_id);

                let daemon = daemon_factory();
                let host = DaemonHost::new(daemon, keypair, self.config.host_config.clone());
                registry.register(host)?;

                self.coord.add_member(MemberInfo {
                    index,
                    origin_hash,
                    node_id: decision.node_id,
                    entity_id_bytes,
                    healthy: true,
                });
            }
        } else if n < current {
            while self.coord.member_count() > n {
                let Some(info) = self.coord.remove_last() else {
                    debug_assert!(
                        false,
                        "member_count > n but remove_last is None — coord invariant violation",
                    );
                    break;
                };
                let _ = registry.unregister(info.origin_hash);
            }
        }

        self.config.replica_count = n;
        Ok(())
    }

    /// Phase G slice 5 — `on_node_failure` with score-based
    /// placement. Replaces affected members via
    /// [`GroupCoordinator::place_member`]; on placement failure the
    /// slot is left registered (same recovery-friendly behavior as
    /// [`Self::on_node_failure`]) so a later recovery / scale_to
    /// can re-mark it healthy.
    pub fn on_node_failure_with_placement<F>(
        &mut self,
        failed_node_id: u64,
        daemon_factory: F,
        scheduler: &Scheduler,
        registry: &DaemonRegistry,
        placement: &dyn PlacementFilter,
        tie_break: &TieBreakContext<'_>,
    ) -> Result<Vec<u8>, GroupError>
    where
        F: Fn() -> Box<dyn MeshDaemon>,
    {
        let mut replaced = Vec::new();

        let prototype = daemon_factory();
        let requirements = prototype.requirements();
        let required = prototype.required_capabilities();
        let optional = prototype.optional_capabilities();
        drop(prototype);

        let mut exclude: HashSet<u64> = HashSet::new();
        exclude.insert(failed_node_id);

        let affected = self.coord.members_on_node(failed_node_id);

        for index in affected {
            self.coord.mark_unhealthy(index);

            let keypair = derive_replica_keypair(&self.config.group_seed, index);
            let entity_id_bytes: NodeId = *keypair.entity_id().as_bytes();

            let artifact = Artifact::Daemon {
                daemon_id: entity_id_bytes,
                required: &required,
                optional: &optional,
            };

            let decision = match GroupCoordinator::place_member(
                scheduler,
                &artifact,
                &requirements,
                &exclude,
                placement,
                tie_break,
            ) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(
                        index,
                        error = %e,
                        "ReplicaGroup::on_node_failure_with_placement: place_member failed; \
                         slot left registered for later recovery"
                    );
                    continue;
                }
            };

            let daemon = daemon_factory();
            let host = DaemonHost::new(daemon, keypair, self.config.host_config.clone());
            registry.replace(host);

            self.coord
                .update_member_placement(index, decision.node_id, entity_id_bytes);
            exclude.insert(decision.node_id);
            replaced.push(index);
        }

        Ok(replaced)
    }

    /// Handle recovery of a node.
    ///
    /// Only re-marks members healthy if they are still registered in the
    /// `DaemonRegistry`. Prevents routing to origin_hashes that were
    /// unregistered during failure and never replaced.
    pub fn on_node_recovery(&mut self, recovered_node_id: u64, registry: &DaemonRegistry) {
        self.coord.on_node_recovery(recovered_node_id, registry);
    }

    /// Aggregate health.
    pub fn health(&self) -> GroupHealth {
        self.coord.health()
    }

    /// Get the group ID.
    pub fn group_id(&self) -> u32 {
        self.group_id
    }

    /// Get all member info.
    pub fn replicas(&self) -> &[MemberInfo] {
        self.coord.members()
    }

    /// Number of replicas.
    pub fn replica_count(&self) -> u8 {
        self.coord.member_count()
    }

    /// Number of healthy replicas.
    pub fn healthy_count(&self) -> u8 {
        self.coord.healthy_count()
    }

    /// Retry placement against the current healthy node pool for
    /// every replica slot currently marked unhealthy. Caps at
    /// `MAX_RECOVERIES_PER_TICK` so a pathological "every slot
    /// unhealthy" state makes progress without wedging the caller.
    /// Returns the slot indices that were successfully placed.
    /// Replica keypairs are derived from `(group_seed, index)` so
    /// recovery reuses the same identity that the slot originally
    /// held.
    fn try_recover_inner<F>(
        &mut self,
        scheduler: &Scheduler,
        registry: &DaemonRegistry,
        daemon_factory: F,
    ) -> Vec<u8>
    where
        F: Fn() -> Box<dyn MeshDaemon>,
    {
        const MAX_RECOVERIES_PER_TICK: usize = 4;
        let unhealthy: Vec<u8> = self
            .coord
            .members()
            .iter()
            .filter(|m| !m.healthy)
            .map(|m| m.index)
            .take(MAX_RECOVERIES_PER_TICK)
            .collect();
        if unhealthy.is_empty() {
            return Vec::new();
        }

        let requirements = daemon_factory().requirements();
        let mut exclude: HashSet<u64> = self
            .coord
            .members()
            .iter()
            .filter(|m| m.healthy)
            .map(|m| m.node_id)
            .collect();
        let mut recovered = Vec::with_capacity(unhealthy.len());

        for index in unhealthy {
            let keypair = derive_replica_keypair(&self.config.group_seed, index);
            let entity_id_bytes: NodeId = *keypair.entity_id().as_bytes();

            let placement =
                match GroupCoordinator::place_with_spread(scheduler, &requirements, &exclude) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::trace!(
                            index,
                            error = %e,
                            "ReplicaGroup::try_recover: place_with_spread still failing; \
                             slot remains unhealthy for next tick"
                        );
                        continue;
                    }
                };

            let daemon = daemon_factory();
            let host = DaemonHost::new(daemon, keypair, self.config.host_config.clone());
            registry.replace(host);

            self.coord
                .update_member_placement(index, placement.node_id, entity_id_bytes);
            exclude.insert(placement.node_id);
            recovered.push(index);
        }

        // X-1 epoch bump on successful recovery — see
        // ForkGroup::try_recover_inner for rationale.
        if !recovered.is_empty() {
            self.term = self.term.saturating_add(1);
        }
        recovered
    }
}

impl crate::adapter::net::compute::UnhealthySlotRecovery for ReplicaGroup {
    fn has_unhealthy_slots(&self) -> bool {
        self.coord.members().iter().any(|m| !m.healthy)
    }

    fn try_recover(
        &mut self,
        scheduler: &Scheduler,
        registry: &DaemonRegistry,
        daemon_factory: &dyn Fn() -> Box<dyn MeshDaemon>,
    ) -> Vec<u8> {
        self.try_recover_inner(scheduler, registry, daemon_factory)
    }
}

impl std::fmt::Debug for ReplicaGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplicaGroup")
            .field("group_id", &format!("{:#x}", self.group_id))
            .field("replicas", &self.coord.member_count())
            .field("healthy", &self.coord.healthy_count())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::capability::{
        CapabilityAnnouncement, CapabilityFilter, CapabilitySet,
    };
    use crate::adapter::net::compute::DaemonError;
    use crate::adapter::net::state::causal::CausalEvent;
    use bytes::Bytes;
    use std::sync::Arc;

    struct NoopDaemon;

    impl MeshDaemon for NoopDaemon {
        fn name(&self) -> &str {
            "noop"
        }
        fn requirements(&self) -> CapabilityFilter {
            CapabilityFilter::default()
        }
        fn process(&mut self, _event: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
            Ok(vec![])
        }
    }

    fn make_scheduler() -> Scheduler {
        use crate::adapter::net::behavior::fold::{capability_bridge, CapabilityFold, Fold};
        let fold: Arc<Fold<CapabilityFold>> =
            Arc::new(Fold::with_sweep_interval(std::time::Duration::ZERO));
        let eid = crate::adapter::net::identity::EntityId::from_bytes([0u8; 32]);
        for node_id in [0x1111u64, 0x2222, 0x3333, 0x4444] {
            capability_bridge::apply_legacy_announcement(
                &fold,
                CapabilityAnnouncement::new(node_id, eid.clone(), 1, CapabilitySet::new()),
                None,
                0,
            )
            .expect("apply legacy announcement in fixture");
        }
        Scheduler::new(fold, 0x1111, CapabilitySet::new())
    }

    fn test_config(n: u8) -> ReplicaGroupConfig {
        ReplicaGroupConfig {
            replica_count: n,
            group_seed: [42u8; 32],
            lb_strategy: Strategy::RoundRobin,
            host_config: DaemonHostConfig::default(),
        }
    }

    #[test]
    fn test_spawn_group() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();

        let group =
            ReplicaGroup::spawn(test_config(3), || Box::new(NoopDaemon), &sched, &reg).unwrap();

        assert_eq!(group.replica_count(), 3);
        assert_eq!(group.health(), GroupHealth::Healthy);
        assert_eq!(reg.count(), 3);

        let hashes: HashSet<u64> = group.replicas().iter().map(|r| r.origin_hash).collect();
        assert_eq!(hashes.len(), 3);
    }

    #[test]
    fn test_deterministic_keypairs() {
        let seed = [7u8; 32];
        let kp1 = derive_replica_keypair(&seed, 0);
        let kp2 = derive_replica_keypair(&seed, 0);
        assert_eq!(kp1.origin_hash(), kp2.origin_hash());

        let kp3 = derive_replica_keypair(&seed, 1);
        assert_ne!(kp1.origin_hash(), kp3.origin_hash());
    }

    #[test]
    fn test_zero_replicas_rejected() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();

        let err =
            ReplicaGroup::spawn(test_config(0), || Box::new(NoopDaemon), &sched, &reg).unwrap_err();
        assert_eq!(
            err,
            GroupError::InvalidConfig("replica_count must be > 0".into())
        );
    }

    #[test]
    fn test_route_event() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();

        let group =
            ReplicaGroup::spawn(test_config(3), || Box::new(NoopDaemon), &sched, &reg).unwrap();

        let ctx = RequestContext::default();
        let origin = group.route_event(&ctx).unwrap();
        assert!(group.replicas().iter().any(|r| r.origin_hash == origin));
    }

    #[test]
    fn test_scale_up() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();

        let mut group =
            ReplicaGroup::spawn(test_config(2), || Box::new(NoopDaemon), &sched, &reg).unwrap();

        group
            .scale_to(4, || Box::new(NoopDaemon), &sched, &reg)
            .unwrap();
        assert_eq!(group.replica_count(), 4);
        assert_eq!(reg.count(), 4);
    }

    /// Regression: BUG_REPORT.md #6 — `scale_to` previously
    /// computed `used_nodes` once before the placement loop and
    /// never inserted the newly-chosen node id between iterations.
    /// `place_with_spread` saw the same exclusion set every
    /// iteration and returned the same first non-excluded node,
    /// so every newly-added replica got colocated on a single
    /// node — the spread invariant was silently violated.
    /// `fork_group.rs:185-199` had the correct `used_nodes.insert`
    /// pattern; this test pins `replica_group` to that contract.
    #[test]
    fn scale_up_does_not_colocate_new_replicas() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();

        // Start with 1 replica, scale to 4 — the test scheduler
        // exposes 4 distinct nodes (0x1111..0x4444), so all 4
        // replicas should land on distinct nodes.
        let mut group =
            ReplicaGroup::spawn(test_config(1), || Box::new(NoopDaemon), &sched, &reg).unwrap();

        group
            .scale_to(4, || Box::new(NoopDaemon), &sched, &reg)
            .unwrap();

        let node_ids: HashSet<u64> = group.replicas().iter().map(|r| r.node_id).collect();
        assert_eq!(
            node_ids.len(),
            4,
            "all 4 replicas should land on distinct nodes — \
             colocation indicates BUG_REPORT.md #6 has regressed; \
             got node ids {:?}",
            group
                .replicas()
                .iter()
                .map(|r| r.node_id)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_scale_down() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();

        let mut group =
            ReplicaGroup::spawn(test_config(4), || Box::new(NoopDaemon), &sched, &reg).unwrap();

        group
            .scale_to(2, || Box::new(NoopDaemon), &sched, &reg)
            .unwrap();
        assert_eq!(group.replica_count(), 2);
        assert_eq!(reg.count(), 2);
    }

    #[test]
    fn test_node_failure_and_replacement() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();

        let mut group =
            ReplicaGroup::spawn(test_config(3), || Box::new(NoopDaemon), &sched, &reg).unwrap();

        let failed_node = group.replicas()[0].node_id;
        let failed_origin = group.replicas()[0].origin_hash;

        let replaced = group
            .on_node_failure(failed_node, || Box::new(NoopDaemon), &sched, &reg)
            .unwrap();

        assert!(!replaced.is_empty());
        assert_ne!(group.health(), GroupHealth::Dead);
        assert!(group
            .replicas()
            .iter()
            .any(|r| r.origin_hash == failed_origin));
    }

    #[test]
    fn test_node_recovery() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();

        let mut group =
            ReplicaGroup::spawn(test_config(2), || Box::new(NoopDaemon), &sched, &reg).unwrap();

        let node = group.replicas()[0].node_id;

        // Mark unhealthy manually
        group.coord.mark_unhealthy(0);

        assert_eq!(
            group.health(),
            GroupHealth::Degraded {
                healthy: 1,
                total: 2
            }
        );

        group.on_node_recovery(node, &reg);
        assert_eq!(group.health(), GroupHealth::Healthy);
    }

    #[test]
    fn test_group_health_dead() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();

        let mut group =
            ReplicaGroup::spawn(test_config(2), || Box::new(NoopDaemon), &sched, &reg).unwrap();

        group.coord.mark_unhealthy(0);
        group.coord.mark_unhealthy(1);
        assert_eq!(group.health(), GroupHealth::Dead);
    }

    #[test]
    fn test_group_id_deterministic() {
        let reg1 = DaemonRegistry::new();
        let reg2 = DaemonRegistry::new();
        let sched = make_scheduler();

        let g1 =
            ReplicaGroup::spawn(test_config(1), || Box::new(NoopDaemon), &sched, &reg1).unwrap();
        let g2 =
            ReplicaGroup::spawn(test_config(1), || Box::new(NoopDaemon), &sched, &reg2).unwrap();

        assert_eq!(g1.group_id(), g2.group_id());
    }

    /// Regression: BUG_REPORT.md #7 — `on_node_failure` previously
    /// called `mark_unhealthy` + `unregister` BEFORE attempting
    /// `place_with_spread`. If placement failed (no spare nodes
    /// available) the loop `continue`d without restoring state —
    /// the slot was left unhealthy AND unregistered, and
    /// `on_node_recovery` only re-marks slots whose origin_hash
    /// is still in the registry. Result: permanent degradation.
    ///
    /// We trigger placement failure by using a single-node
    /// scheduler: when that node "fails" and is excluded from the
    /// candidate set, no spare exists. Pre-fix the slot ends up
    /// unregistered. Post-fix it stays registered so recovery
    /// can repair it.
    #[test]
    fn place_failure_does_not_strand_slot_in_unregistered_state() {
        // Build a scheduler with exactly one node so the
        // exclude-the-failed-node candidate search returns nothing.
        fn single_node_scheduler() -> Scheduler {
            use crate::adapter::net::behavior::fold::{capability_bridge, CapabilityFold, Fold};
            let fold: Arc<Fold<CapabilityFold>> =
                Arc::new(Fold::with_sweep_interval(std::time::Duration::ZERO));
            let eid = crate::adapter::net::identity::EntityId::from_bytes([0u8; 32]);
            capability_bridge::apply_legacy_announcement(
                &fold,
                CapabilityAnnouncement::new(0x9999, eid, 1, CapabilitySet::new()),
                None,
                0,
            )
            .expect("apply legacy announcement in fixture");
            Scheduler::new(fold, 0x9999, CapabilitySet::new())
        }

        let reg = DaemonRegistry::new();
        let sched = single_node_scheduler();
        let mut group =
            ReplicaGroup::spawn(test_config(1), || Box::new(NoopDaemon), &sched, &reg).unwrap();

        let failed_node = group.replicas()[0].node_id;
        let failed_origin = group.replicas()[0].origin_hash;
        assert_eq!(failed_node, 0x9999);
        assert!(reg.contains(failed_origin));

        // Trigger failure on the only node. `place_with_spread`
        // excludes it and finds no candidates → returns Err.
        let replaced = group
            .on_node_failure(failed_node, || Box::new(NoopDaemon), &sched, &reg)
            .unwrap();
        assert!(
            replaced.is_empty(),
            "with no spare nodes, placement must fail and no replacement is recorded"
        );

        // The crucial invariant: the slot's origin_hash is still
        // in the registry, so on_node_recovery can fix it.
        // Pre-fix: this assertion failed because `unregister` ran
        // before `place_with_spread` and was never undone.
        assert!(
            reg.contains(failed_origin),
            "BUG_REPORT.md #7: slot must remain registered when placement \
             fails — otherwise on_node_recovery cannot restore it"
        );

        // Recovery on the same node restores the slot to healthy.
        group.on_node_recovery(failed_node, &reg);
        assert_eq!(
            group.health(),
            GroupHealth::Healthy,
            "after recovery the slot must be healthy again — the pre-fix \
             code left it permanently unhealthy + unregistered"
        );
    }

    // ──────────────────────────────────────────────────────────────────
    // Phase G slice 5 — `*_with_placement` v2 wiring tests.
    //
    // Pin three guarantees:
    //   1. v2 entry points work end-to-end (spawn / scale_to /
    //      on_node_failure_with_placement) and exhibit the same
    //      observable contract as v1 when given a permissive filter.
    //   2. v2 honors a score-based filter (different selection vs v1).
    //   3. v2 keeps the spread invariant (no two replicas on one node).
    // ──────────────────────────────────────────────────────────────────

    use crate::adapter::net::behavior::placement::{NodeId as PlacementNodeId, ResourceAxis};

    fn make_scheduler_and_index(node_ids: &[u64]) -> Scheduler {
        use crate::adapter::net::behavior::fold::{capability_bridge, CapabilityFold, Fold};
        let fold: Arc<Fold<CapabilityFold>> =
            Arc::new(Fold::with_sweep_interval(std::time::Duration::ZERO));
        let eid = crate::adapter::net::identity::EntityId::from_bytes([0u8; 32]);
        for &id in node_ids {
            capability_bridge::apply_legacy_announcement(
                &fold,
                CapabilityAnnouncement::new(id, eid.clone(), 1, CapabilitySet::new()),
                None,
                0,
            )
            .expect("apply legacy announcement in fixture");
        }
        let local = node_ids.first().copied().unwrap_or(0xFFFF);
        Scheduler::new(fold, local, CapabilitySet::new())
    }

    /// Permissive placement filter — every candidate scores 1.0.
    /// Lets us pin "v2 doesn't break observable behavior" against v1.
    struct AllowAll;
    impl PlacementFilter for AllowAll {
        fn placement_score(&self, _: &PlacementNodeId, _: &Artifact<'_>) -> Option<f32> {
            Some(1.0)
        }
    }

    /// `spawn_with_placement` produces N replicas across distinct
    /// nodes when placement is permissive (parity with `spawn`).
    #[test]
    fn spawn_with_placement_spreads_across_nodes() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler_and_index(&[0x1111, 0x2222, 0x3333, 0x4444]);
        let tb = TieBreakContext {
            rtt_lookup: None,
            resource_axis: ResourceAxis::Compute,
        };

        let group = ReplicaGroup::spawn_with_placement(
            test_config(3),
            || Box::new(NoopDaemon),
            &sched,
            &reg,
            &AllowAll,
            &tb,
        )
        .expect("spawn_with_placement should succeed with 4 candidate nodes");

        assert_eq!(group.replica_count(), 3);
        assert_eq!(group.health(), GroupHealth::Healthy);
        let node_ids: HashSet<u64> = group.replicas().iter().map(|r| r.node_id).collect();
        assert_eq!(
            node_ids.len(),
            3,
            "spread invariant: all 3 replicas on distinct nodes"
        );
    }

    /// `spawn_with_placement` honors a score-based filter — pins
    /// that v2 is genuinely score-aware, not just a rename of v1.
    /// With a filter that pegs 0x4444 to 1.0 and others to 0.1,
    /// the FIRST replica (lowest exclusion-set score) MUST land on
    /// 0x4444. v1 (`spawn`) would land on the local node 0x1111.
    #[test]
    fn spawn_with_placement_routes_first_replica_to_highest_scorer() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler_and_index(&[0x1111, 0x2222, 0x3333, 0x4444]);
        let tb = TieBreakContext {
            rtt_lookup: None,
            resource_axis: ResourceAxis::Compute,
        };

        struct PreferHighest;
        impl PlacementFilter for PreferHighest {
            fn placement_score(&self, t: &PlacementNodeId, _: &Artifact<'_>) -> Option<f32> {
                Some(if *t == 0x4444 { 1.0 } else { 0.1 })
            }
        }

        let group = ReplicaGroup::spawn_with_placement(
            test_config(1),
            || Box::new(NoopDaemon),
            &sched,
            &reg,
            &PreferHighest,
            &tb,
        )
        .expect("spawn_with_placement with 1 replica should succeed");

        assert_eq!(group.replicas()[0].node_id, 0x4444);
    }

    /// `spawn_with_placement` propagates a vetoed-everywhere
    /// filter as `PlacementFailed` — pins the failure mode.
    #[test]
    fn spawn_with_placement_returns_placement_failed_when_all_vetoed() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler_and_index(&[0x1111, 0x2222]);
        let tb = TieBreakContext {
            rtt_lookup: None,
            resource_axis: ResourceAxis::Compute,
        };

        struct VetoAll;
        impl PlacementFilter for VetoAll {
            fn placement_score(&self, _: &PlacementNodeId, _: &Artifact<'_>) -> Option<f32> {
                None
            }
        }

        let err = ReplicaGroup::spawn_with_placement(
            test_config(1),
            || Box::new(NoopDaemon),
            &sched,
            &reg,
            &VetoAll,
            &tb,
        )
        .expect_err("VetoAll filter should make placement fail");

        assert!(matches!(err, GroupError::PlacementFailed(_)));
        assert_eq!(reg.count(), 0, "no host registered when placement fails");
    }

    /// `scale_to_with_placement` adds replicas under v2 placement
    /// and respects the spread invariant — same regression coverage
    /// as `scale_up_does_not_colocate_new_replicas` (v1) but on the
    /// v2 path.
    #[test]
    fn scale_to_with_placement_spreads_new_replicas() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler_and_index(&[0x1111, 0x2222, 0x3333, 0x4444]);
        let tb = TieBreakContext {
            rtt_lookup: None,
            resource_axis: ResourceAxis::Compute,
        };

        let mut group = ReplicaGroup::spawn_with_placement(
            test_config(1),
            || Box::new(NoopDaemon),
            &sched,
            &reg,
            &AllowAll,
            &tb,
        )
        .unwrap();

        group
            .scale_to_with_placement(4, || Box::new(NoopDaemon), &sched, &reg, &AllowAll, &tb)
            .expect("scale_to_with_placement should succeed with 4 nodes");

        let node_ids: HashSet<u64> = group.replicas().iter().map(|r| r.node_id).collect();
        assert_eq!(
            node_ids.len(),
            4,
            "spread invariant under v2: all 4 replicas on distinct nodes"
        );
        assert_eq!(reg.count(), 4);
    }

    /// `scale_to_with_placement` scale-DOWN works without invoking
    /// the placement filter (unregisters extra members).
    #[test]
    fn scale_to_with_placement_scale_down_does_not_invoke_filter() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler_and_index(&[0x1111, 0x2222, 0x3333]);
        let tb = TieBreakContext {
            rtt_lookup: None,
            resource_axis: ResourceAxis::Compute,
        };

        let mut group = ReplicaGroup::spawn_with_placement(
            test_config(3),
            || Box::new(NoopDaemon),
            &sched,
            &reg,
            &AllowAll,
            &tb,
        )
        .unwrap();
        assert_eq!(reg.count(), 3);

        // Scale down to 1 — even with VetoAll, the scale-down
        // path should NOT invoke the filter.
        struct VetoAll;
        impl PlacementFilter for VetoAll {
            fn placement_score(&self, _: &PlacementNodeId, _: &Artifact<'_>) -> Option<f32> {
                None
            }
        }

        group
            .scale_to_with_placement(1, || Box::new(NoopDaemon), &sched, &reg, &VetoAll, &tb)
            .expect("scale-down does not invoke the filter");

        assert_eq!(group.replica_count(), 1);
        assert_eq!(reg.count(), 1);
    }

    /// `on_node_failure_with_placement` re-spawns the affected
    /// member on a different node when one is available. Mirrors
    /// the v1 happy-path test on the v2 path.
    #[test]
    fn on_node_failure_with_placement_replaces_member_on_spare_node() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler_and_index(&[0x1111, 0x2222, 0x3333, 0x4444]);
        let tb = TieBreakContext {
            rtt_lookup: None,
            resource_axis: ResourceAxis::Compute,
        };

        let mut group = ReplicaGroup::spawn_with_placement(
            test_config(2),
            || Box::new(NoopDaemon),
            &sched,
            &reg,
            &AllowAll,
            &tb,
        )
        .unwrap();

        let failed_node = group.replicas()[0].node_id;
        let failed_index = group.replicas()[0].index;
        let failed_origin = group.replicas()[0].origin_hash;

        let replaced = group
            .on_node_failure_with_placement(
                failed_node,
                || Box::new(NoopDaemon),
                &sched,
                &reg,
                &AllowAll,
                &tb,
            )
            .unwrap();

        assert_eq!(replaced, vec![failed_index]);
        // Member at `failed_index` now lives on a different node.
        let new_node = group
            .replicas()
            .iter()
            .find(|r| r.index == failed_index)
            .unwrap()
            .node_id;
        assert_ne!(new_node, failed_node);
        // Deterministic origin_hash unchanged — replace, not new register.
        assert!(reg.contains(failed_origin));
    }

    /// `on_node_failure_with_placement` keeps the slot registered
    /// when no spare node is available — pins the recovery
    /// guarantee (#7) for the v2 path.
    #[test]
    fn on_node_failure_with_placement_preserves_slot_when_placement_fails() {
        let reg = DaemonRegistry::new();
        // Single-node mesh — failure leaves NO spare; placement
        // MUST fail and the slot MUST stay registered.
        let sched = make_scheduler_and_index(&[0x9999]);
        let tb = TieBreakContext {
            rtt_lookup: None,
            resource_axis: ResourceAxis::Compute,
        };

        let mut group = ReplicaGroup::spawn_with_placement(
            test_config(1),
            || Box::new(NoopDaemon),
            &sched,
            &reg,
            &AllowAll,
            &tb,
        )
        .unwrap();

        let failed_node = group.replicas()[0].node_id;
        let failed_origin = group.replicas()[0].origin_hash;
        assert_eq!(failed_node, 0x9999);

        let replaced = group
            .on_node_failure_with_placement(
                failed_node,
                || Box::new(NoopDaemon),
                &sched,
                &reg,
                &AllowAll,
                &tb,
            )
            .unwrap();

        assert!(
            replaced.is_empty(),
            "no spare → placement must fail and no replacement recorded"
        );
        assert!(
            reg.contains(failed_origin),
            "slot must remain registered when placement fails (recovery guarantee #7)"
        );

        group.on_node_recovery(failed_node, &reg);
        assert_eq!(group.health(), GroupHealth::Healthy);
    }

    /// `spawn` (v1) is unchanged after the v2 surface lands —
    /// pins the "additive, no behavior change" guarantee for slice 5.
    #[test]
    fn spawn_v1_path_unchanged_after_v2_added() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();
        let group =
            ReplicaGroup::spawn(test_config(3), || Box::new(NoopDaemon), &sched, &reg).unwrap();
        assert_eq!(group.replica_count(), 3);
        assert_eq!(group.health(), GroupHealth::Healthy);
    }
}
