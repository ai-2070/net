//! MeshDB — federated query layer above the capability-query
//! primitives + CortEX folds.
//!
//! Phase A of [`MESHDB_PLAN.md`](../../../../../docs/plans/MESHDB_PLAN.md).
//! Lands the typed [`MeshQuery`] AST + supporting types + the
//! [`MeshError`] enum. The planner + executor follow in Phases A
//! (atomic operators) → B (time-travel) → C..F (lineage, joins,
//! aggregates, windowing, bindings).
//!
//! # Surface map
//!
//! - [`query`] — `MeshQuery::V1(QueryV1)` versioned AST + the
//!   supporting types (`ChainRef`, `SeqNum`, `JoinKey`,
//!   `AggregateFn`, etc.). Closed under composition;
//!   serde-round-trippable via postcard + JSON per the plan's
//!   locked decision #1 (AST stability).
//! - [`error`] — `MeshError` typed-error surface for planner
//!   + executor errors.
//! - [`planner`] — `MeshQueryPlanner::plan(query)` returns an
//!   `ExecutionPlan` tree the executor walks. Phase A handles
//!   atomic operators (`At` / `Between` / `Latest`); other
//!   variants surface `MeshError::PlannerError` until their
//!   phase activates.
//!
//! # AST versioning (locked decision #1)
//!
//! The outer enum is explicitly versioned:
//!
//! ```rust
//! # use net::adapter::net::behavior::meshdb::{MeshQuery, QueryV1, ChainRef, SeqNum};
//! let _ = MeshQuery::V1(QueryV1::Latest {
//!     origin: ChainRef::OriginHash([0; 32]),
//! });
//! ```
//!
//! - Unknown versions reject cleanly at decode time
//!   (`MeshError::PlannerError { detail: "unsupported query
//!   version" }`).
//! - Adding a new operator variant inside an existing `Vn` is a
//!   non-bump if the new operator is optional and old planners
//!   reject unknown variants cleanly. `QueryV1` is
//!   `#[non_exhaustive]` so additions are non-breaking source-
//!   side; serde-side, postcard's varint discriminant + the
//!   plan's "reject unknown variants cleanly" contract are the
//!   load-bearing pieces.
//!
//! # Activation
//!
//! Gated behind the `meshdb` Cargo feature. Disabled by default;
//! activation requires a concrete consumer workload (Hermes
//! telemetry + Deck metrics are the named candidates per the
//! plan's Status). Until a consumer drives semantics (default
//! watermark, sketch parameters, common query shapes), Phase A's
//! AST + planner skeleton is the only surface in code.

pub mod error;
pub mod planner;
pub mod query;

pub use error::{BudgetMetric, MeshError};
pub use planner::{ExecutionPlan, MeshQueryPlanner, OperatorNode, OperatorPlan};
pub use query::{
    AggregateFn, ChainRef, Expr, JoinKey, JoinKind, MeshQuery, OrderDir, OrderKey, QueryV1,
    ResultRow, SeqNum,
};
