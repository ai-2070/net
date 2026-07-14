//! Relay delivery: store, pack, down-sample (plan §4.4) — the
//! in-process composition of gate + continuity + interest table,
//! per provider-targeted branch (v4.1 Layer 2).
//!
//! Attestations are origin-signed, self-contained, latest-wins — a
//! relay is a *cache with a schedule*:
//!
//! - each downstream is delivered the latest attestation per key at
//!   its OWN D (min-dominance governs what flows up; per-subscriber
//!   schedules govern what flows down);
//! - a **status edge is never held** — transitions flush
//!   immediately; only same-key continuity beats wait, bounded by
//!   the downstream's D;
//! - a late joiner is **warm-started as provisional**: the cached
//!   latest attestation is delivered with `continuity_bearing =
//!   false`, always — a cached Ready must never become "fresh" by
//!   being forwarded (§4.5);
//! - **hop-by-hop continuity** (§4.4): a relay MUST NOT deliver as
//!   continuity-bearing while its OWN upstream continuity for the
//!   branch is Unestablished or Expired. The flag is local delivery
//!   metadata on the relay→downstream envelope (which the relay
//!   authors and the session authenticates) — never a field inside
//!   the origin-signed attestation. Establishment therefore
//!   propagates hop-by-hop from the live origin stream, and a chain
//!   of staggered caches cannot manufacture apparent
//!   post-registration continuity (SI-0 test 13).
//!
//! Layer discipline (v4.1, plan §3.5): everything here moves
//! provider *observations* — signed proofs — between hops. Nothing
//! here computes or forwards a capability-level verdict: budgets
//! make viability consumer-relative, so the result-mode aggregate
//! lives with the Layer-1 controller at each consumer.
//!
//! SI-0 note: [`Attestation`] here is the semantic object; its
//! `fingerprint` stands in for the digest of the signed wire bytes
//! (SI-1). Everything else — admission, continuity, scheduling — is
//! the logic the wire path will reuse unchanged.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::continuity::{
    AttestedStatus, Continuity, DeliveredBeat, ObservationCell, ProjectedReadiness,
};
use super::identity::{
    AudienceScopeCommitment, CapabilityInterestKey, Digest256, ProviderInterestKey,
    ProviderObservationKey,
};
use super::incarnation::{Incarnation, IncarnationSeqGate};
use super::table::{DownstreamId, InterestTable, RegisterOutcome};

/// One origin-signed readiness attestation (plan §4.2, semantic
/// form). Relays forward these bytes identically — suppress or
/// delay, never alter.
#[derive(Clone, Debug)]
pub struct Attestation {
    /// The full observation identity: interest + provider + the
    /// provider's own announce generation.
    pub key: ProviderObservationKey,
    /// Signed boot epoch (§4.6).
    pub origin_incarnation: Incarnation,
    /// Signed status.
    pub status: AttestedStatus,
    /// Signed time-to-start estimate — a PROVIDER-side quantity;
    /// each consumer adds its own route estimate against its own
    /// budget (§3.3).
    pub estimated_start: Option<Duration>,
    /// Signed per-(incarnation, interest) sequence number
    /// (generation-independent, §3.4).
    pub seq: u64,
    /// Signed emission cadence for the aggregated branch.
    pub promised_cadence: Duration,
    /// SI-0 stand-in for the digest of the signed wire bytes — feeds
    /// equivocation detection at the seq gate.
    pub fingerprint: Digest256,
}

impl Attestation {
    /// Build an attestation, deriving the fingerprint from the
    /// signed fields (two emitters producing different payloads for
    /// one (incarnation, seq) therefore collide at the gate — §4.6).
    pub fn new(
        key: ProviderObservationKey,
        origin_incarnation: Incarnation,
        status: AttestedStatus,
        estimated_start: Option<Duration>,
        seq: u64,
        promised_cadence: Duration,
    ) -> Self {
        let mut hasher = blake3::Hasher::new_derive_key("net.sensing.attestation.fingerprint.v1");
        hasher.update(&key.provider.to_le_bytes());
        hasher.update(&key.capability_generation.to_le_bytes());
        hasher.update(&origin_incarnation.get().to_le_bytes());
        hasher.update(key.interest.interest_digest.as_bytes());
        hasher.update(&[match status {
            AttestedStatus::Ready => 0u8,
            AttestedStatus::NotReady => 1,
            AttestedStatus::ProviderUnknown => 2,
        }]);
        hasher.update(
            &estimated_start
                .map(|d| d.as_nanos())
                .unwrap_or(u128::MAX)
                .to_le_bytes(),
        );
        hasher.update(&seq.to_le_bytes());
        hasher.update(&promised_cadence.as_nanos().to_le_bytes());
        let fingerprint = Digest256::from_bytes(*hasher.finalize().as_bytes());
        Self {
            key,
            origin_incarnation,
            status,
            estimated_start,
            seq,
            promised_cadence,
            fingerprint,
        }
    }

    /// The routed branch this attestation answers.
    pub fn branch(&self) -> ProviderInterestKey {
        ProviderInterestKey::new(self.key.interest.clone(), self.key.provider)
    }

    fn as_beat(&self, continuity_bearing: bool) -> DeliveredBeat {
        DeliveredBeat {
            attested_status: self.status,
            estimated_start: self.estimated_start,
            source_incarnation: self.origin_incarnation,
            capability_generation: self.key.capability_generation,
            seq: self.seq,
            promised_cadence: self.promised_cadence,
            continuity_bearing,
        }
    }
}

/// One relay→downstream delivery: identical signed bytes plus the
/// relay-authored envelope flag.
#[derive(Clone, Debug)]
pub struct Delivery {
    /// Which downstream this goes to.
    pub to: DownstreamId,
    /// The forwarded attestation (identical signed bytes).
    pub attestation: Attestation,
    /// Envelope metadata: live-stream (`true`) vs provisional
    /// warm-start (`false`). Never set while the relay's own
    /// upstream continuity is not Established.
    pub continuity_bearing: bool,
}

struct RelayKeyState {
    /// The relay's OWN continuity toward the provider — the hop
    /// rule's input.
    upstream: ObservationCell,
    /// Latest admitted attestation (the cache; never history).
    cached: Option<Attestation>,
}

#[derive(Clone, Copy)]
struct DeliverySlot {
    last_status: Option<AttestedStatus>,
    last_delivered: Option<(Incarnation, u64)>,
    next_due: Instant,
    /// A newer-than-delivered beat is waiting for the schedule.
    pending: bool,
}

/// An in-process sensing relay: seq gate + own upstream continuity +
/// interest table + per-downstream delivery schedule, all per
/// provider-targeted branch ([`ProviderInterestKey`]). SI-2/SI-4
/// wire this onto real sessions; the semantics are frozen here.
pub struct SensingRelay {
    factor: u32,
    gate: IncarnationSeqGate,
    /// The per-hop interest table (public: the harness drives
    /// registration outcomes/upstream actions through it).
    pub table: InterestTable,
    keys: HashMap<ProviderInterestKey, RelayKeyState>,
    slots: HashMap<(ProviderInterestKey, DownstreamId), DeliverySlot>,
}

impl SensingRelay {
    /// New relay with the given continuity factor k and
    /// per-downstream interest cap.
    pub fn new(factor: u32, max_interests_per_peer: usize) -> Self {
        Self {
            factor,
            gate: IncarnationSeqGate::new(),
            table: InterestTable::new(max_interests_per_peer),
            keys: HashMap::new(),
            slots: HashMap::new(),
        }
    }

    /// Register (or refresh) a downstream on a branch. ONLY a NEWLY
    /// CREATED row is warm-started from the cache (SI-4 review P1,
    /// carried into this relay by the re-review): a refresh resend
    /// must never restart a live delivery clock, clear pending work,
    /// or record itself as a live delivery — under D > ttl/2 with
    /// ttl/2 refreshes that starved the downstream to permanent
    /// provisional Unknown. The warm-start is ALWAYS provisional
    /// (`continuity_bearing = false`), regardless of the relay's own
    /// continuity: the downstream's optimism must be earned by a
    /// subsequent live beat, never seeded by a cache (§4.4).
    pub fn register_downstream(
        &mut self,
        key: &ProviderInterestKey,
        downstream: DownstreamId,
        requested_sample_interval: Duration,
        soft_state_ttl: Duration,
        owner_root: AudienceScopeCommitment,
        now: Instant,
    ) -> (RegisterOutcome, Option<Delivery>) {
        let outcome = self.table.register(
            key,
            downstream,
            requested_sample_interval,
            soft_state_ttl,
            owner_root,
            now,
        );
        if !matches!(outcome, RegisterOutcome::Registered(_)) {
            return (outcome, None);
        }
        let factor = self.factor;
        let state = self
            .keys
            .entry(key.clone())
            .or_insert_with(|| RelayKeyState {
                upstream: ObservationCell::register(now, requested_sample_interval, factor),
                cached: None,
            });
        let is_new_row = !self.slots.contains_key(&(key.clone(), downstream));
        let slot = self
            .slots
            .entry((key.clone(), downstream))
            .or_insert(DeliverySlot {
                last_status: None,
                last_delivered: None,
                next_due: now,
                pending: false,
            });
        let warm_start = if is_new_row {
            state.cached.as_ref().map(|cached| {
                slot.last_status = Some(cached.status);
                slot.last_delivered = Some((cached.origin_incarnation, cached.seq));
                slot.next_due = now + requested_sample_interval;
                slot.pending = false;
                Delivery {
                    to: downstream,
                    attestation: cached.clone(),
                    continuity_bearing: false,
                }
            })
        } else {
            None
        };
        (outcome, warm_start)
    }

    /// Ingest one attestation from upstream (origin emission or a
    /// relay forward; `upstream_bearing` is the received envelope
    /// flag). Returns the deliveries this triggers: status edges
    /// flush to every live downstream immediately; unchanged beats
    /// go only to downstreams whose schedule is due.
    pub fn on_attestation(
        &mut self,
        now: Instant,
        attestation: &Attestation,
        upstream_bearing: bool,
    ) -> Vec<Delivery> {
        let branch = attestation.branch();
        let Some(state) = self.keys.get_mut(&branch) else {
            // No registered interest — no idle work, no cache.
            return Vec::new();
        };
        if !self
            .gate
            .admit(
                attestation.key.provider,
                attestation.key.interest.interest_digest,
                attestation.origin_incarnation,
                attestation.seq,
                attestation.fingerprint,
            )
            .is_admitted()
        {
            return Vec::new();
        }
        state
            .upstream
            .on_admitted_beat(now, attestation.as_beat(upstream_bearing));
        self.table
            .set_upstream_continuity(&branch, state.upstream.continuity());
        state.cached = Some(attestation.clone());

        // The hop rule: outgoing bearing derives from the relay's OWN
        // continuity, never from the incoming flag alone.
        let bearing = state.upstream.continuity() == Continuity::Established;
        let mut deliveries = Vec::new();
        for downstream in self.table.downstreams(&branch, now) {
            let Some(row) = self.table.downstream_entry(&branch, downstream) else {
                continue;
            };
            let interval = row.requested_sample_interval;
            let Some(slot) = self.slots.get_mut(&(branch.clone(), downstream)) else {
                continue;
            };
            let edge = slot.last_status != Some(attestation.status);
            let due = now >= slot.next_due;
            if edge || due {
                slot.last_status = Some(attestation.status);
                slot.last_delivered = Some((attestation.origin_incarnation, attestation.seq));
                slot.next_due = now + interval;
                slot.pending = false;
                deliveries.push(Delivery {
                    to: downstream,
                    attestation: attestation.clone(),
                    continuity_bearing: bearing,
                });
            } else {
                slot.pending = true;
            }
        }
        deliveries
    }

    /// Drive the relay's clock: expire upstream continuity windows
    /// and flush pending down-sampled beats whose schedule came due.
    pub fn poll(&mut self, now: Instant) -> Vec<Delivery> {
        let mut deliveries = Vec::new();
        for (key, state) in self.keys.iter_mut() {
            state.upstream.expire_if_due(now);
            self.table
                .set_upstream_continuity(key, state.upstream.continuity());
            let Some(cached) = &state.cached else {
                continue;
            };
            let bearing = state.upstream.continuity() == Continuity::Established;
            for downstream in self.table.downstreams(key, now) {
                let Some(row) = self.table.downstream_entry(key, downstream) else {
                    continue;
                };
                let interval = row.requested_sample_interval;
                let Some(slot) = self.slots.get_mut(&(key.clone(), downstream)) else {
                    continue;
                };
                let newer = slot
                    .last_delivered
                    .is_none_or(|prev| (cached.origin_incarnation, cached.seq) > prev);
                if slot.pending && newer && now >= slot.next_due {
                    slot.last_status = Some(cached.status);
                    slot.last_delivered = Some((cached.origin_incarnation, cached.seq));
                    slot.next_due = now + interval;
                    slot.pending = false;
                    deliveries.push(Delivery {
                        to: downstream,
                        attestation: cached.clone(),
                        continuity_bearing: bearing,
                    });
                }
            }
        }
        deliveries
    }

    /// The relay's own upstream continuity for a branch (test
    /// introspection).
    pub fn upstream_continuity(&self, key: &ProviderInterestKey) -> Option<Continuity> {
        self.keys.get(key).map(|state| state.upstream.continuity())
    }

    /// SI-4 re-review (leader relay reclamation): drop every cached
    /// and scheduling trace of a branch whose last table row died —
    /// the cache must never warm-start a later same-key lifecycle,
    /// and an abandoned relay must drain to EMPTY, not to "empty
    /// table plus retained private state". The seq gate deliberately
    /// stays: strictly-newer admission must survive branch churn (a
    /// replayed old beat must not re-admit after a re-registration),
    /// and it is LRU-bounded (SI-1c), so churn cannot grow it.
    pub fn reclaim_branch(&mut self, key: &ProviderInterestKey) {
        self.keys.remove(key);
        self.slots.retain(|(branch, _), _| branch != key);
    }

    /// SI-4 re-review: GC delivery slots whose downstream row died
    /// while the branch survives — the mesh sweep's all-slot
    /// liveness rule, on the relay's own state.
    pub fn gc_dead_slots(&mut self, now: Instant) {
        let table = &self.table;
        self.slots.retain(|(branch, downstream), _| {
            table
                .downstream_entry(branch, *downstream)
                .is_some_and(|row| row.expires_at > now)
        });
    }

    /// Whether the relay retains no table rows, no branch caches,
    /// and no delivery schedules — the HONEST drained state (SI-4
    /// re-review: draining must account for private state, not just
    /// the table).
    pub fn is_drained(&self) -> bool {
        self.table.is_empty() && self.keys.is_empty() && self.slots.is_empty()
    }

    /// Retained branch caches (tests/observability).
    pub fn retained_branches(&self) -> usize {
        self.keys.len()
    }

    /// Retained delivery slots (tests/observability).
    pub fn retained_slots(&self) -> usize {
        self.slots.len()
    }
}

/// A terminal consumer: seq gate + one `ObservationCell` per
/// registered branch. The Layer-1 controller reads
/// [`Self::branch_projections`] to compute its LOCAL result-mode
/// aggregate (plan §3.5); this type deliberately has no aggregate
/// opinion of its own.
pub struct SensingConsumer {
    factor: u32,
    gate: IncarnationSeqGate,
    cells: HashMap<ProviderInterestKey, ObservationCell>,
}

impl SensingConsumer {
    /// New consumer with continuity factor k.
    pub fn new(factor: u32) -> Self {
        Self {
            factor,
            gate: IncarnationSeqGate::new(),
            cells: HashMap::new(),
        }
    }

    /// Register interest in a resolved branch at this consumer's
    /// own D.
    pub fn register_interest(
        &mut self,
        key: &ProviderInterestKey,
        own_interval: Duration,
        now: Instant,
    ) {
        self.cells.insert(
            key.clone(),
            ObservationCell::register(now, own_interval, self.factor),
        );
    }

    /// Ingest one delivery: gate first, then the continuity cell.
    pub fn on_delivery(&mut self, now: Instant, delivery: &Delivery) {
        let attestation = &delivery.attestation;
        let Some(cell) = self.cells.get_mut(&attestation.branch()) else {
            return;
        };
        if !self
            .gate
            .admit(
                attestation.key.provider,
                attestation.key.interest.interest_digest,
                attestation.origin_incarnation,
                attestation.seq,
                attestation.fingerprint,
            )
            .is_admitted()
        {
            return;
        }
        cell.on_admitted_beat(now, attestation.as_beat(delivery.continuity_bearing));
    }

    /// Drive the clock across every registered branch.
    pub fn poll(&mut self, now: Instant) {
        for cell in self.cells.values_mut() {
            cell.expire_if_due(now);
        }
    }

    /// Current projection for a branch (unregistered → Unknown).
    pub fn projected(&self, key: &ProviderInterestKey) -> ProjectedReadiness {
        self.cells
            .get(key)
            .map(ObservationCell::projected)
            .unwrap_or(ProjectedReadiness::Unknown)
    }

    /// Per-provider projections (+ provider start estimates) for one
    /// capability interest — the Layer-1 aggregate's input (§3.5).
    pub fn branch_projections(
        &self,
        interest: &CapabilityInterestKey,
    ) -> Vec<(u64, ProjectedReadiness, Option<Duration>)> {
        self.cells
            .iter()
            .filter(|(key, _)| &key.interest == interest)
            .map(|(key, cell)| {
                (
                    key.provider,
                    cell.projected(),
                    cell.observation().and_then(|obs| obs.estimated_start),
                )
            })
            .collect()
    }

    /// The cell for a branch (test introspection).
    pub fn cell(&self, key: &ProviderInterestKey) -> Option<&ObservationCell> {
        self.cells.get(key)
    }
}

#[cfg(test)]
mod tests {
    use super::super::identity::{
        CanonicalConstraints, CapabilityId, DisclosureClass, InterestSpec, ProviderSelector,
        ResultMode, WorkLatencyEnvelope,
    };
    use super::*;

    const K: u32 = 3;
    const ORIGIN: u64 = 0xE0;
    const GEN: u64 = 4;
    const TTL: Duration = Duration::from_secs(30);
    const CADENCE: Duration = Duration::from_millis(100);

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    fn root() -> AudienceScopeCommitment {
        AudienceScopeCommitment::from_bytes([0xAA; 32])
    }

    fn key_for(fps: &str) -> ProviderInterestKey {
        let spec = InterestSpec {
            capability_id: CapabilityId::new("video.transcode"),
            constraints: CanonicalConstraints::from_entries([("fps", fps)]).unwrap(),
            work_latency: WorkLatencyEnvelope::start_within(ms(200)),
            providers: ProviderSelector::AnyAuthorized,
            result_mode: ResultMode::Any,
            disclosure_class: DisclosureClass::Owner,
            audience: root(),
        };
        ProviderInterestKey::new(spec.key(), ORIGIN)
    }

    /// Scripted origin: emits live attestations with its own
    /// incarnation + seq counter.
    struct TestOrigin {
        incarnation: Incarnation,
        seq: u64,
    }

    impl TestOrigin {
        fn new(incarnation: u64) -> Self {
            Self {
                incarnation: Incarnation::new(incarnation),
                seq: 0,
            }
        }

        fn emit(&mut self, key: &ProviderInterestKey, status: AttestedStatus) -> Attestation {
            self.seq += 1;
            Attestation::new(
                ProviderObservationKey::new(key.interest.clone(), key.provider, GEN),
                self.incarnation,
                status,
                None,
                self.seq,
                CADENCE,
            )
        }
    }

    /// Feed a set of relay deliveries addressed to `who` into a
    /// consumer.
    fn feed(consumer: &mut SensingConsumer, who: DownstreamId, now: Instant, out: &[Delivery]) {
        for delivery in out.iter().filter(|d| d.to == who) {
            consumer.on_delivery(now, delivery);
        }
    }

    #[test]
    fn two_interests_on_one_capability_stay_independent() {
        // SI-0 test 7: 720p@30 and 4K@60 are two keys, two
        // observations, two lifecycles — one going Unknown never
        // touches the other.
        let t0 = Instant::now();
        let k30 = key_for("30");
        let k60 = key_for("60");
        let a = DownstreamId::Peer(1);
        let mut origin = TestOrigin::new(1);
        let mut relay = SensingRelay::new(K, 512);
        let mut consumer = SensingConsumer::new(K);

        for key in [&k30, &k60] {
            consumer.register_interest(key, ms(100), t0);
            relay.register_downstream(key, a, ms(100), TTL, root(), t0);
        }

        // Both streams live: one Ready, one NotReady.
        for tick in 1..=3u64 {
            let now = t0 + CADENCE * u32::try_from(tick).unwrap();
            let out30 = relay.on_attestation(now, &origin.emit(&k30, AttestedStatus::Ready), true);
            let out60 =
                relay.on_attestation(now, &origin.emit(&k60, AttestedStatus::NotReady), true);
            feed(&mut consumer, a, now, &out30);
            feed(&mut consumer, a, now, &out60);
        }
        assert_eq!(consumer.projected(&k30), ProjectedReadiness::Ready);
        assert_eq!(consumer.projected(&k60), ProjectedReadiness::NotReady);

        // The 4K@60 stream dies; 720p@30 keeps beating. Only the
        // dead key degrades.
        for tick in 4..=20u64 {
            let now = t0 + CADENCE * u32::try_from(tick).unwrap();
            let out = relay.on_attestation(now, &origin.emit(&k30, AttestedStatus::Ready), true);
            feed(&mut consumer, a, now, &out);
            consumer.poll(now);
        }
        assert_eq!(consumer.projected(&k30), ProjectedReadiness::Ready);
        assert_eq!(consumer.projected(&k60), ProjectedReadiness::Unknown);
    }

    #[test]
    fn origin_restart_behind_relay_rejects_delayed_old_incarnation() {
        // SI-0 test 8: the new incarnation is admitted through the
        // relay; a delayed old-incarnation Ready is rejected at the
        // relay's gate and never reaches the consumer.
        let t0 = Instant::now();
        let key = key_for("30");
        let a = DownstreamId::Peer(1);
        let mut relay = SensingRelay::new(K, 512);
        let mut consumer = SensingConsumer::new(K);
        consumer.register_interest(&key, ms(100), t0);
        relay.register_downstream(&key, a, ms(100), TTL, root(), t0);

        // Old incarnation, live.
        let mut old = TestOrigin::new(7);
        let t1 = t0 + ms(100);
        let out = relay.on_attestation(t1, &old.emit(&key, AttestedStatus::Ready), true);
        feed(&mut consumer, a, t1, &out);
        assert_eq!(consumer.projected(&key), ProjectedReadiness::Ready);

        // Restart: incarnation 8 supersedes.
        let mut new = TestOrigin::new(8);
        let t2 = t1 + ms(100);
        let out = relay.on_attestation(t2, &new.emit(&key, AttestedStatus::Ready), true);
        assert_eq!(out.len(), 1);
        feed(&mut consumer, a, t2, &out);
        assert_eq!(
            consumer
                .cell(&key)
                .unwrap()
                .observation()
                .unwrap()
                .source_incarnation,
            Incarnation::new(8),
        );

        // A delayed old-incarnation beat (higher seq!) arrives late:
        // dropped at the relay, consumer view unchanged.
        old.seq = 100;
        let t3 = t2 + ms(100);
        let out = relay.on_attestation(t3, &old.emit(&key, AttestedStatus::Ready), false);
        assert!(out.is_empty(), "stale incarnation must not be forwarded");
        assert_eq!(
            consumer
                .cell(&key)
                .unwrap()
                .observation()
                .unwrap()
                .source_incarnation,
            Incarnation::new(8),
        );
        assert_eq!(consumer.projected(&key), ProjectedReadiness::Ready);
    }

    #[test]
    fn down_sampling_edges_and_provisional_warm_start() {
        // SI-0 test 11. Strict watcher A (D = origin cadence) sees
        // every beat; loose watcher B (D = 5×cadence) is delivered at
        // its OWN D and is never false-Unknowned; a status edge
        // reaches both immediately; late joiners warm-start as
        // provisional (cached Ready → Unknown, cached NotReady →
        // NotReady).
        let t0 = Instant::now();
        let key = key_for("30");
        let (a, b) = (DownstreamId::Peer(1), DownstreamId::Peer(2));
        let mut origin = TestOrigin::new(1);
        let mut relay = SensingRelay::new(K, 512);
        let mut watcher_a = SensingConsumer::new(K);
        let mut watcher_b = SensingConsumer::new(K);
        watcher_a.register_interest(&key, ms(100), t0);
        watcher_b.register_interest(&key, ms(500), t0);
        relay.register_downstream(&key, a, ms(100), TTL, root(), t0);
        relay.register_downstream(&key, b, ms(500), TTL, root(), t0);

        let (mut count_a, mut count_b) = (0usize, 0usize);
        for tick in 1..=10u64 {
            let now = t0 + CADENCE * u32::try_from(tick).unwrap();
            let out = relay.on_attestation(now, &origin.emit(&key, AttestedStatus::Ready), true);
            count_a += out.iter().filter(|d| d.to == a).count();
            count_b += out.iter().filter(|d| d.to == b).count();
            feed(&mut watcher_a, a, now, &out);
            feed(&mut watcher_b, b, now, &out);
            watcher_a.poll(now);
            watcher_b.poll(now);
            // The loose watcher's own D dominates its window
            // (3 × 500ms) — down-sampled delivery must never
            // false-Unknown it.
            assert_eq!(
                watcher_b.projected(&key),
                ProjectedReadiness::Ready,
                "loose watcher false-Unknowned at tick {tick}",
            );
        }
        assert_eq!(count_a, 10, "strict watcher sees the full cadence");
        assert_eq!(
            count_b, 2,
            "loose watcher is delivered at its own D, not the origin cadence",
        );

        // Status edge at an off-schedule moment: BOTH watchers get it
        // immediately (B's next_due is still ~400ms out).
        let t_edge = t0 + CADENCE * 10 + ms(50);
        let out = relay.on_attestation(t_edge, &origin.emit(&key, AttestedStatus::NotReady), true);
        assert_eq!(
            out.iter().filter(|d| d.to == b).count(),
            1,
            "a status edge is never held by the down-sampler",
        );
        feed(&mut watcher_a, a, t_edge, &out);
        feed(&mut watcher_b, b, t_edge, &out);
        assert_eq!(watcher_a.projected(&key), ProjectedReadiness::NotReady);
        assert_eq!(watcher_b.projected(&key), ProjectedReadiness::NotReady);

        // Late joiner while the cache holds NotReady: warm-start is
        // provisional AND projects immediately (pessimism is safe).
        let t_join = t_edge + ms(10);
        let mut joiner = SensingConsumer::new(K);
        joiner.register_interest(&key, ms(100), t_join);
        let (_, warm) =
            relay.register_downstream(&key, DownstreamId::Peer(3), ms(100), TTL, root(), t_join);
        let warm = warm.expect("cache must warm-start a late joiner");
        assert!(
            !warm.continuity_bearing,
            "warm-starts are always provisional"
        );
        joiner.on_delivery(t_join, &warm);
        assert_eq!(joiner.projected(&key), ProjectedReadiness::NotReady);

        // And the Ready flavor: flip the stream back to Ready, then
        // join. The cached Ready warm-start projects Unknown until a
        // continuity-bearing strictly-newer beat lands — the
        // single-hop freshness-laundering tripwire.
        let t_flip = t_join + ms(40);
        let out = relay.on_attestation(t_flip, &origin.emit(&key, AttestedStatus::Ready), true);
        assert!(!out.is_empty());
        let t_join2 = t_flip + ms(10);
        let mut joiner2 = SensingConsumer::new(K);
        joiner2.register_interest(&key, ms(100), t_join2);
        let (_, warm) =
            relay.register_downstream(&key, DownstreamId::Peer(4), ms(100), TTL, root(), t_join2);
        let warm = warm.expect("cache must warm-start the second joiner");
        assert!(!warm.continuity_bearing);
        joiner2.on_delivery(t_join2, &warm);
        assert_eq!(
            joiner2.projected(&key),
            ProjectedReadiness::Unknown,
            "cached Ready must not project Ready through a warm-start",
        );
        // The next live beat is delivered continuity-bearing and
        // earns the projection.
        let t_live = t_join2 + ms(100);
        let out = relay.on_attestation(t_live, &origin.emit(&key, AttestedStatus::Ready), true);
        feed(&mut joiner2, DownstreamId::Peer(4), t_live, &out);
        assert_eq!(joiner2.projected(&key), ProjectedReadiness::Ready);
    }

    #[test]
    fn refresh_never_resets_a_live_delivery_schedule() {
        // SI-4 re-review P1 (the mesh relay's warm-start discipline,
        // carried into this relay): a ttl/2 refresh must not
        // warm-start, restart the delivery clock, or clear pending
        // work — under D > ttl/2 that starved the downstream to
        // permanent provisional Unknown.
        let t0 = Instant::now();
        let key = key_for("30");
        let a = DownstreamId::Peer(1);
        let mut origin = TestOrigin::new(1);
        let mut relay = SensingRelay::new(K, 512);
        relay.register_downstream(&key, a, ms(500), TTL, root(), t0);

        // First live beat: an edge — delivered immediately, schedule
        // re-arms to t1 + 500.
        let t1 = t0 + ms(100);
        let out = relay.on_attestation(t1, &origin.emit(&key, AttestedStatus::Ready), true);
        assert_eq!(out.iter().filter(|d| d.to == a).count(), 1);

        // A newer unchanged beat inside D parks as pending.
        let t2 = t1 + ms(100);
        let out = relay.on_attestation(t2, &origin.emit(&key, AttestedStatus::Ready), true);
        assert!(out.is_empty(), "inside D: the beat waits for the schedule");

        // The ttl/2-style refresh: no warm-start (the row exists),
        // and the parked work + schedule survive untouched.
        let t3 = t2 + ms(100);
        let (outcome, warm) = relay.register_downstream(&key, a, ms(500), TTL, root(), t3);
        assert!(matches!(outcome, RegisterOutcome::Registered(_)));
        assert!(warm.is_none(), "a refresh must not re-send the cache");

        // The pending beat flushes at the ORIGINAL schedule — a
        // reset clock (t3 + 500) would still hold it parked here.
        let t4 = t1 + ms(500);
        let out = relay.poll(t4);
        let flushed: Vec<&Delivery> = out.iter().filter(|d| d.to == a).collect();
        assert_eq!(
            flushed.len(),
            1,
            "pending live work flushes on the un-reset schedule",
        );
        assert_eq!(flushed[0].attestation.seq, 2);
    }

    #[test]
    fn multi_hop_cache_chain_cannot_launder_continuity() {
        // SI-0 test 13: X → C → B → A with staggered caches. C holds
        // cached seq 101, B holds cached seq 100, X is silent. A
        // registers → warm-started with 100 → Unknown. C's cached 101
        // later reaches A via B — strictly newer post-registration,
        // yet A MUST still be Unknown, because B's own upstream
        // continuity was never Established (the hop rule). Only a
        // live X beat establishes hop-by-hop.
        let t0 = Instant::now();
        let key = key_for("30");
        let mut origin = TestOrigin::new(1);
        // Align the counter with the plan's scenario numbering: the
        // next three emissions are seqs 100, 101, 102.
        origin.seq = 99;
        let mut relay_c = SensingRelay::new(K, 512);
        let mut relay_b = SensingRelay::new(K, 512);
        // C's OWN local watcher keeps the branch (and its cache)
        // alive; B's subscription at C carries a short ttl so its
        // row can lapse and re-join below — after the SI-4
        // re-review, a NEW row lifecycle is the only path to a
        // cache warm-start (refreshes never re-send).
        relay_c.register_downstream(&key, DownstreamId::Local, ms(100), TTL, root(), t0);
        relay_c.register_downstream(&key, DownstreamId::Peer(0xB), ms(100), ms(500), root(), t0);
        relay_b.register_downstream(&key, DownstreamId::Local, ms(100), TTL, root(), t0);

        // Seq 100 flows the whole chain live.
        let t1 = t0 + ms(100);
        let att100 = origin.emit(&key, AttestedStatus::Ready);
        let out_c = relay_c.on_attestation(t1, &att100, true);
        let to_b = out_c
            .iter()
            .find(|d| d.to == DownstreamId::Peer(0xB))
            .expect("C forwards the live beat to B");
        assert!(to_b.continuity_bearing);
        relay_b.on_attestation(t1, &to_b.attestation, to_b.continuity_bearing);

        // Seq 101 reaches C but is LOST between C and B.
        let t2 = t1 + ms(100);
        let att101 = origin.emit(&key, AttestedStatus::Ready);
        let _lost = relay_c.on_attestation(t2, &att101, true);

        // X goes silent long enough for every upstream window to
        // expire (window = 3 × 100ms).
        let t3 = t2 + ms(400);
        relay_c.poll(t3);
        relay_b.poll(t3);
        assert_eq!(relay_c.upstream_continuity(&key), Some(Continuity::Expired));
        assert_eq!(relay_b.upstream_continuity(&key), Some(Continuity::Expired));

        // A registers at B: warm-started from B's cache (seq 100),
        // provisional → Unknown.
        let mut a = SensingConsumer::new(K);
        a.register_interest(&key, ms(100), t3);
        let (_, warm) =
            relay_b.register_downstream(&key, DownstreamId::Peer(0xA), ms(100), TTL, root(), t3);
        let warm = warm.expect("B's cache warm-starts A");
        assert_eq!(warm.attestation.seq, 100);
        assert!(!warm.continuity_bearing);
        a.on_delivery(t3, &warm);
        assert_eq!(a.projected(&key), ProjectedReadiness::Unknown);

        // B's short-ttl row at C lapsed during the silence; the
        // sweep shape GCs the dead row + slot while C's local
        // watcher keeps the branch cache. B then RE-registers — a
        // NEW row lifecycle, so C warm-starts it with its cached
        // 101, provisional (C's continuity is Expired).
        relay_c.table.expire(t3);
        relay_c.gc_dead_slots(t3);
        let (_, warm_b) =
            relay_c.register_downstream(&key, DownstreamId::Peer(0xB), ms(100), TTL, root(), t3);
        let warm_b = warm_b.expect("C's cache warm-starts B's re-registration");
        assert_eq!(warm_b.attestation.seq, 101);
        assert!(!warm_b.continuity_bearing);
        let out = relay_b.on_attestation(t3, &warm_b.attestation, warm_b.continuity_bearing);
        // B may forward 101 to A on its schedule — as provisional
        // only (B's upstream continuity is NOT Established).
        let t4 = t3 + ms(100);
        let mut forwarded: Vec<Delivery> = out;
        forwarded.extend(relay_b.poll(t4));
        let to_a: Vec<&Delivery> = forwarded
            .iter()
            .filter(|d| d.to == DownstreamId::Peer(0xA))
            .collect();
        assert!(
            !to_a.is_empty(),
            "the cached 101 does reach A (down-sampled catch-up)",
        );
        for delivery in &to_a {
            assert!(
                !delivery.continuity_bearing,
                "hop rule: B must not deliver continuity-bearing while its own \
                 upstream continuity is Expired",
            );
            a.on_delivery(t4, delivery);
        }
        // THE assertion: strictly-newer post-registration seq via a
        // cache chain does not establish continuity.
        assert_eq!(
            a.projected(&key),
            ProjectedReadiness::Unknown,
            "multi-hop cache laundering: A projected optimism from a dead stream",
        );

        // X resumes: seq 102 live → C establishes → B establishes →
        // A establishes; Ready propagates hop-by-hop.
        let t5 = t4 + ms(100);
        let att102 = origin.emit(&key, AttestedStatus::Ready);
        let out_c = relay_c.on_attestation(t5, &att102, true);
        let to_b = out_c
            .iter()
            .find(|d| d.to == DownstreamId::Peer(0xB))
            .expect("C forwards the live beat to B");
        assert!(to_b.continuity_bearing, "C is Established again");
        let out_b = relay_b.on_attestation(t5, &to_b.attestation, to_b.continuity_bearing);
        let to_a = out_b
            .iter()
            .find(|d| d.to == DownstreamId::Peer(0xA))
            .expect("B forwards the live beat to A");
        assert!(to_a.continuity_bearing, "B is Established again");
        a.on_delivery(t5, to_a);
        assert_eq!(a.projected(&key), ProjectedReadiness::Ready);
    }
}
