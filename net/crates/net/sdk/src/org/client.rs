//! OSDK §2 — [`OrgClient`] and the binding verb [`Mesh::org`].
//!
//! Binding is where a credential set stops being data and becomes an operating
//! capability: it pins the set to a durable mesh identity and an installed node
//! authority, and leases the consumer audiences private discovery needs.
//!
//! S0 lands the binding relation and the lease. The call verb (`org.call`)
//! lands in S1.

#[cfg(feature = "cortex")]
use std::collections::BTreeMap;
use std::sync::Arc;
#[cfg(feature = "cortex")]
use std::time::{Duration, Instant};

#[cfg(feature = "cortex")]
use net::adapter::net::behavior::org_cold_plan::OrgColdAuthority;
use net::adapter::net::behavior::org_grant::CapabilityAuthorityId;
#[cfg(feature = "cortex")]
use net::adapter::net::behavior::org_sensing_demand::{OrgSensingFamily, MAX_SENSED_POPULATION};
use net::adapter::net::identity::EntityKeypair;
use net::adapter::net::MeshNode;

use super::credentials::OrgCredentials;
use super::error::{hex32, OrgCredentialError, OrgSdkError};
use super::lease::AudienceLeaseGuard;
use super::types::{OrgCapabilityGrant, OrgDispatcherGrant, OrgId, OrgMembershipCert};
use crate::mesh::Mesh;

/// This client's organization-sensing acquisition, decided ONCE at bind.
///
/// Two-state by construction. The family mint is fallible — the routing
/// registry's family identity space is bounded and terminal — and a binding
/// that cannot sense is INERT, not broken: it plans in the deterministic
/// unsensed order and calls exactly as it always did. So `bind_node` MAPS the
/// mint result instead of propagating it, and gains no error variant.
///
/// `Active` holds one `Arc`-shared family body plus this binding's own
/// reconciliation schedule. Cloning the binding shares both, so every clone of
/// an [`OrgClient`] shares one acquisition: an intermediate clone's drop
/// retires nothing, and the last one retires everything (the family body's own
/// `Drop`). `Inert` owns nothing, so its drop is a no-op and there is no
/// per-call re-mint to hammer an exhausted space — recovery is a new bind.
///
/// `pub(crate)`: this type is not part of any public signature. What a witness
/// can observe about it is a small set of `#[doc(hidden)]`, test/fixtures-only
/// DATA accessors on [`OrgClient`] — never the cloneable family handle itself.
#[cfg(feature = "cortex")]
#[derive(Clone)]
pub(crate) enum OrgSensingBinding {
    /// The mint succeeded; this client shares one acquisition.
    Active(OrgSensingAcquisition),
    /// The mint was refused. Deterministic unsensed planning, no sensing work.
    Inert,
}

#[cfg(feature = "cortex")]
impl OrgSensingBinding {
    /// The acquisition, when this binding is active.
    pub(crate) fn acquisition(&self) -> Option<&OrgSensingAcquisition> {
        match self {
            Self::Active(acquisition) => Some(acquisition),
            Self::Inert => None,
        }
    }
}

/// One binding's acquisition: the shared family, the shared record of what
/// each capability's demand was last certified as, and the lock that makes a
/// reconciliation decision one section.
#[cfg(feature = "cortex")]
#[derive(Clone)]
pub(crate) struct OrgSensingAcquisition {
    family: OrgSensingFamily,
    schedule: Arc<ConvergenceSchedule>,
}

/// What each capability's demand was last CERTIFIED as.
///
/// Bounded in both directions. An unchanged, fully certified population must
/// not be re-acquired on every call; a changed one — a newly discovered
/// provider, a departed one, a pin that came or went, a holder that died —
/// must not stay frozen. And the record set itself is capped: a caller that
/// walks many capabilities cannot grow it without limit.
///
/// A record only certifies the demand it was taken FROM: it carries that
/// demand's identity and the population core published, so a demand replaced
/// or retired by another clone is never vouched for by an older record.
#[cfg(feature = "cortex")]
#[derive(Default)]
pub(crate) struct ConvergenceSchedule {
    records: parking_lot::Mutex<BTreeMap<CapabilityAuthorityId, ConvergedFor>>,
    /// Serializes decide → converge → record for this binding, so two clones
    /// cannot both decide to converge one change. Held only across synchronous
    /// work.
    reconcile: parking_lot::Mutex<()>,
    /// Instrumented builds only.
    ///
    /// * `arrivals` counts callers that reached the section's door;
    /// * `contended` counts callers that found the section ACTUALLY HELD -
    ///   `reconcile_lock` tries first and only counts when the try fails, so
    ///   this is acquisition-time contention, never an inference from elapsed
    ///   time or from spawning;
    /// * `convergences` counts attempts that got past the decision;
    /// * `occupancy`/`peak_occupancy` gauge how many callers are inside the
    ///   WHOLE transaction at once - entered before the decision and left
    ///   after the record is committed, with the guard still held. A lock that
    ///   covered only the observer would leave the real transaction
    ///   overlapping and show a peak above one;
    /// * `in_section` fires once inside the section so a witness can hold it
    ///   open while the others arrive;
    /// * `last_expectation` is the expectation the most recent attempt
    ///   ACTUALLY derived, so a witness observes the captured input instead of
    ///   assuming what a pre-call sample implies. Last-writer-wins: meaningful
    ///   for a single-caller witness, not for concurrent ones.
    #[cfg(any(test, feature = "fixtures"))]
    arrivals: std::sync::atomic::AtomicU64,
    #[cfg(any(test, feature = "fixtures"))]
    contended: std::sync::atomic::AtomicU64,
    #[cfg(any(test, feature = "fixtures"))]
    convergences: std::sync::atomic::AtomicU64,
    #[cfg(any(test, feature = "fixtures"))]
    occupancy: std::sync::atomic::AtomicU64,
    #[cfg(any(test, feature = "fixtures"))]
    peak_occupancy: std::sync::atomic::AtomicU64,
    #[cfg(any(test, feature = "fixtures"))]
    in_section: parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(any(test, feature = "fixtures"))]
    last_expectation: parking_lot::Mutex<Vec<u64>>,
    /// Instrumented builds only. Who HOLDS the section right now.
    ///
    /// Every acquisition takes a fresh ticket from `holder_seq` and publishes
    /// it; releasing publishes zero. The transaction then re-checks its own
    /// ticket at the points that must be protected - the decision, and the
    /// record it commits - and `unguarded` counts the steps that ran when this
    /// caller was NOT the holder. That is a statement about the guard itself,
    /// so it needs no overlap: a section released early fails it even if the
    /// awakened contender politely waits, and even with one caller.
    #[cfg(any(test, feature = "fixtures"))]
    holder_seq: std::sync::atomic::AtomicU64,
    #[cfg(any(test, feature = "fixtures"))]
    current_holder: std::sync::atomic::AtomicU64,
    #[cfg(any(test, feature = "fixtures"))]
    unguarded: std::sync::atomic::AtomicU64,
}

/// One capability's last convergence attempt.
#[cfg(feature = "cortex")]
struct ConvergedFor {
    /// The expectation the attempt was driven by — this call's own pinned
    /// same-organization candidates. Compared by value, so a discovery or pin
    /// change is the trigger rather than a clock.
    expected: Vec<u64>,
    /// What the attempt produced.
    outcome: Outcome,
    /// When it ran, so a paced case is retried on a floor rather than per
    /// call.
    attempted: Instant,
}

/// What a convergence attempt produced.
#[cfg(feature = "cortex")]
enum Outcome {
    /// Core published a demand. `agreed` says whether that demand AGREES with
    /// the expectation it was asked for and holds a holder for every member of
    /// its own population; anything less is retried on the floor.
    Certified {
        /// The identity of the demand this record describes: a demand replaced
        /// or retired since then invalidates the record outright.
        ///
        /// Core's own monotone id, NOT the demand's address. This record holds
        /// no `Arc`, so an address could be reused by a fresh demand allocated
        /// where a dropped one used to live - and with the same capability and
        /// population that compared equal, skipping a convergence that was
        /// genuinely needed.
        demand: u64,
        /// The population core actually published for it.
        population: Vec<u64>,
        agreed: bool,
    },
    /// Core refused. Nothing is certified, and the ATTEMPT is what paces the
    /// next one — including when no demand is installed at all, which is
    /// exactly the shape a `FamilyAtCapacity` refusal leaves behind.
    ///
    /// `under` is the installed demand's state at the moment of the refusal.
    /// A demand whose authority moved or whose holders died forces an
    /// immediate convergence, because no record can vouch for state it cannot
    /// see — but a wave of callers that all queued behind one refusal would
    /// otherwise each re-derive the SAME dead state and be refused again,
    /// per call, forever. Recording the state the refusal happened under lets
    /// the floor pace that repeat while any genuine CHANGE still bypasses it.
    Refused {
        /// `None` when no demand was installed.
        under: Option<DemandState>,
        /// The AUTHORITY observation the refused attempt derived under.
        ///
        /// The installed demand's own booleans cannot carry this: a demand
        /// staled by an authority loss stays stale after that authority is
        /// restored, so a rule that compared only those booleans paced a
        /// fresh, usable capture as if it were the same failure. Asking the
        /// node whether THIS observation is still the one in force answers the
        /// real question - a rotated or restored authority, a cleared poison,
        /// a raised floor or consumer-grant churn all end the context the
        /// refusal belonged to.
        authority: OrgColdAuthority,
    },
}

/// What a reuse decision needs to know about the authority context.
///
/// Borrowed for the length of one decision: the node it asks, and the
/// authority observation the CURRENT attempt is deriving under - which a
/// refusal records so a later decision can ask whether it still holds.
#[cfg(feature = "cortex")]
pub(crate) struct RefusalContext<'a> {
    node: &'a MeshNode,
    authority: &'a OrgColdAuthority,
}

#[cfg(feature = "cortex")]
impl<'a> RefusalContext<'a> {
    pub(crate) fn new(node: &'a MeshNode, authority: &'a OrgColdAuthority) -> Self {
        Self { node, authority }
    }

    /// Whether the authority a past refusal was recorded under is STILL the
    /// one in force. `false` means that refusal's context is over.
    fn still_in_force(&self, recorded: &OrgColdAuthority) -> bool {
        self.node.org_cold_authority_is_current(recorded)
    }
}

/// The installed demand's identity and liveness at one instant.
///
/// Identity is included on purpose: a replaced demand is a different fact, and
/// its first refusal is never paced by its predecessor's.
#[cfg(feature = "cortex")]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct DemandState {
    demand: u64,
    authority_current: bool,
    holders_live: bool,
}

#[cfg(feature = "cortex")]
impl DemandState {
    fn of(
        demand: &Arc<net::adapter::net::behavior::org_sensing_demand::OrgSensingCapabilityDemand>,
    ) -> Self {
        Self {
            demand: demand.id(),
            authority_current: demand.authority_is_current(),
            holders_live: demand.holders_are_live(),
        }
    }

    /// Whether this state is one no record can vouch for.
    fn degraded(&self) -> bool {
        !self.authority_current || !self.holders_live
    }
}

/// The most capabilities one binding keeps convergence records for.
///
/// A bound of its own, not core's: it exists so a caller walking many
/// capability names cannot grow this map without limit. Evicting the least
/// recently attempted record costs at most one later convergence, because
/// every reuse decision re-derives its inputs anyway.
#[cfg(feature = "cortex")]
const MAX_CONVERGENCE_RECORDS: usize = 64;

#[cfg(feature = "cortex")]
impl ConvergenceSchedule {
    /// Whether `expected` demands a convergence now, given `installed` — the
    /// demand currently in force for that capability.
    ///
    /// Two independent things are paced here, and neither may invalidate the
    /// other by construction:
    ///
    /// * a REFUSED attempt paces the next attempt, whether or not a demand is
    ///   installed. A capability that keeps meeting `FamilyAtCapacity` must not
    ///   retry on every call. But it paces only its OWN CONTEXT: the same
    ///   authority observation still in force, and the same installed-demand
    ///   state. That is what separates the queued wave - several callers behind
    ///   one refusal, all re-deriving the same corpse under the same dead
    ///   authority - from a genuinely new capture. A rotated or restored
    ///   authority, a cleared poison, a raised floor, consumer-grant churn, a
    ///   replaced demand or a recovered holder ends that context and converges
    ///   at once, floor or no floor;
    /// * a CERTIFIED demand is reusable only while it still describes what is
    ///   installed and it AGREED with the expectation. A demand that disagrees
    ///   — narrower or wider than the expectation — is retried on the floor
    ///   until the two sides agree. Repetition is not agreement: an identical
    ///   mismatch seen twice is still a mismatch.
    ///
    /// A degraded installed demand — moved sensing authority, dead holder —
    /// overrides a CERTIFIED record outright, because no certificate can vouch
    /// for state it cannot see.
    pub(crate) fn needs_convergence(
        &self,
        capability: &CapabilityAuthorityId,
        expected: &[u64],
        installed: Option<
            &Arc<net::adapter::net::behavior::org_sensing_demand::OrgSensingCapabilityDemand>,
        >,
        now: Instant,
        retry_floor: Duration,
        context: &RefusalContext<'_>,
    ) -> bool {
        let state = installed.map(DemandState::of);
        let records = self.records.lock();
        let Some(record) = records.get(capability) else {
            return true; // Never attempted for this capability.
        };
        if record.expected != expected {
            return true; // Different inputs: decide again, with no floor.
        }
        let floored = now.saturating_duration_since(record.attempted) < retry_floor;
        match &record.outcome {
            Outcome::Refused { under, authority } => {
                if *under != state || !context.still_in_force(authority) {
                    // A different demand state, or an authority observation
                    // that is no longer the one in force: this is not the
                    // situation that was refused.
                    return true;
                }
                !floored
            }
            Outcome::Certified {
                demand,
                population,
                agreed,
            } => {
                let Some(installed) = installed else {
                    // The demand this record certified is gone.
                    return true;
                };
                if state.is_some_and(|state| state.degraded()) {
                    // A moved authority or a dead holder: no certificate can
                    // vouch for it, so this converges immediately.
                    return true;
                }
                if *demand != installed.id() || *population != population_of(installed) {
                    return true;
                }
                !*agreed && !floored
            }
        }
    }

    /// Certify `demand` for `capability` under `expected`.
    ///
    /// AGREED means two things at once: every member of the published
    /// population has a live holder, and that population is the CANONICAL one
    /// this expectation asks for.
    ///
    /// Canonical is core's own rule, not a size test. Core sorts the
    /// authorized population, deduplicates it, and keeps the LOWEST
    /// [`MAX_SENSED_POPULATION`] node ids
    /// (`MeshNode::org_sensing_authorized_population`, and the same clamp
    /// again inside the convergence). So the population that agrees with an
    /// expectation is exactly that expectation's leading prefix - the whole
    /// set when it fits under the bound, its lowest `MAX_SENSED_POPULATION`
    /// members when it does not.
    ///
    /// Accepting "cap-sized AND contained" instead froze a real defect: a
    /// provider that belongs in the canonical prefix but was missing from the
    /// published population - because its discovery row expired between this
    /// call's capture and core's query - yields a cap-sized subset, which
    /// looked like the cap and settled forever, so the provider never came
    /// back even after it was rediscovered under an UNCHANGED expectation.
    ///
    /// Nothing else counts as agreement. In particular a mismatch is NOT
    /// settled by being seen twice: a discovery row that expired between this
    /// call's capture and core's own query yields a narrower population, and a
    /// row that appeared in that window yields a wider one - and in both cases
    /// the only thing that resolves it is a later attempt whose two sides
    /// agree.
    ///
    /// `expected` is the caller's already-sorted, deduplicated expectation
    /// (see `OrgClient::apply_sensed_order`); the prefix rule is meaningless
    /// against an unordered list, so it is canonicalized here too rather than
    /// trusted.
    pub(crate) fn certify(
        &self,
        capability: CapabilityAuthorityId,
        expected: Vec<u64>,
        demand: &Arc<net::adapter::net::behavior::org_sensing_demand::OrgSensingCapabilityDemand>,
        now: Instant,
    ) {
        let mut expected = expected;
        expected.sort_unstable();
        expected.dedup();
        let population = population_of(demand);
        let mut retained = demand.retained_providers();
        retained.sort_unstable();
        retained.dedup();
        let holders_complete = retained == population;
        let ceiling = expected.len().min(MAX_SENSED_POPULATION);
        let agreed = holders_complete && population.as_slice() == &expected[..ceiling];
        self.insert(
            capability,
            ConvergedFor {
                expected,
                outcome: Outcome::Certified {
                    demand: demand.id(),
                    population,
                    agreed,
                },
                attempted: now,
            },
        );
    }

    /// Record a REFUSED convergence: nothing is certified, and the attempt
    /// paces the next one.
    pub(crate) fn record_refusal(
        &self,
        capability: CapabilityAuthorityId,
        expected: Vec<u64>,
        installed: Option<
            &Arc<net::adapter::net::behavior::org_sensing_demand::OrgSensingCapabilityDemand>,
        >,
        context: &RefusalContext<'_>,
        now: Instant,
    ) {
        self.insert(
            capability,
            ConvergedFor {
                expected,
                outcome: Outcome::Refused {
                    under: installed.map(DemandState::of),
                    authority: context.authority.clone(),
                },
                attempted: now,
            },
        );
    }

    /// Instrumented: one caller reached the reconciliation section's door.
    #[cfg(any(test, feature = "fixtures"))]
    pub(crate) fn note_arrival(&self) {
        self.arrivals
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(not(any(test, feature = "fixtures")))]
    pub(crate) fn note_arrival(&self) {}

    /// Instrumented: one caller got past the decision and is converging.
    #[cfg(any(test, feature = "fixtures"))]
    pub(crate) fn note_convergence(&self) {
        self.convergences
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(not(any(test, feature = "fixtures")))]
    pub(crate) fn note_convergence(&self) {}

    /// Instrumented: fire the in-section hook, if one is installed.
    #[cfg(any(test, feature = "fixtures"))]
    pub(crate) fn fire_in_section(&self) {
        let hook = self.in_section.lock().clone();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(not(any(test, feature = "fixtures")))]
    pub(crate) fn fire_in_section(&self) {}

    /// Instrumented: one caller found the section ALREADY HELD.
    #[cfg(any(test, feature = "fixtures"))]
    pub(crate) fn note_contended(&self) {
        self.contended
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(not(any(test, feature = "fixtures")))]
    pub(crate) fn note_contended(&self) {}

    /// Enter the transaction's OCCUPANCY span. The returned guard leaves it on
    /// drop, which - declared after the serializing guard - happens while that
    /// guard is still held, so the span covers decide, converge and record.
    pub(crate) fn section_span(&self) -> SectionSpan<'_> {
        #[cfg(any(test, feature = "fixtures"))]
        {
            let inside = self
                .occupancy
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            self.peak_occupancy
                .fetch_max(inside, std::sync::atomic::Ordering::SeqCst);
        }
        SectionSpan { schedule: self }
    }

    #[cfg(any(test, feature = "fixtures"))]
    fn leave_section(&self) {
        self.occupancy
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }

    /// Instrumented: the expectation this attempt actually derived.
    #[cfg(any(test, feature = "fixtures"))]
    pub(crate) fn note_expectation(&self, expected: &[u64]) {
        *self.last_expectation.lock() = expected.to_vec();
    }

    #[cfg(not(any(test, feature = "fixtures")))]
    pub(crate) fn note_expectation(&self, _expected: &[u64]) {}

    /// The expectation the most recent attempt derived.
    #[cfg(any(test, feature = "fixtures"))]
    pub(crate) fn last_expectation(&self) -> Vec<u64> {
        self.last_expectation.lock().clone()
    }

    /// Publish a fresh holder ticket for an acquisition that just succeeded.
    #[cfg(any(test, feature = "fixtures"))]
    pub(crate) fn take_holder(&self) -> u64 {
        let ticket = self
            .holder_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        self.current_holder
            .store(ticket, std::sync::atomic::Ordering::SeqCst);
        ticket
    }

    #[cfg(not(any(test, feature = "fixtures")))]
    pub(crate) fn take_holder(&self) -> u64 {
        0
    }

    /// Retract `ticket`'s hold, if it is still the published one.
    #[cfg(any(test, feature = "fixtures"))]
    fn release_holder(&self, ticket: u64) {
        let _ = self.current_holder.compare_exchange(
            ticket,
            0,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        );
    }

    /// One step of the transaction that MUST run under this caller's own hold.
    ///
    /// Records a violation when the section has been released - or taken over -
    /// since `ticket` acquired it. Nothing is thrown: the call proceeds exactly
    /// as before, and the count is the witness's to read.
    #[cfg(any(test, feature = "fixtures"))]
    pub(crate) fn verify_holding(&self, ticket: u64) {
        if self
            .current_holder
            .load(std::sync::atomic::Ordering::SeqCst)
            != ticket
        {
            self.unguarded
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[cfg(not(any(test, feature = "fixtures")))]
    pub(crate) fn verify_holding(&self, _ticket: u64) {}

    /// Transaction steps that ran without this caller's hold.
    #[cfg(any(test, feature = "fixtures"))]
    pub(crate) fn unguarded_steps(&self) -> u64 {
        self.unguarded.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Counters for a witness:
    /// `(arrivals, contended, convergences, peak_occupancy)`.
    #[cfg(any(test, feature = "fixtures"))]
    pub(crate) fn instrumentation(&self) -> (u64, u64, u64, u64) {
        (
            self.arrivals.load(std::sync::atomic::Ordering::SeqCst),
            self.contended.load(std::sync::atomic::Ordering::SeqCst),
            self.convergences.load(std::sync::atomic::Ordering::SeqCst),
            self.peak_occupancy
                .load(std::sync::atomic::Ordering::SeqCst),
        )
    }

    /// Install the in-section hook.
    #[cfg(any(test, feature = "fixtures"))]
    pub(crate) fn set_in_section_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.in_section.lock() = hook;
    }

    /// Forget `capability`'s record — used when its demand is retired, so
    /// nothing certifies a demand that no longer exists.
    pub(crate) fn forget(&self, capability: &CapabilityAuthorityId) {
        self.records.lock().remove(capability);
    }

    /// Insert, evicting the least recently attempted record at the bound.
    fn insert(&self, capability: CapabilityAuthorityId, record: ConvergedFor) {
        let mut records = self.records.lock();
        if records.len() >= MAX_CONVERGENCE_RECORDS && !records.contains_key(&capability) {
            if let Some(oldest) = records
                .iter()
                .min_by_key(|(_, record)| record.attempted)
                .map(|(capability, _)| *capability)
            {
                records.remove(&oldest);
            }
        }
        records.insert(capability, record);
    }

    /// How many capabilities this binding holds records for (test/fixtures).
    #[cfg(any(test, feature = "fixtures"))]
    pub(crate) fn len(&self) -> usize {
        self.records.lock().len()
    }
}

/// A demand's published population, ascending — the comparison basis for every
/// record.
#[cfg(feature = "cortex")]
fn population_of(
    demand: &net::adapter::net::behavior::org_sensing_demand::OrgSensingCapabilityDemand,
) -> Vec<u64> {
    let mut population = demand.population().to_vec();
    population.sort_unstable();
    population.dedup();
    population
}

#[cfg(feature = "cortex")]
impl OrgSensingAcquisition {
    pub(crate) fn new(family: OrgSensingFamily) -> Self {
        Self {
            family,
            schedule: Arc::new(ConvergenceSchedule::default()),
        }
    }

    pub(crate) fn family(&self) -> &OrgSensingFamily {
        &self.family
    }

    pub(crate) fn schedule(&self) -> &ConvergenceSchedule {
        &self.schedule
    }

    /// Take the reconciliation section for this binding.
    ///
    /// Try first, then block. The fast path is what `lock` would have done
    /// anyway, and the slow path is the only place a caller can HONESTLY be
    /// said to have contended: it found the section held by somebody else. In
    /// production both arms are the same acquisition - `note_contended`
    /// compiles away.
    pub(crate) fn reconcile_lock(&self) -> ReconcileGuard<'_> {
        let guard = match self.schedule.reconcile.try_lock() {
            Some(guard) => guard,
            None => {
                self.schedule.note_contended();
                self.schedule.reconcile.lock()
            }
        };
        ReconcileGuard {
            ticket: self.schedule.take_holder(),
            schedule: &self.schedule,
            _guard: guard,
        }
    }
}

/// The reconciliation section's guard, plus the HOLDER TICKET the transaction
/// re-checks at the steps that must be protected.
///
/// Owning the guard is what serializes; owning the ticket is what makes that
/// checkable. The ticket is a plain copy, so a transaction can ask "am I still
/// the holder?" at the decision and at the record without borrowing the guard —
/// which is the whole point: a guard released early still answers honestly,
/// with no second caller and no overlap required.
#[cfg(feature = "cortex")]
pub(crate) struct ReconcileGuard<'a> {
    ticket: u64,
    schedule: &'a ConvergenceSchedule,
    _guard: parking_lot::MutexGuard<'a, ()>,
}

#[cfg(feature = "cortex")]
impl ReconcileGuard<'_> {
    /// This hold's ticket.
    pub(crate) fn ticket(&self) -> u64 {
        self.ticket
    }
}

#[cfg(feature = "cortex")]
impl Drop for ReconcileGuard<'_> {
    fn drop(&mut self) {
        #[cfg(any(test, feature = "fixtures"))]
        self.schedule.release_holder(self.ticket);
        #[cfg(not(any(test, feature = "fixtures")))]
        let _ = self.schedule;
    }
}

/// The reconciliation transaction's occupancy span (instrumented builds
/// measure it; production compiles it away).
///
/// Created immediately after the serializing guard and dropped at the end of
/// the block — and because locals drop in reverse declaration order, that drop
/// runs while the guard is still held. So the span brackets the DECISION, the
/// convergence and the record, not just the observer hook: a lock narrowed to
/// the hook alone would let a second caller enter the span while the first is
/// still committing, and the peak would exceed one.
#[cfg(feature = "cortex")]
pub(crate) struct SectionSpan<'a> {
    #[cfg_attr(
        not(any(test, feature = "fixtures")),
        allow(dead_code, reason = "the span is an instrumented-build gauge")
    )]
    schedule: &'a ConvergenceSchedule,
}

#[cfg(feature = "cortex")]
impl Drop for SectionSpan<'_> {
    fn drop(&mut self) {
        #[cfg(any(test, feature = "fixtures"))]
        self.schedule.leave_section();
    }
}

/// A credential set bound to a live mesh — the caller half of the org facade.
///
/// Obtained from [`Mesh::org`]. Cloning shares one audience lease rather than
/// taking a second reference, so clones are free and dropping a clone never
/// withdraws another clone's ingest authority.
#[derive(Clone)]
pub struct OrgClient {
    /// The node this client calls through. Used by the call verb, which rides
    /// the nRPC surface.
    #[cfg_attr(not(feature = "cortex"), allow(dead_code))]
    pub(crate) node: Arc<MeshNode>,
    /// The mesh's durable identity — signs every proof this client mints.
    pub(crate) caller: Arc<EntityKeypair>,
    pub(crate) membership: OrgMembershipCert,
    pub(crate) dispatcher: OrgDispatcherGrant,
    pub(crate) grants: Vec<OrgCapabilityGrant>,
    pub(crate) acting_org: OrgId,
    /// Clock-skew tolerance from the installed authority — the same tolerance
    /// the provider applies, so local temporal checks agree with remote ones.
    pub(crate) skew_secs: u64,
    /// Dropped with the last clone; releases the consumer-audience references.
    pub(crate) _lease: Arc<AudienceLeaseGuard>,
    /// Instrumented builds only: the provider the last planned call selected,
    /// shared by every clone exactly like the lease.
    #[cfg(all(feature = "cortex", any(test, feature = "fixtures")))]
    pub(crate) selected: Arc<parking_lot::Mutex<Option<net::adapter::net::identity::EntityId>>>,
    /// This client's sensing acquisition, minted once at bind. Shared by every
    /// clone exactly like `_lease`; the last clone's drop retires the demand.
    #[cfg(feature = "cortex")]
    pub(crate) _sensing: OrgSensingBinding,
}

impl OrgClient {
    /// The organization this client acts for.
    pub fn acting_org(&self) -> OrgId {
        self.acting_org
    }

    /// The entity this client calls as (the mesh's durable identity).
    pub fn caller(&self) -> &net::adapter::net::identity::EntityId {
        self.caller.entity_id()
    }

    /// The held cross-org capability grants.
    pub fn grants(&self) -> &[OrgCapabilityGrant] {
        &self.grants
    }

    /// Whether this client's sensing binding is active.
    ///
    /// # The test/fixtures observation seam
    ///
    /// This and the four accessors below are the WHOLE observable surface of
    /// the sensing binding, they are `#[doc(hidden)]` and gated to
    /// test/`fixtures` builds, and they return plain DATA — never the
    /// cloneable family handle, which would hand a caller ownership of this
    /// binding's acquisition. They exist because the binding's contract is
    /// about ownership and reconciliation, and neither is observable from a
    /// reply body. Nothing in production reads them.
    #[cfg(all(feature = "cortex", any(test, feature = "fixtures")))]
    #[doc(hidden)]
    pub fn sensing_is_active(&self) -> bool {
        self._sensing.acquisition().is_some()
    }

    /// How many owners share this binding's acquisition, or `None` if inert.
    #[cfg(all(feature = "cortex", any(test, feature = "fixtures")))]
    #[doc(hidden)]
    pub fn sensing_owners(&self) -> Option<usize> {
        self._sensing
            .acquisition()
            .map(|acquisition| acquisition.family().owners())
    }

    /// The capabilities this binding currently retains demand for.
    #[cfg(all(feature = "cortex", any(test, feature = "fixtures")))]
    #[doc(hidden)]
    pub fn sensing_capabilities(&self) -> Vec<CapabilityAuthorityId> {
        self._sensing
            .acquisition()
            .map(|acquisition| acquisition.family().capabilities())
            .unwrap_or_default()
    }

    /// One capability's retained demand as plain data:
    /// `(population, retained_holders, demand_identity)`.
    ///
    /// The identity is core's own monotone demand id, which is how a witness
    /// distinguishes "the same demand was reused" from "an identical one was
    /// re-acquired". It is an opaque number, not a handle - and deliberately
    /// not the `Arc`'s address, which a later demand allocated where a dropped
    /// one used to live can reuse.
    #[cfg(all(feature = "cortex", any(test, feature = "fixtures")))]
    #[doc(hidden)]
    pub fn sensing_demand_state(
        &self,
        capability: &CapabilityAuthorityId,
    ) -> Option<(Vec<u64>, Vec<u64>, u64)> {
        let demand = self._sensing.acquisition()?.family().demand(capability)?;
        let identity = demand.id();
        Some((
            demand.population().to_vec(),
            demand.retained_providers(),
            identity,
        ))
    }

    /// How many capabilities this binding holds convergence records for.
    #[cfg(all(feature = "cortex", any(test, feature = "fixtures")))]
    #[doc(hidden)]
    pub fn sensing_records(&self) -> Option<usize> {
        self._sensing
            .acquisition()
            .map(|acquisition| acquisition.schedule().len())
    }

    /// Whether core still considers every retained holder of this client's
    /// demand to be the installation it was committed with.
    #[cfg(all(feature = "cortex", any(test, feature = "fixtures")))]
    #[doc(hidden)]
    pub fn sensing_holders_are_live(&self, capability: &CapabilityAuthorityId) -> Option<bool> {
        let demand = self._sensing.acquisition()?.family().demand(capability)?;
        Some(demand.holders_are_live())
    }

    /// RELEASE the demand's own holder tickets, killing their ownership while
    /// leaving the demand's recorded provider list untouched.
    ///
    /// Test/fixtures only. It is how a witness reaches the state a refused
    /// restoration under an older authority leaves behind — a recorded ticket
    /// that is no longer the holder — through the shipped release verb rather
    /// than by fabricating one.
    #[cfg(all(feature = "cortex", any(test, feature = "fixtures")))]
    #[doc(hidden)]
    pub fn sensing_release_holders_for_test(
        &self,
        capability: &CapabilityAuthorityId,
    ) -> Option<usize> {
        let demand = self._sensing.acquisition()?.family().demand(capability)?;
        let mut released = 0;
        for (_, ticket, _) in demand.retained_holders_for_test() {
            if self.node.try_release_sensing_interest_lease(ticket).is_ok() {
                released += 1;
            }
        }
        Some(released)
    }

    /// `(arrivals, contended, convergences, peak_occupancy)` at this binding's
    /// reconciliation section.
    ///
    /// Arrivals are counted at the door; `contended` counts the callers whose
    /// acquisition actually found the section held, which is what establishes
    /// overlap without appealing to elapsed time; convergences count the
    /// attempts that got past the decision; and the peak occupancy is the most
    /// callers ever simultaneously inside the whole decide → converge → record
    /// transaction. One change under real contention must produce many
    /// arrivals, real contention, exactly one convergence, and a peak of one.
    #[cfg(all(feature = "cortex", any(test, feature = "fixtures")))]
    #[doc(hidden)]
    pub fn sensing_section_counters(&self) -> Option<(u64, u64, u64, u64)> {
        self._sensing
            .acquisition()
            .map(|acquisition| acquisition.schedule().instrumentation())
    }

    /// The expectation this binding's most recent attempt ACTUALLY derived.
    ///
    /// The sensed expectation is computed inside the call path from that
    /// call's own capture, so a witness that sampled discovery beforehand is
    /// asserting about a different observation. This reports the real one.
    #[cfg(all(feature = "cortex", any(test, feature = "fixtures")))]
    #[doc(hidden)]
    pub fn sensing_last_expectation(&self) -> Option<Vec<u64>> {
        self._sensing
            .acquisition()
            .map(|acquisition| acquisition.schedule().last_expectation())
    }

    /// How many transaction steps ran WITHOUT the section guard this caller
    /// took — the decision and the record it commits each check.
    ///
    /// Zero is the property. It is independent of scheduling: a guard dropped
    /// early is caught by the next step whether or not anybody else was
    /// waiting, so a witness need not arrange an overlap to observe it.
    #[cfg(all(feature = "cortex", any(test, feature = "fixtures")))]
    #[doc(hidden)]
    pub fn sensing_section_unguarded_steps(&self) -> Option<u64> {
        self._sensing
            .acquisition()
            .map(|acquisition| acquisition.schedule().unguarded_steps())
    }

    /// Install a hook that fires INSIDE this binding's reconciliation section,
    /// after the lock and before the decision's convergence.
    ///
    /// It is how a witness holds the section open while other callers arrive,
    /// which is the only way to observe contention rather than infer it.
    #[cfg(all(feature = "cortex", any(test, feature = "fixtures")))]
    #[doc(hidden)]
    pub fn set_sensing_section_hook_for_test(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        if let Some(acquisition) = self._sensing.acquisition() {
            acquisition.schedule().set_in_section_hook(hook);
        }
    }

    /// The provider the LAST planned call selected, if any.
    ///
    /// Recorded by the call path in instrumented builds so a witness can
    /// observe the actual selection — including when the send then fails and
    /// no reply names anyone.
    #[cfg(all(feature = "cortex", any(test, feature = "fixtures")))]
    #[doc(hidden)]
    pub fn last_selected_provider(&self) -> Option<net::adapter::net::identity::EntityId> {
        self.selected.lock().clone()
    }

    /// One capability's readiness projection at a caller-supplied instant.
    ///
    /// The instant is the point: freshness is request-relative, so a witness
    /// proves aging by ASKING at a later instant rather than by sleeping.
    #[cfg(all(feature = "cortex", any(test, feature = "fixtures")))]
    #[doc(hidden)]
    pub fn sensing_projection(
        &self,
        capability: &CapabilityAuthorityId,
        at: Instant,
        budget: &net::adapter::net::behavior::sensing::ConsumerLatencyBudget,
    ) -> Option<net::adapter::net::behavior::org_sensing_demand::OrgSensedProjection> {
        let demand = self._sensing.acquisition()?.family().demand(capability)?;
        Some(demand.project_sensed_order(at, budget))
    }

    /// The membership certificate this client calls under.
    pub fn membership(&self) -> &OrgMembershipCert {
        &self.membership
    }

    /// The dispatcher grant this client calls under.
    pub fn dispatcher(&self) -> &OrgDispatcherGrant {
        &self.dispatcher
    }

    /// Stage-3 of the validity contract: are the credentials that back EVERY
    /// call currently within their validity windows?
    ///
    /// `call` performs this itself (plus the selected grant), so this is for
    /// callers that want to check before committing to work — a long-lived
    /// client crosses expiry, and a bound client is not a permanently valid
    /// one. Uses the installed authority's skew tolerance, so it agrees with
    /// the provider's own window arithmetic.
    pub fn check_current(&self) -> Result<(), OrgCredentialError> {
        // `current_timestamp` is the canonical wall clock the credential family
        // uses; taking our own `SystemTime` here would be a second clock that
        // could disagree with the one the windows were minted against. A call
        // arriving through `plan` uses its capture's instant instead — see
        // [`Self::check_current_at`].
        self.membership
            .is_valid_with_skew(self.skew_secs)
            .map_err(|source| OrgCredentialError::NotCurrentlyValid {
                credential: "membership".to_string(),
                source,
            })?;
        self.dispatcher
            .is_valid_with_skew(self.skew_secs)
            .map_err(|source| OrgCredentialError::NotCurrentlyValid {
                credential: "dispatcher grant".to_string(),
                source,
            })?;
        Ok(())
    }

    /// [`Self::check_current`] at the cold plan's CAPTURED instant
    /// (OLB-2B.3d-pre).
    ///
    /// Every call goes through here rather than through the sampling twin: two
    /// independent clock samples let a plan pass the membership window at one
    /// instant and the dispatcher window at another, so a credential set that
    /// was never simultaneously valid could authorize a call. One captured
    /// instant makes that unrepresentable, and it is the SAME instant the grant
    /// windows, the discovery filters and the expiry checks use.
    // Its only consumers are the call verb's two currentness checks in
    // `org/call.rs`, and that module is `#[cfg(feature = "cortex")]`
    // (`org.rs`). `net-aggregator-daemon` is the one workspace build that
    // takes this SDK without `cortex`, and `ffi-clippy` lints it at
    // `-D warnings`, so there the method is genuinely dead. Same shape as
    // the `node` field above and `hex_capability` in `org/error.rs`.
    #[cfg_attr(not(feature = "cortex"), allow(dead_code))]
    pub(crate) fn check_current_at(&self, now_secs: u64) -> Result<(), OrgCredentialError> {
        self.membership
            .is_valid_at_with_skew(now_secs, self.skew_secs)
            .map_err(|source| OrgCredentialError::NotCurrentlyValid {
                credential: "membership".to_string(),
                source,
            })?;
        self.dispatcher
            .is_valid_at_with_skew(now_secs, self.skew_secs)
            .map_err(|source| OrgCredentialError::NotCurrentlyValid {
                credential: "dispatcher grant".to_string(),
                source,
            })?;
        Ok(())
    }
}

impl std::fmt::Debug for OrgClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrgClient")
            .field("acting_org", &self.acting_org)
            .field("caller", self.caller.entity_id())
            .field("grants", &self.grants.len())
            .finish()
    }
}

impl OrgClient {
    /// Bind a credential set to a NODE — the one implementation of the bind
    /// pipeline (OSDK-L N).
    ///
    /// [`Mesh::org`] delegates here, and so does every language binding: the
    /// Node, Python, and Go/C surfaces hold `Arc<MeshNode>` rather than an SDK
    /// [`Mesh`], and neither fabricating a throwaway `Mesh` per bind nor
    /// pinning a permanent one is acceptable — the first makes
    /// `Mesh::from_node_arc` an accidental binding adapter, the second adds a
    /// permanent `Arc<MeshNode>` that blocks shutdown.
    ///
    /// `#[doc(hidden)]` because applications should use [`Mesh::org`]; this is
    /// the binding seam, not a second public way to do the same thing. There is
    /// exactly one authority pipeline and both doors reach it.
    ///
    /// Refuses unless the complete private-discovery identity relation holds:
    ///
    /// 1. the node's identity was EXPLICITLY configured — org membership binds
    ///    to a durable cryptographic entity, never a generated ephemeral
    ///    keypair whose entity id changes on restart;
    /// 2. a node authority is installed — consumer-audience installation and
    ///    owner-private discovery both require it, so binding without one would
    ///    search private state that can never exist;
    /// 3. that authority's owner org is the membership's org;
    /// 4. the membership vouches for THIS node's entity (the provider's TOFU
    ///    member binding would refuse otherwise — fail before signing).
    ///
    /// Then each DISCOVER grant's audience is leased into the node's consumer
    /// registry. A grant that cannot currently be installed (expired, no
    /// discovery binding, conflicting, registry full) fails the bind loudly
    /// rather than leaving a client that silently discovers nothing.
    ///
    /// The lease is released when the last clone of the returned client drops.
    #[doc(hidden)]
    pub fn bind_node(
        node: Arc<MeshNode>,
        credentials: OrgCredentials,
    ) -> Result<Self, OrgSdkError> {
        // Node metadata, not an authority decision — but the facade's contract
        // is that org credentials bind to a durable entity, and a generated
        // fallback identity is not one.
        if !node.has_configured_identity() {
            return Err(OrgCredentialError::PersistentIdentityRequired.into());
        }
        let authority = node
            .node_authority()
            .ok_or(OrgCredentialError::NodeAuthorityRequired)?;

        let authority_org = authority.owner_org();
        if authority_org != credentials.acting_org() {
            return Err(OrgCredentialError::NodeAuthorityOrgMismatch {
                authority_org,
                membership_org: credentials.acting_org(),
            }
            .into());
        }
        if credentials.member() != node.entity_id() {
            return Err(OrgCredentialError::MemberBindingMismatch {
                expected: node.entity_id().clone(),
                credential: credentials.member().clone(),
            }
            .into());
        }

        let acting_org = credentials.acting_org();
        let (membership, dispatcher, grants, secrets) = credentials.into_parts();

        // Pair each secret with its grant. Construction proved every secret
        // matches exactly one held grant, so this loses nothing; a grant with no
        // secret (INVOKE-only, or DISCOVER whose key was not supplied) simply
        // installs no audience.
        let mut pairs = Vec::with_capacity(secrets.len());
        for secret in secrets {
            let Some(grant) = grants.iter().find(|g| secret.matches_grant(g)) else {
                // Unreachable: `OrgCredentials::new` rejects an unmatched secret.
                continue;
            };
            pairs.push((grant.clone(), secret));
        }

        // The lease registry lives on the NODE, so every wrapper over this node
        // shares one refcount per grant id.
        let grant_ids = node
            .acquire_consumer_audience_leases(pairs)
            .map_err(|(id, source)| OrgCredentialError::AudienceInstallRefused {
                grant_id: hex32(&id),
                source,
            })?;

        // SENSING ACQUISITION, once per bind. Mapped, never propagated: a
        // refused mint is an inert binding, not a failed bind, and the error
        // type of this function is unchanged. Recorded once, here - never per
        // call - because the mint does not happen again on this client.
        #[cfg(feature = "cortex")]
        let _sensing = match OrgSensingFamily::mint(&node) {
            Ok(family) => OrgSensingBinding::Active(OrgSensingAcquisition::new(family)),
            Err(refusal) => {
                // `eprintln!` because this crate takes no logging dependency
                // (the `compute` verbs do the same for their one operator
                // warning). Once per BIND, never per call: the mint does not
                // happen again on this client, so there is nothing to rate
                // limit. It names the typed refusal so an operator can tell
                // "no authority" from "family space exhausted".
                eprintln!(
                    "WARN: org sensing family unavailable ({refusal:?}); \
                     this binding plans unsensed"
                );
                OrgSensingBinding::Inert
            }
        };

        Ok(OrgClient {
            caller: node.entity_keypair_arc(),
            membership,
            dispatcher,
            grants,
            acting_org,
            skew_secs: authority.config.verification_skew_secs,
            _lease: Arc::new(AudienceLeaseGuard::new(node.clone(), grant_ids)),
            #[cfg(all(feature = "cortex", any(test, feature = "fixtures")))]
            selected: Arc::new(parking_lot::Mutex::new(None)),
            #[cfg(feature = "cortex")]
            _sensing,
            node,
        })
    }
}

impl Mesh {
    /// Bind an organization credential set to this mesh (OSDK §1).
    ///
    /// Thin delegation to [`OrgClient::bind_node`], which documents the
    /// complete relation this refuses on. One pipeline, two doors.
    pub fn org(&self, credentials: OrgCredentials) -> Result<OrgClient, OrgSdkError> {
        OrgClient::bind_node(self.node().clone(), credentials)
    }
}
