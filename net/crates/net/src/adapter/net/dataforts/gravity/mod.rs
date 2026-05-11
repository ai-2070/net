//! Data gravity — heat-counter annotations on `causal:` chain
//! tags (Rebel Yell Phase 4).
//!
//! Once Phases 0 + 1 ship, the mesh has the substrate to observe
//! which chains are most-read. Phase 4 closes the loop: per-chain
//! read-rate counters with exponential decay, throttled emission
//! of `heat:<hex>=<rate>` tags onto the existing capability-
//! announcement path, and a preference function that the greedy
//! runtime consults to weight cache pulls by heat × scope-match
//! × proximity-rank. Cold chains evict first under LRU pressure;
//! hot chains gravitate toward the readers that drive the heat.
//!
//! **No separate migration engine.** Two primitives compose into
//! the desired property:
//!
//! 1. Phase 0 advertises chains as capability tags.
//! 2. Phase 1 pulls chains within scope + proximity + budget.
//!
//! Adding a heat counter to (1) and a heat-weighted preference in
//! (2) gets gravity for free. See
//! `docs/misc/DATAFORTS_PLAN.md` § Phase 4 for the locked design
//! decisions.
//!
//! Pure-logic pieces (counter decay + emission decision + policy
//! validation) live here. The runtime / mesh integration + heat
//! tag wire emission land in subsequent slices.

mod counter;
mod policy;
mod sink;

pub use counter::{HeatCounter, HeatEmission, HeatRegistry};
pub use policy::{
    should_emit_heat, DataGravityPolicy, DataGravityPolicyError, EmissionDecision,
    DEFAULT_DECAY_HALF_LIFE_SECS, DEFAULT_EMIT_THRESHOLD_RATIO, MAX_EMIT_THRESHOLD_RATIO,
    MIN_EMIT_THRESHOLD_RATIO,
};
pub use sink::HeatSink;
