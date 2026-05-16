//! MeshDB SDK surface — federated query plane above the
//! substrate's chains.
//!
//! Pulls the consumer-relevant types up to
//! `net_sdk::meshdb::*` so tenant tooling (and a future Deck
//! MeshDB Console) doesn't have to reach into
//! `net::adapter::net::behavior::meshdb::*` directly.
//!
//! # Scope
//!
//! Three layers are re-exported:
//!
//! - **AST + helpers** (`MeshQuery`, `QueryV1`, `ChainRef`,
//!   `SeqNum`, plus the supporting expression / aggregate /
//!   ordering / window / join types). Consumers build queries
//!   by composing AST nodes; the executor walks them.
//! - **Executor surface** (`MeshQueryExecutor` async trait,
//!   `LocalMeshQueryExecutor` implementation, `ChainReader`
//!   trait, `RunningQuery` / `ResultStream` / `QueryHandle`).
//!   Consumers wire their own `ChainReader` over a RedEX
//!   handle (or in-memory fixture) and pass plans through the
//!   executor.
//! - **Error + result-row types** (`MeshError`, `BudgetMetric`,
//!   `ResultRow`, the typed result-payload enums).
//!
//! # Not re-exported
//!
//! - **Federated transport.** `FederatedMeshQueryExecutor` +
//!   `MeshDbTransport` are substrate-internal — the wire
//!   protocol is opaque to consumers; cross-node fan-out is
//!   the executor's job, not the consumer's. Tenant tools
//!   that need direct transport access depend on `net` with
//!   the `meshdb` feature.
//! - **Wire protocol.** `MeshDbRequest` / `MeshDbResponse` /
//!   `MeshDbFrame` / `SUBPROTOCOL_MESHDB` are substrate-
//!   internal. The executor handles framing on the consumer's
//!   behalf.
//! - **Planner internals.** `MeshQueryPlanner`, `ExecutionPlan`
//!   internals, `OperatorPlan`, etc. Consumers call
//!   `executor.execute(plan)`; the planner is exposed only
//!   far enough to construct a plan from a `MeshQuery`.
//! - **Cache primitives.** `LruResultCache` + `CachePolicy`
//!   are re-exported because consumers tuning caching matters,
//!   but the cache trait machinery stays substrate-internal.
//!
//! # Activation
//!
//! Gated behind `--features meshdb` on the SDK. Composes
//! against `net/meshdb` (which itself gates the substrate-side
//! Phase A skeleton). MeshDB is documented as needing a
//! consumer to drive its semantics (`MESHDB_PLAN.md` § Status);
//! exposing the SDK surface lets tenant tooling start building
//! against the types ahead of the Deck consumer slice.

pub use net::adapter::net::behavior::meshdb::{
    // AST — what consumers compose.
    clamp_join_watermark_secs, AggregateFn, AggregateRowPayload, AggregateValue, ChainRef, Expr,
    GroupKey, JoinKey, JoinKind, JoinedRowPayload, MeshQuery, NumericAggregateKind,
    NumericReductionKind, OrderDir, OrderKey, QueryV1, ResultRow, SeqNum, WindowBoundary,
    WindowSpec, DEFAULT_JOIN_WATERMARK_SECS,
    // Executor — what consumers run plans through.
    ChainReader, LocalMeshQueryExecutor, MeshQueryExecutor, QueryHandle, QueryId, ResultStream,
    RunningQuery,
    // Plan — minimally exposed so the planner output can flow
    // into the executor; consumers don't construct
    // `ExecutionPlan` directly, they get it from `planner.plan(query)`.
    ExecutionPlan, MeshQueryPlanner,
    // Errors.
    BudgetMetric, MeshError,
    // Cache (Phase F) — consumers tuning result reuse opt in
    // explicitly via `LocalMeshQueryExecutor::with_cache`.
    CachePolicy, LruResultCache,
};
