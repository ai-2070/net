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

mod claim_registry;
mod daemon_ref;
mod lifecycle;
mod liveness;
mod migration;
mod projection;

pub use claim_registry::ClaimRegistry;
pub use daemon_ref::{daemon_ref, daemon_ref_shard};
pub use lifecycle::{apply_lifecycle, build_daemon_task_map, LifecycleTransition};
pub use liveness::{project_liveness, LivenessDelta};
pub use migration::{migrate, ClaimHeld, MigrationEligible, MigrationPlan};
pub use projection::{project_daemon_intents, project_forced_placements};
