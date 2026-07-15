//! MeshOS ↔ Scheduler integration bridge.
//!
//! The single module that sees *both* halves of the integration:
//! `cortex::workflow` (the task-lifecycle `WorkflowState`) and
//! `behavior::meshos` (`DesiredState` / `DaemonIntent`), plus
//! `behavior::gang` (`ActiveClaim` / `IslandTopology`). It exists so
//! `workflow`/`gang` and `meshos` never import each other (integration
//! plan Locked Decision 5): every cross-layer connection is a pure
//! projection at the desired/observed boundary that lives *here* — no
//! I/O, no calls back into either layer.
//!
//! Projections (see `docs/plans/MESHOS_SCHEDULER_INTEGRATION_PLAN.md`):
//! - 1 — task → daemon intent (`project_daemon_intents`). Implemented.
//! - 2 — claim → forced placement (`project_forced_placements`, backed
//!   by `ClaimRegistry`). Implemented — closes Phase A.
//! - 3 — daemon lifecycle → step state (`apply_lifecycle` +
//!   `build_daemon_task_map`). Implemented — `Trigger::AfterTerminal`
//!   already exists in `workflow/trigger.rs`, so the gate is lifted.
//! - 4 — observed liveness → fold-update delta (`project_liveness`) +
//!   the `gang::match_islands` host-prune applier (via
//!   `MeshNode::set_liveness_down`). Implemented; per-tick wiring deferred.
//! - 5 — migration veto (`migrate` / `MigrationEligible`, type-enforced
//!   via `ClaimRegistry::holds_exclusive`). Implemented.
//! - 6 — sensed capability readiness → per-interest candidate delta
//!   (`project_sensed_candidates`, sensing plan SI-6) + the
//!   `gang::match_islands_sensed` applier (via
//!   `MeshNode::match_islands_sensed`). Implemented.

#[cfg(all(feature = "cortex", feature = "meshos"))]
mod claim_registry;
#[cfg(all(feature = "cortex", feature = "meshos"))]
mod daemon_ref;
#[cfg(all(feature = "cortex", feature = "meshos"))]
mod driver;
#[cfg(all(feature = "cortex", feature = "meshos"))]
mod lifecycle;
#[cfg(all(feature = "cortex", feature = "meshos"))]
mod liveness;
#[cfg(all(feature = "cortex", feature = "meshos"))]
mod migration;
#[cfg(all(feature = "cortex", feature = "meshos"))]
mod projection;
// Projection 6 stands alone: fold + sensing only (SI-6) — available
// wherever the sensing plane is, no cortex/meshos required.
mod readiness;
#[cfg(all(feature = "cortex", feature = "meshos"))]
mod runtime;

#[cfg(all(feature = "cortex", feature = "meshos"))]
pub use claim_registry::ClaimRegistry;
#[cfg(all(feature = "cortex", feature = "meshos"))]
pub use daemon_ref::{daemon_ref, daemon_ref_shard};
#[cfg(all(feature = "cortex", feature = "meshos"))]
pub use driver::{fan_out_lifecycle, SchedulerBridgeDriver, TickReport};
#[cfg(all(feature = "cortex", feature = "meshos"))]
pub use lifecycle::{apply_lifecycle, build_daemon_task_map, LifecycleTransition};
#[cfg(all(feature = "cortex", feature = "meshos"))]
pub use liveness::{project_liveness, project_liveness_from_snapshot, LivenessDelta};
#[cfg(all(feature = "cortex", feature = "meshos"))]
pub use migration::{migrate, ClaimHeld, MigrationEligible, MigrationPlan};
#[cfg(all(feature = "cortex", feature = "meshos"))]
pub use projection::{project_daemon_intents, project_forced_placements};
pub use readiness::{project_sensed_candidates, SensedCandidates};
#[cfg(all(feature = "cortex", feature = "meshos"))]
pub use runtime::{desired_daemon_intents, SchedulerBridge};
