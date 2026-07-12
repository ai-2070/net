//! Capability sensing — SI-0 semantic spike.
//!
//! `docs/plans/SENSING_INTEREST_COALESCING_PLAN.md` (v4.1). The
//! product primitive is existential — "can any authorized provider
//! currently satisfy capability Y under characteristics C and
//! latency envelope L?" — with the provider identity in the answer,
//! and explicit provider surveillance (a node, a group, a
//! tag-selected population) as the operator override. Two layers:
//! a LOCAL capability sensing controller (interest identity,
//! candidate resolution, bounded exploration, result-mode
//! aggregation) over the routed provider-readiness protocol
//! (provider-targeted interests along `next_hop(provider)`, per-hop
//! coalescing, signed attestations, continuity).
//!
//! This module freezes that semantic model — the two-level identity
//! and digest, incarnation-scoped ordering, the continuity state
//! machine and its projection table, the evaluator contract, the
//! per-hop interest table, and the relay delivery rules — together
//! with the SI-0 test matrix (plan §6, tests 7–23) — BEFORE any wire
//! format or subprotocol id is committed.
//!
//! SI-0 is deliberately **in-process**: nothing here is reachable
//! from `MeshNode` dispatch, no subprotocol ids are consumed, and
//! the 0x0C02/0x0C03 reservations stay uncommitted until the SI-1
//! gate conditions (plan §6, items (a)–(p)) hold. The wire shapes in
//! plan §4.2 serialize these types in SI-1; changing a type here
//! before SI-1 lands is cheap, changing it after is a wire break.

pub mod continuity;
pub mod controller;
pub mod delivery;
pub mod evaluator;
pub mod identity;
pub mod incarnation;
pub mod table;

pub use controller::{
    population_is_boundable, project_aggregate, resolve_candidates, AggregateView, BranchView,
    CandidatePolicy, CandidateProvider, ResolutionRefusal, ResolvedCandidates, TagAssertion,
};

pub use delivery::{Attestation, Delivery, SensingConsumer, SensingRelay};

pub use table::{
    DownstreamEntry, DownstreamId, InterestTable, RefusalPartition, RegisterOutcome, UpstreamAction,
};

pub use evaluator::{
    check_cadence, project_evaluation, validate_interest_constraints, CadenceRefusal,
    EvaluationRequest, ReadinessEvaluation, ReadinessEvaluator, SensingCounters, StatusReason,
    DEFAULT_ATTESTATION_CADENCE_FLOOR,
};

pub use continuity::{
    project, AttestedStatus, Continuity, DeliveredBeat, DisruptReason, ObservationCell,
    ProjectedReadiness, ReadinessObservation,
};

pub use incarnation::{
    next_incarnation, Admission, Incarnation, IncarnationError, IncarnationPersistence,
    IncarnationSeqGate, PersistenceFault,
};

pub use identity::{
    strictest_sample_interval, AudienceScopeCommitment, CanonicalConstraints, CapabilityId,
    CapabilityInterestKey, ConstraintError, ConsumerLatencyBudget, Digest256, DisclosureClass,
    GroupRef, InterestRegistration, InterestSpec, ProviderInterestKey, ProviderObservationKey,
    ProviderSelector, ResultMode, TagMatch, WorkLatencyEnvelope, INTEREST_DIGEST_DOMAIN,
    MAX_CONSTRAINT_BYTES,
};
