//! The frozen `ReadinessEvaluator` contract (plan §4.4, SI-0
//! item 12).
//!
//! Capability integrations implement ONE narrow trait; without it
//! every integration invents its own meaning for `ProviderUnknown`.
//! The five-variant result model is frozen in SI-0 (SI-1 gate
//! condition (j)): the three non-Ready/NotReady variants all project
//! onto the wire as `ProviderUnknown`, but each carries a distinct
//! compact `status_reason` code — observability keeps the
//! distinction even though consumers treat all three as Unknown.
//!
//! Two provider-side refusals live here as well:
//!
//! - **Unsupported cadence is refused, not silently degraded** — a
//!   coalesced strictest D below the provider's floor produces a
//!   structured [`CadenceRefusal`] carrying `minimum_supported`, so
//!   relays can partition their downstreams on it (§4.4; the
//!   partitioning itself is the interest table's job).
//! - **A `constraints_digest` mismatch is malformed or tampered
//!   protocol input**, not merely an unevaluable predicate: it
//!   increments the protocol-invalid/security counter even though it
//!   projects publicly as `ProviderUnknown { InvalidConstraints }`.
//!
//! `ReadinessEvaluators` is the node's **crate-internal** registry of
//! those implementations (CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md
//! S0 item 7, §4.4). It is deliberately not public API: the storage
//! choice, the mutation methods, and the inspection methods are all
//! internal, and the only things that cross the crate boundary are the
//! opaque [`EvaluatorRegistrationId`], the
//! [`EvaluatorInstallRefusal`] it can refuse with, and the `MeshNode`
//! lifecycle seams built on them.
//!
//! Ownership has three parts, and all three are enforced by one mutex
//! rather than by check-then-act:
//!
//! - an install issues a fresh, never-reused id, and a vacancy-required
//!   install refuses rather than silently stealing a live registration;
//! - removal is conditional on that id, so a superseded handle can
//!   never evict its replacement;
//! - **publication** of an evaluated result is conditional on the same
//!   id, so a result computed under a registration that has since been
//!   closed or replaced cannot become the latest observation.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;

use super::continuity::AttestedStatus;
use super::identity::{
    CanonicalConstraints, CapabilityId, ConstraintError, Digest256, WorkLatencyEnvelope,
};

/// Default provider cadence floor (plan §5,
/// `attestation_cadence_floor`): a coalesced strictest D below this
/// is refused with `sampling_interval_unsupported`.
pub const DEFAULT_ATTESTATION_CADENCE_FLOOR: Duration = Duration::from_millis(50);

/// The semantic inputs of one predicate evaluation. The spike
/// freezes these parameters — SI-3 binds the fold's capability entry
/// to them when the origin emitter lands; the entry adds context,
/// never replaces a parameter.
///
/// v4: there is deliberately NO generation parameter — a
/// capability-directed interest cannot bind one provider's
/// generation (plan §3.2). The provider always evaluates against
/// its CURRENT generation and stamps that generation into the
/// signed attestation, where the observation key binds it.
#[derive(Clone, Copy, Debug)]
pub struct EvaluationRequest<'a> {
    /// Capability the predicate targets.
    pub capability_id: &'a CapabilityId,
    /// Work characteristics C (already digest-validated).
    pub constraints: &'a CanonicalConstraints,
    /// Latency envelope L.
    pub work_latency: &'a WorkLatencyEnvelope,
}

/// The frozen evaluation result model (plan §4.4).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReadinessEvaluation {
    /// The predicate holds.
    Ready {
        /// Provider's estimate of time-to-start, if it has one.
        estimated_start: Option<Duration>,
    },
    /// The predicate evaluated false.
    NotReady {
        /// Provider-defined compact detail code (queue full, model
        /// cold, disk pressure, …) — diagnostics, never semantics.
        reason: u16,
    },
    /// This capability cannot answer this (C, L) shape at all.
    UnsupportedPredicate,
    /// Transient local failure — the evaluator itself is degraded.
    TemporarilyUnevaluable,
    /// Constraints were undecodable or failed digest validation.
    InvalidConstraints,
}

/// Compact `status_reason` code carried beside the wire status
/// (plan §4.2/§4.4). Consumers treat every `ProviderUnknown` alike;
/// these codes exist for observability distributions (SI-7).
///
/// Serde exists for the SI-1 wire codec
/// (`super::wire::ReadinessAttestation`, postcard); the signature
/// transcript never hashes a serde encoding — it uses the
/// fixed-width canonical tag in `super::wire`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum StatusReason {
    /// No detail (the normal Ready case).
    None,
    /// Provider-defined NotReady detail code.
    Provider(u16),
    /// Capability cannot answer this predicate shape.
    UnsupportedPredicate,
    /// Transient evaluator failure.
    TemporarilyUnevaluable,
    /// Undecodable / digest-mismatched constraints.
    InvalidConstraints,
    /// The coalesced strictest D was below the provider floor
    /// ([`CadenceRefusal`]).
    SamplingIntervalUnsupported,
}

/// Project an evaluation onto the wire pair
/// `(attested status, status_reason)` (plan §4.4): the three
/// non-Ready/NotReady variants collapse to `ProviderUnknown` with
/// distinct reasons.
pub const fn project_evaluation(
    evaluation: &ReadinessEvaluation,
) -> (AttestedStatus, StatusReason) {
    match evaluation {
        ReadinessEvaluation::Ready { .. } => (AttestedStatus::Ready, StatusReason::None),
        ReadinessEvaluation::NotReady { reason } => {
            (AttestedStatus::NotReady, StatusReason::Provider(*reason))
        }
        ReadinessEvaluation::UnsupportedPredicate => (
            AttestedStatus::ProviderUnknown,
            StatusReason::UnsupportedPredicate,
        ),
        ReadinessEvaluation::TemporarilyUnevaluable => (
            AttestedStatus::ProviderUnknown,
            StatusReason::TemporarilyUnevaluable,
        ),
        ReadinessEvaluation::InvalidConstraints => (
            AttestedStatus::ProviderUnknown,
            StatusReason::InvalidConstraints,
        ),
    }
}

/// The one trait a capability integration implements (plan §4.4).
/// The provider compiles the predicate once per distinct
/// `interest_digest` and calls this at the aggregated cadence plus
/// on status edges; implementations must be cheap and non-blocking.
pub trait ReadinessEvaluator {
    /// Evaluate the predicate against current local state.
    fn evaluate(&self, request: &EvaluationRequest<'_>) -> ReadinessEvaluation;
}

/// Structured refusal for an unsupportable sampling interval (plan
/// §4.4): never a silently weaker stream. Relays partition their
/// downstreams on `minimum_supported` and re-register the
/// satisfiable aggregate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CadenceRefusal {
    /// The provider's floor M — the strictest interval it will
    /// serve.
    pub minimum_supported: Duration,
}

impl CadenceRefusal {
    /// How the refusal appears on the attestation surface.
    pub const fn as_status(&self) -> (AttestedStatus, StatusReason) {
        (
            AttestedStatus::ProviderUnknown,
            StatusReason::SamplingIntervalUnsupported,
        )
    }
}

/// Provider-side cadence admission: a coalesced strictest D below
/// the floor is refused with the floor attached, so satisfiable
/// co-subscribers can be re-aggregated by the relay (§4.4).
pub const fn check_cadence(
    requested_strictest: Duration,
    floor: Duration,
) -> Result<(), CadenceRefusal> {
    // Duration lacks const PartialOrd; compare the raw parts.
    if requested_strictest.as_nanos() < floor.as_nanos() {
        Err(CadenceRefusal {
            minimum_supported: floor,
        })
    } else {
        Ok(())
    }
}

/// Sensing-plane counters (plan §6 SI-7 observability surface).
/// Shared-reference friendly: relaxed atomics, monotonic,
/// diagnostics only — never load-bearing for any decision. Read a
/// snapshot through [`super::super::super::MeshNode::sensing_counters`].
///
/// The counters fall in three groups: refusals-by-kind (the SI-0
/// subset), the coalescing / delivery lifecycle (SI-7), and the
/// coalescing-efficacy headline (SI-7, plan §4.1 future gate).
#[derive(Default, Debug)]
pub struct SensingCounters {
    // ── Refusals by kind (SI-0) ──
    /// Every constraint rejection (any [`ConstraintError`]).
    pub invalid_constraints: AtomicU64,
    /// The security-relevant subset: protocol-invalid input —
    /// constraint digest mismatches (plan §4.4), wire scope claims the
    /// session does not back (plan §4.10), and a LEGACY registration whose
    /// declared audience is an organization-derived commitment while this
    /// node holds organization authority (the C1 authority-aware
    /// classification — an honest legacy sender never claims the org
    /// audience, so the combination is a protocol violation, not merely an
    /// authorization refusal).
    pub protocol_invalid: AtomicU64,
    /// Structured cadence refusals issued.
    pub cadence_refusals: AtomicU64,
    /// Scope-validation refusals (plan §4.10) — every
    /// [`super::scope::ScopeError`], security-relevant or not.
    pub scope_refusals: AtomicU64,
    /// Selector-too-broad refusals at the resolver (plan §4.7
    /// each-mode amplification guard): an `Each`-mode selector
    /// matched more providers than `each_mode_max_providers`.
    pub broad_selector_refusals: AtomicU64,

    // ── Organization-gate refusals by reason (review §4) ──
    // Security-relevant org-sensing intake refusals, one counter per reason so a
    // forged-cert flood or revocation-evasion attempt leaves an operator-visible
    // signal (previously every arm but `Semantic` was silent). Diagnostics only.
    /// Certificate signature / TTL / validity-window check failed.
    pub org_cert_invalid: AtomicU64,
    /// Certificate generation is below the revocation floor (a revoked member
    /// still sending).
    pub org_below_floor: AtomicU64,
    /// The certificate's organization is not this node's owner organization.
    pub org_foreign_org: AtomicU64,
    /// The authenticated session sender is not the certificate's member.
    pub org_sender_member_mismatch: AtomicU64,
    /// The interest audience is not the canonical commitment for the org.
    pub org_audience_mismatch: AtomicU64,
    /// No installed authority/store at gate time (`MissingAuthority`) or the
    /// dispatch snapshot capture found none — this node cannot verify membership.
    pub org_authority_unavailable: AtomicU64,
    /// The pinned authority view went stale before the table mutation (a floor
    /// raise, authority swap, or A→B→A rotation between gate and register).
    pub org_stale_stamp: AtomicU64,
    /// The installed revocation store is poisoned — all org intake fails dark.
    pub org_store_poisoned: AtomicU64,

    // ── Coalescing + delivery lifecycle (SI-7) ──
    /// Consumer capability-registrations admitted at THIS node's
    /// sensing-leader role — the denominator for the local
    /// coalescing ratio.
    pub interests_registered: AtomicU64,
    /// The subset of [`Self::interests_registered`] that JOINED an
    /// existing coalesced interest row rather than resolving fresh
    /// candidates — demand that merged at the leader BEFORE the
    /// provider hop (plan §4.1 "scope-wide, pre-selection"
    /// coalescing). `interests_coalesced / interests_registered` is
    /// the local coalescing efficacy.
    pub interests_coalesced: AtomicU64,
    /// Sum of resolved active-branch counts across fresh interest
    /// resolutions — the candidate fan-out the leader opened (plan
    /// §4.7 bounded exploration).
    pub candidate_fanout_total: AtomicU64,
    /// Signed origin beats this node's origin emitter produced (plan
    /// §4.4). One per branch per due tick, fanned to every
    /// downstream by the relay machinery — NOT multiplied by
    /// watchers (the coalescing economic claim, SI-1d).
    pub attestations_emitted: AtomicU64,
    /// Signed attestations this node forwarded VERBATIM as a relay
    /// (plan §4.2 — relays never author), counted per downstream
    /// forward, so the value is fan-out volume.
    pub attestations_forwarded: AtomicU64,
    /// Attestations dropped at the §4.6 observer gate (stale/rewound
    /// sequence, duplicate) before touching latest/cells/overlay.
    pub attestations_gated: AtomicU64,
    /// Attestations dropped because their `(incarnation, generation)`
    /// epoch was globally superseded (SI-5 review P0): a delayed
    /// valid-but-obsolete beat under a provider's older boot or
    /// capability definition.
    pub attestations_superseded: AtomicU64,

    // ── Coalescing efficacy: the §4.1 future-gate headline (SI-7) ──
    /// Provider-FREE `ProviderRegistration`s this node admitted as
    /// the target provider — the denominator for the merge-miss
    /// rate. Provider-targeted (`Node`/`Nodes`) registrations are
    /// excluded: multiple direct surveillants of one provider is
    /// intended, not a coalescing failure.
    pub provider_free_registrations: AtomicU64,
    /// The divergent-resolution MERGE-MISS (plan §4.1): a
    /// provider-free registration admitted while the branch already
    /// carried another distinct upstream — two independent leaders
    /// resolved the same interest to this provider (split-brain
    /// islands, or the window while an election result propagates).
    /// `divergent_resolution_merge_miss / provider_free_registrations`
    /// is the residual-divergence rate that feeds the §4.1 future
    /// gate: materially non-zero justifies a convergence refinement;
    /// ~zero shows the split-brain tolerance is empirically cheap.
    pub divergent_resolution_merge_miss: AtomicU64,
}

impl SensingCounters {
    /// Snapshot one counter (test/observability convenience).
    pub fn get(counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }
}

/// Provider-side inline-constraint intake (plan §4.2): parse +
/// digest-validate, counting rejections. A digest mismatch counts on
/// BOTH `invalid_constraints` and `protocol_invalid`; plain decode
/// failures count only on the former. The caller maps any `Err` to
/// `ReadinessEvaluation::InvalidConstraints`.
pub fn validate_interest_constraints(
    bytes: &[u8],
    claimed: &Digest256,
    counters: &SensingCounters,
) -> Result<CanonicalConstraints, ConstraintError> {
    match CanonicalConstraints::validate_inline(bytes, claimed) {
        Ok(constraints) => Ok(constraints),
        Err(error) => {
            counters.invalid_constraints.fetch_add(1, Ordering::Relaxed);
            if error.is_security_relevant() {
                counters.protocol_invalid.fetch_add(1, Ordering::Relaxed);
            }
            Err(error)
        }
    }
}

/// A node-local provider-readiness registration identity (plan
/// §4.4: "return an opaque registration token/generation").
///
/// Minted by the node's registry under a **checked, non-reusing**
/// allocator: an id is issued at most once for the lifetime of the
/// node, so an id that is no longer the installed one can only be a
/// superseded registration and can never alias a later one. Node-local
/// and never on the wire.
///
/// Unstable, workspace-internal SDK bridge; not supported core API.
/// Public only because `net-mesh-sdk` is a separate crate and its
/// provider handle must hold one across the crate boundary. Opaque by
/// construction — no constructor, no accessor, no ordering.
#[doc(hidden)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct EvaluatorRegistrationId(u64);

/// The largest id the allocator will ever issue.
///
/// `u64::MAX` is reserved as the terminal *exhausted* sentinel and is
/// never handed out, so a stale id can never equal the allocator's
/// resting value.
const MAX_REGISTRATION_ID: u64 = u64::MAX - 1;

/// Why an evaluator install was refused (plan §4.4: "reject or
/// explicitly replace an existing evaluator; never silently steal
/// it").
///
/// Deliberately carries NO id: handing the loser the incumbent's
/// registration id would give it a token it could use to unregister
/// the winner, which is the ownership hole this type exists to close.
///
/// Deliberately NOT `#[non_exhaustive]`. The only cross-crate consumer
/// is the workspace's own SDK, versioned in lockstep, and a wildcard arm
/// there could only report a new refusal under a wrong name. Exhaustive
/// means adding a variant is a compile error at the one place that has
/// to decide how to report it.
///
/// Unstable, workspace-internal SDK bridge; not supported core API.
/// Public only because `net-mesh-sdk` is a separate crate — the
/// supported refusals are `net_sdk::sensing::SensingError` variants.
#[doc(hidden)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EvaluatorInstallRefusal {
    /// A live registration already serves this capability. Only a
    /// vacancy-required install can produce this — see
    /// `MeshNode::register_readiness_evaluator`.
    Occupied,
    /// The node's registration-identity space is exhausted. No further
    /// install of any kind can publish, because a fresh non-aliasing id
    /// can no longer be issued and reusing one would let a stale handle
    /// remove a live registration. Terminal: incumbents keep serving
    /// and removal keeps working, but nothing new installs.
    IdentityExhausted,
}

impl std::fmt::Display for EvaluatorInstallRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Occupied => {
                f.write_str("a readiness evaluator is already registered for this capability")
            }
            Self::IdentityExhausted => {
                f.write_str("the node's readiness-registration identity space is exhausted")
            }
        }
    }
}

impl std::error::Error for EvaluatorInstallRefusal {}

/// One capability's installed evaluator plus the registration
/// identity that owns it.
struct EvaluatorSlot {
    registration_id: EvaluatorRegistrationId,
    evaluator: Arc<dyn ReadinessEvaluator + Send + Sync>,
}

/// A held publication section (plan §4.4 ownership).
///
/// Existence of this value proves the registry's commit mutex is held
/// AND that, at the moment it was taken, the capability's installed
/// registration matched the one the caller evaluated under. Because
/// install/replace/remove take the same mutex for their whole
/// operation, a publication guarded by this token cannot land after a
/// close or replacement has *returned*.
///
/// The guard deliberately exposes nothing: it is a fence, not a handle.
pub(crate) struct EvaluationCommit<'a> {
    _guard: parking_lot::MutexGuard<'a, ()>,
}

/// The node's provider-readiness evaluator registry (plan §4.4, S0
/// item 7).
///
/// One evaluator per capability id — the addition over a plain map is
/// **ownership**. Every install issues a fresh
/// [`EvaluatorRegistrationId`], removal is conditional on that id still
/// being the installed one, and publication of an evaluated result is
/// conditional on the same id through [`Self::begin_commit`].
///
/// # Concurrency contract
///
/// `commit_mu` is the linearization point for ownership transfer. Every
/// mutation ([`Self::install_vacant`], [`Self::install_replacing`],
/// [`Self::remove_if_current`]) holds it across its whole operation,
/// and every publication holds it across the currentness test AND the
/// publication itself. So there is no check-then-act window: the
/// decision and the effect are one critical section.
///
/// **Lock order.** `commit_mu` is strictly OUTERMOST among the sensing
/// locks. It may be held while taking
/// `sensing_local_projection_mu` → `sensing_interest_table` →
/// `sensing_observations` (the frozen order) or `sensing_emitter`;
/// nothing ever acquires it while holding any of those. No user
/// evaluator, no `.await`, and no network I/O may run inside it —
/// [`Self::installed`] exists precisely so the user evaluator runs
/// before the section is entered.
///
/// Readiness is not reservation, admission, or execution authority:
/// this registry answers only "who evaluates capability Y on this
/// node".
#[derive(Default)]
pub(crate) struct ReadinessEvaluators {
    slots: DashMap<CapabilityId, EvaluatorSlot>,
    /// Next id to issue, or `u64::MAX` once exhausted. Never wraps.
    next_id: AtomicU64,
    commit_mu: parking_lot::Mutex<()>,
    /// Fixtures-only contention observer, mirroring the node's
    /// `sensing_projection_contention_hook`: invoked by an ownership
    /// transition when its `try_lock` on `commit_mu` observes the mutex
    /// HELD, before falling back to the blocking `lock()`.
    ///
    /// The concurrency witnesses wait on this signal, so "the rival
    /// reached the real ownership-mutex boundary and found it held" is
    /// proved by actual observed contention rather than by a
    /// scheduler-dependent timeout. Absent from production builds.
    #[cfg(any(test, feature = "fixtures"))]
    contention_hook: parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl ReadinessEvaluators {
    /// Enter the ownership section, acknowledging observed contention.
    ///
    /// Try-then-block so the fixtures-only observer fires ONLY after a
    /// `try_lock` actually found the mutex held — never on the
    /// uncontended fast path. Production is a plain `lock()`.
    fn acquire_commit(&self) -> parking_lot::MutexGuard<'_, ()> {
        match self.commit_mu.try_lock() {
            Some(guard) => guard,
            None => {
                #[cfg(any(test, feature = "fixtures"))]
                {
                    let hook = self.contention_hook.lock().clone();
                    if let Some(hook) = hook {
                        hook();
                    }
                }
                self.commit_mu.lock()
            }
        }
    }

    /// Install (or clear) the fixtures-only contention observer.
    #[cfg(any(test, feature = "fixtures"))]
    pub(crate) fn set_contention_hook_for_test(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.contention_hook.lock() = hook;
    }

    /// Issue the next id, or `None` once the space is exhausted.
    ///
    /// Checked and non-reusing: the counter saturates at the reserved
    /// `u64::MAX` sentinel instead of wrapping, so no id is ever issued
    /// twice and a stale id can never alias a live one. A `fetch_update`
    /// rather than `fetch_add` because the terminal state must be
    /// reached exactly once and never stepped past.
    fn mint(&self) -> Option<EvaluatorRegistrationId> {
        self.next_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                if current > MAX_REGISTRATION_ID {
                    None
                } else {
                    Some(current + 1)
                }
            })
            .ok()
            .map(EvaluatorRegistrationId)
    }

    /// Whether the identity space is exhausted (tests + observability).
    #[cfg(any(test, feature = "fixtures"))]
    pub(crate) fn identities_exhausted(&self) -> bool {
        self.next_id.load(Ordering::Relaxed) > MAX_REGISTRATION_ID
    }

    /// Install an evaluator ONLY if the capability is currently
    /// unserved.
    ///
    /// A refusal is total: no id is issued and the incumbent is
    /// untouched. Serialized against publication and removal by
    /// `commit_mu`, so a registration that returns has fully taken
    /// ownership before any later publication can test currentness.
    pub(crate) fn install_vacant(
        &self,
        capability_id: CapabilityId,
        evaluator: Arc<dyn ReadinessEvaluator + Send + Sync>,
    ) -> Result<EvaluatorRegistrationId, EvaluatorInstallRefusal> {
        // Nothing should be displaced on a vacancy-required install, but
        // the value is carried out of the section anyway: if that
        // invariant ever broke, dropping a user evaluator under the
        // ownership mutex could deadlock (see `install_replacing`).
        let mut displaced: Option<EvaluatorSlot> = None;
        let outcome = {
            let _commit = self.acquire_commit();
            if self.slots.contains_key(&capability_id) {
                Err(EvaluatorInstallRefusal::Occupied)
            } else {
                match self.mint() {
                    None => Err(EvaluatorInstallRefusal::IdentityExhausted),
                    Some(registration_id) => {
                        displaced = self.slots.insert(
                            capability_id,
                            EvaluatorSlot {
                                registration_id,
                                evaluator,
                            },
                        );
                        Ok(registration_id)
                    }
                }
            }
        };
        // Section released. Any user `Drop` may run now.
        drop(displaced);
        outcome
    }

    /// Install an evaluator, EXPLICITLY superseding any incumbent.
    ///
    /// The new id is fresh, so the superseded registration's id is
    /// non-current the instant this returns: its later removal is inert
    /// and an evaluation already in flight under it can no longer
    /// publish.
    ///
    /// Fallible only through
    /// [`EvaluatorInstallRefusal::IdentityExhausted`] — on which the
    /// incumbent keeps serving, because superseding it with an
    /// un-ownable registration would be strictly worse than refusing.
    ///
    /// **The displaced evaluator is dropped OUTSIDE the section.** The
    /// map mutation is fully serialized under `commit_mu`, but the
    /// superseded slot is moved out as a value and only released after
    /// the guard is. Its `Drop` is arbitrary user code — it may
    /// legitimately call back into provide/replace/close — and
    /// `commit_mu` is not reentrant, so dropping it under the guard
    /// would deadlock the node.
    pub(crate) fn install_replacing(
        &self,
        capability_id: CapabilityId,
        evaluator: Arc<dyn ReadinessEvaluator + Send + Sync>,
    ) -> Result<EvaluatorRegistrationId, EvaluatorInstallRefusal> {
        let mut displaced: Option<EvaluatorSlot> = None;
        let outcome = {
            let _commit = self.acquire_commit();
            match self.mint() {
                None => Err(EvaluatorInstallRefusal::IdentityExhausted),
                Some(registration_id) => {
                    displaced = self.slots.insert(
                        capability_id,
                        EvaluatorSlot {
                            registration_id,
                            evaluator,
                        },
                    );
                    Ok(registration_id)
                }
            }
        };
        // Section released — the superseded evaluator's `Drop` runs here,
        // where it may re-enter the lifecycle freely.
        drop(displaced);
        outcome
    }

    /// Remove the capability's evaluator ONLY if `registration_id` is
    /// still the installed one.
    ///
    /// Returns whether THIS call performed the removal — `true` at most
    /// once per registration, so a stale handle's drop is a pure no-op.
    /// Remains available after identity exhaustion: a node that can no
    /// longer install must still be able to stop serving.
    ///
    /// **The removed evaluator is dropped OUTSIDE the section**, for the
    /// same reason as [`Self::install_replacing`]: a user `Drop` that
    /// re-enters the lifecycle must not meet a held, non-reentrant
    /// mutex.
    ///
    /// Written explicitly rather than relying on temporary-drop order.
    /// This previously read `remove_if(..).is_some()` as the function's
    /// TAIL expression, which happens to drop the removed value *after*
    /// the guard — so it was safe, but only by a rule that binding the
    /// result to a local silently reverses. The explicit form states the
    /// requirement instead of depending on it.
    pub(crate) fn remove_if_current(
        &self,
        capability_id: &CapabilityId,
        registration_id: EvaluatorRegistrationId,
    ) -> bool {
        let removed = {
            let _commit = self.acquire_commit();
            self.slots.remove_if(capability_id, |_, slot| {
                slot.registration_id == registration_id
            })
        };
        let performed_removal = removed.is_some();
        // Section released — the removed evaluator's `Drop` runs here.
        drop(removed);
        performed_removal
    }

    /// The capability's installed registration and its evaluator.
    ///
    /// The `Arc` is cloned out so the shard guard drops before the
    /// caller runs user code, and the id rides along so the caller can
    /// prove at publication time that it evaluated under the
    /// registration that is still current.
    ///
    /// `None` means "targeted but cannot answer" — the caller projects
    /// `ProviderUnknown { TemporarilyUnevaluable }`, never Ready and
    /// never NotReady.
    pub(crate) fn installed(
        &self,
        capability_id: &CapabilityId,
    ) -> Option<(
        EvaluatorRegistrationId,
        Arc<dyn ReadinessEvaluator + Send + Sync>,
    )> {
        self.slots
            .get(capability_id)
            .map(|slot| (slot.registration_id, slot.evaluator.clone()))
    }

    /// Enter the publication section, but only if the capability's
    /// installed registration is EXACTLY the one the caller evaluated
    /// under.
    ///
    /// `evaluated_under` is `None` for a beat produced with no evaluator
    /// installed, so the test is total in both directions: a result
    /// computed under a since-superseded registration is refused, and so
    /// is an "unevaluable" beat for a capability that has since gained
    /// an evaluator (publishing "cannot answer" about a capability that
    /// now can is a false negative — dropping the beat leaves the
    /// consumer's continuity to hold or expire to Unknown, which is the
    /// conservative answer, and the emitter re-arms).
    ///
    /// The returned guard MUST be held for the whole publication. Do not
    /// test currentness, drop the guard, and then publish: that is the
    /// check-then-publish race this method exists to remove.
    pub(crate) fn begin_commit(
        &self,
        capability_id: &CapabilityId,
        evaluated_under: Option<EvaluatorRegistrationId>,
    ) -> Option<EvaluationCommit<'_>> {
        let guard = self.acquire_commit();
        let current = self
            .slots
            .get(capability_id)
            .map(|slot| slot.registration_id);
        (current == evaluated_under).then_some(EvaluationCommit { _guard: guard })
    }

    /// Run `poke` while holding the publication section, but only if
    /// `registration_id` is still the installed registration.
    ///
    /// This is the state-edge seam: the currentness test and the poke are
    /// one critical section, so a superseded handle can never move the
    /// schedule its successor owns, and there is no window between the
    /// test and the effect.
    pub(crate) fn poke_if_current<R: Default>(
        &self,
        capability_id: &CapabilityId,
        registration_id: EvaluatorRegistrationId,
        poke: impl FnOnce() -> R,
    ) -> R {
        let _commit = self.acquire_commit();
        let current = self
            .slots
            .get(capability_id)
            .is_some_and(|slot| slot.registration_id == registration_id);
        if current {
            poke()
        } else {
            R::default()
        }
    }

    /// Installed evaluator count (tests + observability).
    #[cfg(any(test, feature = "fixtures"))]
    pub(crate) fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether no capability on this node has an evaluator.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Whether `registration_id` is the capability's installed
    /// registration.
    ///
    /// Tests and observability ONLY — never a decision input, because a
    /// check-then-act on this value is exactly the race
    /// [`Self::begin_commit`] and [`Self::poke_if_current`] exist to
    /// prevent.
    #[cfg(test)]
    pub(crate) fn is_current(
        &self,
        capability_id: &CapabilityId,
        registration_id: EvaluatorRegistrationId,
    ) -> bool {
        self.slots
            .get(capability_id)
            .is_some_and(|slot| slot.registration_id == registration_id)
    }

    /// Force the allocator's resting value, to reach the terminal
    /// exhausted state without 2^64 real registrations.
    #[cfg(any(test, feature = "fixtures"))]
    pub(crate) fn set_next_id_for_test(&self, next: u64) {
        self.next_id.store(next, Ordering::Relaxed);
    }

    /// The largest issuable id — so a test can name the boundary
    /// without duplicating the constant.
    #[cfg(any(test, feature = "fixtures"))]
    pub(crate) const fn max_issuable_id_for_test() -> u64 {
        MAX_REGISTRATION_ID
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::*;

    /// A minimal integration: readiness is driven by a "load"
    /// constraint, and unrecognized constraint keys are an
    /// unsupported predicate — exactly the shape SI-3's real
    /// evaluators will take.
    struct LoadEvaluator {
        current_load: u16,
    }

    impl ReadinessEvaluator for LoadEvaluator {
        fn evaluate(&self, request: &EvaluationRequest<'_>) -> ReadinessEvaluation {
            let Some(max_load) = request.constraints.get("max_load") else {
                return ReadinessEvaluation::UnsupportedPredicate;
            };
            let Ok(max_load) = max_load.parse::<u16>() else {
                return ReadinessEvaluation::InvalidConstraints;
            };
            if self.current_load <= max_load {
                ReadinessEvaluation::Ready {
                    estimated_start: Some(Duration::from_millis(5)),
                }
            } else {
                ReadinessEvaluation::NotReady { reason: 42 }
            }
        }
    }

    fn request<'a>(
        capability_id: &'a CapabilityId,
        constraints: &'a CanonicalConstraints,
        work_latency: &'a WorkLatencyEnvelope,
    ) -> EvaluationRequest<'a> {
        EvaluationRequest {
            capability_id,
            constraints,
            work_latency,
        }
    }

    #[test]
    fn evaluator_contract_round_trips_through_a_real_impl() {
        let id = CapabilityId::new("job.run");
        let latency = WorkLatencyEnvelope::start_within(Duration::from_millis(100));
        let ok = CanonicalConstraints::from_entries([("max_load", "50")]).unwrap();
        let alien = CanonicalConstraints::from_entries([("gpu_class", "h100")]).unwrap();

        let idle = LoadEvaluator { current_load: 10 };
        let busy = LoadEvaluator { current_load: 90 };
        assert_eq!(
            idle.evaluate(&request(&id, &ok, &latency)),
            ReadinessEvaluation::Ready {
                estimated_start: Some(Duration::from_millis(5)),
            },
        );
        assert_eq!(
            busy.evaluate(&request(&id, &ok, &latency)),
            ReadinessEvaluation::NotReady { reason: 42 },
        );
        assert_eq!(
            idle.evaluate(&request(&id, &alien, &latency)),
            ReadinessEvaluation::UnsupportedPredicate,
        );
    }

    #[test]
    fn projection_collapses_to_provider_unknown_with_distinct_reasons() {
        use AttestedStatus as S;
        assert_eq!(
            project_evaluation(&ReadinessEvaluation::Ready {
                estimated_start: None,
            }),
            (S::Ready, StatusReason::None),
        );
        assert_eq!(
            project_evaluation(&ReadinessEvaluation::NotReady { reason: 7 }),
            (S::NotReady, StatusReason::Provider(7)),
        );
        // The three Unknown-projecting variants stay distinguishable
        // through status_reason even though the wire status is one
        // value.
        let unknowns = [
            (
                ReadinessEvaluation::UnsupportedPredicate,
                StatusReason::UnsupportedPredicate,
            ),
            (
                ReadinessEvaluation::TemporarilyUnevaluable,
                StatusReason::TemporarilyUnevaluable,
            ),
            (
                ReadinessEvaluation::InvalidConstraints,
                StatusReason::InvalidConstraints,
            ),
        ];
        for (evaluation, expected_reason) in unknowns {
            assert_eq!(
                project_evaluation(&evaluation),
                (S::ProviderUnknown, expected_reason),
            );
        }
    }

    #[test]
    fn cadence_below_floor_is_refused_with_the_floor_attached() {
        let floor = DEFAULT_ATTESTATION_CADENCE_FLOOR;
        assert_eq!(check_cadence(Duration::from_millis(50), floor), Ok(()));
        assert_eq!(check_cadence(Duration::from_secs(1), floor), Ok(()));
        let refusal = check_cadence(Duration::from_millis(20), floor).unwrap_err();
        assert_eq!(refusal.minimum_supported, floor);
        assert_eq!(
            refusal.as_status(),
            (
                AttestedStatus::ProviderUnknown,
                StatusReason::SamplingIntervalUnsupported,
            ),
        );
    }

    #[test]
    fn digest_mismatch_counts_as_security_plain_decode_failures_do_not() {
        let counters = SensingCounters::default();
        let constraints = CanonicalConstraints::from_entries([("a", "1")]).unwrap();
        let bytes = constraints.canonical_bytes();
        let right = constraints.constraints_digest();
        let wrong = Digest256::from_bytes([0u8; 32]);

        // Valid intake: no counters move.
        assert!(validate_interest_constraints(&bytes, &right, &counters).is_ok());
        assert_eq!(SensingCounters::get(&counters.invalid_constraints), 0);
        assert_eq!(SensingCounters::get(&counters.protocol_invalid), 0);

        // Digest mismatch: both counters move (plan §4.4 — malformed
        // or tampered protocol input, not merely unevaluable).
        assert_eq!(
            validate_interest_constraints(&bytes, &wrong, &counters),
            Err(ConstraintError::DigestMismatch),
        );
        assert_eq!(SensingCounters::get(&counters.invalid_constraints), 1);
        assert_eq!(SensingCounters::get(&counters.protocol_invalid), 1);

        // Truncation: only the invalid-constraints counter moves.
        assert!(validate_interest_constraints(&bytes[..3], &right, &counters).is_err());
        assert_eq!(SensingCounters::get(&counters.invalid_constraints), 2);
        assert_eq!(SensingCounters::get(&counters.protocol_invalid), 1);
    }

    /// A marker evaluator whose verdict identifies WHICH instance is
    /// installed, so a replacement is distinguishable from an
    /// incumbent through the public read seam alone.
    struct MarkerEvaluator {
        marker: u16,
    }

    impl ReadinessEvaluator for MarkerEvaluator {
        fn evaluate(&self, _request: &EvaluationRequest<'_>) -> ReadinessEvaluation {
            ReadinessEvaluation::NotReady {
                reason: self.marker,
            }
        }
    }

    fn marker(evaluators: &ReadinessEvaluators, capability: &CapabilityId) -> Option<u16> {
        let constraints = CanonicalConstraints::from_entries([("max_load", "1")]).unwrap();
        let work_latency = WorkLatencyEnvelope::start_within(Duration::from_secs(1));
        let request = EvaluationRequest {
            capability_id: capability,
            constraints: &constraints,
            work_latency: &work_latency,
        };
        let (_, evaluator) = evaluators.installed(capability)?;
        match evaluator.evaluate(&request) {
            ReadinessEvaluation::NotReady { reason } => Some(reason),
            other => panic!("marker evaluator answered {other:?}"),
        }
    }

    #[test]
    fn every_install_mints_a_distinct_registration_id() {
        let evaluators = ReadinessEvaluators::default();
        let a = CapabilityId::new("gpu.infer");
        let b = CapabilityId::new("print.document");

        let first = evaluators
            .install_vacant(a.clone(), Arc::new(MarkerEvaluator { marker: 1 }))
            .expect("vacant install");
        let second = evaluators
            .install_vacant(b.clone(), Arc::new(MarkerEvaluator { marker: 2 }))
            .expect("vacant install");
        let third = evaluators
            .install_replacing(a.clone(), Arc::new(MarkerEvaluator { marker: 3 }))
            .expect("replacement install");

        assert_ne!(first, second);
        assert_ne!(first, third);
        assert_ne!(second, third);
        assert_eq!(evaluators.len(), 2);
    }

    /// The inverse witness for an UNCONDITIONAL unregister: a
    /// superseded registration's id must remove nothing. An
    /// implementation that removed by capability id alone would drop
    /// the replacement here.
    #[test]
    fn a_superseded_registration_id_cannot_remove_its_replacement() {
        let evaluators = ReadinessEvaluators::default();
        let capability = CapabilityId::new("gpu.infer");

        let old = evaluators
            .install_vacant(capability.clone(), Arc::new(MarkerEvaluator { marker: 7 }))
            .expect("vacant install");
        let new = evaluators
            .install_replacing(capability.clone(), Arc::new(MarkerEvaluator { marker: 9 }))
            .expect("replacement install");

        assert!(!evaluators.is_current(&capability, old));
        assert!(evaluators.is_current(&capability, new));

        // The stale handle's drop.
        assert!(!evaluators.remove_if_current(&capability, old));
        assert_eq!(
            marker(&evaluators, &capability),
            Some(9),
            "the replacement must survive the superseded handle's removal",
        );
        assert_eq!(evaluators.len(), 1);

        // The current handle's close removes exactly its own row.
        assert!(evaluators.remove_if_current(&capability, new));
        assert!(evaluators.is_empty());
        assert_eq!(marker(&evaluators, &capability), None);
    }

    /// The removal seam is `true` at most once per registration, so an
    /// explicit close followed by a drop is safe and a repeat is inert.
    #[test]
    fn remove_if_current_reports_the_removal_exactly_once() {
        let evaluators = ReadinessEvaluators::default();
        let capability = CapabilityId::new("gpu.infer");
        let id = evaluators
            .install_vacant(capability.clone(), Arc::new(MarkerEvaluator { marker: 4 }))
            .expect("vacant install");

        assert!(evaluators.remove_if_current(&capability, id));
        assert!(!evaluators.remove_if_current(&capability, id));
        assert!(!evaluators.remove_if_current(&capability, id));
        assert!(evaluators.is_empty());
    }

    /// The inverse witness for SILENT OWNERSHIP THEFT: a
    /// vacancy-required install must refuse totally — the incumbent
    /// keeps serving and its id stays current.
    #[test]
    fn a_vacancy_required_install_refuses_without_disturbing_the_incumbent() {
        let evaluators = ReadinessEvaluators::default();
        let capability = CapabilityId::new("gpu.infer");
        let incumbent = evaluators
            .install_vacant(capability.clone(), Arc::new(MarkerEvaluator { marker: 11 }))
            .expect("vacant install");

        assert_eq!(
            evaluators.install_vacant(capability.clone(), Arc::new(MarkerEvaluator { marker: 12 })),
            Err(EvaluatorInstallRefusal::Occupied),
        );
        assert!(evaluators.is_current(&capability, incumbent));
        assert_eq!(marker(&evaluators, &capability), Some(11));
        assert_eq!(evaluators.len(), 1);

        // Only after the incumbent closes does the slot admit a new
        // registration.
        assert!(evaluators.remove_if_current(&capability, incumbent));
        let successor = evaluators
            .install_vacant(capability.clone(), Arc::new(MarkerEvaluator { marker: 12 }))
            .expect("vacated install");
        assert_ne!(successor, incumbent);
        assert_eq!(marker(&evaluators, &capability), Some(12));
    }

    /// An unserved capability yields no evaluator — the caller's
    /// "targeted but cannot answer" input, which projects Unknown, not
    /// Ready and not NotReady.
    #[test]
    fn an_unserved_capability_has_no_evaluator_and_no_current_registration() {
        let evaluators = ReadinessEvaluators::default();
        let served = CapabilityId::new("gpu.infer");
        let unserved = CapabilityId::new("print.document");
        let id = evaluators
            .install_vacant(served.clone(), Arc::new(MarkerEvaluator { marker: 5 }))
            .expect("vacant install");

        assert!(evaluators.installed(&unserved).is_none());
        assert!(!evaluators.is_current(&unserved, id));
        assert!(!evaluators.remove_if_current(&unserved, id));
        assert_eq!(
            marker(&evaluators, &served),
            Some(5),
            "a removal aimed at another capability must not touch this one",
        );
    }

    // ---------------------------------------------------------------
    // The publication fence (H1)
    // ---------------------------------------------------------------

    /// The fence's whole contract in one place: a commit section opens
    /// only when the capability's installed registration is EXACTLY the
    /// one the caller evaluated under.
    ///
    /// Fails against: publishing without revalidating ownership;
    /// revalidating against the capability alone rather than the id.
    #[test]
    fn a_commit_section_opens_only_for_the_registration_that_was_evaluated() {
        let evaluators = ReadinessEvaluators::default();
        let capability = CapabilityId::new("gpu.infer");
        let first = evaluators
            .install_vacant(capability.clone(), Arc::new(MarkerEvaluator { marker: 1 }))
            .expect("vacant install");

        // The current registration may publish.
        assert!(evaluators.begin_commit(&capability, Some(first)).is_some());

        // Replacement makes the old id non-current, so its in-flight
        // result can no longer commit — while the successor's can.
        let second = evaluators
            .install_replacing(capability.clone(), Arc::new(MarkerEvaluator { marker: 2 }))
            .expect("replacement install");
        assert!(
            evaluators.begin_commit(&capability, Some(first)).is_none(),
            "a superseded registration must not open a commit section",
        );
        assert!(evaluators.begin_commit(&capability, Some(second)).is_some());

        // Close makes it non-current too.
        assert!(evaluators.remove_if_current(&capability, second));
        assert!(
            evaluators.begin_commit(&capability, Some(second)).is_none(),
            "a closed registration must not open a commit section",
        );
    }

    /// The test is total in the other direction as well: a beat produced
    /// with NO evaluator installed must not publish "cannot answer"
    /// about a capability that has since gained one.
    #[test]
    fn an_unevaluable_beat_cannot_commit_once_an_evaluator_appears() {
        let evaluators = ReadinessEvaluators::default();
        let capability = CapabilityId::new("gpu.infer");

        // Nothing installed: the unevaluable beat may publish.
        assert!(evaluators.begin_commit(&capability, None).is_some());

        let id = evaluators
            .install_vacant(capability.clone(), Arc::new(MarkerEvaluator { marker: 3 }))
            .expect("vacant install");
        assert!(
            evaluators.begin_commit(&capability, None).is_none(),
            "a stale 'cannot answer' must not outrank a live evaluator",
        );

        // And once it closes again, it may.
        assert!(evaluators.remove_if_current(&capability, id));
        assert!(evaluators.begin_commit(&capability, None).is_some());
    }

    /// The state-edge seam is id-conditional and does not run the poke
    /// at all for a superseded registration — the currentness test and
    /// the effect are one section, so there is no check-then-poke gap.
    ///
    /// Fails against: poking by capability alone. (The check-then-poke
    /// RACE needs a concurrent rival and is witnessed separately, by
    /// `a_rival_install_cannot_complete_while_a_poke_holds_the_section`.)
    #[test]
    fn poke_if_current_runs_only_for_the_installed_registration() {
        let evaluators = ReadinessEvaluators::default();
        let capability = CapabilityId::new("gpu.infer");
        let old = evaluators
            .install_vacant(capability.clone(), Arc::new(MarkerEvaluator { marker: 1 }))
            .expect("vacant install");

        let pokes = AtomicU64::new(0);
        let poke = || {
            pokes.fetch_add(1, Ordering::Relaxed);
            true
        };

        assert!(evaluators.poke_if_current(&capability, old, poke));
        assert_eq!(pokes.load(Ordering::Relaxed), 1);

        let new = evaluators
            .install_replacing(capability.clone(), Arc::new(MarkerEvaluator { marker: 2 }))
            .expect("replacement install");
        assert!(
            !evaluators.poke_if_current(&capability, old, poke),
            "a superseded registration must not move its successor's schedule",
        );
        assert_eq!(
            pokes.load(Ordering::Relaxed),
            1,
            "the poke must not even run for a superseded registration",
        );

        assert!(evaluators.poke_if_current(&capability, new, poke));
        assert_eq!(pokes.load(Ordering::Relaxed), 2);

        // After close, the successor's own id is inert too.
        assert!(evaluators.remove_if_current(&capability, new));
        assert!(!evaluators.poke_if_current(&capability, new, poke));
        assert_eq!(pokes.load(Ordering::Relaxed), 2);
    }

    /// Bounded wait for an acknowledgement flag. Never unbounded: a
    /// missing acknowledgement is a named FAILURE, not a hang.
    fn await_ack(flag: &AtomicBool, label: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !flag.load(Ordering::Acquire) {
            assert!(
                std::time::Instant::now() < deadline,
                "{label} was never acknowledged within 10s",
            );
            std::thread::yield_now();
        }
    }

    /// Supplementary non-completion window, used ONLY after the
    /// acquisition boundary has already been acknowledged. The
    /// load-bearing proof is the acknowledgement; this only adds "and it
    /// still had not completed".
    const SETTLE: Duration = Duration::from_millis(50);

    /// **The check-then-publish witness.** A rival install must not be
    /// able to complete while a publication holds its commit section.
    ///
    /// The proof is an EXACT acknowledgement, not a timeout: the
    /// registry's contention observer fires only when a transition's
    /// `try_lock` on the ownership mutex actually finds it HELD. So the
    /// witness knows the rival reached the real mutex boundary and was
    /// blocked there. Under check-then-publish the rival's `try_lock`
    /// would SUCCEED, the observer would never fire, and the test fails
    /// on the acknowledgement rather than on scheduling luck.
    ///
    /// Fails against: `begin_commit` that returns no guard, or a guard
    /// that does not exclude install/replace/remove.
    #[test]
    fn a_rival_install_cannot_complete_while_a_publication_holds_the_section() {
        let evaluators = Arc::new(ReadinessEvaluators::default());
        let capability = CapabilityId::new("gpu.infer");
        let id = evaluators
            .install_vacant(capability.clone(), Arc::new(MarkerEvaluator { marker: 1 }))
            .expect("vacant install");

        let contended = Arc::new(AtomicBool::new(false));
        let rival_done = Arc::new(AtomicBool::new(false));
        evaluators.set_contention_hook_for_test(Some({
            let contended = Arc::clone(&contended);
            Arc::new(move || contended.store(true, Ordering::Release))
        }));

        let commit = evaluators
            .begin_commit(&capability, Some(id))
            .expect("the installed registration opens a section");

        let rival = {
            let evaluators = Arc::clone(&evaluators);
            let capability = capability.clone();
            let rival_done = Arc::clone(&rival_done);
            std::thread::spawn(move || {
                evaluators
                    .install_replacing(capability, Arc::new(MarkerEvaluator { marker: 2 }))
                    .expect("replacement install");
                rival_done.store(true, Ordering::Release);
            })
        };

        // (1) the rival reached the REAL ownership-mutex boundary and
        // found it held.
        await_ack(
            &contended,
            "the rival's arrival at the held ownership mutex",
        );
        // (2) supplementary: it still has not completed.
        std::thread::sleep(SETTLE);
        assert!(
            !rival_done.load(Ordering::Acquire),
            "a rival install completed while a publication held the commit section",
        );
        // (3) the publication's view is unchanged for the whole section.
        assert_eq!(marker(&evaluators, &capability), Some(1));

        drop(commit);
        rival.join().expect("rival thread");
        assert!(rival_done.load(Ordering::Acquire));
        assert_eq!(marker(&evaluators, &capability), Some(2));
        assert!(
            evaluators.begin_commit(&capability, Some(id)).is_none(),
            "the old registration is non-current once the rival completed",
        );
        evaluators.set_contention_hook_for_test(None);
    }

    /// **The check-then-poke witness.** Same property on the state-edge
    /// seam, with the same exact acknowledgement: a rival install must
    /// not complete while a poke is running, and must be observed
    /// blocked at the real mutex boundary.
    ///
    /// Under check-then-poke (test the id under the lock, release, then
    /// poke) the rival's `try_lock` succeeds, so the contention observer
    /// never fires and the poke's own assertion fails.
    ///
    /// Fails against: `poke_if_current` that releases the section before
    /// running the poke.
    #[test]
    fn a_rival_install_cannot_complete_while_a_poke_holds_the_section() {
        let evaluators = Arc::new(ReadinessEvaluators::default());
        let capability = CapabilityId::new("gpu.infer");
        let id = evaluators
            .install_vacant(capability.clone(), Arc::new(MarkerEvaluator { marker: 1 }))
            .expect("vacant install");

        let contended = Arc::new(AtomicBool::new(false));
        let rival_done = Arc::new(AtomicBool::new(false));
        let poked = Arc::new(AtomicU64::new(0));
        let rival_start = Arc::new(AtomicBool::new(false));
        evaluators.set_contention_hook_for_test(Some({
            let contended = Arc::clone(&contended);
            Arc::new(move || contended.store(true, Ordering::Release))
        }));

        let rival = {
            let evaluators = Arc::clone(&evaluators);
            let capability = capability.clone();
            let rival_done = Arc::clone(&rival_done);
            let rival_start = Arc::clone(&rival_start);
            std::thread::spawn(move || {
                await_ack(&rival_start, "the poke's request to start the rival");
                evaluators
                    .install_replacing(capability, Arc::new(MarkerEvaluator { marker: 2 }))
                    .expect("replacement install");
                rival_done.store(true, Ordering::Release);
            })
        };

        let ran = evaluators.poke_if_current(&capability, id, || {
            poked.fetch_add(1, Ordering::Relaxed);
            rival_start.store(true, Ordering::Release);
            // The rival must be observed BLOCKED at the real mutex, which
            // can only happen if this poke is running inside the section.
            await_ack(
                &contended,
                "the rival's arrival at the held ownership mutex during a poke",
            );
            std::thread::sleep(SETTLE);
            assert!(
                !rival_done.load(Ordering::Acquire),
                "a rival install completed while a poke held the commit section — \
                 the currentness test and the poke are not one critical section",
            );
            true
        });

        assert!(ran, "the installed registration's poke must run");
        assert_eq!(poked.load(Ordering::Relaxed), 1);
        rival.join().expect("rival thread");

        // Ownership has transferred, and the old id is now inert.
        assert!(!evaluators.poke_if_current(&capability, id, || true));
        assert_eq!(poked.load(Ordering::Relaxed), 1);
        evaluators.set_contention_hook_for_test(None);
    }

    // ---------------------------------------------------------------
    // User destructors never run under the ownership mutex (H7)
    // ---------------------------------------------------------------

    /// An evaluator whose DESTRUCTOR re-enters the real lifecycle.
    ///
    /// This is legitimate user code: an integration may perfectly well
    /// tear down a sibling capability's registration when its own
    /// evaluator is dropped. `commit_mu` is not reentrant, so if a
    /// displaced or removed slot were dropped while the ownership
    /// section were held, this destructor would deadlock the node.
    struct ReentrantOnDrop {
        registry: Arc<ReadinessEvaluators>,
        /// Sibling capability this destructor installs into, proving the
        /// re-entry really reached the lifecycle rather than bailing out.
        sibling: CapabilityId,
        /// Set once the destructor has completed its re-entrant call.
        completed: Arc<AtomicBool>,
    }

    impl ReadinessEvaluator for ReentrantOnDrop {
        fn evaluate(&self, _request: &EvaluationRequest<'_>) -> ReadinessEvaluation {
            ReadinessEvaluation::NotReady { reason: 1 }
        }
    }

    impl Drop for ReentrantOnDrop {
        fn drop(&mut self) {
            // Re-enter the ownership path. If the caller still held
            // `commit_mu`, this blocks forever.
            let _ = self.registry.install_replacing(
                self.sibling.clone(),
                Arc::new(MarkerEvaluator { marker: 77 }),
            );
            self.completed.store(true, Ordering::Release);
        }
    }

    /// Bounded completion for the reentrancy witnesses.
    ///
    /// Deliberately not an unbounded hang: the operation runs on its own
    /// thread and the witness fails with a named assertion if it has not
    /// finished, so a deadlock is a test FAILURE rather than a job that
    /// eats its timeout.
    fn run_bounded(label: &str, body: impl FnOnce() + Send + 'static) {
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let handle = std::thread::spawn(move || {
            body();
            // A send failure means the receiver already gave up; the
            // assertion below is what reports it.
            let _ = done_tx.send(());
        });
        assert!(
            done_rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .is_ok(),
            "{label} did not return within 10s — a user destructor almost \
             certainly deadlocked on the ownership mutex",
        );
        handle.join().expect("worker thread");
    }

    /// **Inverse witness: dropping the displaced slot under
    /// `commit_mu`.**
    ///
    /// A replacement displaces an evaluator whose destructor re-enters
    /// `install_replacing`. The replacement must RETURN, the destructor
    /// must complete its re-entrant call, and the resulting ownership
    /// state must be exactly as specified.
    #[test]
    fn a_replacement_does_not_drop_the_displaced_evaluator_under_the_section() {
        let registry = Arc::new(ReadinessEvaluators::default());
        let capability = CapabilityId::new("gpu.infer");
        let sibling = CapabilityId::new("print.document");
        let completed = Arc::new(AtomicBool::new(false));

        let displaced_id = registry
            .install_vacant(
                capability.clone(),
                Arc::new(ReentrantOnDrop {
                    registry: Arc::clone(&registry),
                    sibling: sibling.clone(),
                    completed: Arc::clone(&completed),
                }),
            )
            .expect("vacant install");

        let successor_id = {
            let registry = Arc::clone(&registry);
            let capability = capability.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            run_bounded(
                "install_replacing over a reentrant-drop evaluator",
                move || {
                    let id = registry
                        .install_replacing(capability, Arc::new(MarkerEvaluator { marker: 9 }))
                        .expect("replacement install");
                    let _ = tx.send(id);
                },
            );
            rx.recv().expect("replacement id")
        };

        assert!(
            completed.load(Ordering::Acquire),
            "the displaced evaluator's destructor never ran its re-entrant call",
        );

        // Ownership state, stated explicitly.
        assert!(registry.is_current(&capability, successor_id));
        assert!(!registry.is_current(&capability, displaced_id));
        assert_eq!(marker(&registry, &capability), Some(9));
        // The destructor's own re-entrant install really landed.
        assert_eq!(marker(&registry, &sibling), Some(77));
        assert_eq!(registry.len(), 2);
        // And the section is usable again afterwards.
        assert!(registry
            .begin_commit(&capability, Some(successor_id))
            .is_some());
    }

    /// **Inverse witness: dropping the removed slot under `commit_mu`.**
    ///
    /// The same property on the removal edge: `remove_if_current` must
    /// return, the destructor must complete, and the capability must end
    /// up unserved.
    #[test]
    fn a_removal_does_not_drop_the_removed_evaluator_under_the_section() {
        let registry = Arc::new(ReadinessEvaluators::default());
        let capability = CapabilityId::new("gpu.infer");
        let sibling = CapabilityId::new("print.document");
        let completed = Arc::new(AtomicBool::new(false));

        let id = registry
            .install_vacant(
                capability.clone(),
                Arc::new(ReentrantOnDrop {
                    registry: Arc::clone(&registry),
                    sibling: sibling.clone(),
                    completed: Arc::clone(&completed),
                }),
            )
            .expect("vacant install");

        let removed = {
            let registry = Arc::clone(&registry);
            let capability = capability.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            run_bounded(
                "remove_if_current over a reentrant-drop evaluator",
                move || {
                    let removed = registry.remove_if_current(&capability, id);
                    let _ = tx.send(removed);
                },
            );
            rx.recv().expect("removal outcome")
        };

        assert!(removed, "the live registration must be removed");
        assert!(
            completed.load(Ordering::Acquire),
            "the removed evaluator's destructor never ran its re-entrant call",
        );

        // Ownership state, stated explicitly: the capability is unserved,
        // the stale id is inert, and the destructor's sibling install
        // landed.
        assert!(registry.installed(&capability).is_none());
        assert!(!registry.remove_if_current(&capability, id));
        assert_eq!(marker(&registry, &sibling), Some(77));
        assert_eq!(registry.len(), 1);
        // A fresh registration can take the vacated capability.
        let successor = registry
            .install_vacant(capability.clone(), Arc::new(MarkerEvaluator { marker: 3 }))
            .expect("vacated install");
        assert_ne!(successor, id);
    }

    /// The same guarantee for the LAST reference held by the emitter.
    ///
    /// `installed()` hands out an `Arc` clone; if a close then removes
    /// the registry's copy, the emitter's clone is the final one and its
    /// `Drop` runs wherever the emitter drops it — which must be outside
    /// any section. Here the registry's own drop order is checked: the
    /// clone outlives `remove_if_current`, so the destructor runs at the
    /// end of this test, not inside the removal.
    #[test]
    fn a_removal_leaves_a_live_arc_clone_to_drop_outside_the_section() {
        let registry = Arc::new(ReadinessEvaluators::default());
        let capability = CapabilityId::new("gpu.infer");
        let sibling = CapabilityId::new("print.document");
        let completed = Arc::new(AtomicBool::new(false));

        let id = registry
            .install_vacant(
                capability.clone(),
                Arc::new(ReentrantOnDrop {
                    registry: Arc::clone(&registry),
                    sibling: sibling.clone(),
                    completed: Arc::clone(&completed),
                }),
            )
            .expect("vacant install");
        // The emitter's snapshot: a live clone across the removal.
        let (snapshot_id, snapshot) = registry.installed(&capability).expect("installed");
        assert_eq!(snapshot_id, id);

        run_bounded("remove_if_current with a live Arc clone outstanding", {
            let registry = Arc::clone(&registry);
            let capability = capability.clone();
            move || {
                assert!(registry.remove_if_current(&capability, id));
            }
        });
        assert!(
            !completed.load(Ordering::Acquire),
            "the destructor must NOT have run yet — the emitter's clone is still live",
        );

        // Releasing the last clone runs the destructor here, with no
        // section held, and its re-entrant install succeeds.
        drop(snapshot);
        assert!(
            completed.load(Ordering::Acquire),
            "releasing the final clone must run the destructor",
        );
        assert_eq!(marker(&registry, &sibling), Some(77));
    }

    // ---------------------------------------------------------------
    // Registration-identity exhaustion (H3)
    // ---------------------------------------------------------------

    /// The last issuable id still installs; one more is terminal.
    ///
    /// Fails against: a wrapping `fetch_add` (which would keep issuing
    /// ids forever, aliasing old ones) and against a saturating
    /// allocator that reuses `u64::MAX`.
    #[test]
    fn the_final_issuable_id_installs_and_the_next_vacancy_is_terminal() {
        let evaluators = ReadinessEvaluators::default();
        let a = CapabilityId::new("gpu.infer");
        let b = CapabilityId::new("print.document");
        evaluators.set_next_id_for_test(ReadinessEvaluators::max_issuable_id_for_test());
        assert!(!evaluators.identities_exhausted());

        let last = evaluators
            .install_vacant(a.clone(), Arc::new(MarkerEvaluator { marker: 1 }))
            .expect("the last issuable id installs");
        assert!(evaluators.identities_exhausted());

        assert_eq!(
            evaluators.install_vacant(b.clone(), Arc::new(MarkerEvaluator { marker: 2 })),
            Err(EvaluatorInstallRefusal::IdentityExhausted),
        );
        assert_eq!(evaluators.len(), 1, "no new registration was published");
        assert_eq!(marker(&evaluators, &a), Some(1));
        assert!(evaluators.installed(&b).is_none());

        // Terminal: repeated attempts stay refused and never step past
        // the sentinel into reuse.
        assert_eq!(
            evaluators.install_vacant(b.clone(), Arc::new(MarkerEvaluator { marker: 3 })),
            Err(EvaluatorInstallRefusal::IdentityExhausted),
        );

        // Removal remains available — a node that cannot install must
        // still be able to stop serving.
        assert!(evaluators.remove_if_current(&a, last));
        assert!(evaluators.is_empty());
        // ...but the vacated slot still cannot be refilled.
        assert_eq!(
            evaluators.install_vacant(a.clone(), Arc::new(MarkerEvaluator { marker: 4 })),
            Err(EvaluatorInstallRefusal::IdentityExhausted),
        );
    }

    /// Exhaustion refuses REPLACEMENT too, and leaves the incumbent
    /// serving: superseding a live registration with an un-ownable one
    /// would be strictly worse than refusing.
    #[test]
    fn exhaustion_refuses_replacement_and_leaves_the_incumbent_serving() {
        let evaluators = ReadinessEvaluators::default();
        let capability = CapabilityId::new("gpu.infer");
        let incumbent = evaluators
            .install_vacant(capability.clone(), Arc::new(MarkerEvaluator { marker: 1 }))
            .expect("vacant install");
        evaluators.set_next_id_for_test(u64::MAX);
        assert!(evaluators.identities_exhausted());

        assert_eq!(
            evaluators
                .install_replacing(capability.clone(), Arc::new(MarkerEvaluator { marker: 2 })),
            Err(EvaluatorInstallRefusal::IdentityExhausted),
        );
        assert!(evaluators.is_current(&capability, incumbent));
        assert_eq!(marker(&evaluators, &capability), Some(1));
        assert!(
            evaluators
                .begin_commit(&capability, Some(incumbent))
                .is_some(),
            "the incumbent must keep publishing after exhaustion",
        );
    }

    /// The non-aliasing property the whole ownership scheme rests on:
    /// every id the allocator ever issues is distinct, and at the
    /// boundary it STOPS rather than continuing into reuse.
    ///
    /// The stopping half is what makes non-aliasing true — an allocator
    /// that kept issuing would eventually hand a live registration the
    /// value a long-closed handle still holds. So the witness walks the
    /// last two issuable ids, checks every id ever issued is unique, and
    /// then requires a refusal.
    ///
    /// Fails against: `fetch_add`, which returns `Ok` here instead of
    /// stopping and later reissues id 0.
    #[test]
    fn every_issued_id_is_distinct_and_the_allocator_stops_at_the_boundary() {
        let evaluators = ReadinessEvaluators::default();
        let capability = CapabilityId::new("gpu.infer");
        let other = CapabilityId::new("print.document");
        let mut issued = Vec::new();

        // The first id ever issued, held by a long-closed handle.
        let ancient = evaluators
            .install_vacant(capability.clone(), Arc::new(MarkerEvaluator { marker: 1 }))
            .expect("vacant install");
        issued.push(ancient);
        assert!(evaluators.remove_if_current(&capability, ancient));

        // Two ids left before the boundary. Both must issue, and both
        // must differ from each other and from the ancient one.
        evaluators.set_next_id_for_test(ReadinessEvaluators::max_issuable_id_for_test() - 1);
        let penultimate = evaluators
            .install_vacant(capability.clone(), Arc::new(MarkerEvaluator { marker: 2 }))
            .expect("penultimate id issues");
        issued.push(penultimate);
        assert!(evaluators.remove_if_current(&capability, penultimate));
        let last = evaluators
            .install_vacant(capability.clone(), Arc::new(MarkerEvaluator { marker: 3 }))
            .expect("last issuable id issues");
        issued.push(last);

        let mut unique = issued.clone();
        unique.sort_by_key(|id| format!("{id:?}"));
        unique.dedup_by_key(|id| format!("{id:?}"));
        assert_eq!(
            unique.len(),
            issued.len(),
            "the allocator issued the same id twice: {issued:?}",
        );

        // ...and now it stops. A wrapping allocator would keep going and
        // eventually reissue `ancient`.
        assert!(evaluators.identities_exhausted());
        assert_eq!(
            evaluators.install_vacant(other.clone(), Arc::new(MarkerEvaluator { marker: 4 })),
            Err(EvaluatorInstallRefusal::IdentityExhausted),
            "the allocator continued past its last issuable id",
        );
        assert!(evaluators.installed(&other).is_none());

        // The ancient handle is inert on every edge against the live
        // registration.
        assert!(!evaluators.remove_if_current(&capability, ancient));
        assert!(evaluators
            .begin_commit(&capability, Some(ancient))
            .is_none());
        assert!(!evaluators.poke_if_current(&capability, ancient, || true));
        assert_eq!(marker(&evaluators, &capability), Some(3));
    }

    // ---------------------------------------------------------------
    // Public-surface guard (H4)
    // ---------------------------------------------------------------

    /// The public surface of this module is the pre-existing frozen
    /// evaluator contract PLUS exactly two hidden, workspace-internal
    /// SDK bridges. The registry, its storage, its mutation methods, and
    /// the publication fence must stay crate-internal so none of them
    /// becomes API and the storage choice stays free.
    ///
    /// The two bridge items are allowlisted as *reachable*, not as
    /// *supported*: `the_sdk_bridges_are_hidden_and_marked_unstable`
    /// separately requires each of them to carry `#[doc(hidden)]` and
    /// the unstable workspace-internal wording.
    ///
    /// Non-vacuous by construction: it reads this module's own source
    /// and the re-export list, so a new public ITEM or a widened
    /// re-export fails it rather than silently expanding the surface.
    /// Scoped to items rather than struct fields — a new field on an
    /// allowlisted type is a change to that type's own frozen SI-0
    /// contract, guarded by the wire/golden fixtures, not by this.
    #[test]
    fn the_public_surface_of_this_module_is_exactly_the_allowlist() {
        const ALLOWED: &[&str] = &[
            // Pre-existing frozen contract (SI-0).
            "pub const DEFAULT_ATTESTATION_CADENCE_FLOOR",
            "pub struct EvaluationRequest",
            "pub enum ReadinessEvaluation",
            "pub enum StatusReason",
            "pub const fn project_evaluation",
            "pub trait ReadinessEvaluator",
            "pub struct CadenceRefusal",
            "pub const fn as_status",
            "pub const fn check_cadence",
            "pub struct SensingCounters",
            "pub fn get",
            "pub fn validate_interest_constraints",
            // S0 item 7: reachable across the crate boundary, but NOT
            // supported API — each must be `#[doc(hidden)]` and marked
            // unstable/workspace-internal (guarded separately by
            // `the_sdk_bridges_are_hidden_and_marked_unstable`).
            "pub struct EvaluatorRegistrationId",
            "pub enum EvaluatorInstallRefusal",
        ];

        /// Item-declaration keywords that can follow `pub`. Anything
        /// else after `pub ` is a struct field.
        const ITEM_KEYWORDS: &[&str] = &[
            "struct",
            "enum",
            "trait",
            "fn",
            "const",
            "type",
            "mod",
            "use",
            "static",
            "unsafe",
            "async",
            "extern",
            "union",
            "macro_rules!",
        ];

        let source = include_str!("evaluator.rs");
        let declared: Vec<String> = source
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("pub ") && !line.starts_with("pub(crate)"))
            .filter(|line| {
                line.split_whitespace()
                    .nth(1)
                    .is_some_and(|second| ITEM_KEYWORDS.contains(&second))
            })
            .map(|line| {
                // Keep the declaration head only: everything up to the
                // name, so signatures and field lists do not matter.
                line.split(['(', '<', ':', '{', ';'])
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string()
            })
            .filter(|head| !head.is_empty())
            .collect();

        for head in &declared {
            assert!(
                ALLOWED.contains(&head.as_str()),
                "undeclared public item `{head}` in the sensing evaluator module — \
                 either make it crate-internal or add it to the reviewed allowlist",
            );
        }
        for allowed in ALLOWED {
            assert!(
                declared.iter().any(|head| head == allowed),
                "allowlisted public item `{allowed}` disappeared — update the allowlist \
                 in the same commit so surface loss is deliberate",
            );
        }

        // The registry itself must not be re-exported publicly.
        let re_exports = include_str!("mod.rs");
        let evaluator_block = re_exports
            .split("pub use evaluator::{")
            .nth(1)
            .and_then(|rest| rest.split('}').next())
            .expect("the evaluator re-export block must exist");
        assert!(
            !evaluator_block.contains("ReadinessEvaluators"),
            "the evaluator registry must not be publicly re-exported",
        );
        assert!(
            re_exports.contains("pub(crate) use evaluator::ReadinessEvaluators;"),
            "the registry must stay a crate-internal re-export",
        );
    }

    /// Every core item that is public ONLY because `net-mesh-sdk` is a
    /// separate crate, or ONLY so the workspace's own suites can reach
    /// it, must be `#[doc(hidden)]` AND carry the wording that says so.
    /// Reachability is unavoidable; being mistaken for supported API is
    /// not.
    ///
    /// Two inventories, with DISTINCT required sentences:
    ///
    /// - **production bridges** — public in every build, reachable by
    ///   any dependent, carrying the workspace-internal SDK wording;
    /// - **fixtures-only bridges** — gated on `cfg(test)` or the
    ///   `fixtures` feature, but still `pub` whenever a dependency
    ///   enables that feature, so they show up in all-features builds
    ///   and rustdoc. They carry the fixtures-only wording, and the
    ///   guard also requires the cfg gate to still be there.
    ///
    /// Because the two sentences differ, annotating the production list
    /// cannot satisfy the fixtures list; and the fixtures inventory
    /// carries a reviewed count, so dropping a method from it fails
    /// rather than silently narrowing the guard.
    ///
    /// Non-vacuous by construction: it reads the real source of both
    /// modules, locates each declaration, walks back over its contiguous
    /// doc/attribute block, and requires every marker. Losing any one
    /// fails it.
    ///
    /// `MeshNode::sensing_origin_active` is deliberately absent: it
    /// predates this slice and is read by the crate's own integration
    /// suites as a plain observability query, so it is not public solely
    /// for the SDK.
    #[test]
    fn the_sdk_bridges_are_hidden_and_marked_unstable() {
        /// The exact sentence every production bridge must carry, so the
        /// guard cannot be satisfied by vague prose.
        const UNSTABLE: &str = "Unstable, workspace-internal SDK bridge; not supported core API.";
        /// The exact sentence every fixtures-only bridge must carry.
        /// Deliberately different from `UNSTABLE`, so the two inventories
        /// cannot satisfy each other.
        const FIXTURES_ONLY: &str = "Unstable fixtures-only test bridge; not supported core API.";
        /// The cfg gate a fixtures-only bridge must keep.
        const FIXTURES_CFG: &str = "#[cfg(any(test, feature = \"fixtures\"))]";

        let mesh = include_str!("../../mesh.rs");
        let evaluator = include_str!("evaluator.rs");
        let bridges: &[(&str, &str)] = &[
            (mesh, "pub fn register_readiness_evaluator("),
            (mesh, "pub fn replace_readiness_evaluator("),
            (mesh, "pub fn unregister_readiness_evaluator("),
            (mesh, "pub fn notify_sensing_state_changed_owned("),
            (mesh, "pub fn sensing_enabled("),
            (mesh, "pub fn sensing_identity_is_durable("),
            (evaluator, "pub struct EvaluatorRegistrationId("),
            (evaluator, "pub enum EvaluatorInstallRefusal {"),
        ];
        // The reviewed fixtures-only inventory. The count is part of the
        // contract: removing an entry to make this guard pass fails it.
        let fixture_bridges: &[&str] = &[
            "pub fn sensing_evaluator_count(",
            "pub fn sensing_evaluator_identities_exhausted(",
            "pub fn set_sensing_evaluator_next_id_for_test(",
            "pub fn sensing_max_registration_id_for_test(",
            "pub fn set_sensing_commit_pause_hook_for_test(",
            "pub fn set_sensing_ownership_contention_hook_for_test(",
        ];
        assert_eq!(
            fixture_bridges.len(),
            6,
            "the fixtures-only inventory must list all six bridges; dropping one \
             would hide it from this guard instead of annotating it",
        );

        /// The contiguous doc/attribute block immediately above a
        /// declaration. The split leaves the declaration's own
        /// indentation as a trailing fragment, so skip empties before
        /// collecting.
        fn attribute_block(source: &str, declaration: &str) -> Option<String> {
            let before = source.split(declaration).next()?;
            if before.len() == source.len() {
                return None;
            }
            let block: Vec<&str> = before
                .lines()
                .rev()
                .map(str::trim)
                .skip_while(|line| line.is_empty())
                .take_while(|line| line.starts_with("///") || line.starts_with("#["))
                .collect();
            Some(block.join("\n"))
        }

        for (source, declaration) in bridges {
            let block = attribute_block(source, declaration).unwrap_or_else(|| {
                panic!(
                    "bridge declaration `{declaration}` not found — if it was renamed, \
                     rename it here in the same commit",
                )
            });
            assert!(
                block.contains("#[doc(hidden)]"),
                "bridge `{declaration}` is missing #[doc(hidden)] — it would appear \
                 as supported core API in rustdoc",
            );
            assert!(
                block.contains(UNSTABLE),
                "bridge `{declaration}` is missing the exact wording {UNSTABLE:?} — \
                 a reader must be told it is not supported core API",
            );
        }

        for declaration in fixture_bridges {
            let block = attribute_block(mesh, declaration).unwrap_or_else(|| {
                panic!(
                    "fixtures-only bridge `{declaration}` not found — if it was \
                     renamed, rename it here in the same commit",
                )
            });
            assert!(
                block.contains("#[doc(hidden)]"),
                "fixtures-only bridge `{declaration}` is missing #[doc(hidden)] — a \
                 dependency enabling `fixtures` would surface it as core API",
            );
            assert!(
                block.contains(FIXTURES_ONLY),
                "fixtures-only bridge `{declaration}` is missing the exact wording \
                 {FIXTURES_ONLY:?}",
            );
            assert!(
                block.contains(FIXTURES_CFG),
                "fixtures-only bridge `{declaration}` lost its {FIXTURES_CFG} gate — \
                 it would become unconditionally public",
            );
        }

        // ...and the guard must not pass by the marker being everywhere:
        // the frozen evaluator-contract types are NOT bridges.
        for supported in [
            "pub struct EvaluationRequest {",
            "pub enum ReadinessEvaluation {",
            "pub trait ReadinessEvaluator {",
        ] {
            let block = attribute_block(evaluator, supported)
                .unwrap_or_else(|| panic!("`{supported}` not found"));
            assert!(
                !block.contains("#[doc(hidden)]"),
                "`{supported}` is part of the SUPPORTED evaluator contract and must \
                 not be hidden merely because the SDK re-exports it",
            );
        }
    }
}
