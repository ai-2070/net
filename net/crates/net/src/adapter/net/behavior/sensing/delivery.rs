//! Relay delivery: store, pack, down-sample (plan §4.4) — the
//! in-process composition of gate + continuity + interest table.
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
//! - **hop-by-hop continuity** (§4.4, v3.1): a relay MUST NOT
//!   deliver as continuity-bearing while its OWN upstream continuity
//!   for the key is Unestablished or Expired. The flag is local
//!   delivery metadata on the relay→downstream envelope (which the
//!   relay authors and the session authenticates) — never a field
//!   inside the origin-signed attestation. Establishment therefore
//!   propagates hop-by-hop from the live origin stream, and a chain
//!   of staggered caches cannot manufacture apparent
//!   post-registration continuity (SI-0 test 13).
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
use super::identity::{AudienceScopeCommitment, Digest256, ReadinessKey};
use super::incarnation::{Incarnation, IncarnationSeqGate};
use super::table::{DownstreamId, InterestTable, RegisterOutcome};

/// One origin-signed readiness attestation (plan §4.2, semantic
/// form). Relays forward these bytes identically — suppress or
/// delay, never alter.
#[derive(Clone, Debug)]
pub struct Attestation {
    /// Provider node id.
    pub origin: u64,
    /// Signed boot epoch (§4.6).
    pub origin_incarnation: Incarnation,
    /// The full conditional identity this attests.
    pub key: ReadinessKey,
    /// Signed status.
    pub status: AttestedStatus,
    /// Signed time-to-start estimate.
    pub estimated_start: Option<Duration>,
    /// Signed per-(incarnation, interest) sequence number.
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
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        origin: u64,
        origin_incarnation: Incarnation,
        key: ReadinessKey,
        status: AttestedStatus,
        estimated_start: Option<Duration>,
        seq: u64,
        promised_cadence: Duration,
    ) -> Self {
        let mut hasher = blake3::Hasher::new_derive_key("net.sensing.attestation.fingerprint.v1");
        hasher.update(&origin.to_le_bytes());
        hasher.update(&origin_incarnation.get().to_le_bytes());
        hasher.update(key.interest_digest.as_bytes());
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
            origin,
            origin_incarnation,
            key,
            status,
            estimated_start,
            seq,
            promised_cadence,
            fingerprint,
        }
    }

    fn as_beat(&self, continuity_bearing: bool) -> DeliveredBeat {
        DeliveredBeat {
            attested_status: self.status,
            estimated_start: self.estimated_start,
            source_incarnation: self.origin_incarnation,
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
    /// The relay's OWN continuity toward the origin — the hop rule's
    /// input.
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
/// interest table + per-downstream delivery schedule. SI-2/SI-4 wire
/// this onto real sessions; the semantics are frozen here.
pub struct SensingRelay {
    factor: u32,
    gate: IncarnationSeqGate,
    /// The per-hop interest table (public: the harness drives
    /// registration outcomes/upstream actions through it).
    pub table: InterestTable,
    keys: HashMap<ReadinessKey, RelayKeyState>,
    slots: HashMap<(ReadinessKey, DownstreamId), DeliverySlot>,
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

    /// Register (or refresh) a downstream and warm-start it from the
    /// cache: every registration re-sends the cached latest per key
    /// (anti-entropy for forwards lost in transit; the downstream's
    /// gate absorbs duplicates as StaleSeq) — ALWAYS as provisional
    /// (`continuity_bearing = false`), regardless of the relay's own
    /// continuity: the downstream's optimism must be earned by a
    /// subsequent live beat, never seeded by a cache (§4.4).
    #[allow(clippy::too_many_arguments)]
    pub fn register_downstream(
        &mut self,
        key: &ReadinessKey,
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
        let slot = self
            .slots
            .entry((key.clone(), downstream))
            .or_insert(DeliverySlot {
                last_status: None,
                last_delivered: None,
                next_due: now,
                pending: false,
            });
        // Every registration — including a ttl/2 refresh — re-sends
        // the cached latest. That is the anti-entropy for a forward
        // lost in transit (the relay's slot believed it delivered);
        // a downstream that already holds the beat absorbs the
        // duplicate at its gate as StaleSeq.
        let warm_start = state.cached.as_ref().map(|cached| {
            slot.last_status = Some(cached.status);
            slot.last_delivered = Some((cached.origin_incarnation, cached.seq));
            slot.next_due = now + requested_sample_interval;
            slot.pending = false;
            Delivery {
                to: downstream,
                attestation: cached.clone(),
                continuity_bearing: false,
            }
        });
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
        let Some(state) = self.keys.get_mut(&attestation.key) else {
            // No registered interest — no idle work, no cache.
            return Vec::new();
        };
        if !self
            .gate
            .admit(
                attestation.origin,
                attestation.key.interest_digest,
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
            .set_upstream_continuity(&attestation.key, state.upstream.continuity());
        state.cached = Some(attestation.clone());

        // The hop rule: outgoing bearing derives from the relay's OWN
        // continuity, never from the incoming flag alone.
        let bearing = state.upstream.continuity() == Continuity::Established;
        let mut deliveries = Vec::new();
        for downstream in self.table.downstreams(&attestation.key, now) {
            let Some(row) = self.table.downstream_entry(&attestation.key, downstream) else {
                continue;
            };
            let interval = row.requested_sample_interval;
            let Some(slot) = self.slots.get_mut(&(attestation.key.clone(), downstream)) else {
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

    /// The relay's own upstream continuity for a key (test
    /// introspection).
    pub fn upstream_continuity(&self, key: &ReadinessKey) -> Option<Continuity> {
        self.keys.get(key).map(|state| state.upstream.continuity())
    }
}

/// A terminal consumer: seq gate + one `ObservationCell` per
/// registered key. This is the shape SI-4's fold-overlay apply layer
/// takes; kept minimal here.
pub struct SensingConsumer {
    factor: u32,
    gate: IncarnationSeqGate,
    cells: HashMap<ReadinessKey, ObservationCell>,
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

    /// Register interest in a key at this consumer's own D.
    pub fn register_interest(&mut self, key: &ReadinessKey, own_interval: Duration, now: Instant) {
        self.cells.insert(
            key.clone(),
            ObservationCell::register(now, own_interval, self.factor),
        );
    }

    /// Ingest one delivery: gate first, then the continuity cell.
    pub fn on_delivery(&mut self, now: Instant, delivery: &Delivery) {
        let attestation = &delivery.attestation;
        let Some(cell) = self.cells.get_mut(&attestation.key) else {
            return;
        };
        if !self
            .gate
            .admit(
                attestation.origin,
                attestation.key.interest_digest,
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

    /// Drive the clock across every registered key.
    pub fn poll(&mut self, now: Instant) {
        for cell in self.cells.values_mut() {
            cell.expire_if_due(now);
        }
    }

    /// Current projection for a key (unregistered → Unknown).
    pub fn projected(&self, key: &ReadinessKey) -> ProjectedReadiness {
        self.cells
            .get(key)
            .map(ObservationCell::projected)
            .unwrap_or(ProjectedReadiness::Unknown)
    }

    /// The cell for a key (test introspection).
    pub fn cell(&self, key: &ReadinessKey) -> Option<&ObservationCell> {
        self.cells.get(key)
    }
}

#[cfg(test)]
mod tests {
    use super::super::identity::{
        CanonicalConstraints, CapabilityId, DisclosureClass, InterestSpec, WorkLatencyEnvelope,
    };
    use super::*;

    const K: u32 = 3;
    const ORIGIN: u64 = 0xE0;
    const TTL: Duration = Duration::from_secs(30);
    const CADENCE: Duration = Duration::from_millis(100);

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    fn root() -> AudienceScopeCommitment {
        AudienceScopeCommitment::from_bytes([0xAA; 32])
    }

    fn key_for(fps: &str) -> ReadinessKey {
        let spec = InterestSpec {
            capability_id: CapabilityId::new("video.transcode"),
            capability_generation: 4,
            constraints: CanonicalConstraints::from_entries([("fps", fps)]).unwrap(),
            work_latency: WorkLatencyEnvelope { max_start: ms(200) },
            disclosure_class: DisclosureClass::Owner,
            audience: root(),
        };
        ReadinessKey::for_interest(ORIGIN, &spec)
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

        fn emit(&mut self, key: &ReadinessKey, status: AttestedStatus) -> Attestation {
            self.seq += 1;
            Attestation::new(
                ORIGIN,
                self.incarnation,
                key.clone(),
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
            if tick >= 1 {
                assert_eq!(
                    watcher_b.projected(&key),
                    ProjectedReadiness::Ready,
                    "loose watcher false-Unknowned at tick {tick}",
                );
            }
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
        // B was subscribed at C all along (its earlier local watcher
        // keeps the branch alive), and B has a local row too.
        relay_c.register_downstream(&key, DownstreamId::Peer(0xB), ms(100), TTL, root(), t0);
        relay_b.register_downstream(&key, DownstreamId::Local, ms(100), TTL, root(), t0);

        // Seq 100 flows the whole chain live.
        let t1 = t0 + ms(100);
        let att100 = origin.emit(&key, AttestedStatus::Ready);
        let out_c = relay_c.on_attestation(t1, &att100, true);
        assert_eq!(out_c.len(), 1);
        let to_b = &out_c[0];
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

        // B refreshes upstream at C; C warm-starts B with its cached
        // 101 — provisional (C's continuity is Expired).
        let (_, warm_b) =
            relay_c.register_downstream(&key, DownstreamId::Peer(0xB), ms(100), TTL, root(), t3);
        let warm_b = warm_b.expect("C's cache warm-starts B's refresh");
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
