//! Per-hop interest table — per-downstream soft state (plan §4.3)
//! and cadence-refusal partitioning (plan §4.4).
//!
//! Each downstream owns its own entry per `ProviderInterestKey` and expires
//! independently (refresh at ttl/2, dropped after 2 missed refreshes
//! — one shared expiry would let a refreshing A keep a silent C
//! subscribed). Aggregates — the strictest D, whether upstream
//! interest exists at all — are **derived** from live entries; every
//! mutation reports the resulting [`UpstreamAction`] so the caller
//! can propagate ONE trailing-edge-coalesced update to
//! `next_hop(target)` (the RT-1 gate shape — the gate itself is
//! SI-2 wiring, not table logic). A relay with one downstream is a
//! pure forwarder; coalescing activates only where fan-in meets, and
//! this table is itself the fan-in measurement.
//!
//! The table key is the [`ProviderInterestKey`] — the routed
//! coalescing unit (plan §3.2, v4.1): one entry per provider-targeted
//! branch, and the interest digest inside it binds disclosure class
//! and audience commitment, so interests belonging to different
//! audiences land in different entries **structurally** (plan
//! §4.10); no aggregation code path can merge them. This table is a
//! Layer-2 object: it tracks per-downstream demand for a branch and
//! NEVER reasons about capability-level aggregates — a branch entry
//! dies when its last downstream row dies, not when some relay
//! decides `Any` is satisfied (plan §3.5).
//!
//! Refusal partitioning (plan §4.4): one impossible subscriber must
//! not poison a satisfiable aggregate. On
//! `sampling_interval_unsupported { minimum_supported: M }` the
//! table partitions downstreams on M, reports exactly which ones to
//! refuse and the satisfiable aggregate to re-register (exactly
//! once — M is cached against refusal/retry loops), and locally
//! refuses late joiners below the cached floor without a provider
//! round-trip. The cached M invalidates on that provider's
//! incarnation change (the floor may have changed with it).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::continuity::Continuity;
use super::identity::{AudienceScopeCommitment, Digest256, ProviderInterestKey};

/// Who registered an interest at this hop: the node's own consumer,
/// the node's INTERNAL leader role, or a downstream peer session.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum DownstreamId {
    /// This node's own application-level interest (the `LOCAL` row
    /// in plan §4.3) — deliveries feed the node's consumer overlay.
    /// Installed by the DIRECT
    /// [`register_sensing_interest`](crate::adapter::net::MeshNode::register_sensing_interest)
    /// API only.
    Local,
    /// A node-global interest-LEASE row (OLB-0): the coalesced demand of the
    /// node's lease holders, installed exclusively through
    /// [`acquire_sensing_interest_lease`](crate::adapter::net::MeshNode::acquire_sensing_interest_lease).
    /// Behaves identically to [`Self::Local`] for delivery/overlay/refusal —
    /// it is a node-local interest — but is a DISTINCT table slot so the lease
    /// lifecycle (acquire / release / acquire-failure rollback) can only ever
    /// touch lease-owned rows. Without this separation a rolled-back lease
    /// acquire would deregister a `Local` row a direct registration installed
    /// for the same `(interest, provider)` (review §1).
    LeasedLocal,
    /// The node's OWN sensing-leader role's coalesced demand (SI-4
    /// review P0): deliveries feed `SensingLeader::on_attestation`,
    /// which fans the proof to the leader's real consumer rows.
    /// Distinct from [`Self::Local`] so an internal leader
    /// subscription can never masquerade as a node-local
    /// application watch.
    Leader,
    /// A downstream peer, by node id (session-authenticated).
    Peer(u64),
}

/// One downstream's soft-state row (plan §4.3).
#[derive(Clone, Copy, Debug)]
pub struct DownstreamEntry {
    /// The downstream's own D.
    pub requested_sample_interval: Duration,
    /// Its subscription lifetime.
    pub soft_state_ttl: Duration,
    /// `last_refresh + ttl` — two missed ttl/2 refreshes.
    pub expires_at: Instant,
    /// The owner root the downstream's SESSION proved (plan §4.9) —
    /// recorded for cross-checks, never trusted from wire fields.
    pub owner_root: AudienceScopeCommitment,
}

/// Upstream consequence of a table mutation. The caller owns
/// propagation (trailing-edge coalesced, RT-1 shape).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UpstreamAction {
    /// Derived aggregate unchanged — nothing to send.
    None,
    /// (Re-)register upstream at this strictest D (first downstream,
    /// or the strictest-D aggregate moved).
    Register {
        /// min-dominance aggregate over live, eligible downstreams.
        strictest: Duration,
    },
    /// Last live downstream gone — withdraw the upstream interest;
    /// emitters die when the last interest dies (plan §4.7).
    Deregister,
}

/// Outcome of a downstream registration/refresh.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RegisterOutcome {
    /// Admitted (or refreshed); carries the upstream consequence.
    Registered(UpstreamAction),
    /// Refused locally against the cached provider floor — no
    /// provider round-trip (plan §4.4).
    RefusedByCachedFloor {
        /// The cached M the request fell below.
        minimum_supported: Duration,
    },
    /// The downstream is over `max_interests_per_peer`
    /// (amplification bound, plan §5) — nothing was recorded.
    OverCap,
}

/// Result of partitioning on a provider refusal (plan §4.4).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RefusalPartition {
    /// Downstreams with `D < M` — propagate the refusal to exactly
    /// these; their rows are removed (they may re-register with a
    /// looser D).
    pub refused: Vec<DownstreamId>,
    /// The PENDING upstream transition (SI-3 closure item 2):
    /// `Register { strictest }` = the satisfiable survivor aggregate
    /// to re-register, returned on every refusal until a sender
    /// consumes it ([`InterestTable::commit_advertised`] after a
    /// successful send, or the next full-spec `register()` refresh
    /// structurally); `Deregister` if no eligible downstream
    /// remains; `None` when nothing changed against what upstream
    /// was last told.
    pub upstream: UpstreamAction,
}

#[derive(Debug)]
struct InterestEntry {
    /// The relay's OWN continuity toward the origin — gates
    /// continuity-bearing forwarding (plan §4.4 hop rule; driven by
    /// the relay delivery layer, stored here per plan §4.3).
    upstream_continuity: Continuity,
    /// Cached provider floor M.
    refused_minimum: Option<Duration>,
    /// The strictest D last reported upstream — what `next_hop`
    /// currently believes. Every mutation diffs the recomputed live
    /// aggregate against this, so "exactly one update per derived
    /// change" holds by construction (and a sweep that discovers
    /// already-lapsed rows still knows what it had advertised).
    last_advertised: Option<Duration>,
    downstreams: HashMap<DownstreamId, DownstreamEntry>,
}

/// The per-hop interest table (plan §4.3).
#[derive(Debug)]
pub struct InterestTable {
    entries: HashMap<ProviderInterestKey, InterestEntry>,
    /// Live (key, downstream) rows per downstream, for the cap.
    per_downstream: HashMap<DownstreamId, usize>,
    max_interests_per_peer: usize,
}

impl InterestTable {
    /// Empty table with the given per-downstream cap
    /// (`max_interests_per_peer`, plan §5 — default 512).
    pub fn new(max_interests_per_peer: usize) -> Self {
        Self {
            entries: HashMap::new(),
            per_downstream: HashMap::new(),
            max_interests_per_peer,
        }
    }

    /// Whether ANY entry exists for the key (live rows or not-yet-
    /// swept ones) — the refusal-tombstone GC's liveness input
    /// (SI-3 closure item 6).
    pub fn has_entry(&self, key: &ProviderInterestKey) -> bool {
        self.entries.contains_key(key)
    }

    /// Strictest live D for a key — the derived upstream aggregate.
    pub fn aggregate(&self, key: &ProviderInterestKey, now: Instant) -> Option<Duration> {
        self.entries.get(key).and_then(|entry| {
            entry
                .downstreams
                .values()
                .filter(|row| row.expires_at > now)
                .map(|row| row.requested_sample_interval)
                .min()
        })
    }

    /// Register or refresh one downstream's interest. Refreshing
    /// re-arms `expires_at = now + ttl`; each downstream expires
    /// independently.
    pub fn register(
        &mut self,
        key: &ProviderInterestKey,
        downstream: DownstreamId,
        requested_sample_interval: Duration,
        soft_state_ttl: Duration,
        owner_root: AudienceScopeCommitment,
        now: Instant,
    ) -> RegisterOutcome {
        // Late joiners below a cached provider floor are refused
        // locally — no provider round-trip, no refusal/retry loop.
        if let Some(entry) = self.entries.get(key) {
            if let Some(floor) = entry.refused_minimum {
                if requested_sample_interval < floor {
                    return RegisterOutcome::RefusedByCachedFloor {
                        minimum_supported: floor,
                    };
                }
            }
        }

        let is_new_row = self
            .entries
            .get(key)
            .is_none_or(|entry| !entry.downstreams.contains_key(&downstream));
        if is_new_row {
            let held = self.per_downstream.get(&downstream).copied().unwrap_or(0);
            if held >= self.max_interests_per_peer {
                return RegisterOutcome::OverCap;
            }
        }

        let entry = self
            .entries
            .entry(key.clone())
            .or_insert_with(|| InterestEntry {
                upstream_continuity: Continuity::Unestablished,
                refused_minimum: None,
                last_advertised: None,
                downstreams: HashMap::new(),
            });
        entry.downstreams.insert(
            downstream,
            DownstreamEntry {
                requested_sample_interval,
                soft_state_ttl,
                expires_at: now + soft_state_ttl,
                owner_root,
            },
        );
        if is_new_row {
            *self.per_downstream.entry(downstream).or_insert(0) += 1;
        }
        RegisterOutcome::Registered(self.action_for(key, now))
    }

    /// Drop expired downstream rows everywhere and report the keys
    /// whose upstream aggregate consequently changed. Empty entries
    /// (and their cached floors) are removed entirely.
    pub fn expire(&mut self, now: Instant) -> Vec<(ProviderInterestKey, UpstreamAction)> {
        let keys: Vec<ProviderInterestKey> = self.entries.keys().cloned().collect();
        let mut actions = Vec::new();
        for key in keys {
            let Some(entry) = self.entries.get_mut(&key) else {
                continue;
            };
            let expired: Vec<DownstreamId> = entry
                .downstreams
                .iter()
                .filter(|(_, row)| row.expires_at <= now)
                .map(|(id, _)| *id)
                .collect();
            for id in &expired {
                entry.downstreams.remove(id);
            }
            self.release_rows(&expired);
            let action = self.action_for(&key, now);
            self.drop_if_empty(&key);
            if action != UpstreamAction::None {
                actions.push((key, action));
            }
        }
        actions
    }

    /// Downstream loss (plan §4.7): drop every row a departed peer
    /// held; derived aggregates recompute per key.
    pub fn remove_downstream(
        &mut self,
        downstream: DownstreamId,
        now: Instant,
    ) -> Vec<(ProviderInterestKey, UpstreamAction)> {
        let keys: Vec<ProviderInterestKey> = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.downstreams.contains_key(&downstream))
            .map(|(key, _)| key.clone())
            .collect();
        let mut actions = Vec::new();
        for key in keys {
            if let Some(entry) = self.entries.get_mut(&key) {
                entry.downstreams.remove(&downstream);
            }
            self.release_rows(&[downstream]);
            let action = self.action_for(&key, now);
            self.drop_if_empty(&key);
            if action != UpstreamAction::None {
                actions.push((key, action));
            }
        }
        actions
    }

    /// Explicit withdrawal (plan §4.2 `Deregister`; SI-2a dispatch):
    /// drop one downstream's rows for `interest_digest` — exactly
    /// the `(digest, provider)` branch when `provider` is `Some`, or
    /// that downstream's rows across every branch of the digest when
    /// `None` (the whole-interest withdrawal). Reports each touched
    /// key's upstream consequence exactly as [`Self::expire`] does.
    /// Unknown digests and absent rows are a no-op — deregistration
    /// of soft state is idempotent, so a duplicated or crossed
    /// `Deregister` frame removes nothing twice.
    pub fn deregister(
        &mut self,
        interest_digest: &Digest256,
        provider: Option<u64>,
        downstream: DownstreamId,
        now: Instant,
    ) -> Vec<(ProviderInterestKey, UpstreamAction)> {
        let keys: Vec<ProviderInterestKey> = self
            .entries
            .iter()
            .filter(|(key, entry)| {
                key.interest.interest_digest == *interest_digest
                    && provider.is_none_or(|p| key.provider == p)
                    && entry.downstreams.contains_key(&downstream)
            })
            .map(|(key, _)| key.clone())
            .collect();
        let mut actions = Vec::new();
        for key in keys {
            if let Some(entry) = self.entries.get_mut(&key) {
                entry.downstreams.remove(&downstream);
            }
            self.release_rows(&[downstream]);
            let action = self.action_for(&key, now);
            self.drop_if_empty(&key);
            if action != UpstreamAction::None {
                actions.push((key, action));
            }
        }
        actions
    }

    /// Apply a provider refusal `sampling_interval_unsupported { M }`
    /// (plan §4.4): partition on M, cache M, report the refused
    /// downstreams and the satisfiable aggregate to re-register —
    /// exactly once; a duplicate refusal at the same M is absorbed.
    pub fn on_refusal(
        &mut self,
        key: &ProviderInterestKey,
        minimum_supported: Duration,
        now: Instant,
    ) -> RefusalPartition {
        let Some(entry) = self.entries.get_mut(key) else {
            // Stale refusal — everything already expired.
            return RefusalPartition {
                refused: Vec::new(),
                upstream: UpstreamAction::None,
            };
        };
        entry.refused_minimum = Some(minimum_supported);
        let refused: Vec<DownstreamId> = entry
            .downstreams
            .iter()
            .filter(|(_, row)| row.requested_sample_interval < minimum_supported)
            .map(|(id, _)| *id)
            .collect();
        for id in &refused {
            entry.downstreams.remove(id);
        }
        self.release_rows(&refused);
        // SI-3 closure item 2: the survivor transition is PEEKED,
        // never consumed here. Committing `last_advertised` inside
        // this method lost the re-registration whenever the caller
        // could not send (the 0x0C03 intake holds no spec cache):
        // the next refresh then diffed against the already-updated
        // value, produced `None`, and the surviving demand stranded
        // permanently at the provider. Now the transition stays
        // pending until a sender consumes it — either explicitly
        // ([`Self::commit_advertised`], the SI-4 relay sender) or
        // structurally, when the next full-spec `register()` refresh
        // recomputes the same `Register` action and its internal
        // diff commits. "Exactly once" is therefore the SENDER's
        // property: a caller that cannot send leaves the transition
        // pending (a duplicate refusal returns it again, unsent — no
        // retry loop forms because nothing rides an unsendable
        // action), and a caller that sends commits.
        let upstream = self.peek_action_for(key, now);
        self.drop_if_empty(key);
        RefusalPartition { refused, upstream }
    }

    /// SI-3 closure item 2: consume a pending upstream transition
    /// after SUCCESSFULLY sending the re-registration the caller
    /// derived from [`Self::on_refusal`]'s `Register { strictest }`.
    /// Callers that cannot send (no spec cache at this hop) simply
    /// never commit — the next downstream refresh repairs through
    /// `register()`'s own diff, which commits structurally.
    pub fn commit_advertised(&mut self, key: &ProviderInterestKey, strictest: Duration) {
        if let Some(entry) = self.entries.get_mut(key) {
            entry.last_advertised = Some(strictest);
        }
    }

    /// SI-6.1 (fold-membership reconciliation): drop one branch
    /// ENTIRELY — every downstream row and the cached floor —
    /// releasing the per-downstream counts. For a provider the fold
    /// no longer makes eligible, there is nothing to partition or
    /// expire toward: the branch itself is dead. The caller owns
    /// the upstream/observation consequences. Returns the rows that
    /// were dropped.
    pub fn remove_branch(&mut self, key: &ProviderInterestKey) -> Vec<DownstreamId> {
        let Some(entry) = self.entries.remove(key) else {
            return Vec::new();
        };
        let downstreams: Vec<DownstreamId> = entry.downstreams.keys().copied().collect();
        self.release_rows(&downstreams);
        downstreams
    }

    /// Provider incarnation OR generation change: the floor may have
    /// changed with either (a restart reconfigures, a redefinition
    /// re-specs), so every cached M for that provider is invalidated
    /// (plan §4.4/§4.8). The branch entries themselves survive — the
    /// routed key binds neither epoch (v4.1 §3.2).
    pub fn invalidate_provider_floors(&mut self, provider: u64) {
        for (key, entry) in self.entries.iter_mut() {
            if key.provider == provider {
                entry.refused_minimum = None;
            }
        }
    }

    /// The relay's own upstream continuity for a key (plan §4.3) —
    /// read by the delivery layer's hop rule.
    pub fn upstream_continuity(&self, key: &ProviderInterestKey) -> Option<Continuity> {
        self.entries.get(key).map(|entry| entry.upstream_continuity)
    }

    /// Update the stored upstream continuity (driven by the relay's
    /// own `ObservationCell` for the key).
    pub fn set_upstream_continuity(&mut self, key: &ProviderInterestKey, continuity: Continuity) {
        if let Some(entry) = self.entries.get_mut(key) {
            entry.upstream_continuity = continuity;
        }
    }

    /// The cached provider floor for a key, if any.
    pub fn cached_floor(&self, key: &ProviderInterestKey) -> Option<Duration> {
        self.entries
            .get(key)
            .and_then(|entry| entry.refused_minimum)
    }

    /// Test seam: install a cached provider floor for `key` (as a live
    /// provider refusal would), creating a floor-only entry if none exists —
    /// so a late joiner below `floor` is refused
    /// ([`RegisterOutcome::RefusedByCachedFloor`]) without driving the real
    /// provider-refusal protocol.
    #[doc(hidden)]
    pub fn set_cached_floor_for_test(&mut self, key: &ProviderInterestKey, floor: Duration) {
        self.entries
            .entry(key.clone())
            .or_insert_with(|| InterestEntry {
                upstream_continuity: Continuity::Unestablished,
                refused_minimum: None,
                last_advertised: None,
                downstreams: HashMap::new(),
            })
            .refused_minimum = Some(floor);
    }

    /// Test seam: clear a cached provider floor for `key` (as a floor
    /// relaxation / provider incarnation change would), leaving the entry.
    #[doc(hidden)]
    pub fn clear_cached_floor_for_test(&mut self, key: &ProviderInterestKey) {
        if let Some(entry) = self.entries.get_mut(key) {
            entry.refused_minimum = None;
        }
    }

    /// Live downstream ids for a key (delivery fan-out, SI-0f).
    pub fn downstreams(&self, key: &ProviderInterestKey, now: Instant) -> Vec<DownstreamId> {
        self.entries
            .get(key)
            .map(|entry| {
                entry
                    .downstreams
                    .iter()
                    .filter(|(_, row)| row.expires_at > now)
                    .map(|(id, _)| *id)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// One downstream's live row for a key (delivery scheduling).
    pub fn downstream_entry(
        &self,
        key: &ProviderInterestKey,
        downstream: DownstreamId,
    ) -> Option<&DownstreamEntry> {
        self.entries
            .get(key)
            .and_then(|entry| entry.downstreams.get(&downstream))
    }

    /// Number of keys with any (possibly expired-but-unswept) rows.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the table holds no interests at all — the zero-idle-
    /// cost criterion (plan §8).
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Diff the recomputed live aggregate against what upstream was
    /// last told, record the new value, and report the delta.
    fn action_for(&mut self, key: &ProviderInterestKey, now: Instant) -> UpstreamAction {
        let action = self.peek_action_for(key, now);
        if let Some(entry) = self.entries.get_mut(key) {
            entry.last_advertised = entry
                .downstreams
                .values()
                .filter(|row| row.expires_at > now)
                .map(|row| row.requested_sample_interval)
                .min();
        }
        action
    }

    /// [`Self::action_for`] without consuming the transition —
    /// `last_advertised` is left for the eventual sender (SI-3
    /// closure item 2; see [`Self::on_refusal`]).
    fn peek_action_for(&self, key: &ProviderInterestKey, now: Instant) -> UpstreamAction {
        let Some(entry) = self.entries.get(key) else {
            return UpstreamAction::None;
        };
        let live = entry
            .downstreams
            .values()
            .filter(|row| row.expires_at > now)
            .map(|row| row.requested_sample_interval)
            .min();
        match (entry.last_advertised, live) {
            (Some(_), None) => UpstreamAction::Deregister,
            (previous, Some(strictest)) if previous != live => {
                UpstreamAction::Register { strictest }
            }
            _ => UpstreamAction::None,
        }
    }

    fn release_rows(&mut self, downstreams: &[DownstreamId]) {
        for id in downstreams {
            if let Some(count) = self.per_downstream.get_mut(id) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    self.per_downstream.remove(id);
                }
            }
        }
    }

    fn drop_if_empty(&mut self, key: &ProviderInterestKey) {
        let empty = self
            .entries
            .get(key)
            .is_some_and(|entry| entry.downstreams.is_empty());
        if empty {
            self.entries.remove(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::identity::{
        CanonicalConstraints, CapabilityId, DisclosureClass, InterestSpec, ProviderSelector,
        ResultMode, WorkLatencyEnvelope,
    };
    use super::*;

    const PROVIDER: u64 = 99;

    fn root(byte: u8) -> AudienceScopeCommitment {
        AudienceScopeCommitment::from_bytes([byte; 32])
    }

    fn key_with_audience(byte: u8) -> ProviderInterestKey {
        let spec = InterestSpec {
            capability_id: CapabilityId::new("video.transcode"),
            constraints: CanonicalConstraints::from_entries([("fps", "60")]).unwrap(),
            work_latency: WorkLatencyEnvelope::start_within(Duration::from_millis(200)),
            providers: ProviderSelector::AnyAuthorized,
            result_mode: ResultMode::Any,
            disclosure_class: DisclosureClass::Owner,
            audience: root(byte),
        };
        ProviderInterestKey::new(spec.key(), PROVIDER)
    }

    fn key() -> ProviderInterestKey {
        key_with_audience(0xAA)
    }

    const TTL: Duration = Duration::from_secs(1);

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    #[test]
    fn first_registration_registers_and_min_dominance_holds() {
        let now = Instant::now();
        let mut table = InterestTable::new(512);
        assert_eq!(
            table.register(&key(), DownstreamId::Peer(1), ms(500), TTL, root(0xAA), now),
            RegisterOutcome::Registered(UpstreamAction::Register { strictest: ms(500) }),
        );
        // A looser downstream joins: aggregate unchanged, no upstream
        // traffic.
        assert_eq!(
            table.register(&key(), DownstreamId::Peer(2), ms(800), TTL, root(0xAA), now),
            RegisterOutcome::Registered(UpstreamAction::None),
        );
        // A stricter one moves the aggregate.
        assert_eq!(
            table.register(&key(), DownstreamId::Local, ms(100), TTL, root(0xAA), now),
            RegisterOutcome::Registered(UpstreamAction::Register { strictest: ms(100) }),
        );
        assert_eq!(table.aggregate(&key(), now), Some(ms(100)));
    }

    #[test]
    fn one_downstream_expires_while_another_refreshes() {
        // SI-0 test 9: aggregates shrink when the strict downstream
        // goes silent; the refreshing survivor is untouched.
        let t0 = Instant::now();
        let mut table = InterestTable::new(512);
        let strict = DownstreamId::Peer(1);
        let survivor = DownstreamId::Peer(2);
        table.register(&key(), strict, ms(100), TTL, root(0xAA), t0);
        table.register(&key(), survivor, ms(500), TTL, root(0xAA), t0);
        assert_eq!(table.aggregate(&key(), t0), Some(ms(100)));

        // The survivor refreshes at ttl/2; the strict one goes
        // silent.
        let half = t0 + TTL / 2;
        assert_eq!(
            table.register(&key(), survivor, ms(500), TTL, root(0xAA), half),
            RegisterOutcome::Registered(UpstreamAction::None),
        );

        // Two missed refreshes for the strict downstream: at t0+ttl
        // it drops, the aggregate loosens to the survivor's D.
        let actions = table.expire(t0 + TTL);
        assert_eq!(
            actions,
            vec![(key(), UpstreamAction::Register { strictest: ms(500) })],
        );
        // Survivor is unaffected: still registered, still delivered.
        assert_eq!(table.downstreams(&key(), t0 + TTL), vec![survivor]);

        // The survivor stops refreshing too: last row gone →
        // upstream deregistration, table empty (zero idle cost).
        let actions = table.expire(t0 + TTL / 2 + TTL);
        assert_eq!(actions, vec![(key(), UpstreamAction::Deregister)]);
        assert!(table.is_empty());
    }

    #[test]
    fn refusal_partitions_downstreams_and_the_sender_commits_the_transition() {
        // SI-0 test 15 (table half), re-specced by SI-3 closure
        // item 2: A@20ms + C@100ms coalesce to 20ms; the provider
        // floor is 50ms. The refusal hits exactly A; C's satisfiable
        // aggregate is returned as a PENDING transition on every
        // refusal until a sender consumes it — "exactly once" is the
        // sender's property, not the partition's.
        let now = Instant::now();
        let mut table = InterestTable::new(512);
        let a = DownstreamId::Peer(1);
        let c = DownstreamId::Peer(2);
        table.register(&key(), a, ms(20), TTL, root(0xAA), now);
        table.register(&key(), c, ms(100), TTL, root(0xAA), now);
        assert_eq!(table.aggregate(&key(), now), Some(ms(20)));

        let partition = table.on_refusal(&key(), ms(50), now);
        assert_eq!(partition.refused, vec![a]);
        assert_eq!(
            partition.upstream,
            UpstreamAction::Register { strictest: ms(100) },
        );

        // A duplicate refusal (in-flight crossing) BEFORE any send
        // returns the still-pending transition — a caller that
        // cannot send must not lose it (closure item 2's stranding
        // bug); nothing loops because nothing rides an unsent
        // action.
        let duplicate = table.on_refusal(&key(), ms(50), now);
        assert_eq!(duplicate.refused, Vec::new());
        assert_eq!(
            duplicate.upstream,
            UpstreamAction::Register { strictest: ms(100) },
        );

        // The sender commits after a successful send: a further
        // duplicate is now fully absorbed.
        table.commit_advertised(&key(), ms(100));
        let absorbed = table.on_refusal(&key(), ms(50), now);
        assert_eq!(absorbed.refused, Vec::new());
        assert_eq!(absorbed.upstream, UpstreamAction::None);

        // Late joiner below the cached floor: refused locally, no
        // upstream traffic, survivor untouched.
        assert_eq!(
            table.register(&key(), DownstreamId::Peer(3), ms(30), TTL, root(0xAA), now),
            RegisterOutcome::RefusedByCachedFloor {
                minimum_supported: ms(50),
            },
        );
        assert_eq!(table.aggregate(&key(), now), Some(ms(100)));

        // A satisfiable late joiner is admitted normally.
        assert_eq!(
            table.register(&key(), DownstreamId::Peer(4), ms(60), TTL, root(0xAA), now),
            RegisterOutcome::Registered(UpstreamAction::Register { strictest: ms(60) }),
        );
    }

    #[test]
    fn unsent_refusal_transition_repairs_through_the_next_refresh() {
        // SI-3 closure item 2, the hop with NO spec cache (the
        // 0x0C03 intake): it partitions but cannot send, so the
        // transition stays pending and the survivor's next ttl/2
        // refresh — which carries the full spec — recomputes the
        // SAME Register action through `register()` and commits it.
        // Before the fix, `on_refusal` had already consumed the
        // transition and the refresh produced `None`, stranding the
        // surviving demand permanently.
        let now = Instant::now();
        let mut table = InterestTable::new(512);
        let sub_floor = DownstreamId::Peer(1);
        let survivor = DownstreamId::Peer(2);
        table.register(&key(), sub_floor, ms(10), TTL, root(0xAA), now);
        table.register(&key(), survivor, ms(100), TTL, root(0xAA), now);

        let partition = table.on_refusal(&key(), ms(50), now);
        assert_eq!(partition.refused, vec![sub_floor]);
        assert_eq!(
            partition.upstream,
            UpstreamAction::Register { strictest: ms(100) },
        );
        // …the intake ignores the action (nothing to send) …

        // … and the survivor's refresh produces the send.
        assert_eq!(
            table.register(&key(), survivor, ms(100), TTL, root(0xAA), now),
            RegisterOutcome::Registered(UpstreamAction::Register { strictest: ms(100) }),
            "the refresh recovers the stranded survivor aggregate",
        );

        // The refresh committed: the next refresh is quiet again.
        assert_eq!(
            table.register(&key(), survivor, ms(100), TTL, root(0xAA), now),
            RegisterOutcome::Registered(UpstreamAction::None),
        );
    }

    #[test]
    fn refusing_every_downstream_deregisters_upstream() {
        let now = Instant::now();
        let mut table = InterestTable::new(512);
        table.register(&key(), DownstreamId::Peer(1), ms(10), TTL, root(0xAA), now);
        table.register(&key(), DownstreamId::Peer(2), ms(20), TTL, root(0xAA), now);
        let partition = table.on_refusal(&key(), ms(50), now);
        assert_eq!(partition.refused.len(), 2);
        assert_eq!(partition.upstream, UpstreamAction::Deregister);
        assert!(table.is_empty());
    }

    #[test]
    fn cached_floor_invalidates_on_provider_epoch_change() {
        // SI-0 test 15 tail: the provider restarted (or redefined
        // the capability) — its floor may have changed, so the
        // cached M must not keep refusing locally on stale grounds.
        let now = Instant::now();
        let mut table = InterestTable::new(512);
        table.register(&key(), DownstreamId::Peer(1), ms(100), TTL, root(0xAA), now);
        table.on_refusal(&key(), ms(50), now);
        assert_eq!(table.cached_floor(&key()), Some(ms(50)));

        table.invalidate_provider_floors(PROVIDER);
        assert_eq!(table.cached_floor(&key()), None);
        // A 30ms joiner now goes through to the provider again.
        assert_eq!(
            table.register(&key(), DownstreamId::Peer(3), ms(30), TTL, root(0xAA), now),
            RegisterOutcome::Registered(UpstreamAction::Register { strictest: ms(30) }),
        );
    }

    #[test]
    fn different_audiences_are_structurally_separate_entries() {
        // Plan §4.9: the digest binds the audience commitment, so two
        // roots with identical predicates occupy two table entries —
        // no aggregation code path exists that could merge them.
        let now = Instant::now();
        let mut table = InterestTable::new(512);
        let key_a = key_with_audience(0xAA);
        let key_b = key_with_audience(0xBB);
        assert_ne!(key_a, key_b);
        table.register(&key_a, DownstreamId::Peer(1), ms(100), TTL, root(0xAA), now);
        table.register(&key_b, DownstreamId::Peer(2), ms(20), TTL, root(0xBB), now);
        assert_eq!(table.len(), 2);
        assert_eq!(table.aggregate(&key_a, now), Some(ms(100)));
        assert_eq!(table.aggregate(&key_b, now), Some(ms(20)));
    }

    #[test]
    fn per_downstream_cap_bounds_amplification() {
        let now = Instant::now();
        let mut table = InterestTable::new(2);
        let peer = DownstreamId::Peer(1);
        assert!(matches!(
            table.register(&key_with_audience(1), peer, ms(100), TTL, root(1), now),
            RegisterOutcome::Registered(_),
        ));
        assert!(matches!(
            table.register(&key_with_audience(2), peer, ms(100), TTL, root(2), now),
            RegisterOutcome::Registered(_),
        ));
        assert_eq!(
            table.register(&key_with_audience(3), peer, ms(100), TTL, root(3), now),
            RegisterOutcome::OverCap,
        );
        // A refresh of an existing row is NOT a new row — never
        // capped.
        assert!(matches!(
            table.register(&key_with_audience(1), peer, ms(100), TTL, root(1), now),
            RegisterOutcome::Registered(_),
        ));
        // Dropping the peer releases its quota.
        table.remove_downstream(peer, now);
        assert!(matches!(
            table.register(&key_with_audience(3), peer, ms(100), TTL, root(3), now),
            RegisterOutcome::Registered(_),
        ));
    }

    #[test]
    fn remove_downstream_recomputes_each_touched_key() {
        let now = Instant::now();
        let mut table = InterestTable::new(512);
        let leaving = DownstreamId::Peer(1);
        let staying = DownstreamId::Peer(2);
        let key_a = key_with_audience(1);
        let key_b = key_with_audience(2);
        table.register(&key_a, leaving, ms(50), TTL, root(1), now);
        table.register(&key_a, staying, ms(200), TTL, root(1), now);
        table.register(&key_b, leaving, ms(50), TTL, root(2), now);

        let mut actions = table.remove_downstream(leaving, now);
        actions.sort_by_key(|(key, _)| key.interest.interest_digest.as_bytes().to_vec());
        let mut expected = vec![
            (
                key_a.clone(),
                UpstreamAction::Register { strictest: ms(200) },
            ),
            (key_b.clone(), UpstreamAction::Deregister),
        ];
        expected.sort_by_key(|(key, _)| key.interest.interest_digest.as_bytes().to_vec());
        assert_eq!(actions, expected);
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn deregister_removes_exactly_the_named_branch() {
        let now = Instant::now();
        let mut table = InterestTable::new(512);
        let leaver = DownstreamId::Peer(1);
        let survivor = DownstreamId::Peer(2);
        table.register(&key(), leaver, ms(100), TTL, root(0xAA), now);
        table.register(&key(), survivor, ms(500), TTL, root(0xAA), now);

        // The strict downstream withdraws its branch row: the
        // survivor stays and the derived aggregate loosens.
        let actions =
            table.deregister(&key().interest.interest_digest, Some(PROVIDER), leaver, now);
        assert_eq!(
            actions,
            vec![(key(), UpstreamAction::Register { strictest: ms(500) })],
        );
        assert_eq!(table.downstreams(&key(), now), vec![survivor]);

        // A duplicated Deregister is idempotent: nothing left to
        // remove, no upstream consequence.
        let duplicate =
            table.deregister(&key().interest.interest_digest, Some(PROVIDER), leaver, now);
        assert_eq!(duplicate, Vec::new());

        // The survivor withdraws too: last row gone → upstream
        // deregistration, entry dropped (zero idle cost) — and the
        // quota released, so the peer can register afresh.
        let actions = table.deregister(
            &key().interest.interest_digest,
            Some(PROVIDER),
            survivor,
            now,
        );
        assert_eq!(actions, vec![(key(), UpstreamAction::Deregister)]);
        assert!(table.is_empty());
        assert!(matches!(
            table.register(&key(), survivor, ms(500), TTL, root(0xAA), now),
            RegisterOutcome::Registered(_),
        ));
    }

    #[test]
    fn deregister_without_target_clears_every_branch_of_the_digest() {
        // The whole-interest withdrawal (`target: None`): one
        // downstream's rows drop across every provider branch of the
        // digest, and only that downstream's — another digest and
        // another downstream are untouched.
        let now = Instant::now();
        let mut table = InterestTable::new(512);
        let peer = DownstreamId::Peer(1);
        let other_peer = DownstreamId::Peer(2);
        let branch_a = key(); // (digest, PROVIDER)
        let branch_b = ProviderInterestKey::new(branch_a.interest.clone(), PROVIDER + 1);
        let unrelated = key_with_audience(0xBB);
        table.register(&branch_a, peer, ms(100), TTL, root(0xAA), now);
        table.register(&branch_b, peer, ms(100), TTL, root(0xAA), now);
        table.register(&branch_b, other_peer, ms(200), TTL, root(0xAA), now);
        table.register(&unrelated, peer, ms(100), TTL, root(0xBB), now);

        let mut actions = table.deregister(&branch_a.interest.interest_digest, None, peer, now);
        actions.sort_by_key(|(key, _)| key.provider);
        assert_eq!(
            actions,
            vec![
                (branch_a.clone(), UpstreamAction::Deregister),
                (
                    branch_b.clone(),
                    UpstreamAction::Register { strictest: ms(200) },
                ),
            ],
        );
        assert_eq!(table.downstreams(&branch_a, now), Vec::new());
        assert_eq!(table.downstreams(&branch_b, now), vec![other_peer]);
        assert_eq!(table.downstreams(&unrelated, now), vec![peer]);
    }

    #[test]
    fn deregister_unknown_digest_is_a_noop() {
        let now = Instant::now();
        let mut table = InterestTable::new(512);
        table.register(&key(), DownstreamId::Peer(1), ms(100), TTL, root(0xAA), now);
        let other_digest = key_with_audience(0xCC).interest.interest_digest;
        let actions = table.deregister(&other_digest, None, DownstreamId::Peer(1), now);
        assert_eq!(actions, Vec::new());
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn upstream_continuity_is_stored_per_key() {
        let now = Instant::now();
        let mut table = InterestTable::new(512);
        assert_eq!(table.upstream_continuity(&key()), None);
        table.register(&key(), DownstreamId::Peer(1), ms(100), TTL, root(0xAA), now);
        assert_eq!(
            table.upstream_continuity(&key()),
            Some(Continuity::Unestablished),
        );
        table.set_upstream_continuity(&key(), Continuity::Established);
        assert_eq!(
            table.upstream_continuity(&key()),
            Some(Continuity::Established),
        );
    }
}
