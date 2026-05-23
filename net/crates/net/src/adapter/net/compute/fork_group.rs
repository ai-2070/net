//! Fork groups — N divergent copies of a daemon with documented lineage.
//!
//! A `ForkGroup` creates N independent daemons forked from a common parent.
//! Each fork has its own identity, its own causal chain, and a `ForkRecord`
//! that cryptographically links it back to the parent's chain at the fork
//! point. Unlike `ReplicaGroup` (where members are interchangeable), forks
//! are independent entities that happen to share lineage.
//!
//! The group provides:
//!
//! - Verifiable lineage via `ForkRecord` with sentinel hashes
//! - Load-balanced event routing across forks
//! - Group-level health tracking
//! - Dynamic scaling (add/remove forks)
//! - Auto-replacement on node failure (re-fork with stored keypair)

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
use crate::adapter::net::continuity::discontinuity::{fork_entity, ForkRecord};
use crate::adapter::net::identity::EntityKeypair;
use crate::adapter::net::state::causal::CausalChainBuilder;

// ── Configuration ────────────────────────────────────────────────────────────

/// Configuration for a fork group.
#[derive(Debug, Clone)]
pub struct ForkGroupConfig {
    /// Number of forks to create.
    pub fork_count: u8,
    /// Load balancing strategy for routing events to forks.
    pub lb_strategy: Strategy,
    /// Daemon host configuration for each fork.
    pub host_config: DaemonHostConfig,
}

// ── Per-fork metadata ────────────────────────────────────────────────────────

/// Extended metadata for a fork, including its lineage record and stored
/// keypair for deterministic recovery.
#[derive(Debug, Clone)]
pub struct ForkInfo {
    /// The fork's index in the group.
    pub index: u8,
    /// Verifiable lineage record linking this fork to the parent.
    pub record: ForkRecord,
    /// Stored keypair bytes for deterministic recovery on failure.
    ///
    /// `fork_entity()` generates random keypairs, so we must store them
    /// to re-create the same identity after a node crash.
    keypair_secret: [u8; 32],
}

// ── ForkGroup ────────────────────────────────────────────────────────────────

/// Manages N divergent forks of a daemon as a logical unit.
///
/// Each fork has:
/// - Its own `EntityKeypair` and `origin_hash`
/// - Its own causal chain starting from a fork genesis
/// - A `ForkRecord` with a verifiable sentinel linking to the parent
///
/// The group coordinates routing, health, and replacement across forks.
pub struct ForkGroup {
    /// Origin hash of the parent daemon that was forked.
    parent_origin: u64,
    /// Sequence number at which the fork occurred.
    fork_seq: u64,
    /// Configuration.
    config: ForkGroupConfig,
    /// Per-fork metadata (lineage records + stored keypairs).
    forks: Vec<ForkInfo>,
    /// Shared coordination (LB, members, health).
    coord: GroupCoordinator,
    /// X-1 epoch — bumped on every recovery-driven re-placement
    /// of a fork slot. See `StandbyGroup::term` for the cross-
    /// node-fencing intent; the wire integration is a separate
    /// change.
    term: u64,
}

impl ForkGroup {
    /// Fork a daemon into N independent copies with documented lineage.
    ///
    /// For each fork:
    /// 1. Call `fork_entity()` to generate a new keypair + ForkRecord
    /// 2. Place via Scheduler
    /// 3. Create DaemonHost with the fork's chain builder
    /// 4. Register in DaemonRegistry
    /// 5. Store keypair for recovery
    ///
    /// The parent daemon is NOT modified — it continues unchanged.
    pub fn fork<F>(
        parent_origin: u64,
        fork_seq: u64,
        config: ForkGroupConfig,
        daemon_factory: F,
        scheduler: &Scheduler,
        registry: &DaemonRegistry,
    ) -> Result<Self, GroupError>
    where
        F: Fn() -> Box<dyn MeshDaemon>,
    {
        if config.fork_count == 0 {
            return Err(GroupError::InvalidConfig("fork_count must be > 0".into()));
        }

        let mut coord = GroupCoordinator::new(config.lb_strategy);
        let mut forks = Vec::with_capacity(config.fork_count as usize);
        let mut used_nodes: HashSet<u64> = HashSet::new();
        let requirements = daemon_factory().requirements();

        for index in 0..config.fork_count {
            let (keypair, record, chain_builder) = fork_entity(parent_origin, fork_seq, None);

            let origin_hash = keypair.origin_hash();
            let entity_id_bytes: NodeId = *keypair.entity_id().as_bytes();
            let keypair_secret = *keypair.secret_bytes();

            let placement =
                GroupCoordinator::place_with_spread(scheduler, &requirements, &used_nodes)?;
            let node_id = placement.node_id;
            used_nodes.insert(node_id);

            // Create daemon host with the forked chain
            let daemon = daemon_factory();
            let host =
                DaemonHost::from_fork(daemon, keypair, chain_builder, config.host_config.clone())?;
            registry.register(host)?;

            coord.add_member(MemberInfo {
                index,
                origin_hash,
                node_id,
                entity_id_bytes,
                healthy: true,
            });

            forks.push(ForkInfo {
                index,
                record,
                keypair_secret,
            });
        }

        Ok(Self {
            parent_origin,
            fork_seq,
            config,
            forks,
            coord,
            term: 1,
        })
    }

    /// Route an inbound event to the best available fork.
    pub fn route_event(&self, ctx: &RequestContext) -> Result<u64, GroupError> {
        self.coord.route_event(ctx)
    }

    /// X-1 epoch counter. Bumped on every successful slot
    /// re-placement via `try_recover` after a node failure.
    /// See `StandbyGroup::term` for the fencing rationale.
    pub fn term(&self) -> u64 {
        self.term
    }

    /// Resize the fork group to `n` forks.
    ///
    /// Scale up creates new forks from the same parent at the same fork_seq.
    /// Scale down removes the highest-index forks.
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
            return Err(GroupError::InvalidConfig("fork_count must be > 0".into()));
        }

        let current = self.coord.member_count();

        if n > current {
            let requirements = daemon_factory().requirements();
            let mut used_nodes: HashSet<u64> =
                self.coord.members().iter().map(|m| m.node_id).collect();

            for index in current..n {
                let (keypair, record, chain_builder) =
                    fork_entity(self.parent_origin, self.fork_seq, None);

                let origin_hash = keypair.origin_hash();
                let entity_id_bytes: NodeId = *keypair.entity_id().as_bytes();
                let keypair_secret = *keypair.secret_bytes();

                let placement =
                    GroupCoordinator::place_with_spread(scheduler, &requirements, &used_nodes)?;
                let node_id = placement.node_id;
                used_nodes.insert(node_id);

                let daemon = daemon_factory();
                let host = DaemonHost::from_fork(
                    daemon,
                    keypair,
                    chain_builder,
                    self.config.host_config.clone(),
                )?;
                registry.register(host)?;

                self.coord.add_member(MemberInfo {
                    index,
                    origin_hash,
                    node_id,
                    entity_id_bytes,
                    healthy: true,
                });

                self.forks.push(ForkInfo {
                    index,
                    record,
                    keypair_secret,
                });
            }
        } else if n < current {
            // Pre-fix this loop relied on an unstated invariant
            // — `coord.remove_last()` returning `Some` always
            // implies a parallel `forks` entry to `pop`. The two
            // structures are populated in lockstep above (every
            // `coord.add_member` is followed by `self.forks.push`),
            // but the loop offered no defense against a divergent
            // state and would spin forever if `remove_last`
            // returned `None` while `member_count() > n` (e.g. a
            // future invariant violation introduced elsewhere).
            //
            // Two hardening steps:
            // 1. `break` on `None` from `remove_last` — a divergence
            //    is reported via the debug_assert; in release we
            //    refuse to spin and exit the loop with the rest of
            //    state best-effort.
            // 2. `debug_assert!` that `forks.pop` returned `Some`
            //    matching the coord remove. CI catches divergence;
            //    release silently moves on.
            while self.coord.member_count() > n {
                let Some(info) = self.coord.remove_last() else {
                    debug_assert!(
                        false,
                        "fork_group: coord.member_count() > n but remove_last() returned None — \
                         coord/forks invariant violated"
                    );
                    break;
                };
                let _ = registry.unregister(info.origin_hash);
                let popped = self.forks.pop();
                debug_assert!(
                    popped.is_some(),
                    "fork_group: removed coord member {origin:#x} but forks vec was empty — \
                     coord and forks must stay in lockstep",
                    origin = info.origin_hash,
                );
            }
        }

        self.config.fork_count = n;
        Ok(())
    }

    /// Handle failure of a node hosting one or more forks.
    ///
    /// Re-creates each affected fork with its stored keypair (same identity)
    /// and a fresh chain from the original fork point.
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

            // Try `place_with_spread` BEFORE touching the registry
            // so a placement failure doesn't leave the slot
            // unregistered (and therefore unrecoverable via
            // `on_node_recovery`).

            // Recover the same keypair from stored secret
            let fork_info = match self.forks.get(index as usize) {
                Some(info) => info,
                None => {
                    tracing::warn!(index, "on_node_failure: fork index out of bounds, skipping");
                    continue;
                }
            };
            let keypair = EntityKeypair::from_bytes(fork_info.keypair_secret);
            let entity_id_bytes: NodeId = *keypair.entity_id().as_bytes();

            let chain_builder =
                CausalChainBuilder::from_head(fork_info.record.fork_genesis, bytes::Bytes::new());

            let placement =
                match GroupCoordinator::place_with_spread(scheduler, &requirements, &exclude) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!(
                            index,
                            error = %e,
                            "ForkGroup::on_node_failure: place_with_spread failed; \
                             slot left registered for later recovery (#7)"
                        );
                        continue;
                    }
                };

            // Fork keypairs are deterministic per (parent_origin,
            // fork_seq) — the new origin_hash matches the old.
            // Use `registry.replace` for an atomic upsert: a single
            // map operation that never leaves the slot empty
            // between callers (the older `unregister` →
            // `register` sequence had a small window where the
            // second step could fail and orphan the slot).
            let daemon = daemon_factory();
            let host = DaemonHost::from_fork(
                daemon,
                keypair,
                chain_builder,
                self.config.host_config.clone(),
            )?;
            registry.replace(host);

            self.coord
                .update_member_placement(index, placement.node_id, entity_id_bytes);
            exclude.insert(placement.node_id);
            replaced.push(index);
        }

        Ok(replaced)
    }

    /// Phase G slice 6 — `fork` with score-based placement. Routes
    /// every per-fork placement decision through
    /// [`GroupCoordinator::place_member`] (i.e.
    /// `Scheduler::select_member_node` + LOCKED §7 tie-breaker).
    ///
    /// The artifact passed to `placement` is built per-iteration
    /// from the daemon's `required_capabilities()` /
    /// `optional_capabilities()` plus the per-fork entity-id (used
    /// as `daemon_id` for stable ordering).
    ///
    /// Args mirror `Self::fork` plus the `(placement, tie_break)`
    /// pair the v2 path needs. Bundling them into a single
    /// `PlacementContext` struct just to dodge clippy's arg-count
    /// lint would obscure the actual surface — every arg is
    /// load-bearing per the LOCKED §7 placement contract.
    #[allow(clippy::too_many_arguments)]
    pub fn fork_with_placement<F>(
        parent_origin: u64,
        fork_seq: u64,
        config: ForkGroupConfig,
        daemon_factory: F,
        scheduler: &Scheduler,
        registry: &DaemonRegistry,
        placement: &dyn PlacementFilter,
        tie_break: &TieBreakContext<'_>,
    ) -> Result<Self, GroupError>
    where
        F: Fn() -> Box<dyn MeshDaemon>,
    {
        if config.fork_count == 0 {
            return Err(GroupError::InvalidConfig("fork_count must be > 0".into()));
        }

        let mut coord = GroupCoordinator::new(config.lb_strategy);
        let mut forks = Vec::with_capacity(config.fork_count as usize);
        let mut used_nodes: HashSet<u64> = HashSet::new();

        // Capture the daemon's capability surface once — required /
        // optional are stable across the fork; `requirements()`
        // (legacy `CapabilityFilter`) still narrows the candidate
        // pool inside `place_member`.
        let prototype = daemon_factory();
        let requirements = prototype.requirements();
        let required = prototype.required_capabilities();
        let optional = prototype.optional_capabilities();
        drop(prototype);

        for index in 0..config.fork_count {
            let (keypair, record, chain_builder) = fork_entity(parent_origin, fork_seq, None);

            let origin_hash = keypair.origin_hash();
            let entity_id_bytes: NodeId = *keypair.entity_id().as_bytes();
            let keypair_secret = *keypair.secret_bytes();

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
            let host =
                DaemonHost::from_fork(daemon, keypair, chain_builder, config.host_config.clone())?;
            registry.register(host)?;

            coord.add_member(MemberInfo {
                index,
                origin_hash,
                node_id,
                entity_id_bytes,
                healthy: true,
            });

            forks.push(ForkInfo {
                index,
                record,
                keypair_secret,
            });
        }

        Ok(Self {
            parent_origin,
            fork_seq,
            config,
            forks,
            coord,
            term: 1,
        })
    }

    /// Phase G slice 6 — `scale_to` with score-based placement.
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
            return Err(GroupError::InvalidConfig("fork_count must be > 0".into()));
        }

        let current = self.coord.member_count();

        if n > current {
            let prototype = daemon_factory();
            let requirements = prototype.requirements();
            let required = prototype.required_capabilities();
            let optional = prototype.optional_capabilities();
            drop(prototype);

            let mut used_nodes: HashSet<u64> =
                self.coord.members().iter().map(|m| m.node_id).collect();

            for index in current..n {
                let (keypair, record, chain_builder) =
                    fork_entity(self.parent_origin, self.fork_seq, None);

                let origin_hash = keypair.origin_hash();
                let entity_id_bytes: NodeId = *keypair.entity_id().as_bytes();
                let keypair_secret = *keypair.secret_bytes();

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
                let host = DaemonHost::from_fork(
                    daemon,
                    keypair,
                    chain_builder,
                    self.config.host_config.clone(),
                )?;
                registry.register(host)?;

                self.coord.add_member(MemberInfo {
                    index,
                    origin_hash,
                    node_id,
                    entity_id_bytes,
                    healthy: true,
                });

                self.forks.push(ForkInfo {
                    index,
                    record,
                    keypair_secret,
                });
            }
        } else if n < current {
            // Same lockstep invariant + hardening as `scale_to` —
            // see that method's comment for rationale.
            while self.coord.member_count() > n {
                let Some(info) = self.coord.remove_last() else {
                    debug_assert!(
                        false,
                        "fork_group: coord.member_count() > n but remove_last() returned None — \
                         coord/forks invariant violated"
                    );
                    break;
                };
                let _ = registry.unregister(info.origin_hash);
                let popped = self.forks.pop();
                debug_assert!(
                    popped.is_some(),
                    "fork_group: removed coord member {origin:#x} but forks vec was empty — \
                     coord and forks must stay in lockstep",
                    origin = info.origin_hash,
                );
            }
        }

        self.config.fork_count = n;
        Ok(())
    }

    /// Phase G slice 6 — `on_node_failure` with score-based
    /// placement. Replaces affected forks via
    /// [`GroupCoordinator::place_member`]; on placement failure the
    /// slot is left registered (same recovery-friendly behavior as
    /// [`Self::on_node_failure`]).
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

            // Recover the same keypair from stored secret.
            let fork_info = match self.forks.get(index as usize) {
                Some(info) => info,
                None => {
                    tracing::warn!(
                        index,
                        "on_node_failure_with_placement: fork index out of bounds, skipping"
                    );
                    continue;
                }
            };
            let keypair = EntityKeypair::from_bytes(fork_info.keypair_secret);
            let entity_id_bytes: NodeId = *keypair.entity_id().as_bytes();

            let chain_builder =
                CausalChainBuilder::from_head(fork_info.record.fork_genesis, bytes::Bytes::new());

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
                        "ForkGroup::on_node_failure_with_placement: place_member failed; \
                         slot left registered for later recovery"
                    );
                    continue;
                }
            };

            let daemon = daemon_factory();
            let host = DaemonHost::from_fork(
                daemon,
                keypair,
                chain_builder,
                self.config.host_config.clone(),
            )?;
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

    /// Get the parent daemon's origin hash.
    pub fn parent_origin(&self) -> u64 {
        self.parent_origin
    }

    /// Get the sequence at which the fork occurred.
    pub fn fork_seq(&self) -> u64 {
        self.fork_seq
    }

    /// Get all fork records (verifiable lineage).
    pub fn fork_records(&self) -> Vec<&ForkRecord> {
        self.forks.iter().map(|f| &f.record).collect()
    }

    /// Verify all fork records are structurally valid.
    pub fn verify_lineage(&self) -> bool {
        self.forks.iter().all(|f| f.record.verify())
    }

    /// Get all member info.
    pub fn members(&self) -> &[MemberInfo] {
        self.coord.members()
    }

    /// Number of forks.
    pub fn fork_count(&self) -> u8 {
        self.coord.member_count()
    }

    /// Number of healthy forks.
    pub fn healthy_count(&self) -> u8 {
        self.coord.healthy_count()
    }

    /// Retry placement against the current healthy node pool for
    /// every fork slot currently marked unhealthy. Caps at
    /// `MAX_RECOVERIES_PER_TICK` so a pathological "every slot
    /// unhealthy" state makes progress without wedging the caller.
    /// Returns the slot indices that were successfully placed.
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
            let fork_info = match self.forks.get(index as usize) {
                Some(info) => info.clone(),
                None => continue,
            };
            let keypair = EntityKeypair::from_bytes(fork_info.keypair_secret);
            let entity_id_bytes: NodeId = *keypair.entity_id().as_bytes();
            let chain_builder =
                CausalChainBuilder::from_head(fork_info.record.fork_genesis, bytes::Bytes::new());

            let placement =
                match GroupCoordinator::place_with_spread(scheduler, &requirements, &exclude) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::trace!(
                            index,
                            error = %e,
                            "ForkGroup::try_recover: place_with_spread still failing; \
                             slot remains unhealthy for next tick"
                        );
                        continue;
                    }
                };

            let daemon = daemon_factory();
            let host = match DaemonHost::from_fork(
                daemon,
                keypair,
                chain_builder,
                self.config.host_config.clone(),
            ) {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(
                        index,
                        error = %e,
                        "ForkGroup::try_recover: DaemonHost::from_fork failed; \
                         slot remains unhealthy"
                    );
                    continue;
                }
            };
            registry.replace(host);

            self.coord
                .update_member_placement(index, placement.node_id, entity_id_bytes);
            exclude.insert(placement.node_id);
            recovered.push(index);
        }

        // X-1 epoch bump: every successful recovery advances the
        // term. A future cross-node wire-fencing layer can use
        // this to reject routed events from a slot that observed
        // a stale `term` at the issuer's end of the partition.
        if !recovered.is_empty() {
            self.term = self.term.saturating_add(1);
        }
        recovered
    }
}

impl crate::adapter::net::compute::UnhealthySlotRecovery for ForkGroup {
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

impl std::fmt::Debug for ForkGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForkGroup")
            .field("parent_origin", &format!("{:#x}", self.parent_origin))
            .field("fork_seq", &self.fork_seq)
            .field("forks", &self.coord.member_count())
            .field("healthy", &self.coord.healthy_count())
            .field("lineage_valid", &self.verify_lineage())
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
        for node_id in [0x1111u64, 0x2222, 0x3333, 0x4444, 0x5555] {
            capability_bridge::apply_legacy_announcement(
                &fold,
                CapabilityAnnouncement::new(node_id, eid.clone(), 1, CapabilitySet::new()),
            )
            .expect("apply legacy announcement in fixture");
        }
        Scheduler::new(fold, 0x1111, CapabilitySet::new())
    }

    fn test_config(n: u8) -> ForkGroupConfig {
        ForkGroupConfig {
            fork_count: n,
            lb_strategy: Strategy::RoundRobin,
            host_config: DaemonHostConfig::default(),
        }
    }

    #[test]
    fn test_fork_group_spawn() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();

        let group = ForkGroup::fork(
            0xAAAA,
            100,
            test_config(3),
            || Box::new(NoopDaemon),
            &sched,
            &reg,
        )
        .unwrap();

        assert_eq!(group.fork_count(), 3);
        assert_eq!(group.health(), GroupHealth::Healthy);
        assert_eq!(group.parent_origin(), 0xAAAA);
        assert_eq!(group.fork_seq(), 100);
        assert_eq!(reg.count(), 3);

        // Each fork has a unique origin_hash
        let hashes: HashSet<u64> = group.members().iter().map(|m| m.origin_hash).collect();
        assert_eq!(hashes.len(), 3);
    }

    #[test]
    fn test_fork_lineage_verifiable() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();

        let group = ForkGroup::fork(
            0xBBBB,
            50,
            test_config(3),
            || Box::new(NoopDaemon),
            &sched,
            &reg,
        )
        .unwrap();

        // All fork records should verify
        assert!(group.verify_lineage());

        // Each record should reference the parent
        for record in group.fork_records() {
            assert_eq!(record.original_origin, 0xBBBB);
            assert_eq!(record.fork_seq, 50);
            assert!(record.verify());
        }
    }

    #[test]
    fn test_fork_zero_rejected() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();

        let err = ForkGroup::fork(
            0xAAAA,
            0,
            test_config(0),
            || Box::new(NoopDaemon),
            &sched,
            &reg,
        )
        .unwrap_err();
        assert_eq!(
            err,
            GroupError::InvalidConfig("fork_count must be > 0".into())
        );
    }

    #[test]
    fn test_fork_route_event() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();

        let group = ForkGroup::fork(
            0xAAAA,
            100,
            test_config(3),
            || Box::new(NoopDaemon),
            &sched,
            &reg,
        )
        .unwrap();

        let ctx = RequestContext::default();
        let origin = group.route_event(&ctx).unwrap();
        assert!(group.members().iter().any(|m| m.origin_hash == origin));
    }

    #[test]
    fn test_fork_scale_up() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();

        let mut group = ForkGroup::fork(
            0xAAAA,
            10,
            test_config(2),
            || Box::new(NoopDaemon),
            &sched,
            &reg,
        )
        .unwrap();

        group
            .scale_to(4, || Box::new(NoopDaemon), &sched, &reg)
            .unwrap();
        assert_eq!(group.fork_count(), 4);
        assert_eq!(reg.count(), 4);
        assert!(group.verify_lineage());
    }

    #[test]
    fn test_fork_scale_down() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();

        let mut group = ForkGroup::fork(
            0xAAAA,
            10,
            test_config(4),
            || Box::new(NoopDaemon),
            &sched,
            &reg,
        )
        .unwrap();

        group
            .scale_to(2, || Box::new(NoopDaemon), &sched, &reg)
            .unwrap();
        assert_eq!(group.fork_count(), 2);
        assert_eq!(reg.count(), 2);
    }

    #[test]
    fn test_fork_node_failure_preserves_identity() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();

        let mut group = ForkGroup::fork(
            0xAAAA,
            100,
            test_config(3),
            || Box::new(NoopDaemon),
            &sched,
            &reg,
        )
        .unwrap();

        let failed_node = group.members()[0].node_id;
        let failed_origin = group.members()[0].origin_hash;

        let replaced = group
            .on_node_failure(failed_node, || Box::new(NoopDaemon), &sched, &reg)
            .unwrap();

        assert!(!replaced.is_empty());
        assert_ne!(group.health(), GroupHealth::Dead);

        // The replaced fork keeps the same origin_hash (stored keypair)
        assert!(group
            .members()
            .iter()
            .any(|m| m.origin_hash == failed_origin));

        // Lineage is still valid
        assert!(group.verify_lineage());
    }

    #[test]
    fn test_fork_node_recovery() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();

        let mut group = ForkGroup::fork(
            0xAAAA,
            10,
            test_config(2),
            || Box::new(NoopDaemon),
            &sched,
            &reg,
        )
        .unwrap();

        let node = group.members()[0].node_id;
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
    fn test_fork_identities_all_different() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();

        let group = ForkGroup::fork(
            0xAAAA,
            100,
            test_config(5),
            || Box::new(NoopDaemon),
            &sched,
            &reg,
        )
        .unwrap();

        // Each fork should have a unique origin
        let origins: HashSet<u64> = group.members().iter().map(|m| m.origin_hash).collect();
        assert_eq!(origins.len(), 5);

        // Each fork record should have a unique forked_origin
        let forked: HashSet<u64> = group
            .fork_records()
            .iter()
            .map(|r| r.forked_origin)
            .collect();
        assert_eq!(forked.len(), 5);
    }

    #[test]
    fn test_regression_spread_rejects_when_all_nodes_excluded() {
        // Regression: place_with_spread used to silently fall back to an
        // excluded node when all candidates were in the exclusion set,
        // defeating the spread constraint.
        use crate::adapter::net::behavior::fold::{capability_bridge, CapabilityFold, Fold};
        let fold: Arc<Fold<CapabilityFold>> =
            Arc::new(Fold::with_sweep_interval(std::time::Duration::ZERO));
        capability_bridge::apply_legacy_announcement(
            &fold,
            CapabilityAnnouncement::new(
                0x1111,
                crate::adapter::net::identity::EntityId::from_bytes([0u8; 32]),
                1,
                CapabilitySet::new(),
            ),
        )
        .expect("apply legacy announcement in fixture");
        let sched = Scheduler::new(fold, 0x1111, CapabilitySet::new());

        let mut exclude = HashSet::new();
        exclude.insert(0x1111); // exclude the only node

        let result =
            GroupCoordinator::place_with_spread(&sched, &CapabilityFilter::default(), &exclude);
        assert!(
            result.is_err(),
            "must fail when all candidate nodes are excluded"
        );
    }

    // ──────────────────────────────────────────────────────────────────
    // Phase G slice 6 — `*_with_placement` v2 wiring tests for
    // ForkGroup. Mirrors the ReplicaGroup slice 5 coverage.
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
            )
            .expect("apply legacy announcement in fixture");
        }
        let local = node_ids.first().copied().unwrap_or(0xFFFF);
        Scheduler::new(fold, local, CapabilitySet::new())
    }

    /// Permissive placement filter — every candidate scores 1.0.
    struct AllowAll;
    impl PlacementFilter for AllowAll {
        fn placement_score(&self, _: &PlacementNodeId, _: &Artifact<'_>) -> Option<f32> {
            Some(1.0)
        }
    }

    /// `fork_with_placement` produces N forks across distinct nodes
    /// when placement is permissive (parity with `fork`).
    #[test]
    fn fork_with_placement_spreads_across_nodes() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler_and_index(&[0x1111, 0x2222, 0x3333, 0x4444]);
        let tb = TieBreakContext {
            rtt_lookup: None,
            resource_axis: ResourceAxis::Compute,
        };

        let group = ForkGroup::fork_with_placement(
            0xAAAA,
            100,
            test_config(3),
            || Box::new(NoopDaemon),
            &sched,
            &reg,
            &AllowAll,
            &tb,
        )
        .expect("fork_with_placement should succeed with 4 candidate nodes");

        assert_eq!(group.fork_count(), 3);
        assert_eq!(group.health(), GroupHealth::Healthy);
        let node_ids: HashSet<u64> = group.members().iter().map(|m| m.node_id).collect();
        assert_eq!(
            node_ids.len(),
            3,
            "spread invariant: all 3 forks on distinct nodes"
        );
    }

    /// `fork_with_placement` honors a score-based filter — pins
    /// genuine score-awareness on the v2 path. With a filter
    /// preferring 0x4444, the first fork lands there; v1 (`fork`)
    /// would prefer the local node 0x1111.
    #[test]
    fn fork_with_placement_routes_first_fork_to_highest_scorer() {
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

        let group = ForkGroup::fork_with_placement(
            0xAAAA,
            100,
            test_config(1),
            || Box::new(NoopDaemon),
            &sched,
            &reg,
            &PreferHighest,
            &tb,
        )
        .expect("fork_with_placement with 1 fork should succeed");

        assert_eq!(group.members()[0].node_id, 0x4444);
    }

    /// `fork_with_placement` propagates a vetoed-everywhere filter
    /// as `PlacementFailed`; registry stays empty.
    #[test]
    fn fork_with_placement_returns_placement_failed_when_all_vetoed() {
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

        let err = ForkGroup::fork_with_placement(
            0xAAAA,
            100,
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

    /// `scale_to_with_placement` adds forks under v2 and respects
    /// the spread invariant.
    #[test]
    fn scale_to_with_placement_spreads_new_forks() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler_and_index(&[0x1111, 0x2222, 0x3333, 0x4444]);
        let tb = TieBreakContext {
            rtt_lookup: None,
            resource_axis: ResourceAxis::Compute,
        };

        let mut group = ForkGroup::fork_with_placement(
            0xAAAA,
            100,
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

        let node_ids: HashSet<u64> = group.members().iter().map(|m| m.node_id).collect();
        assert_eq!(
            node_ids.len(),
            4,
            "spread invariant under v2: all 4 forks on distinct nodes"
        );
        assert_eq!(reg.count(), 4);
    }

    /// `scale_to_with_placement` scale-down does not invoke the
    /// placement filter — pins that even a VetoAll filter still
    /// lets you shrink the group.
    #[test]
    fn scale_to_with_placement_scale_down_does_not_invoke_filter() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler_and_index(&[0x1111, 0x2222, 0x3333]);
        let tb = TieBreakContext {
            rtt_lookup: None,
            resource_axis: ResourceAxis::Compute,
        };

        let mut group = ForkGroup::fork_with_placement(
            0xAAAA,
            100,
            test_config(3),
            || Box::new(NoopDaemon),
            &sched,
            &reg,
            &AllowAll,
            &tb,
        )
        .unwrap();
        assert_eq!(reg.count(), 3);

        struct VetoAll;
        impl PlacementFilter for VetoAll {
            fn placement_score(&self, _: &PlacementNodeId, _: &Artifact<'_>) -> Option<f32> {
                None
            }
        }

        group
            .scale_to_with_placement(1, || Box::new(NoopDaemon), &sched, &reg, &VetoAll, &tb)
            .expect("scale-down does not invoke the filter");

        assert_eq!(group.fork_count(), 1);
        assert_eq!(reg.count(), 1);
    }

    /// `on_node_failure_with_placement` re-spawns the affected
    /// fork on a different node, preserving the deterministic
    /// origin (same keypair recovered from stored secret).
    #[test]
    fn on_node_failure_with_placement_replaces_fork_on_spare_node() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler_and_index(&[0x1111, 0x2222, 0x3333, 0x4444]);
        let tb = TieBreakContext {
            rtt_lookup: None,
            resource_axis: ResourceAxis::Compute,
        };

        let mut group = ForkGroup::fork_with_placement(
            0xAAAA,
            100,
            test_config(2),
            || Box::new(NoopDaemon),
            &sched,
            &reg,
            &AllowAll,
            &tb,
        )
        .unwrap();

        let failed_node = group.members()[0].node_id;
        let failed_index = group.members()[0].index;
        let failed_origin = group.members()[0].origin_hash;

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
        let new_node = group
            .members()
            .iter()
            .find(|m| m.index == failed_index)
            .unwrap()
            .node_id;
        assert_ne!(new_node, failed_node);
        assert!(reg.contains(failed_origin));
    }

    /// `on_node_failure_with_placement` keeps the slot registered
    /// when no spare node exists — recovery guarantee (#7) holds
    /// under v2.
    #[test]
    fn on_node_failure_with_placement_preserves_slot_when_placement_fails() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler_and_index(&[0x9999]);
        let tb = TieBreakContext {
            rtt_lookup: None,
            resource_axis: ResourceAxis::Compute,
        };

        let mut group = ForkGroup::fork_with_placement(
            0xAAAA,
            100,
            test_config(1),
            || Box::new(NoopDaemon),
            &sched,
            &reg,
            &AllowAll,
            &tb,
        )
        .unwrap();

        let failed_node = group.members()[0].node_id;
        let failed_origin = group.members()[0].origin_hash;
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

    /// `fork` (v1) is unchanged after the v2 surface lands.
    #[test]
    fn fork_v1_path_unchanged_after_v2_added() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();
        let group = ForkGroup::fork(
            0xAAAA,
            100,
            test_config(3),
            || Box::new(NoopDaemon),
            &sched,
            &reg,
        )
        .unwrap();
        assert_eq!(group.fork_count(), 3);
        assert_eq!(group.health(), GroupHealth::Healthy);
    }
}
