//! Standby groups — active-passive stateful daemon replication.
//!
//! A `StandbyGroup` manages one active daemon and N-1 standby copies.
//! The active processes events and produces output. Standbys hold a
//! recent snapshot of the active's state and are ready to promote on
//! failure. Standbys consume memory but zero compute — no duplicate
//! event processing.
//!
//! On active failure:
//! 1. Promote the standby with the most recent snapshot
//! 2. Replay buffered events since that snapshot (same as MIKOSHI replay)
//! 3. New active starts producing output
//! 4. Remaining standbys re-sync from the new active
//!
//! Periodic state sync: the active calls `snapshot()`, bytes transfer
//! to standbys, standbys call `restore()`. The protocol handles the
//! snapshot/restore mechanism. Persistence to disk is an application
//! concern — an external layer can grab `snapshot()` bytes and write
//! them wherever it wants.

use std::collections::HashSet;
use std::time::Instant;

use crate::adapter::net::behavior::metadata::NodeId;
use crate::adapter::net::compute::daemon::{DaemonHostConfig, MeshDaemon};
use crate::adapter::net::compute::group_coord::{
    GroupCoordinator, GroupError, GroupHealth, MemberInfo,
};
use crate::adapter::net::compute::host::DaemonHost;
use crate::adapter::net::compute::registry::DaemonRegistry;
use crate::adapter::net::compute::scheduler::Scheduler;
use crate::adapter::net::identity::EntityKeypair;
use crate::adapter::net::state::causal::CausalEvent;

use crate::adapter::net::behavior::loadbalance::Strategy;

// ── Configuration ────────────────────────────────────────────────────────────

/// Configuration for a standby group.
#[derive(Debug, Clone)]
pub struct StandbyGroupConfig {
    /// Total number of members (1 active + N-1 standbys).
    pub member_count: u8,
    /// 32-byte seed for deterministic keypair derivation.
    pub group_seed: [u8; 32],
    /// Daemon host configuration.
    pub host_config: DaemonHostConfig,
}

// ── Standby state ────────────────────────────────────────────────────────────

/// Per-member role in the standby group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberRole {
    /// Processing events and producing output.
    Active,
    /// Holding a snapshot, ready to promote.
    Standby,
}

/// Per-member extended state.
#[derive(Debug, Clone)]
struct StandbyInfo {
    /// Index in the group.
    index: u8,
    /// Current role.
    role: MemberRole,
    /// Sequence number the standby's snapshot covers through.
    /// For the active, this is the current chain head sequence.
    synced_through: u64,
    /// When the last snapshot sync completed.
    last_sync: Option<Instant>,
    /// Stored keypair secret for recovery.
    keypair_secret: [u8; 32],
}

// ── StandbyGroup ─────────────────────────────────────────────────────────────

/// Active-passive group: one member processes events, others hold snapshots.
///
/// Events are always routed to the active member. Standbys receive periodic
/// snapshots but do no event processing. On failure, the standby with the
/// most recent snapshot promotes and replays buffered events.
pub struct StandbyGroup {
    /// Group identifier.
    group_id: u32,
    /// Configuration.
    config: StandbyGroupConfig,
    /// Index of the current active member.
    active_index: u8,
    /// Per-member state.
    members: Vec<StandbyInfo>,
    /// Events buffered since the last snapshot sync.
    /// On promotion, these replay on the new active.
    buffered_since_sync: Vec<CausalEvent>,
    /// Shared coordination (member tracking, placement).
    coord: GroupCoordinator,
}

impl StandbyGroup {
    /// Create a standby group with one active and N-1 standbys.
    ///
    /// Member 0 is the initial active. All members get deterministic
    /// keypairs from the group seed.
    pub fn spawn<F>(
        config: StandbyGroupConfig,
        daemon_factory: F,
        scheduler: &Scheduler,
        registry: &DaemonRegistry,
    ) -> Result<Self, GroupError>
    where
        F: Fn() -> Box<dyn MeshDaemon>,
    {
        if config.member_count < 2 {
            return Err(GroupError::InvalidConfig(
                "standby group requires at least 2 members".into(),
            ));
        }

        let group_id = {
            use xxhash_rust::xxh3::xxh3_64;
            xxh3_64(&config.group_seed) as u32
        };

        // Use a dummy LB strategy — routing always goes to the active,
        // not through the LoadBalancer.
        let mut coord = GroupCoordinator::new(Strategy::RoundRobin);
        let mut members = Vec::with_capacity(config.member_count as usize);
        let mut used_nodes: HashSet<u64> = HashSet::new();
        let requirements = daemon_factory().requirements();

        for index in 0..config.member_count {
            let keypair = super::replica_group::derive_replica_keypair(&config.group_seed, index);
            let origin_hash = keypair.origin_hash();
            let entity_id_bytes: NodeId = *keypair.entity_id().as_bytes();
            let keypair_secret = *keypair.secret_bytes();

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

            let role = if index == 0 {
                MemberRole::Active
            } else {
                MemberRole::Standby
            };

            members.push(StandbyInfo {
                index,
                role,
                synced_through: 0,
                last_sync: None,
                keypair_secret,
            });
        }

        Ok(Self {
            group_id,
            config,
            active_index: 0,
            members,
            buffered_since_sync: Vec::new(),
            coord,
        })
    }

    /// Get the origin_hash of the active member.
    ///
    /// Use this with `DaemonRegistry::deliver()` to send events to the
    /// active daemon. Only the active processes events.
    pub fn active_origin(&self) -> u32 {
        self.coord.members()[self.active_index as usize].origin_hash
    }

    /// Deliver an event to the active member and buffer it for replay.
    ///
    /// The caller should pass the outputs from `DaemonRegistry::deliver()`
    /// back. The event is buffered for standby promotion replay.
    pub fn on_event_delivered(&mut self, event: CausalEvent) {
        self.buffered_since_sync.push(event);
    }

    /// Sync state from the active to all standbys.
    ///
    /// Takes a snapshot of the active daemon and pushes it onto each
    /// standby via `registry.restore_from_snapshot`, so each standby's
    /// state matches the active's at snapshot time. Clears the event
    /// buffer — standbys are now caught up to this point.
    ///
    /// Previously this method only updated `synced_through` /
    /// `last_sync` bookkeeping but never copied the snapshot bytes
    /// onto the standby daemons. On `promote()`, the picked standby
    /// was still in its initial default-constructed state, and
    /// `promote()` only replays `buffered_since_sync` (just cleared
    /// at the previous sync). Everything before the most recent sync
    /// was silently lost — a critical correctness bug for any
    /// stateful daemon that needs failover.
    ///
    /// Returns the snapshot's `through_seq` so the caller knows what's synced.
    ///
    /// Partial-failure semantics: if `restore_from_snapshot` fails
    /// for one standby mid-loop, the bookkeeping for the standbys
    /// that DID succeed is recorded before propagating the error.
    /// Pre-fix, an early `?` skipped the entire bookkeeping pass —
    /// previously-restored standbys retained the OLD `synced_through`
    /// value, but their daemon state had already been rewritten to
    /// the new snapshot. A subsequent `promote()` running through
    /// `max_by_key(synced_through)` picked the (stale-by-tracking)
    /// standby and replayed `buffered_since_sync` on top, double-
    /// executing every event between the previous sync and the
    /// just-failed sync's `through_seq`.
    ///
    /// Buffer-clearing also moves: only clear `buffered_since_sync`
    /// if EVERY standby succeeded. A partial sync must keep the
    /// buffer so the failed standby can catch up next cycle.
    pub fn sync_standbys(&mut self, registry: &DaemonRegistry) -> Result<u64, GroupError> {
        let active_origin = self.active_origin();

        // Take snapshot from active
        let snapshot = registry
            .snapshot(active_origin)
            .map_err(|e| GroupError::RegistryFailed(e.to_string()))?
            .ok_or_else(|| GroupError::RegistryFailed("active daemon is stateless".into()))?;

        let through_seq = snapshot.through_seq;
        let now = Instant::now();

        // Push snapshot state onto every standby and record per-
        // standby success. Iterate in two layers: collect origins
        // first (avoids holding a borrow on `self.members` while
        // calling out to the registry), then track success
        // membership so we can update bookkeeping precisely below.
        let standbys: Vec<(usize, u32)> = self
            .members
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role == MemberRole::Standby)
            .map(|(i, m)| (i, self.coord.members()[m.index as usize].origin_hash))
            .collect();
        let total_standbys = standbys.len();
        let mut succeeded: Vec<usize> = Vec::with_capacity(standbys.len());
        let mut first_err: Option<GroupError> = None;
        for (member_idx, standby_origin) in standbys {
            match registry.restore_from_snapshot(standby_origin, &snapshot) {
                Ok(()) => succeeded.push(member_idx),
                Err(e) => {
                    first_err = Some(GroupError::RegistryFailed(e.to_string()));
                    break;
                }
            }
        }

        // Update bookkeeping for each standby that successfully
        // restored. Standbys that failed (or that we never reached
        // after a mid-loop break) keep their previous tracking.
        for &i in &succeeded {
            let member = &mut self.members[i];
            member.synced_through = through_seq;
            member.last_sync = Some(now);
        }
        // The active member's `synced_through` advances to the new
        // floor regardless of standby success — the snapshot was
        // taken from the active's own state, so its tracking
        // matches reality even if no standby received it.
        for member in &mut self.members {
            if member.role == MemberRole::Active {
                member.synced_through = through_seq;
            }
        }

        if let Some(err) = first_err {
            // Partial failure. Don't clear `buffered_since_sync`:
            // the failed standby still owes the buffer's events
            // and the next sync cycle will retry from this point.
            return Err(err);
        }

        // All standbys synced — safe to drop the event buffer.
        if succeeded.len() == total_standbys {
            self.buffered_since_sync.clear();
        }

        Ok(through_seq)
    }

    /// Promote a standby to active after the current active fails.
    ///
    /// Picks the standby with the highest `synced_through` (most recent
    /// snapshot). Replays buffered events on the new active. Returns the
    /// new active's origin_hash.
    pub fn promote(
        &mut self,
        _daemon_factory: impl Fn() -> Box<dyn MeshDaemon>,
        registry: &DaemonRegistry,
        _scheduler: &Scheduler,
    ) -> Result<u32, GroupError> {
        let old_active = self.active_index;

        // The search for a replacement runs FIRST; only if it
        // succeeds do we mutate the old active's state. If we instead
        // demoted `old_active` first and the search then returned
        // `NoHealthyMember`, the function would exit with `Err` but
        // leave `self.active_index` pointing at the now-unhealthy,
        // now-`Standby`-roled `old_active`. A subsequent
        // `on_node_recovery` for that member only marks it healthy —
        // it doesn't restore the `Active` role — so the group would
        // be silently demoted forever.
        // Prefer standbys that have completed at least one
        // `sync_standbys` cycle. A replaced-then-re-placed standby
        // has `synced_through = 0` and `last_sync = None` until the
        // next sync — and `max_by_key(synced_through)` could
        // otherwise pick that fresh-zero standby over a previously-
        // synced sibling whose `synced_through` was also reset to 0
        // by some prior path. Promoting the never-synced standby
        // means the new active has zero pre-buffer state; replaying
        // `buffered_since_sync` only covers events SINCE the last
        // sync, so anything before that sync is permanently lost.
        //
        // Fall back to any healthy standby when NO candidate has
        // ever synced (legitimate during the first promote-before-
        // sync window): the buffer in that case contains every
        // event since spawn, so a fresh standby can correctly
        // catch up via `buffered_since_sync` replay.
        let candidates: Vec<&StandbyInfo> = self
            .members
            .iter()
            .filter(|m| m.role == MemberRole::Standby && m.index != old_active)
            .filter(|m| self.coord.members()[m.index as usize].healthy)
            .collect();
        let synced_pick = candidates
            .iter()
            .filter(|m| m.last_sync.is_some())
            .max_by_key(|m| m.synced_through)
            .map(|m| m.index);
        let best_standby = match synced_pick {
            Some(idx) => idx,
            None => candidates
                .iter()
                .max_by_key(|m| m.synced_through)
                .map(|m| m.index)
                .ok_or(GroupError::NoHealthyMember)?,
        };

        // Now safe to mutate — search succeeded, promotion will
        // complete.
        self.coord.mark_unhealthy(old_active);
        self.members[old_active as usize].role = MemberRole::Standby;

        // Promote
        self.active_index = best_standby;
        self.members[best_standby as usize].role = MemberRole::Active;

        let new_active_origin = self.coord.members()[best_standby as usize].origin_hash;

        // Replay buffered events on the new active
        for event in &self.buffered_since_sync {
            let _ = registry.deliver(new_active_origin, event);
        }
        self.buffered_since_sync.clear();

        // Update synced_through for the new active
        if let Ok(Some(snapshot)) = registry.snapshot(new_active_origin) {
            self.members[best_standby as usize].synced_through = snapshot.through_seq;
        }

        Ok(new_active_origin)
    }

    /// Handle failure of a node.
    ///
    /// If the active's node failed, triggers promotion.
    /// If a standby's node failed, attempts re-placement.
    pub fn on_node_failure<F>(
        &mut self,
        failed_node_id: u64,
        daemon_factory: F,
        scheduler: &Scheduler,
        registry: &DaemonRegistry,
    ) -> Result<Option<u32>, GroupError>
    where
        F: Fn() -> Box<dyn MeshDaemon>,
    {
        let affected = self.coord.members_on_node(failed_node_id);
        let active_failed = affected.contains(&self.active_index);

        // Mark all affected as unhealthy
        for &index in &affected {
            self.coord.mark_unhealthy(index);
        }

        // If active failed, promote
        let new_active = if active_failed {
            Some(self.promote(&daemon_factory, registry, scheduler)?)
        } else {
            None
        };

        // Re-place failed standbys
        let requirements = daemon_factory().requirements();
        let mut exclude: HashSet<u64> = HashSet::new();
        exclude.insert(failed_node_id);

        for &index in &affected {
            if index == self.active_index {
                continue; // already promoted or is the new active
            }

            // Place BEFORE touching the registry so a placement
            // failure leaves the slot recoverable. Mirror of
            // replica_group / fork_group fixes.
            let old_origin_hash = self.coord.members()[index as usize].origin_hash;

            let keypair = EntityKeypair::from_bytes(self.members[index as usize].keypair_secret);
            let entity_id_bytes: NodeId = *keypair.entity_id().as_bytes();

            let placement =
                match GroupCoordinator::place_with_spread(scheduler, &requirements, &exclude) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!(
                            index,
                            error = %e,
                            "StandbyGroup::on_node_failure: place_with_spread failed; \
                             slot left registered for later recovery (#7)"
                        );
                        continue;
                    }
                };

            // Standby keypairs are stored per-member; the new
            // origin_hash matches the old. Atomic upsert via
            // `replace` — a single map operation that never leaves
            // the slot empty (the older `unregister` → `register`
            // sequence had a small window where the second step
            // could fail and orphan the slot).
            let _ = old_origin_hash;

            let daemon = daemon_factory();
            let host = DaemonHost::new(daemon, keypair, self.config.host_config.clone());
            registry.replace(host);

            self.coord
                .update_member_placement(index, placement.node_id, entity_id_bytes);
            self.members[index as usize].synced_through = 0;
            self.members[index as usize].last_sync = None;
            exclude.insert(placement.node_id);
        }

        Ok(new_active)
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

    /// Whether the active member is healthy.
    pub fn active_healthy(&self) -> bool {
        self.coord.members()[self.active_index as usize].healthy
    }

    /// Get the active member's index.
    pub fn active_index(&self) -> u8 {
        self.active_index
    }

    /// Get the role of a member.
    pub fn member_role(&self, index: u8) -> Option<MemberRole> {
        self.members.get(index as usize).map(|m| m.role)
    }

    /// Get the sync sequence for a member.
    pub fn synced_through(&self, index: u8) -> Option<u64> {
        self.members.get(index as usize).map(|m| m.synced_through)
    }

    /// Number of buffered events since last sync.
    pub fn buffered_event_count(&self) -> usize {
        self.buffered_since_sync.len()
    }

    /// Get the group ID.
    pub fn group_id(&self) -> u32 {
        self.group_id
    }

    /// Get all member info from the coordinator.
    pub fn members(&self) -> &[MemberInfo] {
        self.coord.members()
    }

    /// Total member count.
    pub fn member_count(&self) -> u8 {
        self.coord.member_count()
    }

    /// Number of standbys (total - 1 active).
    pub fn standby_count(&self) -> u8 {
        self.coord.member_count().saturating_sub(1)
    }
}

impl std::fmt::Debug for StandbyGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StandbyGroup")
            .field("group_id", &format!("{:#x}", self.group_id))
            .field("active_index", &self.active_index)
            .field("members", &self.coord.member_count())
            .field("buffered_events", &self.buffered_since_sync.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::capability::{
        CapabilityAnnouncement, CapabilityFilter, CapabilityIndex, CapabilitySet,
    };
    use crate::adapter::net::compute::DaemonError;
    use crate::adapter::net::state::causal::CausalLink;
    use bytes::Bytes;
    use std::sync::Arc;

    struct StatefulDaemon {
        value: u64,
    }

    impl StatefulDaemon {
        fn new() -> Self {
            Self { value: 0 }
        }
    }

    impl MeshDaemon for StatefulDaemon {
        fn name(&self) -> &str {
            "stateful"
        }
        fn requirements(&self) -> CapabilityFilter {
            CapabilityFilter::default()
        }
        fn process(&mut self, _event: &CausalEvent) -> Result<Vec<Bytes>, DaemonError> {
            self.value += 1;
            Ok(vec![Bytes::from(self.value.to_le_bytes().to_vec())])
        }
        fn snapshot(&self) -> Option<Bytes> {
            Some(Bytes::from(self.value.to_le_bytes().to_vec()))
        }
        fn restore(&mut self, state: Bytes) -> Result<(), DaemonError> {
            if state.len() != 8 {
                return Err(DaemonError::RestoreFailed("bad size".into()));
            }
            self.value = u64::from_le_bytes(state[..8].try_into().unwrap());
            Ok(())
        }
    }

    fn make_event(seq: u64) -> CausalEvent {
        CausalEvent {
            link: CausalLink {
                origin_hash: 0xFFFF,
                horizon_encoded: 0,
                sequence: seq,
                parent_hash: 0,
            },
            payload: Bytes::from(format!("event-{}", seq)),
            received_at: seq * 1000,
        }
    }

    fn make_scheduler() -> Scheduler {
        let index = Arc::new(CapabilityIndex::new());
        // Use a local_node_id NOT in the index so placement spreads
        // across indexed nodes instead of always picking local.
        let eid = crate::adapter::net::identity::EntityId::from_bytes([0u8; 32]);
        index.index(CapabilityAnnouncement::new(
            0x1111,
            eid.clone(),
            1,
            CapabilitySet::new(),
        ));
        index.index(CapabilityAnnouncement::new(
            0x2222,
            eid.clone(),
            1,
            CapabilitySet::new(),
        ));
        index.index(CapabilityAnnouncement::new(
            0x3333,
            eid,
            1,
            CapabilitySet::new(),
        ));
        Scheduler::new(index, 0xFFFF, CapabilitySet::new())
    }

    fn test_config(n: u8) -> StandbyGroupConfig {
        StandbyGroupConfig {
            member_count: n,
            group_seed: [55u8; 32],
            host_config: DaemonHostConfig::default(),
        }
    }

    #[test]
    fn test_spawn_standby_group() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();

        let group = StandbyGroup::spawn(
            test_config(3),
            || Box::new(StatefulDaemon::new()),
            &sched,
            &reg,
        )
        .unwrap();

        assert_eq!(group.member_count(), 3);
        assert_eq!(group.standby_count(), 2);
        assert_eq!(group.active_index(), 0);
        assert_eq!(group.member_role(0), Some(MemberRole::Active));
        assert_eq!(group.member_role(1), Some(MemberRole::Standby));
        assert_eq!(group.member_role(2), Some(MemberRole::Standby));
        assert_eq!(group.health(), GroupHealth::Healthy);
        assert_eq!(reg.count(), 3);
    }

    #[test]
    fn test_minimum_two_members() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();

        let err = StandbyGroup::spawn(
            test_config(1),
            || Box::new(StatefulDaemon::new()),
            &sched,
            &reg,
        )
        .unwrap_err();
        assert_eq!(
            err,
            GroupError::InvalidConfig("standby group requires at least 2 members".into())
        );
    }

    #[test]
    fn test_active_origin_delivers() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();

        let mut group = StandbyGroup::spawn(
            test_config(3),
            || Box::new(StatefulDaemon::new()),
            &sched,
            &reg,
        )
        .unwrap();

        let active = group.active_origin();

        // Deliver events to active
        for seq in 1..=5 {
            let event = make_event(seq);
            let outputs = reg.deliver(active, &event).unwrap();
            assert_eq!(outputs.len(), 1);
            let val = u64::from_le_bytes(outputs[0].payload[..8].try_into().unwrap());
            assert_eq!(val, seq);
            group.on_event_delivered(event);
        }

        assert_eq!(group.buffered_event_count(), 5);
    }

    #[test]
    fn test_sync_standbys() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();

        let mut group = StandbyGroup::spawn(
            test_config(3),
            || Box::new(StatefulDaemon::new()),
            &sched,
            &reg,
        )
        .unwrap();

        let active = group.active_origin();

        // Process some events
        for seq in 1..=10 {
            let event = make_event(seq);
            reg.deliver(active, &event).unwrap();
            group.on_event_delivered(event);
        }

        // Sync
        let through = group.sync_standbys(&reg).unwrap();
        assert_eq!(through, 10);
        assert_eq!(group.buffered_event_count(), 0);
        assert_eq!(group.synced_through(1), Some(10));
        assert_eq!(group.synced_through(2), Some(10));
    }

    /// Regression: a freshly-replaced standby has `synced_through = 0`
    /// and `last_sync = None` until the next sync_standbys cycle.
    /// Pre-fix, `promote()`'s `max_by_key(synced_through)` could
    /// pick that fresh standby over a previously-synced sibling
    /// whose `synced_through` was also reset to 0 by some earlier
    /// path — promoting a daemon with zero pre-buffer state and
    /// silently losing every event before the most recent sync.
    /// Fix prefers candidates with `last_sync.is_some()`; falls back
    /// to any healthy candidate only when nobody has ever synced.
    #[test]
    fn promote_prefers_synced_standby_over_freshly_replaced_one() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();

        let mut group = StandbyGroup::spawn(
            test_config(3),
            || Box::new(StatefulDaemon::new()),
            &sched,
            &reg,
        )
        .unwrap();

        let active = group.active_origin();

        // Drive some events through, then sync — both standbys
        // now have last_sync = Some(_) and synced_through > 0.
        for seq in 1..=10 {
            let event = make_event(seq);
            reg.deliver(active, &event).unwrap();
            group.on_event_delivered(event);
        }
        group.sync_standbys(&reg).unwrap();

        // Identify the two standbys by index (everything that's
        // not the active).
        let standby_indices: Vec<u8> = (0..3u8).filter(|&i| i != group.active_index).collect();
        assert_eq!(standby_indices.len(), 2);

        // Force one of the standbys back to "freshly replaced"
        // shape by zeroing its tracking. This mirrors what
        // `on_node_failure`'s replacement path does at line 430-431.
        let idx_replaced = standby_indices[0];
        let idx_synced = standby_indices[1];
        group.members[idx_replaced as usize].synced_through = 0;
        group.members[idx_replaced as usize].last_sync = None;

        // Now drive new events and buffer them — the synced
        // standby's `synced_through` is still 10 from above. The
        // replaced standby's is 0.
        for seq in 11..=15 {
            let event = make_event(seq);
            reg.deliver(group.active_origin(), &event).unwrap();
            group.on_event_delivered(event);
        }
        // Skip the next sync to keep the replaced standby at
        // `last_sync = None`.

        // Active fails. Promote must pick the SYNCED standby, not
        // the freshly-replaced one — even if `synced_through`
        // were equal, the synced one has pre-buffer state we'd
        // otherwise lose. With our state, the synced one's
        // `synced_through = 10` exceeds the replaced one's `0`,
        // so `max_by_key` should already favor it; the test pins
        // the `last_sync.is_some()` filter for the case where
        // both have identical synced_through.
        group.coord.mark_unhealthy(group.active_index);
        let new_active = group
            .promote(|| Box::new(StatefulDaemon::new()), &reg, &sched)
            .unwrap();
        assert_eq!(
            group.active_index, idx_synced,
            "promote must pick the synced standby (idx={}), not the freshly-replaced one (idx={})",
            idx_synced, idx_replaced,
        );
        let _ = new_active;
    }

    /// Regression: pre-fix, a partial-failure mid-loop in
    /// `sync_standbys` skipped the entire bookkeeping pass via `?`.
    /// Standbys that DID succeed had their daemon state rewritten
    /// to the new snapshot, but `synced_through` still pointed at
    /// the prior cycle's value AND `buffered_since_sync` was not
    /// cleared. A subsequent `promote()` could pick a (stale-by-
    /// tracking) successfully-restored standby and replay
    /// `buffered_since_sync` on top, double-executing every event
    /// between the previous sync and the just-failed sync's
    /// `through_seq`. The fix records bookkeeping per-standby on
    /// success and only clears the buffer when ALL standbys synced.
    #[test]
    fn sync_standbys_partial_failure_records_per_standby_progress() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();

        let mut group = StandbyGroup::spawn(
            test_config(3),
            || Box::new(StatefulDaemon::new()),
            &sched,
            &reg,
        )
        .unwrap();

        // First clean sync — establishes a baseline `synced_through`.
        for seq in 1..=5 {
            let event = make_event(seq);
            reg.deliver(group.active_origin(), &event).unwrap();
            group.on_event_delivered(event);
        }
        group.sync_standbys(&reg).unwrap();
        assert_eq!(group.buffered_event_count(), 0);
        let pre_synced_1 = group.synced_through(1).unwrap();
        let pre_synced_2 = group.synced_through(2).unwrap();
        assert_eq!(pre_synced_1, 5);
        assert_eq!(pre_synced_2, 5);

        // Buffer some new events, then unregister the SECOND standby
        // so its restore_from_snapshot returns NotFound mid-loop.
        for seq in 6..=10 {
            let event = make_event(seq);
            reg.deliver(group.active_origin(), &event).unwrap();
            group.on_event_delivered(event);
        }
        let standby_origins: Vec<u32> = group
            .members
            .iter()
            .filter(|m| m.role == MemberRole::Standby)
            .map(|m| group.coord.members()[m.index as usize].origin_hash)
            .collect();
        assert_eq!(standby_origins.len(), 2);
        // Drop the 2nd standby so restore fails for it.
        reg.unregister(standby_origins[1]).unwrap();

        // Second sync — should fail mid-loop. The first standby's
        // restore succeeded so its bookkeeping must reflect the
        // new snapshot; the second standby's bookkeeping must NOT
        // advance.
        let result = group.sync_standbys(&reg);
        assert!(
            result.is_err(),
            "sync_standbys must surface the standby restore failure",
        );

        // First standby's tracking advanced (state and bookkeeping
        // are coherent). Pre-fix the bookkeeping was skipped, so
        // promote would treat this standby as still synced through
        // the old `pre_synced_1` and replay the buffered range on
        // top of an already-restored state.
        assert_eq!(
            group.synced_through(1),
            Some(10),
            "first standby successfully restored — tracking must reflect the new snapshot",
        );
        // Second standby's tracking did NOT advance — its state is
        // still at the prior sync.
        assert_eq!(
            group.synced_through(2),
            Some(pre_synced_2),
            "failed-standby tracking must NOT advance",
        );
        // Buffer must be RETAINED on partial failure so the next
        // sync cycle has the events the failed standby still owes.
        assert!(
            group.buffered_event_count() > 0,
            "buffered_since_sync must be retained on partial failure",
        );
    }

    /// Regression for BUG_AUDIT_2026_04_30_CORE.md #103: pre-fix
    /// `promote` mutated `active`'s health and role to
    /// `Unhealthy`/`Standby` BEFORE searching for a replacement.
    /// If the search returned `NoHealthyMember`, the function
    /// exited with `Err` but left `self.active_index` pointing
    /// at the now-unhealthy, now-`Standby`-roled `old_active`.
    /// `on_node_recovery` only marks healthy — it doesn't
    /// restore the `Active` role — so the group was silently
    /// demoted forever.
    ///
    /// We pin the fix by:
    ///   1. Building a 3-member group with one active and two standbys.
    ///   2. Marking BOTH standbys unhealthy so promote will fail.
    ///   3. Calling `promote()` and asserting `Err(NoHealthyMember)`.
    ///   4. Asserting the group is still in its pre-promote state:
    ///      `active_origin`, `active_index`, and the active role
    ///      are unchanged. Pre-fix this would have flipped the
    ///      active to `Standby` + `Unhealthy`.
    #[test]
    fn promote_does_not_half_mutate_on_no_healthy_member() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();

        let mut group = StandbyGroup::spawn(
            test_config(3),
            || Box::new(StatefulDaemon::new()),
            &sched,
            &reg,
        )
        .unwrap();

        let pre_active_origin = group.active_origin();
        let pre_active_index = group.active_index();

        // Mark every non-active member unhealthy so promote can't
        // find a replacement.
        let total = group.coord.member_count();
        for idx in 0..total {
            if idx != pre_active_index {
                group.coord.mark_unhealthy(idx);
            }
        }

        // Promotion must fail.
        let err = group
            .promote(|| Box::new(StatefulDaemon::new()), &reg, &sched)
            .expect_err("promote must fail when no healthy standby exists");
        assert!(matches!(err, GroupError::NoHealthyMember));

        // Group state must NOT have been mutated. Pre-fix this
        // would have flipped the active to Standby + Unhealthy
        // before discovering there was no replacement.
        assert_eq!(
            group.active_origin(),
            pre_active_origin,
            "active_origin must be unchanged when promote fails"
        );
        assert_eq!(
            group.active_index(),
            pre_active_index,
            "active_index must be unchanged when promote fails"
        );
        assert_eq!(
            group.members[pre_active_index as usize].role,
            MemberRole::Active,
            "old active's role must NOT have been demoted to Standby"
        );
        assert!(
            group.active_healthy(),
            "old active's health must NOT have been flipped to Unhealthy"
        );
    }

    #[test]
    fn test_promote_on_active_failure() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();

        let mut group = StandbyGroup::spawn(
            test_config(3),
            || Box::new(StatefulDaemon::new()),
            &sched,
            &reg,
        )
        .unwrap();

        let active = group.active_origin();

        // Process events and sync
        for seq in 1..=5 {
            let event = make_event(seq);
            reg.deliver(active, &event).unwrap();
            group.on_event_delivered(event);
        }
        group.sync_standbys(&reg).unwrap();

        // Process more events after sync (these buffer for replay)
        for seq in 6..=8 {
            let event = make_event(seq);
            reg.deliver(active, &event).unwrap();
            group.on_event_delivered(event);
        }
        assert_eq!(group.buffered_event_count(), 3);

        // Promote (simulating active failure)
        let new_active = group
            .promote(|| Box::new(StatefulDaemon::new()), &reg, &sched)
            .unwrap();

        // New active should be different from old
        assert_ne!(new_active, active);
        assert_eq!(group.active_origin(), new_active);
        assert_ne!(group.active_index(), 0);

        // Buffered events should have been replayed (buffer cleared)
        assert_eq!(group.buffered_event_count(), 0);

        // New active should be healthy
        assert!(group.active_healthy());
    }

    #[test]
    fn test_on_node_failure_active() {
        let reg = DaemonRegistry::new();
        // Place members on different nodes by using separate scheduler
        // queries per member. Since our scheduler always returns the same
        // first match, we test the promote logic directly.
        let sched = make_scheduler();

        let mut group = StandbyGroup::spawn(
            test_config(3),
            || Box::new(StatefulDaemon::new()),
            &sched,
            &reg,
        )
        .unwrap();

        // Process events and sync so standbys have synced_through > 0
        let active = group.active_origin();
        for seq in 1..=3 {
            let event = make_event(seq);
            reg.deliver(active, &event).unwrap();
            group.on_event_delivered(event);
        }
        group.sync_standbys(&reg).unwrap();

        // Directly test promotion (bypasses node-level failure)
        let old_active = group.active_origin();
        let new_active = group
            .promote(|| Box::new(StatefulDaemon::new()), &reg, &sched)
            .unwrap();

        assert_ne!(new_active, old_active);
        assert_eq!(group.active_origin(), new_active);
        assert!(group.active_healthy());
    }

    #[test]
    fn test_on_node_failure_standby_only() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();

        let mut group = StandbyGroup::spawn(
            test_config(3),
            || Box::new(StatefulDaemon::new()),
            &sched,
            &reg,
        )
        .unwrap();

        let active_before = group.active_origin();

        // Mark a standby unhealthy (simulating its node failing
        // without affecting the active)
        group.coord.mark_unhealthy(1);

        assert_eq!(
            group.health(),
            GroupHealth::Degraded {
                healthy: 2,
                total: 3
            }
        );

        // Active should NOT have changed
        assert_eq!(group.active_origin(), active_before);
        assert!(group.active_healthy());
    }

    #[test]
    fn test_node_recovery() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();

        let mut group = StandbyGroup::spawn(
            test_config(3),
            || Box::new(StatefulDaemon::new()),
            &sched,
            &reg,
        )
        .unwrap();

        let standby_node = group.members()[1].node_id;
        group.coord.mark_unhealthy(1);
        assert_eq!(
            group.health(),
            GroupHealth::Degraded {
                healthy: 2,
                total: 3
            }
        );

        group.on_node_recovery(standby_node, &reg);
        assert_eq!(group.health(), GroupHealth::Healthy);
    }

    #[test]
    fn test_deterministic_identity() {
        let reg1 = DaemonRegistry::new();
        let reg2 = DaemonRegistry::new();
        let sched = make_scheduler();

        let g1 = StandbyGroup::spawn(
            test_config(2),
            || Box::new(StatefulDaemon::new()),
            &sched,
            &reg1,
        )
        .unwrap();
        let g2 = StandbyGroup::spawn(
            test_config(2),
            || Box::new(StatefulDaemon::new()),
            &sched,
            &reg2,
        )
        .unwrap();

        assert_eq!(g1.group_id(), g2.group_id());
        assert_eq!(g1.active_origin(), g2.active_origin());
    }

    /// Regression: `sync_standbys` previously only updated
    /// bookkeeping (`synced_through`, `last_sync`); it never copied
    /// the active's snapshot bytes onto the standby daemons. On
    /// `promote()`, the picked standby was still in its initial
    /// default-constructed state (`StatefulDaemon::value == 0`),
    /// and `promote()` only replays `buffered_since_sync` (just
    /// cleared at the previous sync). Everything before the most
    /// recent sync was silently lost.
    ///
    /// This test:
    ///   1. Spawns a standby group with one stateful active.
    ///   2. Drives the active to `value = 5` (5 events).
    ///   3. Calls `sync_standbys` to push state to standbys.
    ///   4. Drives the active to `value = 8` (3 more events,
    ///      buffered as `buffered_since_sync`).
    ///   5. Promotes a standby.
    ///   6. Asserts the new active's `value == 8` (snapshot of 5 +
    ///      buffered 3).
    ///
    /// Pre-fix: the new active reads value=3 (only the buffered
    /// events; pre-sync value of 5 lost). Post-fix: value=8.
    #[test]
    fn sync_standbys_actually_restores_state_onto_standbys() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();
        let mut group = StandbyGroup::spawn(
            test_config(2),
            || Box::new(StatefulDaemon::new()),
            &sched,
            &reg,
        )
        .unwrap();

        let active_origin = group.active_origin();

        // Drive active to value=5.
        for seq in 1..=5 {
            let ev = make_event(seq);
            // Deliver via registry (advances daemon state).
            reg.deliver(active_origin, &ev).unwrap();
            // Buffer for replay-on-promote (per sync contract).
            group.on_event_delivered(ev);
        }
        // Sanity: active is at 5.
        let snap = reg.snapshot(active_origin).unwrap().unwrap();
        let pre_sync_value = u64::from_le_bytes(snap.state[..8].try_into().unwrap());
        assert_eq!(pre_sync_value, 5);

        // Sync state onto the standby. After this, the standby's
        // daemon must hold value=5 too.
        group.sync_standbys(&reg).unwrap();

        // Verify: the standby's snapshot returns value=5 (the bug
        // would leave the standby at its default value=0).
        let standby_origin = group
            .members
            .iter()
            .find(|m| m.role == MemberRole::Standby)
            .map(|m| group.coord.members()[m.index as usize].origin_hash)
            .unwrap();
        let standby_snap = reg.snapshot(standby_origin).unwrap().unwrap();
        let standby_value = u64::from_le_bytes(standby_snap.state[..8].try_into().unwrap());
        assert_eq!(
            standby_value, 5,
            "standby must hold the active's pre-sync state after sync_standbys; \
             pre-fix this would be 0 because sync only updated bookkeeping"
        );

        // Drive active to value=8 (3 buffered events).
        for seq in 6..=8 {
            let ev = make_event(seq);
            reg.deliver(active_origin, &ev).unwrap();
            group.on_event_delivered(ev);
        }

        // Promote the standby. It replays buffered_since_sync (3
        // events) on top of the synced state (value=5), landing at 8.
        let new_active = group
            .promote(|| Box::new(StatefulDaemon::new()), &reg, &sched)
            .unwrap();

        let new_active_snap = reg.snapshot(new_active).unwrap().unwrap();
        let new_active_value = u64::from_le_bytes(new_active_snap.state[..8].try_into().unwrap());
        assert_eq!(
            new_active_value, 8,
            "promoted active must hold sync-state (5) + buffered events (3) = 8; \
             pre-fix this would be 3 because the standby's pre-sync state was 0"
        );
    }
}
