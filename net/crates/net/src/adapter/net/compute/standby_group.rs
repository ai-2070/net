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
use crate::adapter::net::behavior::placement::{Artifact, PlacementFilter, TieBreakContext};
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
    /// X-1 epoch — bumped on every `promote` /
    /// `promote_with_placement`. The first active starts at 1; each
    /// subsequent promotion increments. The intended use is
    /// cross-node fencing: routed events should carry the issuing
    /// active's `term`, and a receiver that observes a strictly-
    /// higher `term` than its own demotes to standby. The cross-
    /// node wire integration is a separate change; this counter is
    /// the local scaffolding it builds on, and `term()` exposes it
    /// to operators / future wire layers.
    term: u64,
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
            // Spawn-time active is term 1; every subsequent
            // `promote` increments.
            term: 1,
        })
    }

    /// Current epoch counter. Bumped on every successful
    /// `promote` / `promote_with_placement`. Cross-node fencing
    /// (X-1) consumes this to reject events from a stale active
    /// after a partition heal; the wire integration is a
    /// separate change.
    pub fn term(&self) -> u64 {
        self.term
    }

    /// Get the origin_hash of the active member.
    ///
    /// Use this with `DaemonRegistry::deliver()` to send events to the
    /// active daemon. Only the active processes events.
    pub fn active_origin(&self) -> u64 {
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
        let standbys: Vec<(usize, u64)> = self
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
    ) -> Result<u64, GroupError> {
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
        // X-1 epoch bump. Every promotion advances the term; a
        // future wire-fencing layer compares an incoming event's
        // term against the receiver's recorded term and rejects
        // strictly-lower-term events (stale active still
        // emitting after partition heal) or self-demotes on a
        // strictly-higher-term observation. `saturating_add`
        // pins the post-u64::MAX case at the cap rather than
        // wrapping — an astronomical precondition, but cheap to
        // bound.
        self.term = self.term.saturating_add(1);

        let new_active_origin = self.coord.members()[best_standby as usize].origin_hash;

        // Replay buffered events on the new active — but only the
        // ones strictly above the new active's last `synced_through`.
        //
        // `buffered_since_sync` is preserved verbatim across partial
        // sync rounds so a failed standby can catch up next cycle
        // (line ~286); succeeded standbys have their
        // `synced_through` advanced to the snapshot's through_seq
        // but the buffer keeps its older entries. Without the
        // filter, a succeeded-then-promoted standby has already
        // applied events in `[old_synced_through, new_synced_through]`
        // via the snapshot — replaying the full buffer here re-
        // invokes the daemon's `on_event` for each, doubling
        // counters, re-issuing idempotency keys, and re-firing
        // side effects. The new sequence-aware filter restricts
        // the replay to `event.link.sequence > synced_through` so
        // every event lands on the promoted daemon exactly once
        // (covered by the snapshot's pre-state, then extended by
        // the post-snapshot tail). Per-event check is cheap; the
        // buffer is bounded by the sync cadence.
        let synced_through = self.members[best_standby as usize].synced_through;
        for event in &self.buffered_since_sync {
            if event.link.sequence > synced_through {
                let _ = registry.deliver(new_active_origin, event);
            }
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
    ) -> Result<Option<u64>, GroupError>
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

    /// Phase G slice 7 — `spawn` with score-based placement. Routes
    /// every member's placement decision through
    /// [`GroupCoordinator::place_member`].
    ///
    /// Member 0 is still the initial active; remaining members are
    /// standbys. Spread invariant preserved (each member on a
    /// distinct node).
    pub fn spawn_with_placement<F>(
        config: StandbyGroupConfig,
        daemon_factory: F,
        scheduler: &Scheduler,
        registry: &DaemonRegistry,
        placement: &dyn PlacementFilter,
        tie_break: &TieBreakContext<'_>,
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

        let mut coord = GroupCoordinator::new(Strategy::RoundRobin);
        let mut members = Vec::with_capacity(config.member_count as usize);
        let mut used_nodes: HashSet<u64> = HashSet::new();

        // Capture the daemon's capability surface once.
        let prototype = daemon_factory();
        let requirements = prototype.requirements();
        let required = prototype.required_capabilities();
        let optional = prototype.optional_capabilities();
        drop(prototype);

        for index in 0..config.member_count {
            let keypair = super::replica_group::derive_replica_keypair(&config.group_seed, index);
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
            term: 1,
        })
    }

    /// Phase G slice 7 — `promote` with score-based standby
    /// selection.
    ///
    /// Preserves v1's recovery-correctness contract: standbys with
    /// the most recent sync (`last_sync.is_some()` AND highest
    /// `synced_through`) are still preferred. The placement filter
    /// (with the LOCKED §7 tie-breaker) breaks ties AMONG
    /// equivalently-fresh standbys — does NOT override data
    /// freshness, because promoting a less-fresh standby would
    /// mean replaying the `buffered_since_sync` window over a
    /// stale base and silently losing pre-buffer state.
    ///
    /// Falls back to all healthy standbys (placement-scored) when
    /// no candidate has ever synced (legitimate during the first
    /// promote-before-sync window — `buffered_since_sync` covers
    /// every event since spawn in that case).
    ///
    /// Returns the new active's origin_hash, same as
    /// [`Self::promote`].
    pub fn promote_with_placement<F>(
        &mut self,
        daemon_factory: F,
        registry: &DaemonRegistry,
        scheduler: &Scheduler,
        placement: &dyn PlacementFilter,
        tie_break: &TieBreakContext<'_>,
    ) -> Result<u64, GroupError>
    where
        F: Fn() -> Box<dyn MeshDaemon>,
    {
        let old_active = self.active_index;

        // Search runs FIRST; only on success do we mutate state —
        // same half-mutation safety as v1 `promote`.
        let healthy_standbys: Vec<u8> = self
            .members
            .iter()
            .filter(|m| m.role == MemberRole::Standby && m.index != old_active)
            .filter(|m| self.coord.members()[m.index as usize].healthy)
            .map(|m| m.index)
            .collect();

        if healthy_standbys.is_empty() {
            return Err(GroupError::NoHealthyMember);
        }

        // Synced_through pre-filter: among synced standbys, keep
        // those at the maximum `synced_through`. Fall back to all
        // healthy standbys when no candidate has ever synced.
        let max_synced = healthy_standbys
            .iter()
            .filter(|&&idx| self.members[idx as usize].last_sync.is_some())
            .map(|&idx| self.members[idx as usize].synced_through)
            .max();

        let roster: Vec<u8> = match max_synced {
            Some(max) => healthy_standbys
                .iter()
                .copied()
                .filter(|&idx| {
                    self.members[idx as usize].last_sync.is_some()
                        && self.members[idx as usize].synced_through == max
                })
                .collect(),
            None => healthy_standbys.clone(),
        };

        // Score the roster via placement filter; LOCKED §7 breaks
        // ties (RTT → free-resource → lex-NodeId).
        let prototype = daemon_factory();
        let required = prototype.required_capabilities();
        let optional = prototype.optional_capabilities();
        drop(prototype);

        let artifact = Artifact::Daemon {
            daemon_id: [0u8; 32],
            required: &required,
            optional: &optional,
        };

        let candidate_node_ids: Vec<u64> = roster
            .iter()
            .map(|&idx| self.coord.members()[idx as usize].node_id)
            .collect();

        let chosen_node = scheduler
            .select_promotion_target(candidate_node_ids, &artifact, placement, tie_break)
            .ok_or(GroupError::NoHealthyMember)?;

        // Map node_id back to member index — spread invariant
        // guarantees uniqueness, but use `find` defensively.
        let best_standby = roster
            .iter()
            .copied()
            .find(|&idx| self.coord.members()[idx as usize].node_id == chosen_node)
            .ok_or(GroupError::NoHealthyMember)?;

        // Mutation flow mirrors v1.
        self.coord.mark_unhealthy(old_active);
        self.members[old_active as usize].role = MemberRole::Standby;

        self.active_index = best_standby;
        self.members[best_standby as usize].role = MemberRole::Active;
        // X-1 epoch bump — see promote() above for rationale.
        self.term = self.term.saturating_add(1);

        let new_active_origin = self.coord.members()[best_standby as usize].origin_hash;

        // Filter the replay to events strictly past the new
        // active's `synced_through`. See the equivalent block in
        // `promote` for the partial-sync corruption it prevents:
        // a succeeded-then-promoted standby has already applied
        // events up to `synced_through` via the snapshot, and
        // replaying the full buffer here would double every
        // counter / idempotency key / side effect inside that
        // already-applied range.
        let synced_through = self.members[best_standby as usize].synced_through;
        for event in &self.buffered_since_sync {
            if event.link.sequence > synced_through {
                let _ = registry.deliver(new_active_origin, event);
            }
        }
        self.buffered_since_sync.clear();

        if let Ok(Some(snapshot)) = registry.snapshot(new_active_origin) {
            self.members[best_standby as usize].synced_through = snapshot.through_seq;
        }

        Ok(new_active_origin)
    }

    /// Phase G slice 7 — `on_node_failure` with score-based
    /// placement. Active failure triggers
    /// [`Self::promote_with_placement`]; standby re-placement
    /// routes through [`GroupCoordinator::place_member`].
    pub fn on_node_failure_with_placement<F>(
        &mut self,
        failed_node_id: u64,
        daemon_factory: F,
        scheduler: &Scheduler,
        registry: &DaemonRegistry,
        placement: &dyn PlacementFilter,
        tie_break: &TieBreakContext<'_>,
    ) -> Result<Option<u64>, GroupError>
    where
        F: Fn() -> Box<dyn MeshDaemon>,
    {
        let affected = self.coord.members_on_node(failed_node_id);
        let active_failed = affected.contains(&self.active_index);

        for &index in &affected {
            self.coord.mark_unhealthy(index);
        }

        let new_active = if active_failed {
            Some(self.promote_with_placement(
                &daemon_factory,
                registry,
                scheduler,
                placement,
                tie_break,
            )?)
        } else {
            None
        };

        let prototype = daemon_factory();
        let requirements = prototype.requirements();
        let required = prototype.required_capabilities();
        let optional = prototype.optional_capabilities();
        drop(prototype);

        let mut exclude: HashSet<u64> = HashSet::new();
        exclude.insert(failed_node_id);

        for &index in &affected {
            if index == self.active_index {
                continue;
            }

            let keypair = EntityKeypair::from_bytes(self.members[index as usize].keypair_secret);
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
                        "StandbyGroup::on_node_failure_with_placement: place_member failed; \
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
            self.members[index as usize].synced_through = 0;
            self.members[index as usize].last_sync = None;
            exclude.insert(decision.node_id);
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

    /// Retry placement against the current healthy node pool for
    /// every member slot currently marked unhealthy. Caps at
    /// `MAX_RECOVERIES_PER_TICK` so a pathological "every slot
    /// unhealthy" state makes progress without wedging the caller.
    /// Returns the slot indices that were successfully placed.
    ///
    /// Skips `active_index` even when it is marked unhealthy: a
    /// blank `DaemonHost::new` here would `registry.replace` the
    /// live active and wipe its committed state. An unhealthy
    /// active requires `promote` (which transfers the latest
    /// standby snapshot + replays buffered events), not slot
    /// re-placement.
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
        let active_index = self.active_index;
        let unhealthy: Vec<u8> = self
            .coord
            .members()
            .iter()
            .filter(|m| !m.healthy && m.index != active_index)
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
            let keypair = EntityKeypair::from_bytes(self.members[index as usize].keypair_secret);
            let entity_id_bytes: NodeId = *keypair.entity_id().as_bytes();

            let placement =
                match GroupCoordinator::place_with_spread(scheduler, &requirements, &exclude) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::trace!(
                            index,
                            error = %e,
                            "StandbyGroup::try_recover: place_with_spread still failing; \
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
            self.members[index as usize].synced_through = 0;
            self.members[index as usize].last_sync = None;
            exclude.insert(placement.node_id);
            recovered.push(index);
        }

        // X-1 epoch bump on any successful slot re-placement, matching
        // `ForkGroup::try_recover_inner` / `ReplicaGroup::try_recover_inner`.
        // A standby placed on a new node here is a membership change;
        // peers that still believe the slot lives on its old node must
        // observe a higher term so they can refresh routing before the
        // fenced wire-layer rejects their stale-term snapshot syncs.
        // No-op if every placement attempt failed.
        if !recovered.is_empty() {
            self.term = self.term.saturating_add(1);
        }
        recovered
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

impl crate::adapter::net::compute::UnhealthySlotRecovery for StandbyGroup {
    fn has_unhealthy_slots(&self) -> bool {
        // Only standby slots are recoverable here; an unhealthy
        // active must go through `promote`, not slot re-placement
        // (see `try_recover_inner`). Returning `true` for an
        // unhealthy-active-only state would make the recovery
        // tick fire repeatedly with nothing to do, since the
        // active slot is skipped inside `try_recover`.
        let active_index = self.active_index;
        self.coord
            .members()
            .iter()
            .any(|m| !m.healthy && m.index != active_index)
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
        CapabilityAnnouncement, CapabilityFilter, CapabilitySet,
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
        use crate::adapter::net::behavior::fold::{capability_bridge, CapabilityFold, Fold};
        let fold: Arc<Fold<CapabilityFold>> =
            Arc::new(Fold::with_sweep_interval(std::time::Duration::ZERO));
        // Use a local_node_id NOT in the index so placement spreads
        // across indexed nodes instead of always picking local.
        let eid = crate::adapter::net::identity::EntityId::from_bytes([0u8; 32]);
        for node_id in [0x1111u64, 0x2222, 0x3333] {
            capability_bridge::apply_legacy_announcement(
                &fold,
                CapabilityAnnouncement::new(node_id, eid.clone(), 1, CapabilitySet::new()),
            );
        }
        Scheduler::new(fold, 0xFFFF, CapabilitySet::new())
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
        let standby_origins: Vec<u64> = group
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

    /// X-1 epoch scaffolding: every successful `promote` bumps
    /// the `term` counter. A future cross-node fencing layer uses
    /// this to reject events from a stale active after a
    /// partition heal; this test pins the local bump semantic.
    #[test]
    fn promote_bumps_term_counter() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();
        let mut group = StandbyGroup::spawn(
            test_config(3),
            || Box::new(StatefulDaemon::new()),
            &sched,
            &reg,
        )
        .unwrap();

        // Spawn-time active is term 1.
        assert_eq!(group.term(), 1);

        // First promote → term 2.
        let _ = group
            .promote(|| Box::new(StatefulDaemon::new()), &reg, &sched)
            .expect("first promote");
        assert_eq!(group.term(), 2);

        // Second promote → term 3. Drives the term advancement
        // documented for partition-heal fencing.
        let _ = group
            .promote(|| Box::new(StatefulDaemon::new()), &reg, &sched)
            .expect("second promote");
        assert_eq!(group.term(), 3);
    }

    /// X-19 regression: a `promote` that picks a succeeded standby
    /// after a partial `sync_standbys` must NOT replay buffered
    /// events that the chosen standby already received via the
    /// snapshot. The pre-fix path replayed the entire buffer
    /// unconditionally, doubling every side-effect inside the
    /// `[old_synced_through, new_synced_through]` range — silent
    /// state corruption on the promoted daemon.
    #[test]
    fn promote_does_not_double_apply_events_within_synced_range() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler();

        let mut group = StandbyGroup::spawn(
            test_config(3),
            || Box::new(StatefulDaemon::new()),
            &sched,
            &reg,
        )
        .unwrap();

        // Baseline: drive 5 events through the active and sync
        // both standbys cleanly. Each successful event bumps
        // StatefulDaemon::value by 1, so the active's value is 5
        // and both standbys' snapshots restore to value=5.
        for seq in 1..=5 {
            let event = make_event(seq);
            reg.deliver(group.active_origin(), &event).unwrap();
            group.on_event_delivered(event);
        }
        group.sync_standbys(&reg).unwrap();

        // Buffer events 6..=10 on the active, then drop standby 2
        // so its restore fails inside the next sync — exactly the
        // partial-failure shape the audit calls out.
        for seq in 6..=10 {
            let event = make_event(seq);
            reg.deliver(group.active_origin(), &event).unwrap();
            group.on_event_delivered(event);
        }
        let standby_2_origin = group
            .members
            .iter()
            .find(|m| m.role == MemberRole::Standby && m.index == 2)
            .map(|m| group.coord.members()[m.index as usize].origin_hash)
            .expect("standby 2");
        reg.unregister(standby_2_origin).unwrap();

        // Partial sync: standby 1 receives the snapshot (value=10
        // post-restore), standby 2 errors out mid-loop. Buffer is
        // retained so a future cycle can still recover standby 2.
        let _ = group.sync_standbys(&reg);
        assert_eq!(group.synced_through(1), Some(10));
        assert!(group.buffered_event_count() > 0);

        // Promote — must pick standby 1 (highest synced_through).
        let new_active = group
            .promote(|| Box::new(StatefulDaemon::new()), &reg, &sched)
            .expect("promote should pick the succeeded standby");

        // With the fix: every buffered event has sequence ≤ 10
        // (the new active's synced_through), so all are filtered.
        // StatefulDaemon::value stays at 10. Pre-fix the replay
        // unconditionally fired `process` for each of events
        // 6..=10, pushing value to 15.
        let value = reg
            .with_host(new_active, |host| {
                let snap = host.take_snapshot().expect("snapshot");
                u64::from_le_bytes(snap.state[..8].try_into().expect("8 bytes"))
            })
            .expect("with_host");
        assert_eq!(
            value, 10,
            "promote must not double-apply events already in the new active's snapshot"
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

    // ──────────────────────────────────────────────────────────────────
    // Phase G slice 7 — `*_with_placement` v2 wiring tests for
    // StandbyGroup. Mirror slice 5 / 6 coverage plus the
    // promote-specific contract: data freshness still wins over
    // placement score (recovery correctness preserved).
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
            );
        }
        // Use a local_node_id NOT in the index so placement spreads
        // across indexed nodes instead of always picking local.
        Scheduler::new(fold, 0xFFFF, CapabilitySet::new())
    }

    /// Permissive placement filter — every candidate scores 1.0.
    struct AllowAll;
    impl PlacementFilter for AllowAll {
        fn placement_score(&self, _: &PlacementNodeId, _: &Artifact<'_>) -> Option<f32> {
            Some(1.0)
        }
    }

    /// `spawn_with_placement` produces N members across distinct
    /// nodes when placement is permissive.
    #[test]
    fn spawn_with_placement_spreads_across_nodes() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler_and_index(&[0x1111, 0x2222, 0x3333]);
        let tb = TieBreakContext {
            rtt_lookup: None,
            resource_axis: ResourceAxis::Compute,
        };

        let group = StandbyGroup::spawn_with_placement(
            test_config(3),
            || Box::new(StatefulDaemon::new()),
            &sched,
            &reg,
            &AllowAll,
            &tb,
        )
        .expect("spawn_with_placement should succeed with 3 candidate nodes");

        assert_eq!(group.member_count(), 3);
        assert_eq!(group.standby_count(), 2);
        assert_eq!(group.active_index(), 0);
        assert_eq!(group.health(), GroupHealth::Healthy);
        let node_ids: HashSet<u64> = group.coord.members().iter().map(|m| m.node_id).collect();
        assert_eq!(
            node_ids.len(),
            3,
            "spread invariant: all 3 members on distinct nodes"
        );
    }

    /// `spawn_with_placement` rejects vetoed-everywhere filter.
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

        let err = StandbyGroup::spawn_with_placement(
            test_config(2),
            || Box::new(StatefulDaemon::new()),
            &sched,
            &reg,
            &VetoAll,
            &tb,
        )
        .expect_err("VetoAll filter should make placement fail");

        assert!(matches!(err, GroupError::PlacementFailed(_)));
    }

    /// `promote_with_placement` STILL prefers the most-synced
    /// standby — placement score does NOT override data freshness.
    /// Pin the recovery-correctness invariant: even with a filter
    /// that pegs the LESS-synced standby's node to 1.0 and the
    /// most-synced standby's node to 0.0, promote_with_placement
    /// MUST pick the most-synced standby.
    #[test]
    fn promote_with_placement_prefers_synced_standby_over_higher_score() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler_and_index(&[0x1111, 0x2222, 0x3333]);
        let tb = TieBreakContext {
            rtt_lookup: None,
            resource_axis: ResourceAxis::Compute,
        };

        let mut group = StandbyGroup::spawn_with_placement(
            test_config(3),
            || Box::new(StatefulDaemon::new()),
            &sched,
            &reg,
            &AllowAll,
            &tb,
        )
        .unwrap();

        // Sync standbys so synced_through > 0 for all.
        group
            .sync_standbys(&reg)
            .expect("sync standbys before promote");

        // Manually mark only standby index 1 as freshly synced;
        // standby index 2 is reset to "never synced" — so the
        // promote roster is just standby 1.
        group.members[2].last_sync = None;
        group.members[2].synced_through = 0;

        let standby_1_node = group.coord.members()[1].node_id;
        let standby_2_node = group.coord.members()[2].node_id;

        // Filter prefers standby 2's node (the never-synced one).
        // Even so, promote_with_placement MUST pick standby 1 —
        // the synced one — because data freshness is the primary
        // signal, not placement score.
        struct PreferUnsynced {
            preferred_node: u64,
        }
        impl PlacementFilter for PreferUnsynced {
            fn placement_score(&self, t: &PlacementNodeId, _: &Artifact<'_>) -> Option<f32> {
                Some(if *t == self.preferred_node { 1.0 } else { 0.1 })
            }
        }
        let filter = PreferUnsynced {
            preferred_node: standby_2_node,
        };

        let new_active_origin = group
            .promote_with_placement(
                || Box::new(StatefulDaemon::new()),
                &reg,
                &sched,
                &filter,
                &tb,
            )
            .unwrap();

        // Standby 1 (synced) is now the active.
        assert_eq!(group.active_index(), 1);
        assert_eq!(group.member_role(1), Some(MemberRole::Active));
        let new_active_node = group.coord.members()[1].node_id;
        assert_eq!(
            new_active_node, standby_1_node,
            "synced standby on node {standby_1_node:#x} promoted, NOT unsynced standby on node {standby_2_node:#x}"
        );
        assert_eq!(new_active_origin, group.coord.members()[1].origin_hash);
    }

    /// `promote_with_placement` uses the placement filter to break
    /// ties AMONG equivalently-synced standbys. With two standbys
    /// at the same synced_through, the placement filter picks the
    /// higher-scoring one.
    #[test]
    fn promote_with_placement_breaks_ties_by_score_among_equivalently_synced() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler_and_index(&[0x1111, 0x2222, 0x3333]);
        let tb = TieBreakContext {
            rtt_lookup: None,
            resource_axis: ResourceAxis::Compute,
        };

        let mut group = StandbyGroup::spawn_with_placement(
            test_config(3),
            || Box::new(StatefulDaemon::new()),
            &sched,
            &reg,
            &AllowAll,
            &tb,
        )
        .unwrap();

        // Sync all standbys → both standbys (idx 1, 2) at the
        // same synced_through.
        group.sync_standbys(&reg).expect("sync standbys");

        let standby_1_node = group.coord.members()[1].node_id;
        let standby_2_node = group.coord.members()[2].node_id;
        assert_ne!(standby_1_node, standby_2_node);

        // Filter prefers standby 2's node — should be picked
        // because the two standbys are equivalently fresh, so
        // placement score is the deciding signal.
        struct PreferNode {
            preferred_node: u64,
        }
        impl PlacementFilter for PreferNode {
            fn placement_score(&self, t: &PlacementNodeId, _: &Artifact<'_>) -> Option<f32> {
                Some(if *t == self.preferred_node { 1.0 } else { 0.1 })
            }
        }
        let filter = PreferNode {
            preferred_node: standby_2_node,
        };

        group
            .promote_with_placement(
                || Box::new(StatefulDaemon::new()),
                &reg,
                &sched,
                &filter,
                &tb,
            )
            .unwrap();

        assert_eq!(group.active_index(), 2);
        let new_active_node = group.coord.members()[2].node_id;
        assert_eq!(
            new_active_node, standby_2_node,
            "when standbys are equivalently fresh, placement filter picks the highest scorer"
        );
    }

    /// `promote_with_placement` returns `NoHealthyMember` when no
    /// healthy standby exists, and DOES NOT mutate the group's
    /// active state — same half-mutation safety as v1 promote.
    #[test]
    fn promote_with_placement_does_not_half_mutate_on_no_healthy_member() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler_and_index(&[0x1111, 0x2222, 0x3333]);
        let tb = TieBreakContext {
            rtt_lookup: None,
            resource_axis: ResourceAxis::Compute,
        };

        let mut group = StandbyGroup::spawn_with_placement(
            test_config(3),
            || Box::new(StatefulDaemon::new()),
            &sched,
            &reg,
            &AllowAll,
            &tb,
        )
        .unwrap();

        let active_origin_pre = group.active_origin();
        let active_index_pre = group.active_index();

        // Mark every standby unhealthy.
        for idx in 1..group.member_count() {
            group.coord.mark_unhealthy(idx);
        }

        let err = group
            .promote_with_placement(
                || Box::new(StatefulDaemon::new()),
                &reg,
                &sched,
                &AllowAll,
                &tb,
            )
            .expect_err("no healthy standby → NoHealthyMember");
        assert_eq!(err, GroupError::NoHealthyMember);

        assert_eq!(
            group.active_origin(),
            active_origin_pre,
            "active_origin must be unchanged when promote_with_placement fails"
        );
        assert_eq!(
            group.active_index(),
            active_index_pre,
            "active_index must be unchanged when promote_with_placement fails"
        );
        assert_eq!(
            group.member_role(active_index_pre),
            Some(MemberRole::Active)
        );
    }

    /// `on_node_failure_with_placement` triggers
    /// `promote_with_placement` on active failure and re-places
    /// failed standbys via `place_member`.
    #[test]
    fn on_node_failure_with_placement_promotes_active_and_replaces_standby() {
        let reg = DaemonRegistry::new();
        let sched = make_scheduler_and_index(&[0x1111, 0x2222, 0x3333]);
        let tb = TieBreakContext {
            rtt_lookup: None,
            resource_axis: ResourceAxis::Compute,
        };

        let mut group = StandbyGroup::spawn_with_placement(
            test_config(3),
            || Box::new(StatefulDaemon::new()),
            &sched,
            &reg,
            &AllowAll,
            &tb,
        )
        .unwrap();
        let active_node = group.coord.members()[group.active_index() as usize].node_id;

        let new_active = group
            .on_node_failure_with_placement(
                active_node,
                || Box::new(StatefulDaemon::new()),
                &sched,
                &reg,
                &AllowAll,
                &tb,
            )
            .unwrap();

        assert!(
            new_active.is_some(),
            "active failure → promote returns Some"
        );
        assert_ne!(group.active_index(), 0, "active is no longer index 0");
    }

    /// Regression: `try_recover` must skip the active slot even when
    /// `coord.mark_unhealthy(active_index)` is set. Pre-fix the
    /// recovery path constructed a fresh `DaemonHost::new` and
    /// `registry.replace`d the live active, wiping committed state
    /// and resetting `synced_through` / `last_sync`. Active recovery
    /// belongs to `promote`, not slot re-placement; the recovery
    /// trait must also report `has_unhealthy_slots() == false` when
    /// only the active is unhealthy, so the meshos tick doesn't busy-
    /// loop calling `try_recover` with nothing to do.
    #[test]
    fn try_recover_skips_unhealthy_active_and_preserves_state() {
        use crate::adapter::net::compute::UnhealthySlotRecovery;

        let reg = DaemonRegistry::new();
        let sched = make_scheduler();

        let mut group = StandbyGroup::spawn(
            test_config(3),
            || Box::new(StatefulDaemon::new()),
            &sched,
            &reg,
        )
        .unwrap();

        // Advance the active's state so a clobber is detectable.
        let active_origin_before = group.active_origin();
        for seq in 1..=5 {
            let event = make_event(seq);
            reg.deliver(active_origin_before, &event).unwrap();
            group.on_event_delivered(event);
        }
        group.sync_standbys(&reg).unwrap();
        let snapshot_before = reg.snapshot(active_origin_before).unwrap().unwrap();

        // Simulate active node being briefly flagged unhealthy.
        let active_index = group.active_index();
        group.coord.mark_unhealthy(active_index);

        // Recovery probe must NOT advertise work when only the active
        // is unhealthy — promote, not recover, is the right tool.
        assert!(
            !group.has_unhealthy_slots(),
            "has_unhealthy_slots must skip the active slot; only standby slots are recoverable",
        );

        let recovered = group.try_recover(&sched, &reg, &|| Box::new(StatefulDaemon::new()));
        assert!(
            !recovered.contains(&active_index),
            "try_recover must NOT include the active index in the recovered set",
        );

        // Active's daemon-host state is untouched: same origin, same
        // serialized snapshot bytes.
        assert_eq!(
            group.active_origin(),
            active_origin_before,
            "active origin must be unchanged after a try_recover on the active slot",
        );
        let snapshot_after = reg.snapshot(active_origin_before).unwrap().unwrap();
        assert_eq!(
            snapshot_before.state, snapshot_after.state,
            "active's daemon state must be preserved; try_recover must not replace the live active",
        );

        // The standby slots remain healthy too, so no spurious work.
        assert_eq!(
            group.members().iter().filter(|m| m.healthy).count(),
            2,
            "two standbys still healthy",
        );
    }

    /// Regression: every successful standby-slot re-placement in
    /// `try_recover` must bump `term`, matching
    /// `ForkGroup::try_recover_inner` / `ReplicaGroup::try_recover_inner`.
    /// Pre-fix the bump was missing — a sibling that still routed
    /// snapshot syncs to the slot's old node would never observe a
    /// higher term and would keep targeting the wrong node.
    #[test]
    fn try_recover_bumps_term_on_standby_replacement() {
        use crate::adapter::net::compute::UnhealthySlotRecovery;

        let reg = DaemonRegistry::new();
        let sched = make_scheduler();
        let mut group = StandbyGroup::spawn(
            test_config(3),
            || Box::new(StatefulDaemon::new()),
            &sched,
            &reg,
        )
        .unwrap();
        let term_before = group.term();

        // Mark a standby unhealthy so try_recover has work to do.
        let active_index = group.active_index();
        let standby_index: u8 = (0u8..group.member_count())
            .find(|i| *i != active_index)
            .unwrap();
        group.coord.mark_unhealthy(standby_index);

        let recovered = group.try_recover(&sched, &reg, &|| Box::new(StatefulDaemon::new()));
        assert!(
            !recovered.is_empty(),
            "test fixture's scheduler must be able to re-place the standby",
        );
        assert_eq!(
            group.term(),
            term_before.saturating_add(1),
            "term must advance once on successful slot re-placement",
        );

        // Idle try_recover (no unhealthy slots) does NOT bump.
        let term_after_first = group.term();
        let _ = group.try_recover(&sched, &reg, &|| Box::new(StatefulDaemon::new()));
        assert_eq!(
            group.term(),
            term_after_first,
            "no-op try_recover must not advance term",
        );
    }

    /// Companion: when a standby IS unhealthy alongside an unhealthy
    /// active, `try_recover` repairs the standby and leaves the active
    /// alone. `has_unhealthy_slots` returns true on account of the
    /// standby, not the active.
    #[test]
    fn try_recover_repairs_standby_even_when_active_also_unhealthy() {
        use crate::adapter::net::compute::UnhealthySlotRecovery;

        let reg = DaemonRegistry::new();
        let sched = make_scheduler();

        let mut group = StandbyGroup::spawn(
            test_config(3),
            || Box::new(StatefulDaemon::new()),
            &sched,
            &reg,
        )
        .unwrap();
        let active_index = group.active_index();
        let active_origin_before = group.active_origin();

        // Mark the active AND a standby unhealthy.
        group.coord.mark_unhealthy(active_index);
        let standby_index: u8 = (0u8..group.member_count())
            .find(|i| *i != active_index)
            .unwrap();
        group.coord.mark_unhealthy(standby_index);

        assert!(group.has_unhealthy_slots(), "standby slot is recoverable");
        let recovered = group.try_recover(&sched, &reg, &|| Box::new(StatefulDaemon::new()));
        assert!(!recovered.contains(&active_index), "active still skipped",);
        assert_eq!(
            group.active_origin(),
            active_origin_before,
            "active origin preserved",
        );
    }

    /// `spawn` (v1) is unchanged after the v2 surface lands.
    #[test]
    fn spawn_v1_path_unchanged_after_v2_added() {
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
        assert_eq!(group.active_index(), 0);
        assert_eq!(group.health(), GroupHealth::Healthy);
    }
}
