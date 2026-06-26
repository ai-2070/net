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
//! - 3 — daemon lifecycle → step state.
//! - 4 — observed liveness → topology + capability aging.
//! - 5 — migration veto (`ClaimRegistry::holds_exclusive`).

mod claim_registry;
mod daemon_ref;
mod projection;

pub use claim_registry::ClaimRegistry;
pub use daemon_ref::{daemon_ref, daemon_ref_shard};
pub use projection::{project_daemon_intents, project_forced_placements};
