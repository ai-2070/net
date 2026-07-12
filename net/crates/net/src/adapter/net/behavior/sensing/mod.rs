//! Sensing-interest coalescing — SI-0 semantic spike.
//!
//! `docs/plans/SENSING_INTEREST_COALESCING_PLAN.md` (v3.1). Readiness
//! is a conditional relation — (provider, capability, work
//! characteristics C, latency envelope L) → Ready | NotReady |
//! Unknown — not a property of a capability entry. This module
//! freezes that semantic model: the identity types and interest
//! digest, incarnation-scoped ordering, the continuity state machine
//! and its projection table, the evaluator contract, the per-hop
//! interest table, and the relay delivery rules — together with the
//! SI-0 test matrix (plan §6, tests 7–15) — BEFORE any wire format
//! or subprotocol id is committed.
//!
//! SI-0 is deliberately **in-process**: nothing here is reachable
//! from `MeshNode` dispatch, no subprotocol ids are consumed, and the
//! 0x0C02/0x0C03 reservations stay uncommitted until the SI-1 gate
//! conditions (plan §6, items (a)–(j)) hold. The wire shapes in the
//! plan §4.2 serialize these types in SI-1; changing a type here
//! before SI-1 lands is cheap, changing it after is a wire break.

pub mod continuity;
pub mod evaluator;
pub mod identity;
pub mod incarnation;
pub mod table;

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
    ConstraintError, Digest256, DisclosureClass, InterestRegistration, InterestSpec, ReadinessKey,
    WorkLatencyEnvelope, INTEREST_DIGEST_DOMAIN, MAX_CONSTRAINT_BYTES,
};
