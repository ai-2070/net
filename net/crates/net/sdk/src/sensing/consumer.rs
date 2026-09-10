//! Capability sensing — the **consumer** side, own-organization
//! exact-provider only.
//!
//! `docs/internal/plans/CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md` §4.3
//! (consumer surface). An ordinary application asks one question here —
//! "which of the providers this node is currently authorized to see can
//! start capability Y within the bound I name, inside my own end-to-end
//! latency budget?" — and the answer is [`SensingSnapshot`]:
//! request-relative, advisory, plain data.
//!
//! # Two bounds, two parties
//!
//! The question has two halves, and both are the caller's to state:
//!
//! - [`SensingQuery::start_within`] is the PROVIDER-EVALUATED predicate.
//!   It rides in the signed interest and reaches the provider's
//!   evaluator as `EvaluationRequest::work_latency`, so a provider that
//!   cannot start inside it answers `NotReady` — and no local budget can
//!   overturn an answer to that bound. It defaults to
//!   [`DEFAULT_PROVIDER_START_WITHIN`], the same fixed policy the
//!   organization call path's retained demand asks, so a default watch
//!   shares that path's interest rather than forking a second digest;
//! - [`SensingQuery::within`] is the CONSUMER-local end-to-end budget.
//!   It never rides the wire and is applied to this node's own route
//!   economics plus the provider's signed start estimate.
//!
//! Anything a provider must be told is therefore said explicitly; a
//! caller is never silently observing a different question.
//!
//! # Current authorization, on every read
//!
//! Every [`SensingWatch::snapshot`] requalifies before it reports, at
//! that instant and off every pacing floor:
//!
//! - THIS node's own membership must be live — installed, readable,
//!   unexpired, and at or above the current revocation floor. A stamp
//!   being unchanged is publication identity, not membership, so a
//!   revoked observer is refused with
//!   [`SensingError::ObserverNotQualified`] while its leases and its
//!   recovery path stay intact;
//! - the reported rows are CLAMPED to what verified owner-private
//!   discovery says right now. A provider whose announcement expired,
//!   whose certificate clamped its lifetime, or whose pin went away is
//!   absent from the next snapshot immediately — lease convergence for
//!   ADDITIONS remains paced, so the reported set is always a subset of
//!   current visibility and never a superset.
//!
//! The qualification is a CAPTURE: it holds at the instant it is taken,
//! and the projection that follows describes that capture. Authority or
//! discovery movement concurrent with a single call may therefore
//! appear on the NEXT read rather than inside this one — the ordinary
//! property of an observation, not a stale-authorization window: no
//! read reuses a qualification from an earlier call, and a refusal
//! never falls back to a previously reported population.
//!
//! # What this owns, and what it reuses
//!
//! Nothing about the observation is invented here. A [`SensingWatch`] is
//! one owner of the core's retained organization exact-provider demand
//! (`OrgSensingFamily`, the same substrate `OrgClient`'s call path binds):
//!
//! - the AUDIENCE comes from this node's own installed organization
//!   authority. There is no audience argument, so a caller cannot
//!   fabricate one, and a node with no live authority is refused with
//!   [`SensingError::NoOrganizationAuthority`];
//! - the POPULATION comes from verified owner-private discovery
//!   intersected with this node's own entity pins, bounded by the core's
//!   sensed-population cap ([`MAX_SENSED_POPULATION`]). A raw node id, a
//!   grant-plane-only provider, or a caller-supplied list cannot enter
//!   it: this surface has no way to name a provider at all;
//! - the CADENCE and its `ttl/2` renewal are the node's own — one
//!   acquisition arms one refresh record on the node's single refresh
//!   worker, so a watch keeps observing between application calls
//!   without this module owning a background task or a permanent timer
//!   per watch (a parked [`SensingWatch::changed`] does arm one
//!   `sleep_until` for the duration of that park);
//! - the PROJECTION is `OrgSensingCapabilityDemand::project_sensed_order`,
//!   the same classifier the call path consults, and the row economics
//!   this module reports are the values that classification used. This
//!   module adds no second ranking rule and samples no plane itself: it
//!   maps the core's three buckets onto one [`SensedViability`] per row.
//!
//! # What a snapshot is not
//!
//! Not a reservation, not admission, not authority, and not a freshness
//! claim. `Ready` holds no capacity and authorizes no invocation; a
//! protected call still constructs its own proof and is still admitted
//! or refused by the provider. Nothing here exposes an evidence age.
//! `Unknown` is the answer for missing, unretained, withdrawn and
//! expired evidence alike, and it retains potential capacity — it is
//! never a permanent verdict and never a narrowing of who may be called.
//!
//! # The consumer loop
//!
//! The watched name is the capability id its PROVIDERS declare, and a
//! provider only enters the population once this node has verified
//! owner-private discovery for it and pinned its entity — for an
//! organization service that means the provider served
//! `Mesh::serve_org("infer", ..)` and the watched tag is
//! `nrpc:infer`. `provide` alone makes a provider ready, not
//! discoverable.
//!
//! ```no_run
//! use std::time::Duration;
//!
//! use net_sdk::sensing::{SensedViability, SensingQuery};
//!
//! # async fn example(mesh: &net_sdk::mesh::Mesh) -> Result<(), Box<dyn std::error::Error>> {
//! let mut watch = mesh.sensing()?.watch(
//!     SensingQuery::new("nrpc:infer")
//!         // What the PROVIDER is asked to answer.
//!         .start_within(Duration::from_millis(500))
//!         // What THIS consumer will accept end to end.
//!         .within(Duration::from_millis(800)),
//! )?;
//!
//! loop {
//!     // Read coherent current state. A wake is never the value.
//!     let snapshot = watch.snapshot()?;
//!     if let Some(best) = snapshot.preferred() {
//!         println!("try {best:#x} first");
//!     }
//!     for provider in snapshot.providers() {
//!         if provider.viability() == SensedViability::NotViable {
//!             println!("{:#x} says not now", provider.node_id());
//!         }
//!     }
//!
//!     // Park until something that can change the answer moved.
//!     watch.changed().await?;
//! }
//! # }
//! ```
//!
//! # Unsupported forms are refused, not faked
//!
//! [`SensingQuery`] can express exactly what this slice implements: one
//! capability name, one provider-start bound, and one optional
//! end-to-end budget. There is deliberately no provider selector,
//! tag/group predicate, constraint map, result mode, disclosure class or
//! audience on it — a leader-backed provider-free (`AnyAuthorized`)
//! path, cross-organization sensing, `Granted` discovery and warmed
//! pools are not implemented, so this surface declines to name them
//! rather than accepting an argument it would silently ignore. Canonical
//! constraints are fixed EMPTY for the same reason: a constraint map is
//! provider-evaluated and digest-bound, and nothing here can honestly
//! populate one yet. The forms a caller can still get wrong are refused
//! loudly: a blank capability name
//! ([`SensingError::EmptyCapability`]), a zero end-to-end budget
//! ([`SensingError::UnsatisfiableBudget`]) and a zero provider-start
//! bound ([`SensingError::UnsatisfiableStartBound`]).

use std::sync::Arc;
use std::time::{Duration, Instant};

use net::adapter::net::behavior::org_grant::CapabilityAuthorityId;
use net::adapter::net::behavior::org_sensing_demand;
use net::adapter::net::behavior::org_sensing_demand::{
    OrgSensedProjection, OrgSensingCapabilityDemand, OrgSensingDemandRefused, OrgSensingFamily,
};
use net::adapter::net::behavior::sensing;
use net::adapter::net::MeshNode;

use super::{SensingClient, SensingError};

/// The consumer's three-state readiness surface, straight from the core
/// projection table. Re-exported rather than mirrored: a second enum
/// beside it would be a mapping that can drift.
pub use sensing::ProjectedReadiness;

/// The most providers ONE watch ever reports on.
///
/// The core's own bound on a retained demand's population. A consumer
/// authorized to see more providers than this observes the canonical
/// lowest-id prefix of them; the rest are not sensed and are therefore
/// simply absent from the snapshot, never reported as not ready.
pub use net::adapter::net::behavior::org_sensing_demand::MAX_SENSED_POPULATION;

/// How often a watch re-derives its authorized population.
///
/// The readiness of a KNOWN provider moves through the node's own change
/// signal, so it needs no polling. Who the authorized providers ARE is a
/// different fact: it comes from verified discovery and this node's pin
/// map, and neither of those bumps the sensing change signal. So a watch
/// re-derives the population on the application's own
/// [`SensingWatch::snapshot`] call, paced by this floor, and
/// [`SensingWatch::changed`] also returns once the floor has elapsed so
/// a quiet network cannot hide a population change from a parked
/// consumer.
///
/// A DEGRADED demand — moved sensing authority, or a holder that stopped
/// being one — bypasses the floor entirely: no floor may pace a
/// re-derivation that current state has already invalidated.
///
/// Public because it is the one timing fact a consumer can observe: how
/// stale a snapshot's POPULATION may be, and the upper bound on a park.
/// Fixed policy, not configuration — a per-watch value would let one
/// watcher pace another's node-shared demand.
pub const POPULATION_RECONCILE_FLOOR: Duration = Duration::from_secs(1);

/// One bounded readiness question, with BOTH of its bounds explicit.
///
/// The two are different questions, asked of different parties, and the
/// module docs' "can this provider satisfy Y inside my budget" is only
/// answerable because both are here:
///
/// * [`Self::start_within`] is the PROVIDER-EVALUATED predicate. It is
///   part of the signed interest, reaches the provider's evaluator as
///   `EvaluationRequest::work_latency`, and a provider that cannot start
///   inside it legitimately attests `NotReady`. No local budget can undo
///   that answer, because it is an answer to this bound;
/// * [`Self::within`] is the CONSUMER-local end-to-end budget. It never
///   rides the wire and is applied to this node's own route economics
///   plus the provider's signed start estimate.
///
/// Cheap plain data — build it inline at the call site. See the module
/// docs for what this deliberately cannot express.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SensingQuery {
    capability: String,
    budget: sensing::ConsumerLatencyBudget,
    work_latency: sensing::WorkLatencyEnvelope,
}

impl SensingQuery {
    /// Observe readiness for one capability, asking the DEFAULT
    /// provider-start predicate ([`DEFAULT_PROVIDER_START_WITHIN`]) —
    /// the same fixed policy the organization call path's own retained
    /// demand asks, so a watch and that path share one interest.
    ///
    /// `capability` is the SAME id the provider passes to
    /// [`SensingClient::provide`](super::SensingClient::provide) — the
    /// interest digest binds it verbatim, so a differing name observes a
    /// different capability rather than a differently-spelled one.
    pub fn new(capability: impl Into<String>) -> Self {
        Self {
            capability: capability.into(),
            budget: sensing::ConsumerLatencyBudget::default(),
            work_latency: org_sensing_demand::fixed_work_latency(),
        }
    }

    /// Ask the provider whether it can START the work within `bound`.
    ///
    /// This is the predicate the provider actually evaluates and signs,
    /// so it CHANGES THE QUESTION: two watches asking different bounds
    /// hold independent interests on the same capability and provider,
    /// and each provider answers each bound on its own merits. Raise it
    /// when a provider's honest `NotReady` is only about the default
    /// two-second bound rather than about your request.
    ///
    /// The evaluator sees the bound verbatim in
    /// `EvaluationRequest::work_latency.provider_start_within`.
    pub fn start_within(mut self, bound: Duration) -> Self {
        self.work_latency = sensing::WorkLatencyEnvelope::start_within(bound);
        self
    }

    /// Judge viability against this consumer's own end-to-end bound.
    ///
    /// Local by definition and per REQUEST: the budget is never in the
    /// interest digest, never on the wire and never provider-signed, so
    /// two consumers legitimately derive different viability from one
    /// signed attestation. A provider whose proof does not fit is
    /// DEMOTED to [`SensedViability::Potential`], never pruned — a route
    /// change or a fresh beat can make it viable again.
    ///
    /// Left unset, viability asks only whether readiness projects
    /// `Ready`; no path cost can then disqualify a provider. It does NOT
    /// widen the provider-start predicate — use [`Self::start_within`]
    /// for that.
    pub fn within(mut self, end_to_end: Duration) -> Self {
        self.budget.end_to_end_within = Some(end_to_end);
        self
    }

    /// The capability this query observes.
    pub fn capability(&self) -> &str {
        &self.capability
    }

    /// The end-to-end bound, if one was set.
    pub fn budget(&self) -> Option<Duration> {
        self.budget.end_to_end_within
    }

    /// The provider-start bound this query asks the provider.
    pub fn provider_start_within(&self) -> Option<Duration> {
        self.work_latency.provider_start_within
    }

    /// Refuse the shapes a caller can still get wrong.
    fn validate(&self) -> Result<(), SensingError> {
        if self.capability.trim().is_empty() {
            return Err(SensingError::EmptyCapability);
        }
        if self.budget.end_to_end_within == Some(Duration::ZERO) {
            return Err(SensingError::UnsatisfiableBudget);
        }
        if self.work_latency.provider_start_within == Some(Duration::ZERO) {
            return Err(SensingError::UnsatisfiableStartBound);
        }
        Ok(())
    }
}

/// The provider-start bound [`SensingQuery::new`] asks: the fixed
/// internal policy the organization call path's retained demand uses, so
/// a default watch shares that path's interest instead of forking a
/// second digest for the same question.
pub const DEFAULT_PROVIDER_START_WITHIN: Duration =
    match org_sensing_demand::fixed_work_latency().provider_start_within {
        Some(bound) => bound,
        None => Duration::ZERO,
    };

/// How one provider's readiness relates to THIS request's budget.
///
/// A partition of the snapshot's population: every reported provider is
/// in exactly one class, and the class is about this request only.
/// Nothing here changes discovery, membership or admission.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SensedViability {
    /// `Ready` over established continuity, and inside this request's
    /// budget. The snapshot's rank order is over exactly these.
    Viable,
    /// No viability verdict: `Unknown` evidence, or a `Ready` proof that
    /// does not fit this request's budget. NEVER a pruning — absence of
    /// evidence is not evidence of absence, and the provider stays a
    /// legitimate call target.
    Potential,
    /// Sensed explicitly NOT ready for this interest at the captured
    /// instant. Ordered last, never removed.
    NotViable,
}

/// One authorized provider's row in a snapshot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SensedProvider {
    node_id: u64,
    readiness: ProjectedReadiness,
    viability: SensedViability,
    estimated_start: Option<Duration>,
    route_estimate: Option<Duration>,
}

impl SensedProvider {
    /// The provider's mesh node id.
    pub fn node_id(&self) -> u64 {
        self.node_id
    }

    /// Its projected readiness at the captured instant.
    pub fn readiness(&self) -> ProjectedReadiness {
        self.readiness
    }

    /// Its class for this request's budget.
    pub fn viability(&self) -> SensedViability {
        self.viability
    }

    /// The PROVIDER-SIGNED time-to-start estimate that backed the
    /// projection, if it published one.
    ///
    /// `None` whenever [`Self::readiness`] is `Unknown`: an estimate
    /// whose evidence no longer vouches for it is stale metadata, and
    /// reporting it beside `Unknown` would dress it up as current.
    pub fn estimated_start(&self) -> Option<Duration> {
        self.estimated_start
    }

    /// This CONSUMER's own current route estimate toward the provider —
    /// local path economics, not a provider claim, and deliberately a
    /// separate field from [`Self::estimated_start`].
    ///
    /// `None` means the proximity plane knows nothing about this
    /// provider (no measured edge and no known path); viability then
    /// charges the plane's own conservative unknown-route estimate. A
    /// measured estimate that coincides exactly with that conservative
    /// value reads as `None` too — the plane's "nothing known" answer is
    /// the same duration, and reporting the sentinel as a measurement
    /// would be the worse lie.
    pub fn route_estimate(&self) -> Option<Duration> {
        self.route_estimate
    }
}

/// ONE request-relative capture of an authorized population's readiness.
///
/// Immutable plain data, and internally coherent: the rows, the rank
/// order and the preference are folds of one capture, so they cannot
/// disagree with each other. They describe the instant the capture was
/// taken — provider churn afterwards is the next snapshot's business.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SensingSnapshot {
    capability: String,
    providers: Vec<SensedProvider>,
    ranked: Vec<u64>,
}

impl SensingSnapshot {
    /// The capability this capture describes.
    pub fn capability(&self) -> &str {
        &self.capability
    }

    /// Every authorized provider this watch senses, one row each, in the
    /// population's own ascending-id order.
    ///
    /// Empty when this node currently authorizes none — a legitimate
    /// answer, not an error.
    pub fn providers(&self) -> &[SensedProvider] {
        &self.providers
    }

    /// The [`SensedViability::Viable`] providers in SENSED RANK order —
    /// consumer-local route economics first, node id as the
    /// deterministic tie-break.
    pub fn ranked(&self) -> &[u64] {
        &self.ranked
    }

    /// The provider to try first, or `None` when nothing is currently
    /// viable — in which case the caller's own ordering stands. Sensing
    /// never leaves a caller with no candidates.
    pub fn preferred(&self) -> Option<u64> {
        self.ranked.first().copied()
    }

    /// One provider's row, if it is in this capture.
    pub fn provider(&self, node_id: u64) -> Option<&SensedProvider> {
        self.providers.iter().find(|row| row.node_id == node_id)
    }
}

/// An owning observation of one capability's authorized exact-provider
/// readiness.
///
/// # Ownership
///
/// A watch owns its own demand, and demand ownership is NODE-GLOBAL
/// underneath it: two watches over one node — including ones built from
/// separately constructed [`Mesh`](crate::mesh::Mesh) wrappers sharing
/// one node — hold their own leases on the same interest rows, so
/// closing one never deregisters the other's observation, and the node's
/// refresh record survives until the LAST owner releases. A closed watch
/// is inert: its `close` and drop remove nothing further, and
/// [`Self::snapshot`] / [`Self::changed`] refuse with
/// [`SensingError::WatchClosed`] rather than reporting stale state.
///
/// Not `Clone`: the change cursor ([`Self::changed`]) is per-watch state,
/// and two owners of one cursor would make "who has seen what" ambiguous.
/// Open a second watch instead — that is the shared-ownership case above.
pub struct SensingWatch {
    node: Arc<MeshNode>,
    /// THIS watch's demand ownership root. Its body's `Drop` retires
    /// whatever the watch still holds, so a forgotten `close` leaks
    /// nothing.
    family: OrgSensingFamily,
    capability: String,
    authority: CapabilityAuthorityId,
    budget: sensing::ConsumerLatencyBudget,
    /// The node's own change generation. Held for the watch's lifetime,
    /// and SUBSCRIBED before the first retention, so a change that lands
    /// while the watch is being established is not lost.
    changes: tokio::sync::watch::Receiver<u64>,
    /// When this watch last completed a convergence attempt, which is
    /// what [`POPULATION_RECONCILE_FLOOR`] paces.
    converged_at: Instant,
    /// The next floor wake [`Self::changed`] will return at, so a parked
    /// consumer cannot miss a population change in a quiet network — and
    /// cannot spin, because the timer is re-armed each time it fires.
    floor_wake: Instant,
    closed: bool,
    /// Fixtures-only witness seam: see [`SensingWatch::set_capture_seam_for_test`].
    #[cfg(any(test, feature = "fixtures"))]
    capture_seam: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Fixtures-only: how many convergence ATTEMPTS this watch has made
    /// ([`SensingWatch::convergences_for_test`]).
    #[cfg(any(test, feature = "fixtures"))]
    convergences: u64,
    /// Fixtures-only: suspend the paced re-derivation
    /// ([`SensingWatch::suspend_convergence_for_test`]).
    #[cfg(any(test, feature = "fixtures"))]
    convergence_suspended: bool,
    /// Fixtures-only override of [`POPULATION_RECONCILE_FLOOR`]
    /// ([`SensingWatch::set_population_floor_for_test`]).
    #[cfg(any(test, feature = "fixtures"))]
    floor_override: Option<Duration>,
}

impl SensingClient {
    /// Observe the readiness of this node's authorized own-organization
    /// providers of one capability.
    ///
    /// The returned [`SensingWatch`] is established EAGERLY: the
    /// authority capture, the population derivation and the exact-provider
    /// acquisitions all happen before this returns, so a watch that comes
    /// back is one whose demand is really retained — never a handle that
    /// will quietly report nothing forever.
    ///
    /// Refuses with:
    ///
    /// - [`SensingError::EmptyCapability`],
    ///   [`SensingError::UnsatisfiableBudget`] or
    ///   [`SensingError::UnsatisfiableStartBound`] for a query that
    ///   cannot mean anything;
    /// - [`SensingError::ObserverNotQualified`] when THIS node is not
    ///   currently entitled to observe: no installed organization
    ///   authority, a poisoned or exhausted revocation store, or its own
    ///   membership certificate expired or revoked below the current
    ///   floor. Checked before anything is acquired, and re-checked on
    ///   every [`SensingWatch::snapshot`];
    /// - [`SensingError::NoOrganizationAuthority`] when the retention
    ///   itself could not derive an audience — the captured view kept
    ///   moving underneath the attempt. There is no legacy fallback and
    ///   no caller-supplied audience;
    /// - [`SensingError::ObservationIdentityUnavailable`] when the node
    ///   can no longer mint a demand-ownership identity;
    /// - [`SensingError::WatchesAtCapacity`] when a watch's own demand
    ///   root is full. Nothing is evicted to make room.
    ///
    /// What is NOT a refusal: an authorized provider whose acquisition
    /// the node declined (its interest capacity is full, say). That
    /// provider stays in the population and reads `Unknown` — potential
    /// capacity, not a verdict — and a later `snapshot` past the
    /// population floor acquires it once the refusal clears.
    pub fn watch(&self, query: SensingQuery) -> Result<SensingWatch, SensingError> {
        query.validate()?;
        let authority = CapabilityAuthorityId::for_tag(query.capability());
        // QUALIFY THE OBSERVER FIRST. A revoked or expired local
        // membership must not acquire anything, and must not be told
        // "no authority installed" when an authority is installed and it
        // is the caller who no longer qualifies under it.
        if self
            .node
            .org_sensing_current_visibility(&authority)
            .is_none()
        {
            return Err(SensingError::ObserverNotQualified);
        }
        // SUBSCRIBE next. Everything below can move the generation —
        // an acquisition registers rows and can admit a beat — and a
        // cursor taken afterwards would start out believing it had seen
        // that movement.
        let changes = self.node.subscribe_sensing_overlay_changes();
        // The family is bound to THIS query's provider-start predicate,
        // so every registration it authors asks the question the caller
        // actually asked.
        let family = OrgSensingFamily::mint_asking(&self.node, query.work_latency)
            .map_err(retention_refusal)?;
        // Establish the demand now, so a refusal is THIS call's error
        // rather than a silently empty snapshot later. The container is
        // re-read per snapshot, so nothing is cached from it here.
        family
            .retain(query.capability())
            .map_err(retention_refusal)?;
        let now = Instant::now();
        Ok(SensingWatch {
            node: Arc::clone(&self.node),
            family,
            authority,
            capability: query.capability,
            budget: query.budget,
            changes,
            converged_at: now,
            floor_wake: now + POPULATION_RECONCILE_FLOOR,
            closed: false,
            #[cfg(any(test, feature = "fixtures"))]
            capture_seam: None,
            #[cfg(any(test, feature = "fixtures"))]
            convergences: 1,
            #[cfg(any(test, feature = "fixtures"))]
            convergence_suspended: false,
            #[cfg(any(test, feature = "fixtures"))]
            floor_override: None,
        })
    }
}

impl SensingWatch {
    /// The capability this watch observes.
    pub fn capability(&self) -> &str {
        &self.capability
    }

    /// Capture readiness NOW, request-relative.
    ///
    /// This is the read a wake sends you back to: it re-derives coherent
    /// current state rather than reporting whatever a notification
    /// implied. Concretely, one call:
    ///
    /// 1. marks the change cursor seen BEFORE any state is read, so a
    ///    change landing during the capture leaves the cursor unseen and
    ///    the next [`Self::changed`] returns immediately instead of
    ///    parking on a value this capture never saw;
    /// 2. REQUALIFIES the observer and re-derives what is currently
    ///    visible — this node's own live membership plus verified
    ///    owner-private discovery, at this instant and independent of
    ///    any pacing floor;
    /// 3. re-derives the retained population when that is due (see
    ///    [`POPULATION_RECONCILE_FLOOR`]) or when the installed demand is
    ///    degraded, carrying live holders forward untouched;
    /// 4. projects the retained population at one captured instant and
    ///    CLAMPS the result to what step 2 found currently visible.
    ///
    /// So a returned row is always both retained and currently visible:
    /// a provider whose announcement expired, whose certificate clamped
    /// its lifetime, or whose pin went away is gone from the next
    /// snapshot immediately, even though its lease release is paced.
    /// Acquisition of NEW providers stays paced, so the reported set is
    /// a subset of current visibility, never a superset.
    ///
    /// Refuses with [`SensingError::WatchClosed`] on a closed watch, and
    /// with [`SensingError::ObserverNotQualified`] when this node is no
    /// longer entitled to observe. The second refusal keeps the watch's
    /// leases and recovery state intact — a later snapshot succeeds once
    /// this node is qualified again: a readable, unexhausted revocation
    /// store and an installed authority whose view has not moved, as well
    /// as its own membership valid at or above the current floor — but it
    /// will not present historical authorization as a current answer. A
    /// convergence refused while a
    /// qualified demand is already installed is NOT an error: the
    /// refusal retains nothing and releases nothing, so the installed
    /// observation keeps serving, clamped to current visibility.
    pub fn snapshot(&mut self) -> Result<SensingSnapshot, SensingError> {
        if self.closed {
            return Err(SensingError::WatchClosed);
        }
        // Step 1 — see the module note above. `mark_unchanged` takes no
        // guard, so nothing is held across the capture below.
        self.changes.mark_unchanged();
        // WITNESS SEAM — exactly where a concurrent change lands: the cursor
        // is already marked, and no state has been read yet. A change fired
        // here must leave the cursor UNSEEN, so the next `changed` returns at
        // once instead of parking on a value this capture never saw. Marking
        // the cursor after the read instead is precisely what this catches.
        #[cfg(any(test, feature = "fixtures"))]
        if let Some(seam) = self.capture_seam.clone() {
            seam();
        }
        // Step 2 — the qualification gate, BEFORE any state is read and
        // independent of the convergence floor.
        let visible = self
            .node
            .org_sensing_current_visibility(&self.authority)
            .ok_or(SensingError::ObserverNotQualified)?;
        let demand = self.converge()?;
        let projection = demand.project_sensed_order(Instant::now(), &self.budget);
        Ok(self.assemble(&projection, &visible))
    }

    /// Park until something that can change the answer moved.
    ///
    /// Returns on the FIRST of:
    ///
    /// - the node's own sensing change generation moving. That covers
    ///   observation movement on the projected tuple, continuity EXPIRY
    ///   (so a provider going quiet is visible in an otherwise silent
    ///   network), failure-plane disruption, discrete route and topology
    ///   edges, and capability-fold membership;
    /// - the population re-derivation floor elapsing, so a change in WHO
    ///   is authorized — which no observation event announces — cannot
    ///   hide from a parked consumer.
    ///
    /// A wake is not proof that the answer changed, and never carries the
    /// value: call [`Self::snapshot`] and read the current state. Spurious
    /// wakes are permitted by construction; a MISSED one is not.
    ///
    /// A wake ACKNOWLEDGES the generation it observed, whatever the caller
    /// does next. A snapshot that then refuses (an unqualified observer,
    /// say) does not re-deliver that edge, so the retry is bounded by the
    /// population floor rather than by a second notification. This is the
    /// shipped contract, not an oversight: re-marking a cursor on every
    /// refusal would let a persistently refusing observer replay the same
    /// edge without pause.
    ///
    /// Refuses with [`SensingError::WatchClosed`] on a closed watch.
    pub async fn changed(&mut self) -> Result<(), SensingError> {
        if self.closed {
            return Err(SensingError::WatchClosed);
        }
        let floor = tokio::time::Instant::from_std(self.floor_wake);
        tokio::select! {
            // Biased: when both are ready, report the real change edge —
            // a floor wake is the fallback, not the headline.
            biased;
            moved = self.changes.changed() => {
                // The sender lives on the node this watch holds an `Arc`
                // to, so a closed channel means the node itself is gone.
                moved.map_err(|_| SensingError::WatchClosed)?;
            }
            _ = tokio::time::sleep_until(floor) => {
                // RE-ARM. Without this a consumer that wakes and does not
                // snapshot would find the floor permanently elapsed and
                // spin instead of parking.
                self.floor_wake = Instant::now() + self.floor();
            }
        }
        Ok(())
    }

    /// Stop observing.
    ///
    /// Returns whether THIS call retired the watch — so `true` at most
    /// once, and `false` for a repeat close.
    /// Releases exactly this watch's own leases: an interest another
    /// owner still holds keeps its row, its cadence and its refresh
    /// record, and only the last owner's release deregisters it.
    ///
    /// Drop does the same thing, so a forgotten close leaks nothing.
    pub fn close(&mut self) -> bool {
        if self.closed {
            return false;
        }
        self.closed = true;
        self.family.retire(&self.capability);
        true
    }

    /// Whether this watch has been closed.
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Unstable fixtures-only witness seam; not supported API.
    ///
    /// `hook` runs inside [`Self::snapshot`] at the one instant a
    /// concurrent change can be swallowed: after the change cursor has
    /// been marked and before any observation state is read. A witness
    /// makes a REAL change there — a local interest removal, say — and
    /// then requires [`Self::changed`] to return without waiting for the
    /// population floor. That is not observable from outside: a change
    /// fired before `snapshot` or after it returns is not lost by either
    /// ordering, so only a seam at this point discriminates them.
    #[cfg(any(test, feature = "fixtures"))]
    #[doc(hidden)]
    pub fn set_capture_seam_for_test(&mut self, hook: Arc<dyn Fn() + Send + Sync>) {
        self.capture_seam = Some(hook);
    }

    /// Unstable fixtures-only witness seam; not supported API.
    ///
    /// How many convergence ATTEMPTS this watch has made, counting the
    /// eager one at `watch()`. A witness uses it to attribute a result
    /// to the right mechanism: a row that disappeared with this count
    /// unchanged was removed by the per-read visibility clamp, not by a
    /// paced re-derivation, and a recovery that bumps it inside the old
    /// success floor proves the floor was bypassed rather than waited
    /// out.
    #[cfg(any(test, feature = "fixtures"))]
    #[doc(hidden)]
    pub fn convergences_for_test(&self) -> u64 {
        self.convergences
    }

    /// Unstable fixtures-only witness seam; not supported API.
    ///
    /// The providers this watch's installed demand actually RETAINS a lease
    /// for — a subset of the population, since a refused acquisition leaves
    /// its member unretained. A witness distinguishing "a retained row was
    /// removed" from "an unretained member was never there" needs this.
    #[cfg(any(test, feature = "fixtures"))]
    #[doc(hidden)]
    pub fn retained_providers_for_test(&self) -> Vec<u64> {
        self.family
            .demand(&self.authority)
            .map(|demand| demand.retained_providers())
            .unwrap_or_default()
    }

    /// Unstable fixtures-only witness seam; not supported API.
    ///
    /// SUSPEND the paced re-derivation: while suspended, a snapshot with
    /// a demand installed never calls `retain`. It exists so a
    /// visibility witness cannot be satisfied by a convergence that
    /// happened to be due — with re-derivation off, only the clamp can
    /// remove a row.
    #[cfg(any(test, feature = "fixtures"))]
    #[doc(hidden)]
    pub fn suspend_convergence_for_test(&mut self, suspended: bool) {
        self.convergence_suspended = suspended;
    }

    /// Unstable fixtures-only witness seam; not supported API.
    ///
    /// When a parked [`Self::changed`] would return on the population
    /// FALLBACK, so a wake witness can state its own margin instead of
    /// assuming one.
    #[cfg(any(test, feature = "fixtures"))]
    #[doc(hidden)]
    pub fn fallback_deadline_for_test(&self) -> Instant {
        self.floor_wake
    }

    /// The pacing floor in force. [`POPULATION_RECONCILE_FLOOR`] in
    /// production; a witness may widen it so a schedule-qualified claim
    /// does not rest on a short wall-clock interval.
    fn floor(&self) -> Duration {
        #[cfg(any(test, feature = "fixtures"))]
        if let Some(floor) = self.floor_override {
            return floor;
        }
        POPULATION_RECONCILE_FLOOR
    }

    /// Unstable fixtures-only witness seam; not supported API.
    ///
    /// Widen (or narrow) this watch's population re-derivation floor and
    /// re-arm its fallback from now. A recovery witness uses a long floor so
    /// "the previous success floor is still unexpired" is a robust statement
    /// about the SCHEDULE rather than a race against however long a signed
    /// on-disk ceremony takes.
    #[cfg(any(test, feature = "fixtures"))]
    #[doc(hidden)]
    pub fn set_population_floor_for_test(&mut self, floor: Duration) {
        self.floor_override = Some(floor);
        self.floor_wake = self.converged_at + floor;
    }

    /// The demand to project, re-deriving the authorized population when
    /// that is due or when the installed one can no longer be trusted.
    fn converge(&mut self) -> Result<Arc<OrgSensingCapabilityDemand>, SensingError> {
        let installed = self.family.demand(&self.authority);
        let due = match &installed {
            // Nothing installed: only a convergence can produce one.
            None => true,
            // A moved authority or a holder that stopped being one is
            // state the floor may not pace — the observation is already
            // not what it claims to be.
            Some(demand) => {
                !demand.authority_is_current()
                    || !demand.holders_are_live()
                    || self.converged_at.elapsed() >= self.floor()
            }
        };
        // Fixtures-only: a suspended witness keeps whatever is installed,
        // so nothing but the visibility clamp can change its rows.
        #[cfg(any(test, feature = "fixtures"))]
        let due = due && !self.convergence_suspended;
        if !due {
            if let Some(demand) = installed {
                return Ok(demand);
            }
        }
        #[cfg(any(test, feature = "fixtures"))]
        {
            self.convergences += 1;
        }
        match self.family.retain(&self.capability) {
            Ok(demand) => {
                self.converged_at = Instant::now();
                self.floor_wake = self.converged_at + self.floor();
                Ok(demand)
            }
            // A refusal retains nothing, releases nothing and evicts
            // nothing. With a demand still installed, the honest answer
            // is that observation — a transient authority movement must
            // not turn a working watch into an error — and the attempt
            // is what paces the next one.
            Err(refusal) => match installed {
                Some(demand) => {
                    self.converged_at = Instant::now();
                    self.floor_wake = self.converged_at + self.floor();
                    Ok(demand)
                }
                None => Err(retention_refusal(refusal)),
            },
        }
    }

    /// Map ONE core projection onto the snapshot's rows, CLAMPED to the
    /// currently visible population.
    ///
    /// Adds no ordering structure and samples no plane: the rank order
    /// is the projection's own `viable` slice with the same clamp
    /// applied, the per-row class is membership in the projection's own
    /// buckets, and every economics value — readiness, provider start,
    /// route estimate — is the value the projection CLASSIFIED with. A
    /// second read of the proximity plane here would pair this row's
    /// viability and rank with an input that never entered them.
    fn assemble(&self, projection: &OrgSensedProjection, visible: &[u64]) -> SensingSnapshot {
        let providers = projection
            .rows()
            .iter()
            .filter(|row| visible.contains(&row.provider))
            .map(|row| {
                let viability = if projection.viable().contains(&row.provider) {
                    SensedViability::Viable
                } else if projection.non_viable().contains(&row.provider) {
                    SensedViability::NotViable
                } else {
                    SensedViability::Potential
                };
                SensedProvider {
                    node_id: row.provider,
                    readiness: row.readiness,
                    viability,
                    estimated_start: row.estimated_start,
                    // The plane's own "nothing known" answer maps to
                    // `None`; anything else is what the classification
                    // charged this row.
                    route_estimate: (row.route_estimate != sensing::UNKNOWN_ROUTE_ESTIMATE)
                        .then_some(row.route_estimate),
                }
            })
            .collect();
        SensingSnapshot {
            capability: self.capability.clone(),
            providers,
            ranked: projection
                .viable()
                .iter()
                .copied()
                .filter(|provider| visible.contains(provider))
                .collect(),
        }
    }

    /// Unstable fixtures-only witness seam; not supported API.
    ///
    /// The status of the LAST admitted attestation the node holds for
    /// this watch's own branch toward `provider` — the raw received
    /// value, before any freshness projection.
    ///
    /// It exists to discriminate WHY a projection reads `Unknown`: a
    /// last admitted status that is still `Ready` means the projection
    /// aged out (continuity), while a replacement or withdrawal shows up
    /// here as a new status. A witness cannot rebuild the branch key
    /// outside — the acquisition canonicalized it — so this reads it
    /// through the watch's own retained identity.
    #[cfg(any(test, feature = "fixtures"))]
    #[doc(hidden)]
    pub fn last_attested_status_for_test(&self, provider: u64) -> Option<sensing::AttestedStatus> {
        let demand = self.family.demand(&self.authority)?;
        let (_, branch) = demand
            .retained_branches_for_test()
            .into_iter()
            .find(|(retained, _)| *retained == provider)?;
        self.node
            .sensing_latest_attestation(&branch)
            .map(|attestation| attestation.status)
    }
}

impl Drop for SensingWatch {
    fn drop(&mut self) {
        self.close();
    }
}

impl std::fmt::Debug for SensingWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SensingWatch")
            .field("capability", &self.capability)
            .field("budget", &self.budget.end_to_end_within)
            .field("closed", &self.closed)
            .finish()
    }
}

/// Map a core retention refusal onto the SDK's typed refusal.
fn retention_refusal(refusal: OrgSensingDemandRefused) -> SensingError {
    match refusal {
        OrgSensingDemandRefused::NoAuthority => SensingError::NoOrganizationAuthority,
        OrgSensingDemandRefused::FamilyAtCapacity => SensingError::WatchesAtCapacity,
        OrgSensingDemandRefused::FamilyUnavailable => SensingError::ObservationIdentityUnavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A watch and its data are `Send + Sync` because their internals
    /// are — an `Arc<MeshNode>`, the node-bound demand family, a watch
    /// receiver and plain data. Asserted by compilation, so an internal
    /// that stops supporting it breaks the build instead of quietly
    /// breaking the promise.
    #[test]
    fn the_consumer_surface_is_send_and_sync() {
        fn require<T: Send + Sync>() {}
        require::<SensingWatch>();
        require::<SensingQuery>();
        require::<SensingSnapshot>();
        require::<SensedProvider>();
    }

    /// A query that cannot mean anything is refused before any node
    /// state is touched, and each refusal says what to do.
    #[test]
    fn a_meaningless_query_is_refused_with_its_remedy() {
        let blank = SensingQuery::new("   ").validate().expect_err("blank");
        assert_eq!(blank, SensingError::EmptyCapability);
        assert!(blank.to_string().contains("capability"));

        let zero = SensingQuery::new("gpu.infer")
            .within(Duration::ZERO)
            .validate()
            .expect_err("zero budget");
        assert_eq!(zero, SensingError::UnsatisfiableBudget);
        assert!(zero.to_string().contains("budget"));

        let unstartable = SensingQuery::new("gpu.infer")
            .start_within(Duration::ZERO)
            .validate()
            .expect_err("zero start bound");
        assert_eq!(unstartable, SensingError::UnsatisfiableStartBound);
        assert!(unstartable.to_string().contains("start"));
    }

    /// The inverse control for the refusals above: a named capability,
    /// with and without either bound, validates.
    #[test]
    fn a_named_capability_validates_with_and_without_its_bounds() {
        SensingQuery::new("gpu.infer")
            .validate()
            .expect("unbounded");
        let bounded = SensingQuery::new("gpu.infer")
            .within(Duration::from_millis(250))
            .start_within(Duration::from_millis(900));
        bounded.validate().expect("bounded");
        assert_eq!(bounded.budget(), Some(Duration::from_millis(250)));
        assert_eq!(
            bounded.provider_start_within(),
            Some(Duration::from_millis(900))
        );
        assert_eq!(bounded.capability(), "gpu.infer");
        assert_eq!(SensingQuery::new("gpu.infer").budget(), None);
    }

    /// The two bounds are INDEPENDENT: a consumer budget must not move
    /// the provider-evaluated predicate, and the default predicate is
    /// the organization call path's own fixed policy — so a default
    /// watch shares that path's interest instead of forking a digest.
    ///
    /// The blind version of this API is exactly what a caller could not
    /// see: `within()` alone left every observation asking two seconds.
    #[test]
    fn the_consumer_budget_never_moves_the_provider_predicate() {
        let default = SensingQuery::new("gpu.infer");
        assert_eq!(
            default.provider_start_within(),
            Some(DEFAULT_PROVIDER_START_WITHIN)
        );
        assert_eq!(
            org_sensing_demand::fixed_work_latency().provider_start_within,
            Some(DEFAULT_PROVIDER_START_WITHIN),
            "the default must BE the fixed policy, not a copy of its value"
        );

        let budgeted = SensingQuery::new("gpu.infer").within(Duration::from_secs(100));
        assert_eq!(
            budgeted.provider_start_within(),
            Some(DEFAULT_PROVIDER_START_WITHIN),
            "a local budget is not a provider predicate"
        );

        let asked = SensingQuery::new("gpu.infer").start_within(Duration::from_secs(5));
        assert_eq!(asked.provider_start_within(), Some(Duration::from_secs(5)));
        assert_eq!(asked.budget(), None, "asking the provider is not a budget");
    }

    /// Every retention refusal maps onto a DISTINCT typed refusal — a
    /// collapsed mapping would tell an operator "no authority" for a
    /// capacity bound they could act on.
    #[test]
    fn each_retention_refusal_keeps_its_own_meaning() {
        assert_eq!(
            retention_refusal(OrgSensingDemandRefused::NoAuthority),
            SensingError::NoOrganizationAuthority
        );
        assert_eq!(
            retention_refusal(OrgSensingDemandRefused::FamilyAtCapacity),
            SensingError::WatchesAtCapacity
        );
        assert_eq!(
            retention_refusal(OrgSensingDemandRefused::FamilyUnavailable),
            SensingError::ObservationIdentityUnavailable
        );
    }
}
