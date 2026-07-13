//! SI-3 origin emitter (plan §4.4): the provider-side scheduler for
//! signed readiness streams. Hardened by the SI-3 review closure
//! packet (§6 review disposition): live-stream capacity, bounded
//! duration arithmetic, a two-phase due/finalize API that keeps
//! user evaluators OUTSIDE the emitter lock, and stamped retirement
//! that closes the register/retire race.
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
//! # Two-phase emission (closure item 5)
//!
//! `ReadinessEvaluator::evaluate` is arbitrary user code and may
//! legitimately call back into `MeshNode` (a notify hook, stream
//! introspection) — running it under the emitter mutex would
//! deadlock a non-reentrant lock. The API is therefore split:
//!
//! 1. **Under the lock**: [`OriginEmitter::collect_due`] reserves
//!    the sequence number, re-arms the schedule, and snapshots the
//!    compiled predicate into a [`DueBeat`] (the predicate rides an
//!    `Arc`, so the snapshot is cheap).
//! 2. **Without the lock**: the caller runs the evaluator against
//!    [`DueBeat::request`].
//! 3. No re-lock is needed to finalize: everything the §4.2
//!    transcript binds was reserved in step 1, so
//!    [`DueBeat::into_unsigned`] is a pure function. A beat whose
//!    stream was retired between the phases is harmless — its
//!    downstream set reads empty and the caller drops it.
//!
//! # Capacity (closure item 1)
//!
//! Live streams are capped at [`MAX_LIVE_SENSING_STREAMS`] (1024) —
//! sized so worst-case signing at the 50 ms floor stays well under
//! one core (SI-1d: ~13.3 µs/sign → ~27% of a core at cap). At
//! capacity: refreshes of live digests are accepted, new or
//! resurrected digests are refused ([`StreamRefusal::AtCapacity`]),
//! no live stream is ever evicted, and a capacity refusal mints NO
//! sequence slot. The 8192-slot LRU below is SEQUENCE-memory
//! capacity, not live capacity.
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
//! # Stamped retirement (closure item 7)
//!
//! Retirement decisions are made from TABLE state read outside this
//! lock, so a registration can land between that read and the
//! retire call and be killed despite holding a live row (dark until
//! the next refresh). Every successful [`OriginEmitter::register`]
//! stamps the stream from the emitter's monotonic counter; callers
//! snapshot [`OriginEmitter::stamp`] BEFORE reading the table and
//! retire with [`OriginEmitter::retire_if_stale`], which refuses to
//! kill a stream registered after the snapshot.
//!
//! # Cadence refusal (one-shot, rides the attestation plane)
//!
//! A coalesced strictest D below the floor is refused
//! ([`check_cadence`]) — the caller partitions its downstreams
//! (`InterestTable::on_refusal`) and sends the refused ones ONE
//! signed refusal beat ([`OriginEmitter::refusal_beat`]): status
//! `ProviderUnknown` / `SamplingIntervalUnsupported`, with the
//! provider floor M carried in `promised_cadence` — a TAGGED
//! interpretation (SI-3 review): under that `status_reason`, the
//! signed field means `minimum_supported`. Refusal beats draw from
//! the same seq slot as the stream, so the two can never collide on
//! `(incarnation, seq)`; a slot is minted only when a response is
//! actually authored (a capacity refusal sends nothing and mints
//! nothing).
//!
//! # Bounded time arithmetic (closure item 4)
//!
//! The mesh bounds wire intervals at intake
//! (`0 < D ≤ sensing_interest_ttl`), but this module never trusts
//! that: all scheduling uses [`schedule_after`] (checked add,
//! far-future park on overflow) and a zero cadence floor is
//! normalized to the default at construction, so no cadence value —
//! however malformed — can panic or hot-loop the emitter task.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::evaluator::{
    check_cadence, project_evaluation, CadenceRefusal, EvaluationRequest, ReadinessEvaluation,
    DEFAULT_ATTESTATION_CADENCE_FLOOR,
};
use super::identity::{
    AudienceScopeCommitment, CanonicalConstraints, CapabilityId, CapabilityInterestKey, Digest256,
    InterestSpec, WorkLatencyEnvelope,
};
use super::incarnation::Incarnation;
use super::wire::UnsignedAttestation;

/// Hard cap on concurrently LIVE emission streams (closure item 1).
/// Sized from the SI-1d benchmark: 1024 streams all at the 50 ms
/// floor cost ~27% of one core in signatures — the origin role stays
/// a background workload under worst-case demand. Honest working
/// sets are far smaller (identical specs coalesce into one digest);
/// a node near this cap almost certainly has a consumer defeating
/// coalescing, and refusing surfaces that.
pub const MAX_LIVE_SENSING_STREAMS: usize = 1024;

/// Bound on the seq-slot map (live + retired), mirroring the
/// observer gate's bound (SI-1c): comfortably above any honest
/// interest population, small enough that a hostile registration
/// storm cannot grow memory unboundedly.
pub const MAX_STREAM_SLOTS: usize = 8192;

/// Eviction low-water mark: one sweep past [`MAX_STREAM_SLOTS`]
/// frees a batch of retired slots so steady-state churn does not
/// re-trigger the sweep on every insert.
pub const STREAM_SLOTS_LOW_WATER: usize = 6144;

/// Overflow park (closure item 4): a schedule whose checked add
/// overflows re-arms this far out instead — effectively "never",
/// without panicking and without an immediately-due hot loop.
const OVERFLOW_PARK: Duration = Duration::from_secs(60 * 60 * 24 * 365);

/// `base + delta` that can neither panic nor hot-loop: overflow
/// parks the schedule [`OVERFLOW_PARK`] out (and in the
/// never-observed case where even that overflows, falls back to
/// `base` — a bounded extra beat, not a crash).
fn schedule_after(base: Instant, delta: Duration) -> Instant {
    base.checked_add(delta)
        .or_else(|| base.checked_add(OVERFLOW_PARK))
        .unwrap_or(base)
}

/// Why [`OriginEmitter::register`] refused a stream (closure
/// item 1 split the refusal space).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StreamRefusal {
    /// The coalesced strictest D is below the provider floor —
    /// answer with [`OriginEmitter::refusal_beat`] after
    /// partitioning (§4.4).
    Cadence(CadenceRefusal),
    /// [`MAX_LIVE_SENSING_STREAMS`] live streams already exist and
    /// this digest is not one of them. No slot was minted, nothing
    /// was evicted; the refusal is local (log/observe) — there is no
    /// §4.4 wire response for capacity.
    AtCapacity,
}

/// The compiled evaluation inputs of one interest — parsed once at
/// registration, shared into each [`DueBeat`] by `Arc` so the
/// two-phase split never re-clones the constraint map per beat.
#[derive(Debug)]
struct CompiledPredicate {
    capability_id: CapabilityId,
    constraints: CanonicalConstraints,
    work_latency: WorkLatencyEnvelope,
}

/// One interest's live schedule.
#[derive(Debug)]
struct LiveStream {
    /// Table/index identity of the stream (digest + capability id).
    key: CapabilityInterestKey,
    /// Compiled predicate (module docs — compile once per digest).
    predicate: Arc<CompiledPredicate>,
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
    /// Monotonic stamp of the most recent (re-)registration —
    /// [`OriginEmitter::retire_if_stale`] refuses to kill a stream
    /// registered after the caller's snapshot (closure item 7).
    registered_stamp: u64,
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

/// One reserved-but-unevaluated beat (two-phase emission, module
/// docs): everything the §4.2 transcript binds except the
/// evaluation outcome, snapshotted under the emitter lock. Run the
/// evaluator against [`Self::request`] WITHOUT the lock, then seal
/// with [`Self::into_unsigned`] — a pure function.
#[derive(Debug)]
pub struct DueBeat {
    key: CapabilityInterestKey,
    predicate: Arc<CompiledPredicate>,
    audience: AudienceScopeCommitment,
    origin: u64,
    incarnation: Incarnation,
    generation: u64,
    seq: u64,
    promised_cadence: Duration,
    /// The collect-phase stamp — pass to
    /// [`OriginEmitter::retire_if_stale`] when this beat's
    /// downstream set reads empty.
    stamp: u64,
}

impl DueBeat {
    /// The stream identity this beat answers.
    pub fn key(&self) -> &CapabilityInterestKey {
        &self.key
    }

    /// The collect-phase stamp (module docs, stamped retirement).
    pub fn stamp(&self) -> u64 {
        self.stamp
    }

    /// The evaluation inputs — run the integration against this
    /// OUTSIDE the emitter lock.
    pub fn request(&self) -> EvaluationRequest<'_> {
        EvaluationRequest {
            capability_id: &self.predicate.capability_id,
            constraints: &self.predicate.constraints,
            work_latency: &self.predicate.work_latency,
        }
    }

    /// Seal the beat with the evaluation outcome. `None` (no
    /// evaluator registered for the capability) projects as
    /// `ProviderUnknown { TemporarilyUnevaluable }` — an explicit
    /// "targeted but cannot answer" stream beats silence.
    pub fn into_unsigned(self, evaluation: Option<ReadinessEvaluation>) -> UnsignedAttestation {
        let evaluation = evaluation.unwrap_or(ReadinessEvaluation::TemporarilyUnevaluable);
        let (status, status_reason) = project_evaluation(&evaluation);
        let estimated_start = match evaluation {
            ReadinessEvaluation::Ready { estimated_start } => estimated_start,
            _ => None,
        };
        UnsignedAttestation {
            interest_digest: self.key.interest_digest,
            origin: self.origin,
            origin_incarnation: self.incarnation,
            capability_id: self.key.capability_id,
            capability_generation: self.generation,
            status,
            status_reason,
            estimated_start,
            seq: self.seq,
            promised_cadence: self.promised_cadence,
            audience_scope: self.audience,
        }
    }
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
    /// increment-before-participation — TRUSTED caller input, per
    /// the accepted deviation).
    incarnation: Incarnation,
    /// `attestation_cadence_floor` (plan §5): cadence lower bound
    /// AND the status-edge min-gap. Normalized non-zero at
    /// construction (closure item 4).
    cadence_floor: Duration,
    /// Seq slots by interest digest — live streams + retired seq
    /// memory, LRU-bounded (module docs).
    slots: HashMap<Digest256, StreamSlot>,
    /// Live-stream count, maintained on every live transition so
    /// the capacity check is O(1) (closure item 1).
    live_count: usize,
    /// Monotonic LRU/registration stamp source.
    touch_counter: u64,
}

impl OriginEmitter {
    /// New emitter for one `(origin, incarnation)` scope. A zero
    /// `cadence_floor` is normalized to
    /// [`DEFAULT_ATTESTATION_CADENCE_FLOOR`] — a zero floor would
    /// admit a zero cadence and hot-loop the emitter task (closure
    /// item 4).
    pub fn new(origin: u64, incarnation: Incarnation, cadence_floor: Duration) -> Self {
        let cadence_floor = if cadence_floor.is_zero() {
            DEFAULT_ATTESTATION_CADENCE_FLOOR
        } else {
            cadence_floor
        };
        Self {
            origin,
            incarnation,
            cadence_floor,
            slots: HashMap::new(),
            live_count: 0,
            touch_counter: 0,
        }
    }

    fn touch(&mut self) -> u64 {
        self.touch_counter += 1;
        self.touch_counter
    }

    /// The current registration stamp. Snapshot this BEFORE reading
    /// table state that a retirement decision will rest on, then
    /// retire with [`Self::retire_if_stale`] (closure item 7).
    pub fn stamp(&self) -> u64 {
        self.touch_counter
    }

    /// Register (or refresh) one interest stream at the caller's
    /// current strictest aggregate D.
    ///
    /// - A strictest D below the floor is refused
    ///   ([`StreamRefusal::Cadence`]) — stream state untouched; the
    ///   caller partitions and answers with [`Self::refusal_beat`].
    /// - At [`MAX_LIVE_SENSING_STREAMS`], a digest that is not
    ///   already live is refused ([`StreamRefusal::AtCapacity`])
    ///   without minting a slot; live refreshes always succeed.
    /// - First registration schedules the first beat at `now`; a
    ///   refresh only moves the schedule when the CADENCE moved (a
    ///   ttl/2 refresh must never starve the cadence by pushing
    ///   `due_at` forever forward).
    pub fn register(
        &mut self,
        spec: &InterestSpec,
        strictest: Duration,
        now: Instant,
    ) -> Result<(), StreamRefusal> {
        check_cadence(strictest, self.cadence_floor).map_err(StreamRefusal::Cadence)?;
        let promised_cadence = (strictest / 2).max(self.cadence_floor);
        let key = CapabilityInterestKey::for_spec(spec);
        let digest = key.interest_digest;
        let already_live = self
            .slots
            .get(&digest)
            .is_some_and(|slot| slot.live.is_some());
        if !already_live && self.live_count >= MAX_LIVE_SENSING_STREAMS {
            return Err(StreamRefusal::AtCapacity);
        }
        let stamp = self.touch();
        let slot = self.slots.entry(digest).or_insert(StreamSlot {
            next_seq: 0,
            touched: 0,
            live: None,
        });
        slot.touched = stamp;
        match &mut slot.live {
            Some(stream) => {
                stream.registered_stamp = stamp;
                if stream.promised_cadence != promised_cadence {
                    stream.promised_cadence = promised_cadence;
                    // Re-derive the schedule from the last beat under
                    // the new cadence — a tightened aggregate pulls
                    // the next beat earlier, a loosened one pushes it
                    // out; either way the min-gap (floor) holds.
                    stream.due_at = match stream.last_emitted_at {
                        Some(last) => schedule_after(last, promised_cadence).max(now),
                        None => now,
                    };
                }
            }
            None => {
                slot.live = Some(LiveStream {
                    key,
                    predicate: Arc::new(CompiledPredicate {
                        capability_id: spec.capability_id.clone(),
                        constraints: spec.constraints.clone(),
                        work_latency: spec.work_latency,
                    }),
                    audience: spec.audience,
                    promised_cadence,
                    due_at: now,
                    last_emitted_at: None,
                    registered_stamp: stamp,
                });
                self.live_count += 1;
            }
        }
        self.evict_retired();
        Ok(())
    }

    /// The stream's last downstream died (deregister, ttl sweep, or
    /// refusal partition) — stop emitting, keep the seq memory
    /// (module docs). Idempotent. Prefer [`Self::retire_if_stale`]
    /// whenever the decision rests on table state read outside the
    /// emitter lock.
    pub fn retire(&mut self, digest: &Digest256) {
        if let Some(slot) = self.slots.get_mut(digest) {
            if slot.live.take().is_some() {
                self.live_count -= 1;
            }
        }
    }

    /// Retire ONLY if the stream has not been (re-)registered since
    /// the caller's [`Self::stamp`] snapshot — the register/retire
    /// race closure (module docs, item 7). Returns whether the
    /// stream was retired.
    pub fn retire_if_stale(&mut self, digest: &Digest256, seen_stamp: u64) -> bool {
        let Some(slot) = self.slots.get_mut(digest) else {
            return false;
        };
        let Some(stream) = &slot.live else {
            return false;
        };
        if stream.registered_stamp > seen_stamp {
            // A registration landed after the caller observed the
            // table — the emptiness it acted on is stale.
            return false;
        }
        slot.live = None;
        self.live_count -= 1;
        true
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
                Some(last) => schedule_after(last, floor).max(now),
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

    /// Phase 1 of two-phase emission (module docs): reserve and
    /// re-arm every due stream under the lock, WITHOUT evaluating.
    /// `generation` is the provider's OWN announce generation at
    /// this instant — attested content, read at collection time
    /// (§3.4). Evaluate each returned beat via [`DueBeat::request`]
    /// outside the lock, then seal with [`DueBeat::into_unsigned`].
    pub fn collect_due(&mut self, now: Instant, generation: u64) -> Vec<DueBeat> {
        let mut due = Vec::new();
        let stamp = self.touch();
        for slot in self.slots.values_mut() {
            let Some(stream) = &mut slot.live else {
                continue;
            };
            if stream.due_at > now {
                continue;
            }
            let seq = slot.next_seq;
            slot.next_seq += 1;
            slot.touched = stamp;
            stream.last_emitted_at = Some(now);
            stream.due_at = schedule_after(now, stream.promised_cadence);
            due.push(DueBeat {
                key: stream.key.clone(),
                predicate: stream.predicate.clone(),
                audience: stream.audience,
                origin: self.origin,
                incarnation: self.incarnation,
                generation,
                seq,
                promised_cadence: stream.promised_cadence,
                stamp,
            });
        }
        due
    }

    /// One signed refusal beat for a below-floor registration
    /// (module docs): `ProviderUnknown` /
    /// `SamplingIntervalUnsupported`, the floor M in
    /// `promised_cadence` (tagged interpretation), seq drawn from
    /// the digest's shared slot. Mints a slot only because a
    /// response IS being authored — capacity refusals never call
    /// this.
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
        self.live_count
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
    use super::super::evaluator::StatusReason;
    use super::super::identity::{DisclosureClass, ProviderSelector, ResultMode};
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

    fn ready() -> Option<ReadinessEvaluation> {
        Some(ReadinessEvaluation::Ready {
            estimated_start: Some(Duration::from_millis(3)),
        })
    }

    /// Collect + seal in one step for tests that don't exercise the
    /// two-phase split itself.
    fn beats(
        emitter: &mut OriginEmitter,
        now: Instant,
        generation: u64,
    ) -> Vec<(CapabilityInterestKey, UnsignedAttestation)> {
        emitter
            .collect_due(now, generation)
            .into_iter()
            .map(|beat| (beat.key().clone(), beat.into_unsigned(ready())))
            .collect()
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

        let out = beats(&mut emitter, t0, 5);
        assert_eq!(out.len(), 1);
        let (key, beat) = &out[0];
        assert_eq!(key.interest_digest, spec.interest_digest());
        assert_eq!(beat.seq, 0);
        assert_eq!(beat.capability_generation, 5);
        assert_eq!(beat.status, AttestedStatus::Ready);
        assert_eq!(beat.promised_cadence, Duration::from_millis(100));
        assert_eq!(beat.origin, 11);

        // Not due again until t0 + cadence; generation is read at
        // collection time, not registration time.
        assert!(beats(&mut emitter, t0 + Duration::from_millis(99), 6).is_empty());
        let out = beats(&mut emitter, t0 + Duration::from_millis(100), 6);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1.seq, 1);
        assert_eq!(out[0].1.capability_generation, 6);
    }

    #[test]
    fn two_phase_split_reserves_under_lock_and_seals_pure() {
        let t0 = Instant::now();
        let mut emitter = OriginEmitter::new(11, Incarnation::new(2), FLOOR);
        let spec = spec("job.run", "a");
        emitter
            .register(&spec, Duration::from_millis(200), t0)
            .unwrap();

        // Phase 1 already reserved seq + re-armed the schedule …
        let due = emitter.collect_due(t0, 9);
        assert_eq!(due.len(), 1);
        assert_eq!(
            emitter.next_due(),
            Some(t0 + Duration::from_millis(100)),
            "schedule re-armed before evaluation",
        );
        assert!(
            emitter.collect_due(t0, 9).is_empty(),
            "seq/schedule reserved exactly once",
        );

        // … so sealing needs no emitter access at all, and the
        // request borrows only the beat.
        let beat = due.into_iter().next().unwrap();
        assert_eq!(beat.request().capability_id.as_str(), "job.run");
        let unsigned = beat.into_unsigned(None);
        assert_eq!(unsigned.status, AttestedStatus::ProviderUnknown);
        assert_eq!(unsigned.status_reason, StatusReason::TemporarilyUnevaluable);
        assert_eq!(unsigned.seq, 0);
        assert_eq!(unsigned.origin_incarnation, Incarnation::new(2));
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
        let _ = beats(&mut emitter, t0, 1);
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
        let _ = beats(&mut emitter, t0, 1);

        // Edge right after a beat: clamped to last + floor.
        assert!(emitter.poke(
            &CapabilityId::new("job.run"),
            t0 + Duration::from_millis(10)
        ));
        assert_eq!(emitter.next_due(), Some(t0 + FLOOR));

        // Edge long after the last beat: immediate.
        let late = t0 + Duration::from_millis(150);
        let _ = beats(&mut emitter, t0 + FLOOR, 1);
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
        let refused = emitter
            .register(&spec, Duration::from_millis(10), t0)
            .unwrap_err();
        let StreamRefusal::Cadence(refusal) = refused else {
            panic!("expected a cadence refusal, got {refused:?}");
        };
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
        let out = beats(&mut emitter, t0, 9);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1.seq, 1);
    }

    #[test]
    fn retire_stops_emission_and_resurrection_keeps_seq() {
        let t0 = Instant::now();
        let mut emitter = OriginEmitter::new(11, Incarnation::new(1), FLOOR);
        let spec = spec("job.run", "a");
        emitter
            .register(&spec, Duration::from_millis(200), t0)
            .unwrap();
        let _ = beats(&mut emitter, t0, 1);

        // Zero idle emission: retired stream leaves nothing due.
        emitter.retire(&spec.interest_digest());
        assert_eq!(emitter.live_streams(), 0);
        assert_eq!(emitter.next_due(), None);
        assert!(beats(&mut emitter, t0 + Duration::from_secs(5), 1).is_empty());

        // Resurrection continues the seq space (no equivocation on
        // (incarnation, seq) within one incarnation).
        emitter
            .register(
                &spec,
                Duration::from_millis(200),
                t0 + Duration::from_secs(6),
            )
            .unwrap();
        let out = beats(&mut emitter, t0 + Duration::from_secs(6), 1);
        assert_eq!(out[0].1.seq, 1);
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
        let out = beats(&mut emitter, t0, 1);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|(_, beat)| beat.seq == 0));
        let out = beats(&mut emitter, t0 + Duration::from_millis(100), 1);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0.interest_digest, a.interest_digest());
    }

    #[test]
    fn live_capacity_refuses_new_digests_never_evicts_and_mints_no_slot() {
        let t0 = Instant::now();
        let mut emitter = OriginEmitter::new(11, Incarnation::new(1), FLOOR);
        let mut specs = Vec::new();
        for i in 0..MAX_LIVE_SENSING_STREAMS {
            let s = spec("job.run", &format!("live-{i}"));
            emitter
                .register(&s, Duration::from_millis(200), t0)
                .unwrap();
            specs.push(s);
        }
        assert_eq!(emitter.live_streams(), MAX_LIVE_SENSING_STREAMS);
        let slots_at_cap = emitter.slot_count();

        // A new digest is refused, WITHOUT minting a seq slot.
        let overflow = spec("job.run", "overflow");
        assert_eq!(
            emitter.register(&overflow, Duration::from_millis(200), t0),
            Err(StreamRefusal::AtCapacity),
        );
        assert_eq!(emitter.live_streams(), MAX_LIVE_SENSING_STREAMS);
        assert_eq!(emitter.slot_count(), slots_at_cap, "no slot minted");

        // A refresh of an EXISTING live digest is accepted at cap.
        assert!(emitter
            .register(&specs[0], Duration::from_millis(120), t0)
            .is_ok());
        assert_eq!(
            emitter.stream_cadence(&specs[0].interest_digest()),
            Some(Duration::from_millis(60)),
        );

        // A retired digest is a RESURRECTION — refused at cap …
        emitter.retire(&specs[1].interest_digest());
        emitter
            .register(&overflow, Duration::from_millis(200), t0)
            .expect("one live slot freed");
        assert_eq!(emitter.live_streams(), MAX_LIVE_SENSING_STREAMS);
        assert_eq!(
            emitter.register(&specs[1], Duration::from_millis(200), t0),
            Err(StreamRefusal::AtCapacity),
            "resurrection counts against the live cap",
        );
    }

    #[test]
    fn stamped_retire_skips_streams_registered_after_the_snapshot() {
        let t0 = Instant::now();
        let mut emitter = OriginEmitter::new(11, Incarnation::new(1), FLOOR);
        let spec = spec("job.run", "a");
        emitter
            .register(&spec, Duration::from_millis(200), t0)
            .unwrap();
        let digest = spec.interest_digest();

        // Snapshot, then a registration lands (the race): the stale
        // retire must be refused.
        let seen = emitter.stamp();
        emitter
            .register(
                &spec,
                Duration::from_millis(200),
                t0 + Duration::from_millis(5),
            )
            .unwrap();
        assert!(!emitter.retire_if_stale(&digest, seen));
        assert_eq!(emitter.live_streams(), 1);

        // A fresh snapshot with no interleaving registration
        // retires normally.
        let seen = emitter.stamp();
        assert!(emitter.retire_if_stale(&digest, seen));
        assert_eq!(emitter.live_streams(), 0);
        assert!(!emitter.retire_if_stale(&digest, seen), "idempotent");
    }

    #[test]
    fn absurd_durations_never_panic_and_park_instead_of_hot_looping() {
        let t0 = Instant::now();
        // Zero floor is normalized — a zero cadence can never admit.
        let mut emitter = OriginEmitter::new(11, Incarnation::new(1), Duration::ZERO);
        let spec = spec("job.run", "a");
        assert!(matches!(
            emitter.register(&spec, Duration::from_millis(1), t0),
            Err(StreamRefusal::Cadence(_)),
        ));

        // A near-MAX interval schedules without panicking, and the
        // overflowed re-arm parks far out instead of going due
        // immediately.
        emitter.register(&spec, Duration::MAX, t0).unwrap();
        let out = beats(&mut emitter, t0, 1);
        assert_eq!(out.len(), 1, "first beat still fires");
        let next = emitter.next_due().expect("stream live");
        assert!(
            next > t0 + Duration::from_secs(60 * 60 * 24 * 30),
            "overflowed schedule parks far in the future",
        );
        assert!(
            beats(&mut emitter, t0 + Duration::from_secs(1), 1).is_empty(),
            "no hot loop",
        );
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
