//! Observation state: evidence vs. continuity (plan §3.4, §4.5).
//!
//! Two independent facts, stored separately and never conflated:
//! what the provider last **attested** (signed, latest-wins) and
//! whether this consumer's **delivery stream** for the key is live.
//! The public projection is deliberately asymmetric — a stale
//! NotReady only costs unnecessary avoidance, while a stale Ready
//! selects a provider that may no longer be ready: **pessimism is
//! safe, optimism must be earned.**
//!
//! | attested | continuity | projected |
//! |---|---|---|
//! | Ready | Unestablished | **Unknown** |
//! | Ready | Established | Ready |
//! | NotReady | Unestablished or Established | NotReady |
//! | ProviderUnknown | any | Unknown |
//! | any | Expired | Unknown |
//!
//! Continuity is a *stream-suspicion* rule (the failure detector's
//! trick — clock-free, composes per hop), NOT an evidence-age bound:
//! a signature proves authorship, not recency (§4.5). The suspicion
//! window is
//!
//! ```text
//! continuity_window = k × max(promised_cadence, own D)    (k = 3)
//! ```
//!
//! where the `max` term is load-bearing under relay down-sampling —
//! keying off `promised_cadence` alone would false-Unknown every
//! down-sampled subscriber. Registration also starts an
//! *establishment deadline* derived the same way: **Unestablished
//! expires too** (a warm-started cached NotReady on a dead stream
//! must not become permanent pessimism — plan §3.4, SI-0 test 14),
//! and only CONTINUITY-BEARING beats reset either deadline — a chain
//! of strictly-newer *cached* beats is not a live stream (SI-0
//! test 13).

use std::time::{Duration, Instant};

use super::incarnation::Incarnation;

/// What the provider signed: its own evaluation of the predicate.
/// `ProviderUnknown` is emitted only when the provider cannot
/// evaluate (unsupported predicate, transient failure, invalid
/// constraints) — consumer-side Unknown is *derived* via projection,
/// never attested.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AttestedStatus {
    /// Predicate holds; work can start within the envelope.
    Ready,
    /// Predicate evaluated false.
    NotReady,
    /// The provider could not evaluate the predicate.
    ProviderUnknown,
}

/// Delivery-stream continuity for one `ReadinessKey` at one
/// consumer or relay (plan §3.4).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Continuity {
    /// Registered (possibly warm-started from a relay cache), but no
    /// continuity-bearing strictly-newer beat has arrived yet.
    Unestablished,
    /// A live post-registration stream has been observed within the
    /// window.
    Established,
    /// The window elapsed without a qualifying beat, or the path /
    /// incarnation / generation / scope broke. Projects Unknown.
    Expired,
}

/// The consumer-facing three-state surface (plan §3.4). Everything
/// richer stays internal to the overlay.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProjectedReadiness {
    /// Attested Ready over established continuity.
    Ready,
    /// Attested NotReady (pessimism is safe — projected even while
    /// Unestablished, until the establishment deadline).
    NotReady,
    /// No trustworthy signal: unestablished/expired optimism or a
    /// provider that could not evaluate.
    Unknown,
}

/// The projection table (plan §3.4), as one pure function so the
/// full matrix is pinned by a single test.
pub const fn project(status: AttestedStatus, continuity: Continuity) -> ProjectedReadiness {
    match (status, continuity) {
        (_, Continuity::Expired) => ProjectedReadiness::Unknown,
        (AttestedStatus::Ready, Continuity::Established) => ProjectedReadiness::Ready,
        (AttestedStatus::Ready, Continuity::Unestablished) => ProjectedReadiness::Unknown,
        (AttestedStatus::NotReady, _) => ProjectedReadiness::NotReady,
        (AttestedStatus::ProviderUnknown, _) => ProjectedReadiness::Unknown,
    }
}

/// Why continuity was force-expired (plan §4.7) — observability
/// only; every reason projects identically.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DisruptReason {
    /// RT-5 withdrawal or failure-detector edge toward the provider.
    PathFailed,
    /// The provider's incarnation was superseded; the old stream's
    /// continuity cannot vouch for the new one.
    IncarnationSuperseded,
    /// The capability generation changed; every old-generation key
    /// is dead.
    GenerationChanged,
    /// The downstream's scope validation failed.
    ScopeValidationFailed,
}

/// The latest admitted attestation for one key, as held in the fold
/// overlay (plan §3.4). `continuity` lives beside the attested
/// fields but is maintained exclusively by [`ObservationCell`].
#[derive(Clone, Copy, Debug)]
pub struct ReadinessObservation {
    /// The provider-signed status, as attested — never overwritten
    /// by local suspicion (projection handles that).
    pub attested_status: AttestedStatus,
    /// Provider's estimate of time-to-start, when Ready.
    pub estimated_start: Option<Duration>,
    /// Boot epoch the attestation was signed under.
    pub source_incarnation: Incarnation,
    /// The provider's announce generation the attestation was signed
    /// under (v4.1: generation is attested content — the interest
    /// never binds it, and continuity never crosses it, §3.4).
    pub capability_generation: u64,
    /// Seq of the latest admitted beat (post-gate).
    pub last_seq: u64,
    /// The emission cadence the provider signed for this branch.
    pub promised_cadence: Duration,
    /// Stream continuity at this consumer.
    pub continuity: Continuity,
    /// Local arrival time of the latest admitted beat.
    pub locally_observed_at: Instant,
}

impl ReadinessObservation {
    /// Project through the §3.4 table.
    pub const fn projected(&self) -> ProjectedReadiness {
        project(self.attested_status, self.continuity)
    }
}

/// One admitted beat as delivered to a consumer or relay. The
/// `continuity_bearing` flag is **local delivery metadata on the
/// relay→downstream envelope** (plan §4.4) — a relay may only set it
/// while its own upstream continuity for the key is Established, so
/// establishment propagates hop-by-hop from the live origin stream.
/// It is never a field inside the origin-signed attestation.
#[derive(Clone, Copy, Debug)]
pub struct DeliveredBeat {
    /// Signed status.
    pub attested_status: AttestedStatus,
    /// Signed time-to-start estimate.
    pub estimated_start: Option<Duration>,
    /// Signed boot epoch.
    pub source_incarnation: Incarnation,
    /// Signed provider announce generation.
    pub capability_generation: u64,
    /// Signed sequence number.
    pub seq: u64,
    /// Signed emission cadence.
    pub promised_cadence: Duration,
    /// Envelope metadata: live-stream delivery (`true`) vs cached
    /// warm-start (`false`).
    pub continuity_bearing: bool,
}

/// Per-key continuity state machine (plan §3.4 transitions). Owns
/// the observation, both deadlines, and the projection; callers feed
/// it only **gate-admitted** beats ([`super::IncarnationSeqGate`])
/// and drive time explicitly through `expire_if_due` — no hidden
/// clock reads, so every SI-0 test controls the timeline.
#[derive(Debug)]
pub struct ObservationCell {
    observation: Option<ReadinessObservation>,
    continuity: Continuity,
    /// When the current continuity state times out: the
    /// establishment deadline while Unestablished, the suspicion
    /// deadline while Established. Meaningless once Expired.
    deadline: Instant,
    /// This consumer's own requested_sample_interval D.
    own_interval: Duration,
    /// k in the window rule.
    factor: u32,
    last_disrupt: Option<DisruptReason>,
}

impl ObservationCell {
    /// Interest registration (plan §3.4): continuity starts
    /// Unestablished and the establishment deadline starts counting
    /// — before any beat arrives there is no `promised_cadence`, so
    /// the window derives from the consumer's own D alone.
    pub fn register(now: Instant, own_interval: Duration, factor: u32) -> Self {
        Self {
            observation: None,
            continuity: Continuity::Unestablished,
            deadline: now + own_interval.saturating_mul(factor),
            own_interval,
            factor,
            last_disrupt: None,
        }
    }

    fn window(&self, promised_cadence: Duration) -> Duration {
        promised_cadence
            .max(self.own_interval)
            .saturating_mul(self.factor)
    }

    /// Apply one gate-admitted beat.
    ///
    /// - A **continuity-bearing** beat establishes (from
    ///   Unestablished, Established, or Expired — a resumed live
    ///   stream is how continuity recovers) and re-arms the
    ///   suspicion deadline from `max(promised_cadence, own D)`.
    /// - A **warm-start** beat updates the attested content but
    ///   NEVER touches continuity or either deadline: a chain of
    ///   strictly-newer cached beats is not a live stream, so it
    ///   must neither establish nor postpone expiry (SI-0 tests
    ///   13/14).
    /// - Continuity never carries across an incarnation boundary: a
    ///   warm-started beat from a NEW incarnation expires the cell
    ///   (§4.7) until the new stream itself establishes.
    /// - Continuity never carries across a GENERATION boundary
    ///   either (v4.1, §3.4) — but a generation change is a
    ///   *redefinition*, not a failure signal: the cell resets to a
    ///   fresh observation (Unestablished, establishment deadline
    ///   restarted), so a warm-started NotReady under the new
    ///   generation still projects (pessimism is safe) while a
    ///   warm-started Ready must earn continuity anew.
    pub fn on_admitted_beat(&mut self, now: Instant, beat: DeliveredBeat) {
        let crossed_incarnation = self
            .observation
            .is_some_and(|obs| obs.source_incarnation != beat.source_incarnation);
        let crossed_generation = self
            .observation
            .is_some_and(|obs| obs.capability_generation != beat.capability_generation);

        if beat.continuity_bearing {
            self.continuity = Continuity::Established;
            self.deadline = now + self.window(beat.promised_cadence);
            self.last_disrupt = None;
        } else if crossed_incarnation && self.continuity == Continuity::Established {
            self.continuity = Continuity::Expired;
            self.last_disrupt = Some(DisruptReason::IncarnationSuperseded);
        } else if crossed_generation {
            self.continuity = Continuity::Unestablished;
            self.deadline = now + self.own_interval.saturating_mul(self.factor);
            self.last_disrupt = Some(DisruptReason::GenerationChanged);
        }

        self.observation = Some(ReadinessObservation {
            attested_status: beat.attested_status,
            estimated_start: beat.estimated_start,
            source_incarnation: beat.source_incarnation,
            capability_generation: beat.capability_generation,
            last_seq: beat.seq,
            promised_cadence: beat.promised_cadence,
            continuity: self.continuity,
            locally_observed_at: now,
        });
    }

    /// Drive the clock: past-deadline Unestablished OR Established
    /// states expire (plan §3.4 — "Unestablished expires too").
    pub fn expire_if_due(&mut self, now: Instant) {
        if self.continuity != Continuity::Expired && now >= self.deadline {
            self.continuity = Continuity::Expired;
            if let Some(obs) = &mut self.observation {
                obs.continuity = Continuity::Expired;
            }
        }
    }

    /// Force-expire (plan §4.7): route withdrawal, path failure,
    /// generation change, scope-validation failure.
    pub fn disrupt(&mut self, reason: DisruptReason) {
        self.continuity = Continuity::Expired;
        self.last_disrupt = Some(reason);
        if let Some(obs) = &mut self.observation {
            obs.continuity = Continuity::Expired;
        }
    }

    /// Current projection (no observation yet → Unknown).
    pub fn projected(&self) -> ProjectedReadiness {
        match &self.observation {
            None => ProjectedReadiness::Unknown,
            Some(obs) => project(obs.attested_status, self.continuity),
        }
    }

    /// Current continuity state.
    pub const fn continuity(&self) -> Continuity {
        self.continuity
    }

    /// The latest admitted observation, if any.
    pub fn observation(&self) -> Option<&ReadinessObservation> {
        self.observation.as_ref()
    }

    /// The reason for the last forced expiry, if continuity was
    /// disrupted rather than timed out.
    pub const fn last_disrupt(&self) -> Option<DisruptReason> {
        self.last_disrupt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const K: u32 = 3;
    const D: Duration = Duration::from_millis(100);

    fn beat(seq: u64, status: AttestedStatus, bearing: bool) -> DeliveredBeat {
        DeliveredBeat {
            attested_status: status,
            estimated_start: None,
            source_incarnation: Incarnation::new(1),
            capability_generation: 4,
            seq,
            promised_cadence: Duration::from_millis(100),
            continuity_bearing: bearing,
        }
    }

    #[test]
    fn generation_change_starts_a_fresh_observation() {
        // v4.1 §3.4: continuity never crosses a generation change,
        // but the change is a redefinition, not a failure — the cell
        // resets to Unestablished with a restarted establishment
        // deadline.
        let t0 = Instant::now();
        let mut cell = ObservationCell::register(t0, D, K);
        cell.on_admitted_beat(t0, beat(10, AttestedStatus::Ready, true));
        assert_eq!(cell.projected(), ProjectedReadiness::Ready);

        // Warm-started Ready under the NEW generation: optimism must
        // be earned anew.
        let mut regen = beat(11, AttestedStatus::Ready, false);
        regen.capability_generation = 5;
        cell.on_admitted_beat(t0 + D, regen);
        assert_eq!(cell.continuity(), Continuity::Unestablished);
        assert_eq!(cell.projected(), ProjectedReadiness::Unknown);
        assert_eq!(cell.last_disrupt(), Some(DisruptReason::GenerationChanged));

        // The restarted establishment deadline still fires (stale
        // pessimism/optimism cannot sit forever)…
        cell.expire_if_due(t0 + D + D * K);
        assert_eq!(cell.continuity(), Continuity::Expired);

        // …and a live beat under the new generation establishes.
        let mut live = beat(12, AttestedStatus::Ready, true);
        live.capability_generation = 5;
        cell.on_admitted_beat(t0 + D * 5, live);
        assert_eq!(cell.projected(), ProjectedReadiness::Ready);

        // The NotReady polarity: a fresh-generation warm-started
        // NotReady projects immediately (pessimism is safe).
        let mut pessimist = ObservationCell::register(t0, D, K);
        pessimist.on_admitted_beat(t0, beat(1, AttestedStatus::NotReady, true));
        let mut regen_nr = beat(2, AttestedStatus::NotReady, false);
        regen_nr.capability_generation = 5;
        pessimist.on_admitted_beat(t0 + D, regen_nr);
        assert_eq!(pessimist.projected(), ProjectedReadiness::NotReady);
    }

    #[test]
    fn projection_table_is_pinned_exactly() {
        use AttestedStatus::*;
        use Continuity::*;
        use ProjectedReadiness as P;
        let table = [
            (Ready, Unestablished, P::Unknown), // optimism must be earned
            (Ready, Established, P::Ready),
            (Ready, Expired, P::Unknown),
            (NotReady, Unestablished, P::NotReady), // pessimism is safe
            (NotReady, Established, P::NotReady),
            (NotReady, Expired, P::Unknown),
            (ProviderUnknown, Unestablished, P::Unknown),
            (ProviderUnknown, Established, P::Unknown),
            (ProviderUnknown, Expired, P::Unknown),
        ];
        for (status, continuity, expected) in table {
            assert_eq!(
                project(status, continuity),
                expected,
                "project({status:?}, {continuity:?})",
            );
        }
    }

    #[test]
    fn registration_starts_unestablished_and_expires_at_the_deadline() {
        let t0 = Instant::now();
        let mut cell = ObservationCell::register(t0, D, K);
        assert_eq!(cell.continuity(), Continuity::Unestablished);
        assert_eq!(cell.projected(), ProjectedReadiness::Unknown);
        // One tick short of k×D: still waiting.
        cell.expire_if_due(t0 + D * K - Duration::from_millis(1));
        assert_eq!(cell.continuity(), Continuity::Unestablished);
        // At the establishment deadline: Expired.
        cell.expire_if_due(t0 + D * K);
        assert_eq!(cell.continuity(), Continuity::Expired);
        assert_eq!(cell.projected(), ProjectedReadiness::Unknown);
    }

    #[test]
    fn warm_start_ready_projects_unknown_but_notready_projects_immediately() {
        // The single-hop freshness-laundering tripwire (plan §7
        // tripwire 2, SI-0 test 11): a cached Ready must never
        // become "fresh" by being forwarded. Pessimism doesn't wait.
        let t0 = Instant::now();
        let mut ready_cell = ObservationCell::register(t0, D, K);
        ready_cell.on_admitted_beat(t0, beat(100, AttestedStatus::Ready, false));
        assert_eq!(ready_cell.continuity(), Continuity::Unestablished);
        assert_eq!(ready_cell.projected(), ProjectedReadiness::Unknown);

        let mut notready_cell = ObservationCell::register(t0, D, K);
        notready_cell.on_admitted_beat(t0, beat(100, AttestedStatus::NotReady, false));
        assert_eq!(notready_cell.projected(), ProjectedReadiness::NotReady);
    }

    #[test]
    fn continuity_bearing_beat_establishes_and_ready_projects() {
        let t0 = Instant::now();
        let mut cell = ObservationCell::register(t0, D, K);
        cell.on_admitted_beat(t0, beat(100, AttestedStatus::Ready, false));
        cell.on_admitted_beat(t0 + D, beat(101, AttestedStatus::Ready, true));
        assert_eq!(cell.continuity(), Continuity::Established);
        assert_eq!(cell.projected(), ProjectedReadiness::Ready);
    }

    #[test]
    fn cached_newer_beats_never_extend_the_establishment_deadline() {
        // SI-0 test 14 core: a warm-started cached NotReady whose
        // stream only ever produces further CACHED beats expires to
        // Unknown at the original establishment deadline — stale
        // pessimism cannot persist indefinitely, and strictly-newer
        // cache chatter is not a live stream (test 13's single-cell
        // face).
        let t0 = Instant::now();
        let mut cell = ObservationCell::register(t0, D, K);
        cell.on_admitted_beat(t0, beat(100, AttestedStatus::NotReady, false));
        assert_eq!(cell.projected(), ProjectedReadiness::NotReady);
        // Strictly-newer cached beats keep arriving inside the
        // window...
        cell.on_admitted_beat(t0 + D, beat(101, AttestedStatus::NotReady, false));
        cell.on_admitted_beat(t0 + D * 2, beat(102, AttestedStatus::NotReady, false));
        // ...but none of them is qualifying, so the ORIGINAL
        // deadline still fires.
        cell.expire_if_due(t0 + D * K);
        assert_eq!(cell.continuity(), Continuity::Expired);
        assert_eq!(cell.projected(), ProjectedReadiness::Unknown);
    }

    #[test]
    fn established_expires_on_silence_and_a_live_beat_recovers() {
        let t0 = Instant::now();
        let mut cell = ObservationCell::register(t0, D, K);
        cell.on_admitted_beat(t0, beat(1, AttestedStatus::Ready, true));
        // promised_cadence == D == 100ms → window 300ms.
        cell.expire_if_due(t0 + Duration::from_millis(299));
        assert_eq!(cell.projected(), ProjectedReadiness::Ready);
        cell.expire_if_due(t0 + Duration::from_millis(300));
        assert_eq!(cell.continuity(), Continuity::Expired);
        assert_eq!(cell.projected(), ProjectedReadiness::Unknown);
        // The stream resumes: continuity recovers through a live
        // beat, not through cache re-delivery.
        let t1 = t0 + Duration::from_millis(400);
        cell.on_admitted_beat(t1, beat(2, AttestedStatus::Ready, false));
        assert_eq!(cell.projected(), ProjectedReadiness::Unknown);
        cell.on_admitted_beat(t1 + D, beat(3, AttestedStatus::Ready, true));
        assert_eq!(cell.projected(), ProjectedReadiness::Ready);
    }

    #[test]
    fn window_is_k_times_max_of_promised_cadence_and_own_interval() {
        // The max term is what keeps a down-sampled subscriber from
        // being false-Unknowned (plan §4.5): own D = 500ms dominates
        // a 100ms promised cadence.
        let t0 = Instant::now();
        let own_d = Duration::from_millis(500);
        let mut cell = ObservationCell::register(t0, own_d, K);
        cell.on_admitted_beat(t0, beat(1, AttestedStatus::Ready, true));
        // Way past k×promised_cadence (300ms), inside k×D (1500ms):
        // still Established.
        cell.expire_if_due(t0 + Duration::from_millis(1499));
        assert_eq!(cell.projected(), ProjectedReadiness::Ready);
        cell.expire_if_due(t0 + Duration::from_millis(1500));
        assert_eq!(cell.projected(), ProjectedReadiness::Unknown);

        // And the other direction: a slow provider cadence (1s)
        // dominates a strict own D (100ms) — the consumer must not
        // suspect a stream that is delivering exactly as promised.
        let mut slow = ObservationCell::register(t0, D, K);
        let mut b = beat(1, AttestedStatus::Ready, true);
        b.promised_cadence = Duration::from_secs(1);
        slow.on_admitted_beat(t0, b);
        slow.expire_if_due(t0 + Duration::from_millis(2999));
        assert_eq!(slow.projected(), ProjectedReadiness::Ready);
        slow.expire_if_due(t0 + Duration::from_secs(3));
        assert_eq!(slow.projected(), ProjectedReadiness::Unknown);
    }

    #[test]
    fn continuity_never_carries_across_incarnations() {
        // §4.7: a new incarnation's stream must EARN continuity; the
        // old stream's establishment cannot vouch for it.
        let t0 = Instant::now();
        let mut cell = ObservationCell::register(t0, D, K);
        cell.on_admitted_beat(t0, beat(50, AttestedStatus::Ready, true));
        assert_eq!(cell.projected(), ProjectedReadiness::Ready);
        // Warm-started (cached) beat from the NEW incarnation.
        let mut restarted = beat(1, AttestedStatus::Ready, false);
        restarted.source_incarnation = Incarnation::new(2);
        cell.on_admitted_beat(t0 + D, restarted);
        assert_eq!(cell.continuity(), Continuity::Expired);
        assert_eq!(cell.projected(), ProjectedReadiness::Unknown);
        assert_eq!(
            cell.last_disrupt(),
            Some(DisruptReason::IncarnationSuperseded),
        );
        // The new incarnation establishes on its own live beat.
        let mut live = beat(2, AttestedStatus::Ready, true);
        live.source_incarnation = Incarnation::new(2);
        cell.on_admitted_beat(t0 + D * 2, live);
        assert_eq!(cell.projected(), ProjectedReadiness::Ready);
    }

    #[test]
    fn disrupt_expires_immediately_with_reason() {
        let t0 = Instant::now();
        let mut cell = ObservationCell::register(t0, D, K);
        cell.on_admitted_beat(t0, beat(1, AttestedStatus::Ready, true));
        cell.disrupt(DisruptReason::PathFailed);
        assert_eq!(cell.continuity(), Continuity::Expired);
        assert_eq!(cell.projected(), ProjectedReadiness::Unknown);
        assert_eq!(cell.last_disrupt(), Some(DisruptReason::PathFailed));
        let obs = cell.observation().unwrap();
        assert_eq!(obs.continuity, Continuity::Expired);
    }
}
