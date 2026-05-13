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
//! - [`executor`] — `MeshQueryExecutor` async trait +
//!   `LocalMeshQueryExecutor` walks the plan and emits
//!   `ResultRow`s through a pluggable `ChainReader`
//!   abstraction. Phase B-2 handles the atomic operators end-
//!   to-end; cross-node fan-out + partial-result resume land
//!   in Phase B-4.
//! - [`protocol`] — `MeshDbRequest` / `MeshDbResponse` wire
//!   envelopes + `SUBPROTOCOL_MESHDB` slot + the streaming
//!   `ResultBatch` + opaque `ContinuationToken` + the unified
//!   [`protocol::MeshDbFrame`] tag the wire dispatcher demuxes
//!   on.
//! - [`federated`] — `FederatedMeshQueryExecutor<T:
//!   MeshDbTransport>` fans atomic operators out to remote
//!   `target_nodes` via a pluggable transport, with
//!   proximity-ordered failover. `LoopbackTransport` drives
//!   in-process 3-node integration tests without a real wire.
//! - [`transport`] — real-wire hookup. `MeshDbWireDispatcher`
//!   implements [`transport::MeshDbInboundRouter`]
//!   for the mesh's `SUBPROTOCOL_MESHDB` dispatch arm;
//!   [`transport::MeshDbServer`] drives the
//!   server-side executor + streams responses back via the
//!   sender. [`transport::enable_meshdb_on_mesh`]
//!   plumbs everything onto a live `MeshNode`.
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

pub mod cache;
pub mod error;
pub mod executor;
pub mod federated;
pub mod planner;
pub mod protocol;
pub mod query;
pub mod row;
pub mod transport;

pub use cache::{
    CacheKey, CachePolicy, CachedResult, LruResultCache, ResultCache, LRU_MAX_BYTES,
    LRU_MAX_ENTRIES,
};
pub use error::{BudgetMetric, MeshError};
pub use executor::{
    ChainReader, LocalMeshQueryExecutor, MeshQueryExecutor, QueryHandle, QueryId, ResultStream,
    RunningQuery,
};
pub use federated::{
    FederatedMeshQueryExecutor, LoopbackTransport, MeshDbTransport, ResponseStream, TransportError,
};
pub use planner::{
    ExecutionPlan, JoinKeyMode, JoinStrategy, LineageDirection, LineageEntry, MeshQueryPlanner,
    OperatorNode, OperatorPlan,
};
pub use protocol::{
    ContinuationToken, MeshDbFrame, MeshDbRequest, MeshDbResponse, ResultBatch, SUBPROTOCOL_MESHDB,
};
pub use query::{
    clamp_join_watermark_secs, AggregateFn, AggregateRowPayload, AggregateValue, ChainRef,
    Expr, GroupKey, JoinKey, JoinKind, JoinedRowPayload, MeshQuery, NumericAggregateKind,
    NumericReductionKind, OrderDir, OrderKey, QueryV1, ResultRow, SeqNum, WindowBoundary,
    WindowSpec, DEFAULT_JOIN_WATERMARK_SECS,
};
pub use transport::{
    enable_meshdb_on_mesh, MeshDbInboundRouter, MeshDbRouteError, MeshDbServer,
    MeshDbWireDispatcher, MeshDbWireSender, MeshDbWireTransport, MeshNodeMeshDbSender,
    MESHDB_MAX_INBOUND_FRAME_BYTES, MESHDB_RESPONSE_INBOX_CAPACITY, MESHDB_SERVER_BATCH_ROWS,
    MESHDB_SERVER_OUTBOX_CAPACITY, MESHDB_SERVER_PENDING_CANCELS_CAP,
};
