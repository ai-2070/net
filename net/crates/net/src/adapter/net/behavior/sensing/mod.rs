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
//! SI-0 was deliberately **in-process**: nothing was reachable from
//! `MeshNode` dispatch and no subprotocol ids were consumed until
//! the SI-1 gate conditions (plan §6, items (a)–(s)) held. The v4.3
//! gates (r)/(s) added the two-stage frame SHAPES (`frames.rs` —
//! semantic, serde-serializable) and the leader-side frame intake
//! with digest re-derivation + routed-origin authority
//! (`SensingLeader::register_from_frame`,
//! `tests/sensing_routed_origin.rs`).
//!
//! **SI-1 (review 7 sign-off)** commits the wire: `wire.rs` owns the
//! 0x0C02/0x0C03 subprotocol ids, the frozen postcard codec (strict
//! decode, 4 KiB cap), attestation signing + verification over the
//! §4.2 transcript, and the wire→semantic attestation bridge.
//! `frames.rs` gained the review-7 `ProviderRegistration` amendment
//! (selector/mode/class carried for COMPLETE digest verification)
//! and the shared intake pipeline both legs run
//! (`SensingInterestFrame::validated_spec`). Changing any wire-borne
//! type from here on is a wire break.

pub mod continuity;
pub mod controller;
pub mod delivery;
// SI-3: the origin-side emission scheduler — pure and crypto-free;
// the mesh signs what it produces.
pub mod emitter;
pub mod evaluator;
pub mod frames;
pub mod identity;
pub mod incarnation;
pub mod negotiation;
// The rendezvous REUSES the RedEX election (plan §4.1, review 6) —
// the reuse is real, not copied, so the module rides the `redex`
// feature. SI-2 revisits the final layering.
#[cfg(feature = "redex")]
pub mod rendezvous;
pub mod scope;
// SI-2b: the Layer-1 candidate snapshot over the real capability
// fold + proximity + routing planes (plan §4.7/§4.10) — the
// resolver inputs `MeshNode::sensing_candidate_snapshot` assembles.
pub mod snapshot;
pub mod table;
pub mod wire;

pub use frames::{FrameSpecError, SensingInterestFrame, ValidatedProviderRegistration};
pub use negotiation::{select_sensing_path, SensingPath, SENSING_CAPABILITY_TAG};
#[cfg(feature = "redex")]
pub use rendezvous::{
    closeness_score, sensing_leader, FrameRejection, LeaderRegistration, SensingLeader,
};
pub use scope::{validate_subscriber_scope, ScopeError};
pub use wire::{
    decode_attestation, decode_interest_frame, encode_attestation, encode_interest_frame,
    semantic_attestation, sign_attestation, verify_attestation, AttestationBridgeError,
    AttestationSignError, AttestationVerifyError, ReadinessAttestation, UnsignedAttestation,
    WireError, ATTESTATION_SIG_DOMAIN, MAX_SENSING_FRAME_BYTES, SUBPROTOCOL_READINESS_ATTESTATION,
    SUBPROTOCOL_SENSING_INTEREST,
};

pub use controller::{
    population_is_boundable, project_aggregate, resolve_candidates, AggregateView, BranchView,
    CandidatePolicy, CandidateProvider, ResolutionRefusal, ResolvedCandidates, TagAssertion,
};

pub use delivery::{Attestation, Delivery, SensingConsumer, SensingRelay};

pub use emitter::{OriginEmitter, MAX_STREAM_SLOTS, STREAM_SLOTS_LOW_WATER};

pub use snapshot::{
    build_candidate_snapshot, declares_capability, extract_declarers, proximity_route_estimate,
    DeclaredProvider, HOP_FALLBACK_ESTIMATE, UNKNOWN_ROUTE_ESTIMATE,
};

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
