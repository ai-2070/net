//! SI-3 origin emitter (plan §4.4): the provider-side scheduler for
//! signed readiness streams.
//!
//! The origin compiles each distinct interest ONCE (the validated
//! constraints and envelope are cloned into the stream slot at
//! registration; a refresh of the same digest never re-parses),
//! evaluates against its CURRENT generation at every beat, and emits
//! one signed stream per distinct interest at
//! `promised_cadence = max(strictest-D / 2, attestation_cadence_floor)`
//! — status edges are pulled forward to "now" with the floor as the
//! min-gap, and an interest whose last downstream died emits nothing
//! at all (zero idle emission, plan §4.7).
//!
//! This state machine is deliberately **pure and crypto-free**: it
//! produces [`UnsignedAttestation`]s and the mesh layer signs them
//! (`sign_attestation`) where the keypair lives, so every scheduling
//! rule here is fake-clock testable without keys. A burned sequence
//! number on a (never-observed) signing failure is harmless: seq
//! gaps carry no meaning beyond strictly-newer admission (§4.4).
//!
//! # Sequence memory outlives the stream
//!
//! Seqs are per `(origin, origin_incarnation, interest_digest)`
//! (§4.6) and MUST NOT restart when a stream dies and re-forms
//! within one incarnation — a reset would replay `(incarnation,
//! seq)` pairs with different payloads, which downstream observer
//! gates rightly treat as equivocation and poison. Retired streams
//! therefore leave their seq counter behind in the slot map. The map
//! is bounded exactly like the observer gate it feeds
//! (`IncarnationSeqGate`, SI-1c): oldest-touched RETIRED slots evict
//! first past [`MAX_STREAM_SLOTS`], and LIVE slots are never evicted
//! — evicting a live stream's counter would be self-inflicted
//! equivocation. A re-registration after its retired slot was
//! evicted restarts at seq 0 and is contained at the observer gate
//! as an ordinary rollback (stale drops, never a flap).
//!
//! # Cadence refusal (one-shot, rides the attestation plane)
//!
//! A coalesced strictest D below the floor is refused
//! ([`check_cadence`]) — the caller partitions its downstreams
//! (`InterestTable::on_refusal`) and sends the refused ones ONE
//! signed refusal beat ([`OriginEmitter::refusal_beat`]): status
//! `ProviderUnknown` / `SamplingIntervalUnsupported`, with the
//! provider floor M carried in `promised_cadence` — the one field
//! that already means "the cadence this provider will serve", so the
//! §4.4 `sampling_interval_unsupported { minimum_supported: M }`
//! payload rides the frozen §4.2 wire object without a wire break.
//! Refusal beats draw from the same seq slot as the stream, so the
//! two can never collide on `(incarnation, seq)`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::evaluator::{
    check_cadence, project_evaluation, CadenceRefusal, EvaluationRequest, ReadinessEvaluation,
};
use super::identity::{
    AudienceScopeCommitment, CanonicalConstraints, CapabilityId, CapabilityInterestKey, Digest256,
    InterestSpec, WorkLatencyEnvelope,
};
use super::incarnation::Incarnation;
use super::wire::UnsignedAttestation;

/// Bound on the seq-slot map (live + retired), mirroring the
/// observer gate's bound (SI-1c): comfortably above any honest
/// interest population, small enough that a hostile registration
/// storm cannot grow memory unboundedly.
pub const MAX_STREAM_SLOTS: usize = 8192;

/// Eviction low-water mark: one sweep past [`MAX_STREAM_SLOTS`]
/// frees a batch of retired slots so steady-state churn does not
/// re-trigger the sweep on every insert.
pub const STREAM_SLOTS_LOW_WATER: usize = 6144;

/// One interest's compiled evaluation inputs + live schedule.
#[derive(Debug)]
struct LiveStream {
    /// Table/index identity of the stream (digest + capability id).
    key: CapabilityInterestKey,
    /// Compiled work characteristics C (validated at intake, cloned
    /// once — never re-parsed on refresh).
    constraints: CanonicalConstraints,
    /// Latency envelope L.
    work_latency: WorkLatencyEnvelope,
    /// The audience the interest was validated under — signed into
    /// every beat so a proof can never be re-homed (§4.2).
    audience: AudienceScopeCommitment,
    /// `max(strictest-D / 2, floor)` — recomputed whenever the
    /// caller re-registers with a moved aggregate.
    promised_cadence: Duration,
    /// Next scheduled emission.
    due_at: Instant,
    /// Last emission instant — the edge min-gap reference.
    last_emitted_at: Option<Instant>,
}

/// One digest's slot: the seq counter (which outlives the stream —
/// module docs) plus the live stream state, if any.
#[derive(Debug)]
struct StreamSlot {
    /// Next sequence number to sign for this digest.
    next_seq: u64,
    /// LRU touch stamp (monotonic per emitter, not a clock).
    touched: u64,
    /// `Some` while at least one downstream is interested.
    live: Option<LiveStream>,
}

/// The origin's emission scheduler (module docs). One per node;
/// single-writer by construction (the mesh serializes access), which
/// is what makes "never two payloads on one `(incarnation, seq)`"
/// structural.
#[derive(Debug)]
pub struct OriginEmitter {
    /// This node's id — the attestation `origin`.
    origin: u64,
    /// The §4.6 boot epoch every beat is scoped to (caller derives
    /// it via `next_incarnation` over real persistence,
    /// increment-before-participation).
    incarnation: Incarnation,
    /// `attestation_cadence_floor` (plan §5): cadence lower bound
    /// AND the status-edge min-gap.
    cadence_floor: Duration,
    /// Seq slots by interest digest — live streams + retired seq
    /// memory, LRU-bounded (module docs).
    slots: HashMap<Digest256, StreamSlot>,
    /// Monotonic LRU stamp source.
    touch_counter: u64,
}

impl OriginEmitter {
    /// New emitter for one `(origin, incarnation)` scope.
    pub fn new(origin: u64, incarnation: Incarnation, cadence_floor: Duration) -> Self {
        Self {
            origin,
            incarnation,
            cadence_floor,
            slots: HashMap::new(),
            touch_counter: 0,
        }
    }

    fn touch(&mut self) -> u64 {
        self.touch_counter += 1;
        self.touch_counter
    }

    /// Register (or refresh) one interest stream at the caller's
    /// current strictest aggregate D.
    ///
    /// - A strictest D below the floor is refused — the stream state
    ///   is left untouched; the caller partitions its downstreams
    ///   and answers the refused ones with [`Self::refusal_beat`].
    /// - First registration schedules the first beat at `now` (a new
    ///   stream answers promptly); a refresh only moves the schedule
    ///   when the CADENCE moved (a ttl/2 refresh must never starve
    ///   the cadence by pushing `due_at` forever forward).
    pub fn register(
        &mut self,
        spec: &InterestSpec,
        strictest: Duration,
        now: Instant,
    ) -> Result<(), CadenceRefusal> {
        check_cadence(strictest, self.cadence_floor)?;
        let promised_cadence = (strictest / 2).max(self.cadence_floor);
        let key = CapabilityInterestKey::for_spec(spec);
        let digest = key.interest_digest;
        let stamp = self.touch();
        let slot = self.slots.entry(digest).or_insert(StreamSlot {
            next_seq: 0,
            touched: 0,
            live: None,
        });
        slot.touched = stamp;
        match &mut slot.live {
            Some(stream) => {
                if stream.promised_cadence != promised_cadence {
                    stream.promised_cadence = promised_cadence;
                    // Re-derive the schedule from the last beat under
                    // the new cadence — a tightened aggregate pulls
                    // the next beat earlier, a loosened one pushes it
                    // out; either way the min-gap (floor) holds.
                    stream.due_at = match stream.last_emitted_at {
                        Some(last) => (last + promised_cadence).max(now),
                        None => now,
                    };
                }
            }
            None => {
                slot.live = Some(LiveStream {
                    key,
                    constraints: spec.constraints.clone(),
                    work_latency: spec.work_latency,
                    audience: spec.audience,
                    promised_cadence,
                    due_at: now,
                    last_emitted_at: None,
                });
            }
        }
        self.evict_retired();
        Ok(())
    }

    /// The stream's last downstream died (deregister, ttl sweep, or
    /// refusal partition) — stop emitting, keep the seq memory
    /// (module docs). Idempotent.
    pub fn retire(&mut self, digest: &Digest256) {
        if let Some(slot) = self.slots.get_mut(digest) {
            slot.live = None;
        }
    }

    /// A local state change on `capability_id` (the integration's
    /// notify hook): pull every live stream on that capability
    /// forward to "now", min-gapped at the floor since its last
    /// beat. Returns whether any schedule moved (the caller only
    /// needs to wake its loop if one did).
    ///
    /// Deliberately re-emits even if the re-evaluation lands on the
    /// same status: at most one early beat per poke, absorbed
    /// downstream by strictly-newer admission — cheaper and simpler
    /// than caching cross-beat comparisons here.
    pub fn poke(&mut self, capability_id: &CapabilityId, now: Instant) -> bool {
        let floor = self.cadence_floor;
        let mut moved = false;
        for slot in self.slots.values_mut() {
            let Some(stream) = &mut slot.live else {
                continue;
            };
            if stream.key.capability_id != *capability_id {
                continue;
            }
            let earliest = match stream.last_emitted_at {
                Some(last) => (last + floor).max(now),
                None => now,
            };
            if earliest < stream.due_at {
                stream.due_at = earliest;
                moved = true;
            }
        }
        moved
    }

    /// Earliest scheduled beat across live streams — the mesh
    /// loop's sleep target. `None` = fully idle (zero emission).
    pub fn next_due(&self) -> Option<Instant> {
        self.slots
            .values()
            .filter_map(|slot| slot.live.as_ref().map(|stream| stream.due_at))
            .min()
    }

    /// Evaluate and produce every due beat.
    ///
    /// `generation` is the provider's OWN announce generation at
    /// this instant — attested content, read at evaluation time
    /// (§3.4). `evaluate` resolves the integration for the request's
    /// capability and runs it; `None` (no evaluator registered)
    /// projects as `ProviderUnknown { TemporarilyUnevaluable }` — an
    /// explicit "I am targeted but cannot answer" stream beats
    /// silence, and it only costs while interests are live.
    pub fn tick(
        &mut self,
        now: Instant,
        generation: u64,
        mut evaluate: impl FnMut(&EvaluationRequest<'_>) -> Option<ReadinessEvaluation>,
    ) -> Vec<(CapabilityInterestKey, UnsignedAttestation)> {
        let mut beats = Vec::new();
        let stamp = self.touch();
        for (digest, slot) in &mut self.slots {
            let Some(stream) = &mut slot.live else {
                continue;
            };
            if stream.due_at > now {
                continue;
            }
            let request = EvaluationRequest {
                capability_id: &stream.key.capability_id,
                constraints: &stream.constraints,
                work_latency: &stream.work_latency,
            };
            let evaluation =
                evaluate(&request).unwrap_or(ReadinessEvaluation::TemporarilyUnevaluable);
            let (status, status_reason) = project_evaluation(&evaluation);
            let estimated_start = match evaluation {
                ReadinessEvaluation::Ready { estimated_start } => estimated_start,
                _ => None,
            };
            let seq = slot.next_seq;
            slot.next_seq += 1;
            slot.touched = stamp;
            stream.last_emitted_at = Some(now);
            stream.due_at = now + stream.promised_cadence;
            beats.push((
                stream.key.clone(),
                UnsignedAttestation {
                    interest_digest: *digest,
                    origin: self.origin,
                    origin_incarnation: self.incarnation,
                    capability_id: stream.key.capability_id.clone(),
                    capability_generation: generation,
                    status,
                    status_reason,
                    estimated_start,
                    seq,
                    promised_cadence: stream.promised_cadence,
                    audience_scope: stream.audience,
                },
            ));
        }
        beats
    }

    /// One signed refusal beat for a below-floor registration
    /// (module docs): `ProviderUnknown` /
    /// `SamplingIntervalUnsupported`, the floor M in
    /// `promised_cadence`, seq drawn from the digest's shared slot.
    pub fn refusal_beat(
        &mut self,
        spec: &InterestSpec,
        refusal: CadenceRefusal,
        generation: u64,
    ) -> UnsignedAttestation {
        let key = CapabilityInterestKey::for_spec(spec);
        let digest = key.interest_digest;
        let stamp = self.touch();
        let slot = self.slots.entry(digest).or_insert(StreamSlot {
            next_seq: 0,
            touched: 0,
            live: None,
        });
        slot.touched = stamp;
        let seq = slot.next_seq;
        slot.next_seq += 1;
        let (status, status_reason) = refusal.as_status();
        let beat = UnsignedAttestation {
            interest_digest: digest,
            origin: self.origin,
            origin_incarnation: self.incarnation,
            capability_id: key.capability_id,
            capability_generation: generation,
            status,
            status_reason,
            estimated_start: None,
            seq,
            promised_cadence: refusal.minimum_supported,
            audience_scope: spec.audience,
        };
        self.evict_retired();
        beat
    }

    /// Bound the slot map: oldest-touched RETIRED slots evict first;
    /// live slots never evict (module docs).
    fn evict_retired(&mut self) {
        if self.slots.len() <= MAX_STREAM_SLOTS {
            return;
        }
        let mut retired: Vec<(Digest256, u64)> = self
            .slots
            .iter()
            .filter(|(_, slot)| slot.live.is_none())
            .map(|(digest, slot)| (*digest, slot.touched))
            .collect();
        retired.sort_by_key(|(_, touched)| *touched);
        let excess = self.slots.len().saturating_sub(STREAM_SLOTS_LOW_WATER);
        for (digest, _) in retired.into_iter().take(excess) {
            self.slots.remove(&digest);
        }
    }

    /// Live stream count (tests + observability).
    pub fn live_streams(&self) -> usize {
        self.slots
            .values()
            .filter(|slot| slot.live.is_some())
            .count()
    }

    /// Total slot count including retired seq memory (tests +
    /// observability).
    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// A live stream's promised cadence (tests + observability).
    pub fn stream_cadence(&self, digest: &Digest256) -> Option<Duration> {
        self.slots
            .get(digest)
            .and_then(|slot| slot.live.as_ref())
            .map(|stream| stream.promised_cadence)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::super::continuity::AttestedStatus;
    use super::super::evaluator::{
        ReadinessEvaluation, StatusReason, DEFAULT_ATTESTATION_CADENCE_FLOOR,
    };
    use super::super::identity::{
        AudienceScopeCommitment, CanonicalConstraints, CapabilityId, DisclosureClass, InterestSpec,
        ProviderSelector, ResultMode, WorkLatencyEnvelope,
    };
    use super::*;

    const FLOOR: Duration = DEFAULT_ATTESTATION_CADENCE_FLOOR;

    fn spec(capability: &str, marker: &str) -> InterestSpec {
        InterestSpec {
            capability_id: CapabilityId::new(capability),
            constraints: CanonicalConstraints::from_entries([("marker", marker)]).unwrap(),
            work_latency: WorkLatencyEnvelope::start_within(Duration::from_millis(100)),
            providers: ProviderSelector::AnyAuthorized,
            result_mode: ResultMode::Any,
            disclosure_class: DisclosureClass::Owner,
            audience: AudienceScopeCommitment::from_bytes([7u8; 32]),
        }
    }

    fn ready() -> impl FnMut(&EvaluationRequest<'_>) -> Option<ReadinessEvaluation> {
        |_request| {
            Some(ReadinessEvaluation::Ready {
                estimated_start: Some(Duration::from_millis(3)),
            })
        }
    }

    #[test]
    fn first_beat_immediate_then_cadence_spacing_and_monotonic_seq() {
        let t0 = Instant::now();
        let mut emitter = OriginEmitter::new(11, Incarnation::new(1), FLOOR);
        let spec = spec("job.run", "a");
        // strictest 200 ms → cadence 100 ms.
        emitter
            .register(&spec, Duration::from_millis(200), t0)
            .unwrap();
        assert_eq!(emitter.next_due(), Some(t0));

        let beats = emitter.tick(t0, 5, ready());
        assert_eq!(beats.len(), 1);
        let (key, beat) = &beats[0];
        assert_eq!(key.interest_digest, spec.interest_digest());
        assert_eq!(beat.seq, 0);
        assert_eq!(beat.capability_generation, 5);
        assert_eq!(beat.status, AttestedStatus::Ready);
        assert_eq!(beat.promised_cadence, Duration::from_millis(100));
        assert_eq!(beat.origin, 11);

        // Not due again until t0 + cadence; generation is read at
        // evaluation time, not registration time.
        assert!(emitter
            .tick(t0 + Duration::from_millis(99), 6, ready())
            .is_empty());
        let beats = emitter.tick(t0 + Duration::from_millis(100), 6, ready());
        assert_eq!(beats.len(), 1);
        assert_eq!(beats[0].1.seq, 1);
        assert_eq!(beats[0].1.capability_generation, 6);
    }

    #[test]
    fn refresh_does_not_starve_cadence_but_tightening_reschedules() {
        let t0 = Instant::now();
        let mut emitter = OriginEmitter::new(11, Incarnation::new(1), FLOOR);
        let spec = spec("job.run", "a");
        emitter
            .register(&spec, Duration::from_millis(400), t0)
            .unwrap();
        assert_eq!(
            emitter.stream_cadence(&spec.interest_digest()),
            Some(Duration::from_millis(200)),
        );
        let _ = emitter.tick(t0, 1, ready());
        assert_eq!(emitter.next_due(), Some(t0 + Duration::from_millis(200)));

        // Same-aggregate refresh (the ttl/2 keep-alive): schedule
        // untouched.
        emitter
            .register(
                &spec,
                Duration::from_millis(400),
                t0 + Duration::from_millis(50),
            )
            .unwrap();
        assert_eq!(emitter.next_due(), Some(t0 + Duration::from_millis(200)));

        // A stricter co-subscriber arrives: cadence 200 → 60 ms,
        // next beat re-derived from the LAST beat (t0 + 60).
        emitter
            .register(
                &spec,
                Duration::from_millis(120),
                t0 + Duration::from_millis(50),
            )
            .unwrap();
        assert_eq!(
            emitter.stream_cadence(&spec.interest_digest()),
            Some(Duration::from_millis(60)),
        );
        assert_eq!(emitter.next_due(), Some(t0 + Duration::from_millis(60)));

        // Cadence floors at the configured floor even for tiny D.
        emitter
            .register(&spec, FLOOR, t0 + Duration::from_millis(50))
            .unwrap();
        assert_eq!(emitter.stream_cadence(&spec.interest_digest()), Some(FLOOR));
    }

    #[test]
    fn poke_pulls_forward_with_floor_min_gap() {
        let t0 = Instant::now();
        let mut emitter = OriginEmitter::new(11, Incarnation::new(1), FLOOR);
        let spec = spec("job.run", "a");
        emitter
            .register(&spec, Duration::from_millis(400), t0)
            .unwrap();
        let _ = emitter.tick(t0, 1, ready());

        // Edge right after a beat: clamped to last + floor.
        assert!(emitter.poke(
            &CapabilityId::new("job.run"),
            t0 + Duration::from_millis(10)
        ));
        assert_eq!(emitter.next_due(), Some(t0 + FLOOR));

        // Edge long after the last beat: immediate.
        let late = t0 + Duration::from_millis(150);
        let _ = emitter.tick(t0 + FLOOR, 1, ready());
        assert!(emitter.poke(&CapabilityId::new("job.run"), late));
        assert_eq!(emitter.next_due(), Some(late));

        // Unknown capability: nothing moves.
        assert!(!emitter.poke(&CapabilityId::new("other.cap"), late));
    }

    #[test]
    fn refusal_beat_carries_floor_in_promised_cadence_and_shares_seq_space() {
        let t0 = Instant::now();
        let mut emitter = OriginEmitter::new(11, Incarnation::new(1), FLOOR);
        let spec = spec("job.run", "a");

        // Below-floor registration refused, stream stays dark.
        let refusal = emitter
            .register(&spec, Duration::from_millis(10), t0)
            .unwrap_err();
        assert_eq!(refusal.minimum_supported, FLOOR);
        assert_eq!(emitter.live_streams(), 0);

        let beat = emitter.refusal_beat(&spec, refusal, 9);
        assert_eq!(beat.status, AttestedStatus::ProviderUnknown);
        assert_eq!(
            beat.status_reason,
            StatusReason::SamplingIntervalUnsupported
        );
        assert_eq!(beat.promised_cadence, FLOOR);
        assert_eq!(beat.estimated_start, None);
        assert_eq!(beat.seq, 0);

        // A later legal registration continues the SAME seq space —
        // refusal beat consumed seq 0, first stream beat is seq 1.
        emitter
            .register(&spec, Duration::from_millis(200), t0)
            .unwrap();
        let beats = emitter.tick(t0, 9, ready());
        assert_eq!(beats.len(), 1);
        assert_eq!(beats[0].1.seq, 1);
    }

    #[test]
    fn retire_stops_emission_and_resurrection_keeps_seq() {
        let t0 = Instant::now();
        let mut emitter = OriginEmitter::new(11, Incarnation::new(1), FLOOR);
        let spec = spec("job.run", "a");
        emitter
            .register(&spec, Duration::from_millis(200), t0)
            .unwrap();
        let _ = emitter.tick(t0, 1, ready());

        // Zero idle emission: retired stream leaves nothing due.
        emitter.retire(&spec.interest_digest());
        assert_eq!(emitter.live_streams(), 0);
        assert_eq!(emitter.next_due(), None);
        assert!(emitter
            .tick(t0 + Duration::from_secs(5), 1, ready())
            .is_empty());

        // Resurrection continues the seq space (no equivocation on
        // (incarnation, seq) within one incarnation).
        emitter
            .register(
                &spec,
                Duration::from_millis(200),
                t0 + Duration::from_secs(6),
            )
            .unwrap();
        let beats = emitter.tick(t0 + Duration::from_secs(6), 1, ready());
        assert_eq!(beats[0].1.seq, 1);
    }

    #[test]
    fn missing_evaluator_projects_temporarily_unevaluable() {
        let t0 = Instant::now();
        let mut emitter = OriginEmitter::new(11, Incarnation::new(1), FLOOR);
        let spec = spec("job.run", "a");
        emitter
            .register(&spec, Duration::from_millis(200), t0)
            .unwrap();
        let beats = emitter.tick(t0, 1, |_request| None);
        assert_eq!(beats.len(), 1);
        assert_eq!(beats[0].1.status, AttestedStatus::ProviderUnknown);
        assert_eq!(
            beats[0].1.status_reason,
            StatusReason::TemporarilyUnevaluable,
        );
    }

    #[test]
    fn compile_once_per_distinct_digest_and_per_interest_streams() {
        let t0 = Instant::now();
        let mut emitter = OriginEmitter::new(11, Incarnation::new(1), FLOOR);
        let a = spec("job.run", "a");
        let b = spec("job.run", "b");
        emitter
            .register(&a, Duration::from_millis(200), t0)
            .unwrap();
        emitter
            .register(&b, Duration::from_millis(400), t0)
            .unwrap();
        assert_eq!(emitter.live_streams(), 2);

        // Distinct digests emit independent streams with their own
        // seq spaces and cadences.
        let beats = emitter.tick(t0, 1, ready());
        assert_eq!(beats.len(), 2);
        assert!(beats.iter().all(|(_, beat)| beat.seq == 0));
        let beats = emitter.tick(t0 + Duration::from_millis(100), 1, ready());
        assert_eq!(beats.len(), 1);
        assert_eq!(beats[0].0.interest_digest, a.interest_digest());
    }

    #[test]
    fn retired_slots_evict_oldest_first_live_never() {
        let t0 = Instant::now();
        let mut emitter = OriginEmitter::new(11, Incarnation::new(1), FLOOR);
        // One live stream that must survive the sweep.
        let live = spec("job.run", "live");
        emitter
            .register(&live, Duration::from_millis(200), t0)
            .unwrap();
        // Flood retired slots past the cap.
        for i in 0..MAX_STREAM_SLOTS {
            let s = spec("job.run", &format!("retired-{i}"));
            emitter
                .register(&s, Duration::from_millis(200), t0)
                .unwrap();
            emitter.retire(&s.interest_digest());
        }
        assert!(emitter.slot_count() <= STREAM_SLOTS_LOW_WATER + 1);
        assert_eq!(emitter.live_streams(), 1);
        assert_eq!(
            emitter.stream_cadence(&live.interest_digest()),
            Some(Duration::from_millis(100)),
        );
    }
}
