//! `FederatedMeshQueryExecutor` — fans out atomic operators to
//! remote `target_nodes` over a pluggable [`MeshDbTransport`].
//!
//! Two transport impls ship in v0.16: the in-process
//! [`LoopbackTransport`] for substrate-side integration tests,
//! and the real-wire
//! [`MeshDbWireTransport`](super::transport::MeshDbWireTransport)
//! that rides `SUBPROTOCOL_MESHDB` on the mesh's existing
//! encrypted Net session. Call
//! [`enable_meshdb_on_mesh`](super::transport::enable_meshdb_on_mesh)
//! to install the dispatcher + transport on a live `MeshNode`.
//!
//! # Routing
//!
//! The planner produces an [`ExecutionPlan`] whose root
//! `target_nodes` are proximity-ordered (RTT-asc, lex-NodeId
//! tiebreak). The federated executor walks them in order and
//! tries the first; on [`TransportError`] it falls back to the
//! next. When all targets fail, it surfaces
//! [`MeshError::ExecutorError`] carrying the last error and the
//! id of the last target tried.
//!
//! When `target_nodes` is empty (a legal "no holders" result
//! from the planner), the federated executor emits an empty row
//! stream — matching the local executor's behavior for the
//! same condition.
//!
//! # Cancellation
//!
//! [`QueryHandle::cancel`] is cooperative: the federated
//! executor's row-translation task checks the cancel flag
//! between responses and emits [`MeshError::QueryCancelled`].
//! Composite operators (HashJoin / Aggregate* / Window /
//! Filter) share one outer handle across their recursive
//! sub-fetches AND wrap their materialized output streams in
//! the cancel-aware adapter, so a single
//! [`QueryHandle::cancel`] propagates through every nested
//! stage rather than being a no-op on the outer materialized
//! iterator. Out-of-band cancellation to the remote executor
//! (so the remote can free its resources) lands in a later
//! slice; Phase B-4 ships the local-side cancellation only.

use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::Stream;
use futures::StreamExt;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use super::error::MeshError;
use super::executor::{MeshQueryExecutor, QueryHandle, ResultStream, RunningQuery};
use super::planner::{ExecutionPlan, OperatorPlan};
use super::protocol::{MeshDbRequest, MeshDbResponse};
use super::query::ResultRow;

/// Stream of responses returned by a [`MeshDbTransport`]. Pinned
/// + boxed so the transport trait is object-safe.
pub type ResponseStream = Pin<Box<dyn Stream<Item = MeshDbResponse> + Send>>;

/// Errors surfaced by a [`MeshDbTransport`]. The federated
/// executor uses [`Self::NoRoute`] as its failover signal —
/// any [`Other`](TransportError::Other) is bubbled up unchanged
/// inside [`MeshError::ExecutorError`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TransportError {
    /// No route to the target node. Used by the federated
    /// executor to fall back to the next target.
    #[error("no route to node {0:#x}")]
    NoRoute(u64),
    /// Any other transport-layer failure (connection reset,
    /// timeout, framing error, etc).
    #[error("transport error: {0}")]
    Other(String),
}

/// Pluggable transport for cross-node MeshDB queries.
///
/// Decouples the federated executor from the mesh's
/// subprotocol dispatch so integration tests can drive the
/// executor in-process via [`LoopbackTransport`].
#[async_trait]
pub trait MeshDbTransport: Send + Sync {
    /// Send a request to `node` and return a response stream.
    /// The stream terminates after the first
    /// [`MeshDbResponse::End`] / [`MeshDbResponse::Error`] /
    /// final-flagged batch.
    async fn send(
        &self,
        node: u64,
        request: MeshDbRequest,
    ) -> Result<ResponseStream, TransportError>;
}

/// Federated executor — fans atomic operators out to
/// `target_nodes` via the transport.
///
/// Like the local executor, optionally holds a Phase F cache
/// alongside a capability-version closure for pull-invalidation.
/// Phase F locked decision: only top-level plans cache —
/// sub-plan caching (Aggregate inner, HashJoin sides) is
/// deferred until profiling justifies the bookkeeping.
pub struct FederatedMeshQueryExecutor<T: MeshDbTransport> {
    transport: Arc<T>,
    cache: Option<Arc<dyn super::cache::ResultCache>>,
    capability_version: Option<Arc<dyn Fn() -> u64 + Send + Sync>>,
}

/// Process-global counter feeding every federated executor's
/// `call_id`s. The wire contract
/// (`MeshDbRequest::Execute.call_id`) is "unique per (caller,
/// executor) pair while in-flight"; a per-executor counter
/// alone fails that contract when two federated executors on
/// the same host hit a shared remote. A single process-global
/// counter trivially satisfies uniqueness across every
/// federated executor in the caller process while preserving
/// the executor's lifetime-independent monotonic property the
/// LoopbackTransport relies on.
static FEDERATED_CALL_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

impl<T: MeshDbTransport> FederatedMeshQueryExecutor<T> {
    /// Construct a cache-less federated executor.
    pub fn new(transport: Arc<T>) -> Self {
        Self {
            transport,
            cache: None,
            capability_version: None,
        }
    }

    /// Construct a cache-aware federated executor. Same
    /// pull-invalidation semantics as
    /// [`super::executor::LocalMeshQueryExecutor::with_cache`].
    pub fn with_cache(
        transport: Arc<T>,
        cache: Arc<dyn super::cache::ResultCache>,
        capability_version: Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> Self {
        Self {
            transport,
            cache: Some(cache),
            capability_version: Some(capability_version),
        }
    }

    /// Mint a process-unique `call_id` for a new federated
    /// query. Drawn from a single static counter shared by
    /// every federated executor in this process, so no two
    /// in-flight calls can collide at a shared remote
    /// demultiplexer.
    fn allocate_id(&self) -> u64 {
        FEDERATED_CALL_ID_COUNTER.fetch_add(1, Ordering::Relaxed)
    }
}

#[async_trait]
impl<T: MeshDbTransport + 'static> MeshQueryExecutor for FederatedMeshQueryExecutor<T> {
    async fn execute_with(
        &self,
        plan: ExecutionPlan,
        options: super::executor::ExecuteOptions,
    ) -> Result<RunningQuery, MeshError> {
        // One outer handle is allocated here and threaded
        // through every sub-fetch + final stream wrapper so
        // `handle.cancel()` short-circuits the whole tree.
        let handle = QueryHandle::new(self.allocate_id());

        // Cache fast path. Top-level plans only; sub-plan
        // recursion below this point bypasses caching.
        if let (Some(cache), Some(version_fn), false) = (
            self.cache.as_ref(),
            self.capability_version.as_ref(),
            options.bypass_cache,
        ) {
            let version = version_fn();
            // Plans containing un-postcard-able nodes (Filter /
            // Discovered) yield `None` here; we bypass the
            // cache entirely rather than panic.
            if let Some(key) = super::cache::CacheKey::for_plan(&plan, version) {
                if let Some(cached) = cache.get(&key) {
                    let rows = stream_results_cancellable(
                        cached.rows.into_iter().map(Ok).collect(),
                        handle.clone(),
                    );
                    return Ok(RunningQuery { handle, rows });
                }
                // Miss. Run the actual federated path with
                // caching temporarily disabled (so the recursive
                // sub-plan executes don't try to cache too), then
                // drain + cache the top-level rows.
                let drained = self
                    .execute_uncached_with_handle(plan.clone(), handle.clone())
                    .await?;
                let collected = drain_rows(drained.rows).await?;
                if handle.is_cancelled() {
                    return Err(MeshError::QueryCancelled);
                }
                cache.insert(
                    key,
                    super::cache::CachedResult {
                        rows: collected.clone(),
                        inserted_at: std::time::Instant::now(),
                        policy: options.cache_policy,
                    },
                );
                let rows = stream_results_cancellable(
                    collected.into_iter().map(Ok).collect(),
                    handle.clone(),
                );
                return Ok(RunningQuery { handle, rows });
            }
            // Encode bypass — fall through.
        }
        self.execute_uncached_with_handle(plan, handle).await
    }
}

impl<T: MeshDbTransport + 'static> FederatedMeshQueryExecutor<T> {
    /// Execute the plan threading the caller-supplied outer
    /// [`QueryHandle`] through every sub-fetch and through the
    /// returned row stream. This is the cancellation-correct
    /// path: the outer handle's cancel flag short-circuits
    /// inner stages between awaits and per-row in the
    /// materialized output streams.
    async fn execute_uncached_with_handle(
        &self,
        plan: ExecutionPlan,
        handle: QueryHandle,
    ) -> Result<RunningQuery, MeshError> {
        if handle.is_cancelled() {
            return Err(MeshError::QueryCancelled);
        }
        // Phase B-4 scope: atomic root operators dispatch to
        // remote target_nodes. LineageEmit is a planner-only
        // leaf (walk happened at plan time, no remote work);
        // emit its entries locally. Composite operators surface
        // synchronously, mirroring the local executor.
        match &plan.root.operator {
            OperatorPlan::AtRead { .. }
            | OperatorPlan::BetweenRead { .. }
            | OperatorPlan::LatestRead { .. } => {}
            OperatorPlan::LineageEmit { entries, .. } => {
                use super::query::SeqNum;
                let rows_vec: Vec<Result<ResultRow, MeshError>> = entries
                    .iter()
                    .map(|entry| {
                        Ok(ResultRow {
                            origin: entry.origin,
                            seq: entry.tip_seq.unwrap_or(SeqNum(0)),
                            payload: Vec::new(),
                        })
                    })
                    .collect();
                let rows = stream_results_cancellable(rows_vec, handle.clone());
                return Ok(RunningQuery { handle, rows });
            }
            OperatorPlan::HashJoin { .. } => {
                return self.execute_hash_join_federated(plan, handle).await;
            }
            OperatorPlan::AggregateCount { .. } => {
                return self.execute_aggregate_count_federated(plan, handle).await;
            }
            OperatorPlan::AggregateNumeric { .. } => {
                return self.execute_aggregate_numeric_federated(plan, handle).await;
            }
            OperatorPlan::AggregateReduction { .. } | OperatorPlan::AggregateDistinct { .. } => {
                return self.execute_aggregate_e4_federated(plan, handle).await;
            }
            OperatorPlan::Window { .. } => {
                return self.execute_window_federated(plan, handle).await;
            }
            OperatorPlan::Filter { .. } => {
                return self.execute_filter_federated(plan, handle).await;
            }
            OperatorPlan::NotYetImplemented { detail, .. } => {
                return Err(MeshError::PlannerError {
                    detail: format!("operator not yet implemented: {detail}"),
                });
            }
        }

        let targets = plan.root.target_nodes.clone();

        // Empty targets: legal "no holders" result. Emit an
        // empty stream (matches the local executor's behavior
        // for an unknown chain).
        if targets.is_empty() {
            let rows: ResultStream = Box::pin(futures::stream::empty());
            return Ok(RunningQuery { handle, rows });
        }

        let call_id = handle.id();
        let request = MeshDbRequest::Execute {
            call_id,
            plan: plan.clone(),
        };

        // Try each target in proximity order. NoRoute falls
        // through; Other is bubbled up.
        let mut response_stream = None;
        let mut last_attempted: u64 = targets[0];
        let mut last_err: Option<TransportError> = None;
        for &target in &targets {
            if handle.is_cancelled() {
                return Err(MeshError::QueryCancelled);
            }
            last_attempted = target;
            match self.transport.send(target, request.clone()).await {
                Ok(s) => {
                    response_stream = Some(s);
                    break;
                }
                Err(err @ TransportError::NoRoute(_)) => {
                    last_err = Some(err);
                    continue;
                }
                Err(other) => {
                    last_err = Some(other);
                    continue;
                }
            }
        }

        let response_stream = match response_stream {
            Some(s) => s,
            None => {
                let detail = last_err
                    .map(|e| format!("all targets failed; last error: {e}"))
                    .unwrap_or_else(|| "no targets reachable".to_string());
                return Err(MeshError::ExecutorError {
                    node: last_attempted,
                    detail,
                });
            }
        };

        let rows = translate_responses(response_stream, handle.clone());
        Ok(RunningQuery { handle, rows })
    }
}

impl<T: MeshDbTransport + 'static> FederatedMeshQueryExecutor<T> {
    /// Phase D-1 federated hash-join: fetch both sides
    /// through the transport (recurse on this executor),
    /// hash-join locally. Supports all four [`JoinKind`]s.
    async fn execute_hash_join_federated(
        &self,
        plan: ExecutionPlan,
        handle: QueryHandle,
    ) -> Result<RunningQuery, MeshError> {
        use super::planner::CostEstimate;
        use super::query::{JoinKind, JoinedRowPayload, SeqNum};

        let OperatorPlan::HashJoin {
            left,
            right,
            key_mode,
            kind,
            strategy,
            ..
        } = plan.root.operator
        else {
            unreachable!("execute_hash_join_federated dispatched on non-HashJoin");
        };

        // Fetch each side through the federated executor so
        // atomic leaves still dispatch via the transport. The
        // shared handle is threaded into both sub-fetches so a
        // cancel on the outer handle aborts both before the
        // local hash-join runs.
        let left_running = Box::pin(self.execute_uncached_with_handle(
            ExecutionPlan {
                root: *left,
                total_cost: CostEstimate::default(),
            },
            handle.clone(),
        ))
        .await?;
        if handle.is_cancelled() {
            return Err(MeshError::QueryCancelled);
        }
        let right_running = Box::pin(self.execute_uncached_with_handle(
            ExecutionPlan {
                root: *right,
                total_cost: CostEstimate::default(),
            },
            handle.clone(),
        ))
        .await?;
        if handle.is_cancelled() {
            return Err(MeshError::QueryCancelled);
        }

        let left_rows = drain_rows(left_running.rows).await?;
        let right_rows = drain_rows(right_running.rows).await?;
        if handle.is_cancelled() {
            return Err(MeshError::QueryCancelled);
        }

        let pairs = match (strategy, kind) {
            (super::planner::JoinStrategy::HashBroadcast, JoinKind::Inner) => {
                federated_hash_join(left_rows, right_rows, &key_mode, false, false)?
            }
            (super::planner::JoinStrategy::HashBroadcast, JoinKind::LeftOuter) => {
                federated_hash_join(left_rows, right_rows, &key_mode, true, false)?
            }
            (super::planner::JoinStrategy::HashBroadcast, JoinKind::RightOuter) => {
                federated_hash_join(right_rows, left_rows, &key_mode, true, true)?
            }
            (super::planner::JoinStrategy::HashBroadcast, JoinKind::FullOuter) => {
                federated_full_outer(left_rows, right_rows, &key_mode)?
            }
            (super::planner::JoinStrategy::SortMerge, k) => {
                federated_sort_merge(left_rows, right_rows, &key_mode, k)?
            }
        };

        let mut out: Vec<Result<ResultRow, MeshError>> = Vec::new();
        for (l, r) in pairs {
            let payload =
                postcard::to_allocvec(&JoinedRowPayload { left: l, right: r }).map_err(|e| {
                    MeshError::ExecutorError {
                        node: 0,
                        detail: format!("encode JoinedRowPayload: {e}"),
                    }
                })?;
            out.push(Ok(ResultRow {
                origin: 0,
                seq: SeqNum(0),
                payload,
            }));
        }

        let rows = stream_results_cancellable(out, handle.clone());
        Ok(RunningQuery { handle, rows })
    }
}

/// One `(left, right)` pair before encoding as a
/// [`super::query::JoinedRowPayload`]. Either side can be
/// `None` for outer-join unmatched rows.
type JoinedPair = (Option<ResultRow>, Option<ResultRow>);

impl<T: MeshDbTransport + 'static> FederatedMeshQueryExecutor<T> {
    /// Phase E-2 federated row filter: fetch the inner sub-plan
    /// via the transport, evaluate the predicate against each
    /// row's synthetic view locally.
    async fn execute_filter_federated(
        &self,
        plan: ExecutionPlan,
        handle: QueryHandle,
    ) -> Result<RunningQuery, MeshError> {
        use super::planner::CostEstimate;
        use crate::adapter::net::behavior::predicate::EvalContext;

        let OperatorPlan::Filter { input, predicate } = plan.root.operator else {
            unreachable!("execute_filter_federated dispatched on non-Filter");
        };

        let inner = Box::pin(self.execute_uncached_with_handle(
            ExecutionPlan {
                root: *input,
                total_cost: CostEstimate::default(),
            },
            handle.clone(),
        ))
        .await?;
        let rows = drain_rows(inner.rows).await?;
        if handle.is_cancelled() {
            return Err(MeshError::QueryCancelled);
        }

        let pred = predicate
            .into_predicate()
            .map_err(|e| MeshError::PlannerError {
                detail: format!("Filter predicate rebuild failed: {e:?}"),
            })?;

        let mut out: Vec<Result<ResultRow, MeshError>> = Vec::with_capacity(rows.len());
        for row in rows {
            let (tags, metadata) = super::row::synthetic_row_view(&row);
            let ctx = EvalContext::new(&tags, &metadata);
            if pred.evaluate(&ctx) {
                out.push(Ok(row));
            }
        }

        let rows = stream_results_cancellable(out, handle.clone());
        Ok(RunningQuery { handle, rows })
    }

    /// Phase E-5 federated tumbling window. Fetches the inner
    /// sub-plan via the transport, then buckets locally.
    async fn execute_window_federated(
        &self,
        plan: ExecutionPlan,
        handle: QueryHandle,
    ) -> Result<RunningQuery, MeshError> {
        use super::executor::execute_window;
        use super::planner::CostEstimate;

        let OperatorPlan::Window { input, spec } = plan.root.operator else {
            unreachable!("execute_window_federated dispatched on non-Window");
        };
        let inner = Box::pin(self.execute_uncached_with_handle(
            ExecutionPlan {
                root: *input,
                total_cost: CostEstimate::default(),
            },
            handle.clone(),
        ))
        .await?;
        let rows = drain_rows(inner.rows).await?;
        if handle.is_cancelled() {
            return Err(MeshError::QueryCancelled);
        }
        let output_rows = execute_window(rows, &spec)?;

        let out: Vec<Result<ResultRow, MeshError>> = output_rows.into_iter().map(Ok).collect();
        let rows = stream_results_cancellable(out, handle.clone());
        Ok(RunningQuery { handle, rows })
    }

    /// Phase E-4 federated reduction / distinct aggregate.
    /// Fetches the inner sub-plan via the transport, then
    /// reduces locally with the same routine the local
    /// executor uses.
    async fn execute_aggregate_e4_federated(
        &self,
        plan: ExecutionPlan,
        handle: QueryHandle,
    ) -> Result<RunningQuery, MeshError> {
        use super::executor::{execute_aggregate_distinct, execute_aggregate_reduction};
        use super::planner::CostEstimate;

        let output_rows = match plan.root.operator {
            OperatorPlan::AggregateReduction {
                input,
                group_by,
                field_path,
                kind,
            } => {
                let inner = Box::pin(self.execute_uncached_with_handle(
                    ExecutionPlan {
                        root: *input,
                        total_cost: CostEstimate::default(),
                    },
                    handle.clone(),
                ))
                .await?;
                let rows = drain_rows(inner.rows).await?;
                if handle.is_cancelled() {
                    return Err(MeshError::QueryCancelled);
                }
                execute_aggregate_reduction(&rows, group_by.as_ref(), &field_path, kind)?
            }
            OperatorPlan::AggregateDistinct {
                input,
                group_by,
                field_path,
            } => {
                let inner = Box::pin(self.execute_uncached_with_handle(
                    ExecutionPlan {
                        root: *input,
                        total_cost: CostEstimate::default(),
                    },
                    handle.clone(),
                ))
                .await?;
                let rows = drain_rows(inner.rows).await?;
                if handle.is_cancelled() {
                    return Err(MeshError::QueryCancelled);
                }
                execute_aggregate_distinct(&rows, group_by.as_ref(), &field_path)?
            }
            _ => unreachable!("execute_aggregate_e4_federated dispatched on wrong operator"),
        };

        let out: Vec<Result<ResultRow, MeshError>> = output_rows.into_iter().map(Ok).collect();
        let rows = stream_results_cancellable(out, handle.clone());
        Ok(RunningQuery { handle, rows })
    }

    /// Phase E-3 federated numeric aggregate (Sum / Avg).
    async fn execute_aggregate_numeric_federated(
        &self,
        plan: ExecutionPlan,
        handle: QueryHandle,
    ) -> Result<RunningQuery, MeshError> {
        use super::planner::CostEstimate;
        use super::query::{
            AggregateRowPayload, AggregateValue, GroupKey, NumericAggregateKind, SeqNum,
        };
        use std::collections::BTreeMap;

        let OperatorPlan::AggregateNumeric {
            input,
            group_by,
            field_path,
            kind,
        } = plan.root.operator
        else {
            unreachable!("execute_aggregate_numeric_federated dispatched on non-AggregateNumeric");
        };

        let inner = Box::pin(self.execute_uncached_with_handle(
            ExecutionPlan {
                root: *input,
                total_cost: CostEstimate::default(),
            },
            handle.clone(),
        ))
        .await?;
        let rows = drain_rows(inner.rows).await?;
        if handle.is_cancelled() {
            return Err(MeshError::QueryCancelled);
        }

        let mut acc: BTreeMap<Vec<u8>, (Option<GroupKey>, f64, u64)> = BTreeMap::new();
        for row in &rows {
            let Some(value) = super::row::extract_numeric(row, &field_path) else {
                continue;
            };
            let (key_bytes, group) = match &group_by {
                None => (Vec::new(), None),
                Some(mode) => {
                    let Some(bytes) = try_encode_join_key_federated(row, mode) else {
                        continue;
                    };
                    let group = match mode {
                        super::planner::JoinKeyMode::Origin => GroupKey::Origin(row.origin),
                        super::planner::JoinKeyMode::Seq => GroupKey::Seq(row.seq),
                        super::planner::JoinKeyMode::OriginSeq => GroupKey::OriginSeq {
                            origin: row.origin,
                            seq: row.seq,
                        },
                        super::planner::JoinKeyMode::Field(_) => unreachable!(
                            "JoinKeyMode::Field reached federated_aggregate_numeric; payload-keyed group_by is not supported",
                        ),
                    };
                    (bytes, Some(group))
                }
            };
            let entry = acc.entry(key_bytes).or_insert((group, 0.0, 0));
            entry.1 += value;
            entry.2 = entry.2.saturating_add(1);
        }

        let mut out: Vec<Result<ResultRow, MeshError>> = Vec::new();
        let mk_value = |sum: f64, count: u64| match kind {
            NumericAggregateKind::Sum => AggregateValue::Sum(sum),
            NumericAggregateKind::Avg => {
                if count == 0 {
                    AggregateValue::Avg(None)
                } else {
                    AggregateValue::Avg(Some(sum / count as f64))
                }
            }
        };
        if group_by.is_none() {
            let (sum, count) = acc
                .get(&Vec::<u8>::new())
                .map(|(_, s, c)| (*s, *c))
                .unwrap_or((0.0, 0));
            let payload = postcard::to_allocvec(&AggregateRowPayload {
                group: None,
                value: mk_value(sum, count),
            })
            .map_err(|e| MeshError::ExecutorError {
                node: 0,
                detail: format!("encode AggregateRowPayload: {e}"),
            })?;
            out.push(Ok(ResultRow {
                origin: 0,
                seq: SeqNum(0),
                payload,
            }));
        } else {
            for (_, (group, sum, count)) in acc {
                let payload = postcard::to_allocvec(&AggregateRowPayload {
                    group,
                    value: mk_value(sum, count),
                })
                .map_err(|e| MeshError::ExecutorError {
                    node: 0,
                    detail: format!("encode AggregateRowPayload: {e}"),
                })?;
                out.push(Ok(ResultRow {
                    origin: 0,
                    seq: SeqNum(0),
                    payload,
                }));
            }
        }

        let rows_out = stream_results_cancellable(out, handle.clone());
        Ok(RunningQuery {
            handle,
            rows: rows_out,
        })
    }

    /// Phase E-1 federated count aggregate: fetch the inner
    /// sub-plan via the transport, group + count locally.
    async fn execute_aggregate_count_federated(
        &self,
        plan: ExecutionPlan,
        handle: QueryHandle,
    ) -> Result<RunningQuery, MeshError> {
        use super::planner::CostEstimate;
        use super::query::{AggregateRowPayload, AggregateValue, GroupKey, SeqNum};
        use std::collections::BTreeMap;

        let OperatorPlan::AggregateCount { input, group_by } = plan.root.operator else {
            unreachable!("execute_aggregate_count_federated dispatched on non-AggregateCount");
        };

        // Fetch the inner rows via the federated path so atomic
        // leaves still dispatch through the transport.
        let inner = Box::pin(self.execute_uncached_with_handle(
            ExecutionPlan {
                root: *input,
                total_cost: CostEstimate::default(),
            },
            handle.clone(),
        ))
        .await?;
        let rows = drain_rows(inner.rows).await?;
        if handle.is_cancelled() {
            return Err(MeshError::QueryCancelled);
        }

        let mut out: Vec<Result<ResultRow, MeshError>> = Vec::new();
        match group_by {
            None => {
                let payload = postcard::to_allocvec(&AggregateRowPayload {
                    group: None,
                    value: AggregateValue::Count(rows.len() as u64),
                })
                .map_err(|e| MeshError::ExecutorError {
                    node: 0,
                    detail: format!("encode AggregateRowPayload: {e}"),
                })?;
                out.push(Ok(ResultRow {
                    origin: 0,
                    seq: SeqNum(0),
                    payload,
                }));
            }
            Some(mode) => {
                let mut counts: BTreeMap<Vec<u8>, (GroupKey, u64)> = BTreeMap::new();
                for row in &rows {
                    let Some(key_bytes) = try_encode_join_key_federated(row, &mode) else {
                        continue;
                    };
                    let key = match &mode {
                        super::planner::JoinKeyMode::Origin => GroupKey::Origin(row.origin),
                        super::planner::JoinKeyMode::Seq => GroupKey::Seq(row.seq),
                        super::planner::JoinKeyMode::OriginSeq => GroupKey::OriginSeq {
                            origin: row.origin,
                            seq: row.seq,
                        },
                        super::planner::JoinKeyMode::Field(_) => unreachable!(
                            "JoinKeyMode::Field reached federated_aggregate_count; payload-keyed group_by is not supported",
                        ),
                    };
                    let entry = counts.entry(key_bytes).or_insert((key, 0));
                    entry.1 = entry.1.saturating_add(1);
                }
                for (_, (group, count)) in counts {
                    let payload = postcard::to_allocvec(&AggregateRowPayload {
                        group: Some(group),
                        value: AggregateValue::Count(count),
                    })
                    .map_err(|e| MeshError::ExecutorError {
                        node: 0,
                        detail: format!("encode AggregateRowPayload: {e}"),
                    })?;
                    out.push(Ok(ResultRow {
                        origin: 0,
                        seq: SeqNum(0),
                        payload,
                    }));
                }
            }
        }

        let rows_out = stream_results_cancellable(out, handle.clone());
        Ok(RunningQuery {
            handle,
            rows: rows_out,
        })
    }
}

/// Hash-join body for the federated executor. Returns the
/// matched (and optionally unmatched) `(left, right)` pairs
/// before encoding so the caller can wrap them in
/// [`super::query::JoinedRowPayload`].
fn federated_hash_join(
    build_rows: Vec<ResultRow>,
    probe_rows: Vec<ResultRow>,
    key_mode: &super::planner::JoinKeyMode,
    emit_unmatched_build: bool,
    swap: bool,
) -> Result<Vec<JoinedPair>, MeshError> {
    let mut build =
        super::executor::build_hash_join_table(build_rows, key_mode, "broadcast-hash-federated")?;

    let mut out = Vec::new();
    for p in probe_rows {
        let Some(key) = try_encode_join_key_federated(&p, key_mode) else {
            continue;
        };
        if let Some(entries) = build.get_mut(&key) {
            for (b, matched) in entries.iter_mut() {
                *matched = true;
                if swap {
                    out.push((Some(p.clone()), Some(b.clone())));
                } else {
                    out.push((Some(b.clone()), Some(p.clone())));
                }
            }
        }
    }
    if emit_unmatched_build {
        for entries in build.into_values() {
            for (b, matched) in entries {
                if !matched {
                    if swap {
                        out.push((None, Some(b)));
                    } else {
                        out.push((Some(b), None));
                    }
                }
            }
        }
    }
    Ok(out)
}

/// Full-outer mirror for [`federated_hash_join`].
fn federated_full_outer(
    left_rows: Vec<ResultRow>,
    right_rows: Vec<ResultRow>,
    key_mode: &super::planner::JoinKeyMode,
) -> Result<Vec<JoinedPair>, MeshError> {
    let mut right_map =
        super::executor::build_hash_join_table(right_rows, key_mode, "broadcast-hash-federated")?;

    let mut out = Vec::new();
    for l in left_rows {
        let Some(key) = try_encode_join_key_federated(&l, key_mode) else {
            out.push((Some(l), None));
            continue;
        };
        match right_map.get_mut(&key) {
            Some(entries) => {
                for (r, matched) in entries.iter_mut() {
                    *matched = true;
                    out.push((Some(l.clone()), Some(r.clone())));
                }
            }
            None => out.push((Some(l), None)),
        }
    }
    for entries in right_map.into_values() {
        for (r, matched) in entries {
            if !matched {
                out.push((None, Some(r)));
            }
        }
    }
    Ok(out)
}

/// Phase D-2 sort-merge mirror for the federated executor.
fn federated_sort_merge(
    left_rows: Vec<ResultRow>,
    right_rows: Vec<ResultRow>,
    key_mode: &super::planner::JoinKeyMode,
    kind: super::query::JoinKind,
) -> Result<Vec<JoinedPair>, MeshError> {
    use super::query::JoinKind;
    let mut left: Vec<(Vec<u8>, ResultRow)> = left_rows
        .into_iter()
        .filter_map(|r| try_encode_join_key_federated(&r, key_mode).map(|k| (k, r)))
        .collect();
    let mut right: Vec<(Vec<u8>, ResultRow)> = right_rows
        .into_iter()
        .filter_map(|r| try_encode_join_key_federated(&r, key_mode).map(|k| (k, r)))
        .collect();
    left.sort_by(|a, b| a.0.cmp(&b.0));
    right.sort_by(|a, b| a.0.cmp(&b.0));
    let emit_l = matches!(kind, JoinKind::LeftOuter | JoinKind::FullOuter);
    let emit_r = matches!(kind, JoinKind::RightOuter | JoinKind::FullOuter);
    let mut out = Vec::new();
    let (mut li, mut ri) = (0usize, 0usize);
    while li < left.len() && ri < right.len() {
        match left[li].0.cmp(&right[ri].0) {
            std::cmp::Ordering::Less => {
                if emit_l {
                    out.push((Some(left[li].1.clone()), None));
                }
                li += 1;
            }
            std::cmp::Ordering::Greater => {
                if emit_r {
                    out.push((None, Some(right[ri].1.clone())));
                }
                ri += 1;
            }
            std::cmp::Ordering::Equal => {
                let key = left[li].0.clone();
                let mut lj = li;
                while lj < left.len() && left[lj].0 == key {
                    lj += 1;
                }
                let mut rj = ri;
                while rj < right.len() && right[rj].0 == key {
                    rj += 1;
                }
                for l in &left[li..lj] {
                    for r in &right[ri..rj] {
                        out.push((Some(l.1.clone()), Some(r.1.clone())));
                    }
                }
                li = lj;
                ri = rj;
            }
        }
    }
    if emit_l {
        for (_, l) in &left[li..] {
            out.push((Some(l.clone()), None));
        }
    }
    if emit_r {
        for (_, r) in &right[ri..] {
            out.push((None, Some(r.clone())));
        }
    }
    Ok(out)
}

/// Maximum bytes a single federated drain will accumulate before
/// surfacing `QueryBudgetExceeded`. Mirrors the hash-join memory
/// budget (`HASH_JOIN_MEMORY_BYTES = 256 MiB`); the federated
/// aggregate and window operators previously drained their inner
/// `ResultStream` into a `Vec<ResultRow>` with no per-call cap, so
/// a remote peer (or a misconfigured federation target) returning
/// millions of rows OOM'd the aggregator before the grouped
/// processing surfaced any output. The cap lands on the consumer
/// side rather than the producer so a misestimating planner can
/// be caught at runtime.
const AGGREGATE_MAX_BYTES: usize = 256 * 1024 * 1024;

/// Approximate byte cost of a single `ResultRow`. Used by
/// `drain_rows` to maintain an O(1) running budget — the row
/// payload itself plus a small constant for the envelope. We
/// don't try to walk the row's fields here; the constant is a
/// generous over-approximation that keeps `AGGREGATE_MAX_BYTES`
/// the cap that bites.
fn approximate_row_bytes(row: &ResultRow) -> usize {
    // `ResultRow.payload` is the per-row Bytes column; everything
    // else (entity_id, seq, timestamp) sits inside a fixed-shape
    // header. 64 bytes covers the header + alignment slack.
    row.payload.len().saturating_add(64)
}

/// Drain a [`ResultStream`] into a `Vec<ResultRow>`. Errors
/// short-circuit the drain with the first encountered error.
/// Bounded by [`AGGREGATE_MAX_BYTES`] so a remote peer that
/// returns millions of rows can't OOM the aggregator.
async fn drain_rows(mut s: ResultStream) -> Result<Vec<ResultRow>, MeshError> {
    let mut out = Vec::new();
    let mut bytes: usize = 0;
    while let Some(item) = s.next().await {
        let row = item?;
        bytes = bytes.saturating_add(approximate_row_bytes(&row));
        if bytes > AGGREGATE_MAX_BYTES {
            return Err(MeshError::QueryBudgetExceeded {
                metric: super::error::BudgetMetric::MaxBytesScanned,
                used: bytes as u64,
                limit: AGGREGATE_MAX_BYTES as u64,
            });
        }
        out.push(row);
    }
    Ok(out)
}

/// Wrap a materialized `Vec<Result<ResultRow, MeshError>>` in
/// a [`ResultStream`] that re-checks the outer
/// [`QueryHandle`]'s cancel flag at every yield boundary. The
/// federated composite operators (HashJoin / Aggregate* /
/// Window / Filter) materialize their output before returning;
/// without this wrapper, `handle.cancel()` after that point is
/// a no-op against the resulting stream.
fn stream_results_cancellable(
    rows: Vec<Result<ResultRow, MeshError>>,
    handle: QueryHandle,
) -> ResultStream {
    let stream = futures::stream::iter(rows).map(move |item| {
        if handle.is_cancelled() {
            Err(MeshError::QueryCancelled)
        } else {
            item
        }
    });
    Box::pin(stream)
}

/// Federated mirror of `executor::try_encode_join_key`. The
/// two stay in lockstep by construction (key bytes are
/// intentionally the same).
fn try_encode_join_key_federated(
    row: &ResultRow,
    mode: &super::planner::JoinKeyMode,
) -> Option<Vec<u8>> {
    use super::planner::JoinKeyMode;
    // Mirrors the local executor's `try_encode_join_key`,
    // including the canonicalization of
    // `Field("origin"|"seq"|"origin,seq")` to the matching
    // row-intrinsic encoding. The two functions must produce
    // byte-identical output for the same (row, mode) pair so
    // local and federated probe tables cross-correlate.
    match mode {
        JoinKeyMode::Origin => Some(row.origin.to_le_bytes().to_vec()),
        JoinKeyMode::Seq => Some(row.seq.0.to_le_bytes().to_vec()),
        JoinKeyMode::OriginSeq => {
            let mut v = Vec::with_capacity(16);
            v.extend_from_slice(&row.origin.to_le_bytes());
            v.extend_from_slice(&row.seq.0.to_le_bytes());
            Some(v)
        }
        JoinKeyMode::Field(path) => match path.as_str() {
            "origin" => Some(row.origin.to_le_bytes().to_vec()),
            "seq" => Some(row.seq.0.to_le_bytes().to_vec()),
            "origin,seq" => {
                let mut v = Vec::with_capacity(16);
                v.extend_from_slice(&row.origin.to_le_bytes());
                v.extend_from_slice(&row.seq.0.to_le_bytes());
                Some(v)
            }
            _ => super::row::extract_string_projection(row, path).map(String::into_bytes),
        },
    }
}

/// Translate a [`MeshDbResponse`] stream into the row stream
/// returned by [`MeshQueryExecutor::execute`].
///
/// Spawns a tokio task that pumps the response stream and
/// forwards rows / errors over an mpsc channel. The
/// [`QueryHandle`]'s cancel flag is checked between
/// responses; on cancel, the task emits
/// [`MeshError::QueryCancelled`] and exits.
fn translate_responses(mut response_stream: ResponseStream, handle: QueryHandle) -> ResultStream {
    let (tx, rx) = mpsc::channel::<Result<ResultRow, MeshError>>(64);
    tokio::spawn(async move {
        while let Some(response) = response_stream.next().await {
            if handle.is_cancelled() {
                let _ = tx.send(Err(MeshError::QueryCancelled)).await;
                return;
            }
            match response {
                MeshDbResponse::Batch { batch, .. } => {
                    let is_final = batch.r#final;
                    for row in batch.rows {
                        if tx.send(Ok(row)).await.is_err() {
                            return;
                        }
                    }
                    if is_final {
                        return;
                    }
                }
                MeshDbResponse::End { .. } => return,
                MeshDbResponse::Error { error, .. } => {
                    let _ = tx.send(Err(error)).await;
                    return;
                }
            }
        }
        // Stream ended without a terminal frame (no End / Error
        // / final-batch). All terminal arms above `return`, so
        // reaching here means the transport dropped the stream
        // prematurely. Per the protocol contract this is a
        // transport-level error — surface it so consumers don't
        // mistake premature drop for clean EOS.
        let _ = tx
            .send(Err(MeshError::ExecutorError {
                node: 0,
                detail: "transport stream ended before terminal frame".to_string(),
            }))
            .await;
    });
    Box::pin(ReceiverStream::new(rx))
}

/// In-process transport that dispatches requests to a set of
/// [`MeshQueryExecutor`]s registered by `node_id`. Designed
/// for integration tests: lets the federated executor drive
/// multiple local executors without any actual network.
///
/// Behavior:
/// - Registered node → call the local executor, translate
///   its row stream into a [`ResponseStream`].
/// - Unregistered node → [`TransportError::NoRoute`].
/// - Node marked offline via [`Self::set_offline`] →
///   [`TransportError::NoRoute`] (exercises failover).
pub struct LoopbackTransport {
    nodes: parking_lot::RwLock<std::collections::HashMap<u64, LoopbackNode>>,
}

struct LoopbackNode {
    executor: Arc<dyn MeshQueryExecutor>,
    online: bool,
}

impl Default for LoopbackTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl LoopbackTransport {
    /// Construct an empty transport.
    pub fn new() -> Self {
        Self {
            nodes: parking_lot::RwLock::new(std::collections::HashMap::new()),
        }
    }

    /// Register an executor for `node_id`. Replaces any prior
    /// registration. New registrations start online.
    pub fn register(&self, node_id: u64, executor: Arc<dyn MeshQueryExecutor>) {
        self.nodes.write().insert(
            node_id,
            LoopbackNode {
                executor,
                online: true,
            },
        );
    }

    /// Flip a registered node's online state. Offline nodes
    /// surface [`TransportError::NoRoute`] from `send`, so the
    /// federated executor falls back to the next target.
    pub fn set_offline(&self, node_id: u64, offline: bool) {
        if let Some(n) = self.nodes.write().get_mut(&node_id) {
            n.online = !offline;
        }
    }
}

#[async_trait]
impl MeshDbTransport for LoopbackTransport {
    async fn send(
        &self,
        node: u64,
        request: MeshDbRequest,
    ) -> Result<ResponseStream, TransportError> {
        // Snapshot the node lookup so the lock isn't held
        // across the await below.
        let exec = {
            let guard = self.nodes.read();
            let entry = guard.get(&node).ok_or(TransportError::NoRoute(node))?;
            if !entry.online {
                return Err(TransportError::NoRoute(node));
            }
            entry.executor.clone()
        };

        match request {
            MeshDbRequest::Execute { call_id, plan } => {
                let running = exec
                    .execute(plan)
                    .await
                    .map_err(|e| TransportError::Other(format!("remote execute failed: {e}")))?;
                let stream = row_stream_to_responses(running.rows, call_id);
                Ok(stream)
            }
            MeshDbRequest::Resume { .. } => Err(TransportError::Other(
                "Resume not yet implemented in LoopbackTransport (Phase B-4+)".to_string(),
            )),
            MeshDbRequest::Cancel { .. } => {
                // Best-effort: surface as a no-op. The federated
                // executor's local cancel still fires; remote-
                // side cancellation propagation is a later
                // slice.
                let empty: ResponseStream = Box::pin(futures::stream::empty());
                Ok(empty)
            }
        }
    }
}

/// Convert a local executor's row stream into a
/// [`ResponseStream`] of `MeshDbResponse` messages. Each row
/// becomes a one-row [`MeshDbResponse::Batch`]; the stream
/// ends with [`MeshDbResponse::End`] on success or
/// [`MeshDbResponse::Error`] on the first error.
fn row_stream_to_responses(mut rows: ResultStream, call_id: u64) -> ResponseStream {
    use super::protocol::{MeshDbResponse, ResultBatch};
    let (tx, rx) = mpsc::channel::<MeshDbResponse>(64);
    tokio::spawn(async move {
        while let Some(item) = rows.next().await {
            match item {
                Ok(row) => {
                    let resp = MeshDbResponse::Batch {
                        call_id,
                        batch: ResultBatch::chunk(vec![row]),
                    };
                    if tx.send(resp).await.is_err() {
                        return;
                    }
                }
                Err(error) => {
                    let _ = tx.send(MeshDbResponse::Error { call_id, error }).await;
                    return;
                }
            }
        }
        let _ = tx.send(MeshDbResponse::End { call_id }).await;
    });
    Box::pin(ReceiverStream::new(rx))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use super::super::executor::{ChainReader, LocalMeshQueryExecutor};
    use super::super::planner::{CostEstimate, OperatorNode};
    use super::super::query::SeqNum;
    use super::*;

    /// Test-only `ChainReader` backed by an in-memory map.
    /// Lives in this module too (it's also used by the
    /// executor's tests; duplicating keeps each module's
    /// tests self-contained).
    #[derive(Default)]
    struct InMemoryChainReader {
        chains: Mutex<BTreeMap<u64, BTreeMap<SeqNum, Vec<u8>>>>,
    }

    impl InMemoryChainReader {
        fn append(&self, origin: u64, seq: SeqNum, payload: Vec<u8>) {
            self.chains
                .lock()
                .unwrap()
                .entry(origin)
                .or_default()
                .insert(seq, payload);
        }
    }

    impl ChainReader for InMemoryChainReader {
        fn read_one(&self, origin: u64, seq: SeqNum) -> Option<Vec<u8>> {
            self.chains.lock().unwrap().get(&origin)?.get(&seq).cloned()
        }

        fn read_range(&self, origin: u64, start: SeqNum, end: SeqNum) -> Vec<(SeqNum, Vec<u8>)> {
            self.chains
                .lock()
                .unwrap()
                .get(&origin)
                .map(|chain| {
                    chain
                        .range(start..end)
                        .map(|(s, p)| (*s, p.clone()))
                        .collect()
                })
                .unwrap_or_default()
        }

        fn latest_seq(&self, origin: u64) -> Option<SeqNum> {
            self.chains
                .lock()
                .unwrap()
                .get(&origin)?
                .keys()
                .next_back()
                .copied()
        }
    }

    fn local_executor_with(
        rows: &[(u64, u64, &[u8])],
    ) -> Arc<LocalMeshQueryExecutor<InMemoryChainReader>> {
        let reader = Arc::new(InMemoryChainReader::default());
        for (origin, seq, payload) in rows {
            reader.append(*origin, SeqNum(*seq), payload.to_vec());
        }
        Arc::new(LocalMeshQueryExecutor::new(reader))
    }

    fn plan_latest(origin: u64, target_nodes: Vec<u64>) -> ExecutionPlan {
        ExecutionPlan {
            root: OperatorNode {
                operator: OperatorPlan::LatestRead { origin },
                target_nodes,
                cost: CostEstimate::default(),
            },
            total_cost: CostEstimate::default(),
        }
    }

    fn plan_between(origin: u64, start: u64, end: u64, target_nodes: Vec<u64>) -> ExecutionPlan {
        ExecutionPlan {
            root: OperatorNode {
                operator: OperatorPlan::BetweenRead {
                    origin,
                    start: SeqNum(start),
                    end: SeqNum(end),
                },
                target_nodes,
                cost: CostEstimate::default(),
            },
            total_cost: CostEstimate::default(),
        }
    }

    async fn collect_rows(rs: ResultStream) -> Vec<Result<ResultRow, MeshError>> {
        rs.collect::<Vec<_>>().await
    }

    #[tokio::test]
    async fn three_node_happy_path_routes_to_first_holder() {
        // Node A holds the chain. The federated executor sends
        // the plan to the first target; rows arrive over the
        // loopback transport.
        let chain = 0xCAFE_BABE_DEAD_BEEF;
        let node_a =
            local_executor_with(&[(chain, 1, b"a-1"), (chain, 2, b"a-2"), (chain, 3, b"a-3")]);
        let node_b = local_executor_with(&[]);
        let node_c = local_executor_with(&[]);

        let transport = Arc::new(LoopbackTransport::new());
        transport.register(0xA, node_a);
        transport.register(0xB, node_b);
        transport.register(0xC, node_c);

        let fed = FederatedMeshQueryExecutor::new(transport);
        let plan = plan_latest(chain, vec![0xA, 0xB, 0xC]);
        let running = fed.execute(plan).await.unwrap();
        let rows: Vec<_> = collect_rows(running.rows)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].seq, SeqNum(3));
        assert_eq!(rows[0].payload, b"a-3");
    }

    #[tokio::test]
    async fn between_streams_all_rows_through_transport() {
        let chain = 0x01;
        let node = local_executor_with(&[
            (chain, 1, b"p-1"),
            (chain, 2, b"p-2"),
            (chain, 3, b"p-3"),
            (chain, 4, b"p-4"),
        ]);
        let transport = Arc::new(LoopbackTransport::new());
        transport.register(0xAA, node);

        let fed = FederatedMeshQueryExecutor::new(transport);
        let plan = plan_between(chain, 1, 5, vec![0xAA]);
        let running = fed.execute(plan).await.unwrap();
        let rows: Vec<_> = collect_rows(running.rows)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        let seqs: Vec<u64> = rows.iter().map(|r| r.seq.0).collect();
        assert_eq!(seqs, vec![1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn failover_skips_offline_target() {
        // First two targets are offline; third holds the data.
        let chain = 0xBEEF;
        let node_a = local_executor_with(&[]);
        let node_b = local_executor_with(&[]);
        let node_c = local_executor_with(&[(chain, 7, b"c-7")]);

        let transport = Arc::new(LoopbackTransport::new());
        transport.register(0xA, node_a);
        transport.register(0xB, node_b);
        transport.register(0xC, node_c);
        transport.set_offline(0xA, true);
        transport.set_offline(0xB, true);

        let fed = FederatedMeshQueryExecutor::new(transport);
        let plan = plan_latest(chain, vec![0xA, 0xB, 0xC]);
        let running = fed.execute(plan).await.unwrap();
        let rows: Vec<_> = collect_rows(running.rows)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].seq, SeqNum(7));
        assert_eq!(rows[0].payload, b"c-7");
    }

    #[tokio::test]
    async fn all_targets_offline_surfaces_executor_error() {
        let chain = 0xBEEF;
        let node_a = local_executor_with(&[]);
        let node_b = local_executor_with(&[]);
        let transport = Arc::new(LoopbackTransport::new());
        transport.register(0xA, node_a);
        transport.register(0xB, node_b);
        transport.set_offline(0xA, true);
        transport.set_offline(0xB, true);

        let fed = FederatedMeshQueryExecutor::new(transport);
        let plan = plan_latest(chain, vec![0xA, 0xB]);
        let err = fed.execute(plan).await.unwrap_err();
        match err {
            MeshError::ExecutorError { node, detail } => {
                // Last attempted was 0xB.
                assert_eq!(node, 0xB);
                assert!(detail.contains("all targets failed"), "got: {detail}");
            }
            other => panic!("expected ExecutorError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unregistered_target_falls_through_no_route() {
        // Plan points at a node the transport doesn't know
        // about; falls back to a registered second target.
        let chain = 0xBEEF;
        let node = local_executor_with(&[(chain, 5, b"five")]);
        let transport = Arc::new(LoopbackTransport::new());
        transport.register(0xB, node);
        // Note: 0xA is NOT registered.

        let fed = FederatedMeshQueryExecutor::new(transport);
        let plan = plan_latest(chain, vec![0xA, 0xB]);
        let running = fed.execute(plan).await.unwrap();
        let rows: Vec<_> = collect_rows(running.rows)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].seq, SeqNum(5));
    }

    #[tokio::test]
    async fn empty_target_nodes_yields_empty_stream() {
        let transport = Arc::new(LoopbackTransport::new());
        let fed = FederatedMeshQueryExecutor::new(transport);
        let plan = plan_latest(0xBEEF, vec![]);
        let running = fed.execute(plan).await.unwrap();
        let rows = collect_rows(running.rows).await;
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn not_yet_implemented_surfaces_planner_error_before_transport() {
        let transport = Arc::new(LoopbackTransport::new());
        let fed = FederatedMeshQueryExecutor::new(transport);
        let plan = ExecutionPlan {
            root: OperatorNode {
                operator: OperatorPlan::NotYetImplemented {
                    detail: "Join (Phase D)".to_string(),
                    input: None,
                },
                target_nodes: vec![0xA],
                cost: CostEstimate::default(),
            },
            total_cost: CostEstimate::default(),
        };
        let err = fed.execute(plan).await.unwrap_err();
        match err {
            MeshError::PlannerError { detail } => {
                assert!(detail.contains("Join (Phase D)"), "got: {detail}");
            }
            other => panic!("expected PlannerError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cancel_before_first_response_short_circuits_stream() {
        let chain = 0xFEED;
        let node =
            local_executor_with(&[(chain, 1, b"p-1"), (chain, 2, b"p-2"), (chain, 3, b"p-3")]);
        let transport = Arc::new(LoopbackTransport::new());
        transport.register(0xA, node);

        let fed = FederatedMeshQueryExecutor::new(transport);
        let plan = plan_between(chain, 1, 4, vec![0xA]);
        let running = fed.execute(plan).await.unwrap();
        running.handle.cancel();
        // Drain — the cancel flag may have been set before any
        // response was pumped, so at least the first item
        // should be QueryCancelled.
        let rows = collect_rows(running.rows).await;
        assert!(
            rows.iter()
                .any(|r| matches!(r, Err(MeshError::QueryCancelled))),
            "expected at least one QueryCancelled, got {rows:?}"
        );
    }

    #[tokio::test]
    async fn lineage_emit_runs_locally_without_transport_dispatch() {
        // LineageEmit is a planner-only leaf; the federated
        // executor must NOT dispatch it to a remote node.
        // Empty transport proves it: if any send happens,
        // we'd surface NoRoute via ExecutorError. Instead,
        // the entries are emitted as rows locally.
        use super::super::planner::{LineageDirection, LineageEntry};

        let transport = Arc::new(LoopbackTransport::new());
        let fed = FederatedMeshQueryExecutor::new(transport);
        let plan = ExecutionPlan {
            root: OperatorNode {
                operator: OperatorPlan::LineageEmit {
                    origin: 0xAA,
                    direction: LineageDirection::Forward,
                    entries: vec![
                        LineageEntry {
                            origin: 0xAA,
                            depth: 0,
                            tip_seq: Some(SeqNum(1)),
                        },
                        LineageEntry {
                            origin: 0xBB,
                            depth: 1,
                            tip_seq: None,
                        },
                    ],
                },
                target_nodes: vec![],
                cost: CostEstimate::default(),
            },
            total_cost: CostEstimate::default(),
        };
        let running = fed.execute(plan).await.unwrap();
        let rows: Vec<ResultRow> = collect_rows(running.rows)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].origin, 0xAA);
        assert_eq!(rows[0].seq, SeqNum(1));
        assert_eq!(rows[1].origin, 0xBB);
        assert_eq!(rows[1].seq, SeqNum(0));
    }

    #[tokio::test]
    async fn federated_hash_join_fetches_both_sides_and_emits_pairs() {
        use super::super::planner::{CostEstimate, JoinKeyMode};
        use super::super::query::{JoinKind, JoinedRowPayload};
        use std::time::Duration;

        // Two chains, each on a different node. The federated
        // executor dispatches each Between read separately,
        // then hash-joins the results locally.
        let a = 0x111;
        let b = 0x222;
        let node_a = local_executor_with(&[(a, 1, b"a-1"), (a, 2, b"a-2"), (a, 5, b"a-5")]);
        let node_b = local_executor_with(&[(b, 2, b"b-2"), (b, 3, b"b-3"), (b, 5, b"b-5")]);

        let transport = Arc::new(LoopbackTransport::new());
        transport.register(0xA, node_a);
        transport.register(0xB, node_b);

        let fed = FederatedMeshQueryExecutor::new(transport);
        let plan = ExecutionPlan {
            root: OperatorNode {
                operator: OperatorPlan::HashJoin {
                    left: Box::new(OperatorNode {
                        operator: OperatorPlan::BetweenRead {
                            origin: a,
                            start: SeqNum(1),
                            end: SeqNum(10),
                        },
                        target_nodes: vec![0xA],
                        cost: CostEstimate::default(),
                    }),
                    right: Box::new(OperatorNode {
                        operator: OperatorPlan::BetweenRead {
                            origin: b,
                            start: SeqNum(1),
                            end: SeqNum(10),
                        },
                        target_nodes: vec![0xB],
                        cost: CostEstimate::default(),
                    }),
                    key_mode: JoinKeyMode::Seq,
                    kind: JoinKind::Inner,
                    strategy: super::super::planner::JoinStrategy::HashBroadcast,
                    watermark: Duration::from_secs(5),
                },
                target_nodes: vec![],
                cost: CostEstimate::default(),
            },
            total_cost: CostEstimate::default(),
        };
        let running = fed.execute(plan).await.unwrap();
        let rows: Vec<ResultRow> = collect_rows(running.rows)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        // Seqs 2 and 5 match across both sides → 2 pairs.
        assert_eq!(rows.len(), 2);
        let mut decoded: Vec<JoinedRowPayload> = rows
            .iter()
            .map(|r| postcard::from_bytes(&r.payload).unwrap())
            .collect();
        decoded.sort_by_key(|j| j.left.as_ref().unwrap().seq);
        assert_eq!(decoded[0].left.as_ref().unwrap().payload, b"a-2");
        assert_eq!(decoded[0].right.as_ref().unwrap().payload, b"b-2");
        assert_eq!(decoded[1].left.as_ref().unwrap().payload, b"a-5");
        assert_eq!(decoded[1].right.as_ref().unwrap().payload, b"b-5");
    }

    #[tokio::test]
    async fn federated_left_outer_emits_unmatched_lefts_via_transport() {
        use super::super::planner::{CostEstimate, JoinKeyMode};
        use super::super::query::{JoinKind, JoinedRowPayload};
        use std::time::Duration;

        // Left chain a: seqs 1,2. Right chain b: seq 2.
        // LeftOuter on seq → 1 unmatched + 1 matched.
        let a = 0xAAAA;
        let b = 0xBBBB;
        let node_a = local_executor_with(&[(a, 1, b"a-1"), (a, 2, b"a-2")]);
        let node_b = local_executor_with(&[(b, 2, b"b-2")]);

        let transport = Arc::new(LoopbackTransport::new());
        transport.register(0xA, node_a);
        transport.register(0xB, node_b);

        let fed = FederatedMeshQueryExecutor::new(transport);
        let plan = ExecutionPlan {
            root: OperatorNode {
                operator: OperatorPlan::HashJoin {
                    left: Box::new(OperatorNode {
                        operator: OperatorPlan::BetweenRead {
                            origin: a,
                            start: SeqNum(1),
                            end: SeqNum(10),
                        },
                        target_nodes: vec![0xA],
                        cost: CostEstimate::default(),
                    }),
                    right: Box::new(OperatorNode {
                        operator: OperatorPlan::BetweenRead {
                            origin: b,
                            start: SeqNum(1),
                            end: SeqNum(10),
                        },
                        target_nodes: vec![0xB],
                        cost: CostEstimate::default(),
                    }),
                    key_mode: JoinKeyMode::Seq,
                    kind: JoinKind::LeftOuter,
                    strategy: super::super::planner::JoinStrategy::HashBroadcast,
                    watermark: Duration::from_secs(5),
                },
                target_nodes: vec![],
                cost: CostEstimate::default(),
            },
            total_cost: CostEstimate::default(),
        };
        let running = fed.execute(plan).await.unwrap();
        let rows: Vec<ResultRow> = collect_rows(running.rows)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows.len(), 2);
        let decoded: Vec<JoinedRowPayload> = rows
            .iter()
            .map(|r| postcard::from_bytes(&r.payload).unwrap())
            .collect();
        let matched = decoded.iter().filter(|j| j.right.is_some()).count();
        let unmatched = decoded.iter().filter(|j| j.right.is_none()).count();
        assert_eq!(matched, 1);
        assert_eq!(unmatched, 1);
        // The unmatched-left row must always have left=Some.
        assert!(decoded.iter().all(|j| j.left.is_some()));
    }

    #[tokio::test]
    async fn federated_aggregate_count_no_group_by_returns_total() {
        use super::super::planner::CostEstimate;
        use super::super::query::{AggregateRowPayload, AggregateValue};

        let chain = 0xCAFE;
        let node = local_executor_with(&[
            (chain, 1, b"x"),
            (chain, 2, b"y"),
            (chain, 3, b"z"),
            (chain, 4, b"w"),
        ]);
        let transport = Arc::new(LoopbackTransport::new());
        transport.register(0xA, node);

        let fed = FederatedMeshQueryExecutor::new(transport);
        let plan = ExecutionPlan {
            root: OperatorNode {
                operator: OperatorPlan::AggregateCount {
                    input: Box::new(OperatorNode {
                        operator: OperatorPlan::BetweenRead {
                            origin: chain,
                            start: SeqNum(1),
                            end: SeqNum(10),
                        },
                        target_nodes: vec![0xA],
                        cost: CostEstimate::default(),
                    }),
                    group_by: None,
                },
                target_nodes: vec![],
                cost: CostEstimate::default(),
            },
            total_cost: CostEstimate::default(),
        };
        let running = fed.execute(plan).await.unwrap();
        let rows: Vec<ResultRow> = collect_rows(running.rows)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows.len(), 1);
        let decoded: AggregateRowPayload = postcard::from_bytes(&rows[0].payload).unwrap();
        assert_eq!(decoded.group, None);
        assert_eq!(decoded.value, AggregateValue::Count(4));
    }

    #[test]
    fn call_id_is_unique_across_federated_executors_on_same_host() {
        // Regression: per-executor counters previously
        // collided when two federated executors on the same
        // caller hit a shared remote. The process-global
        // counter makes every allocated id unique across all
        // federated executors in the process.
        let t1 = Arc::new(LoopbackTransport::new());
        let t2 = Arc::new(LoopbackTransport::new());
        let fed1 = FederatedMeshQueryExecutor::new(t1);
        let fed2 = FederatedMeshQueryExecutor::new(t2);
        let mut seen = std::collections::HashSet::<u64>::new();
        for _ in 0..32 {
            assert!(seen.insert(fed1.allocate_id()), "fed1 self-collision");
            assert!(seen.insert(fed2.allocate_id()), "fed2 self-collision");
        }
    }

    #[tokio::test]
    async fn cancel_after_composite_aggregate_short_circuits_materialized_stream() {
        // Regression: pre-fix, federated composite operators
        // (Aggregate / Join / Window / Filter) materialized
        // their output into `futures::stream::iter(out)` and
        // allocated a fresh `QueryHandle` per recursive call.
        // The outer `running.handle.cancel()` was a no-op for
        // every composite plan because the materialized iter
        // ignored the cancel flag. This test pins the fixed
        // behavior: cancel after composite materialization
        // surfaces `QueryCancelled` from the row stream.
        use super::super::planner::CostEstimate;

        let chain = 0xC0DE;
        let node = local_executor_with(&[(chain, 1, b"x"), (chain, 2, b"y"), (chain, 3, b"z")]);
        let transport = Arc::new(LoopbackTransport::new());
        transport.register(0xA, node);

        let fed = FederatedMeshQueryExecutor::new(transport);
        let plan = ExecutionPlan {
            root: OperatorNode {
                operator: OperatorPlan::AggregateCount {
                    input: Box::new(OperatorNode {
                        operator: OperatorPlan::BetweenRead {
                            origin: chain,
                            start: SeqNum(1),
                            end: SeqNum(10),
                        },
                        target_nodes: vec![0xA],
                        cost: CostEstimate::default(),
                    }),
                    group_by: None,
                },
                target_nodes: vec![],
                cost: CostEstimate::default(),
            },
            total_cost: CostEstimate::default(),
        };

        let running = fed.execute(plan).await.unwrap();
        running.handle.cancel();
        let rows = collect_rows(running.rows).await;
        assert!(
            rows.iter()
                .any(|r| matches!(r, Err(MeshError::QueryCancelled))),
            "expected QueryCancelled to surface from a cancelled composite stream, got {rows:?}"
        );
    }

    #[tokio::test]
    async fn three_nodes_disjoint_chains_route_independently() {
        // Each node holds a different chain. Two queries fan
        // out to different targets via the same transport.
        let chain_x = 0x111;
        let chain_y = 0x222;
        let node_a = local_executor_with(&[(chain_x, 1, b"x-1")]);
        let node_b = local_executor_with(&[(chain_y, 1, b"y-1")]);
        let node_c = local_executor_with(&[]);

        let transport = Arc::new(LoopbackTransport::new());
        transport.register(0xA, node_a);
        transport.register(0xB, node_b);
        transport.register(0xC, node_c);

        let fed = FederatedMeshQueryExecutor::new(transport);

        let rows_x: Vec<_> = collect_rows(
            fed.execute(plan_latest(chain_x, vec![0xA]))
                .await
                .unwrap()
                .rows,
        )
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();
        assert_eq!(rows_x.len(), 1);
        assert_eq!(rows_x[0].payload, b"x-1");

        let rows_y: Vec<_> = collect_rows(
            fed.execute(plan_latest(chain_y, vec![0xB]))
                .await
                .unwrap()
                .rows,
        )
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();
        assert_eq!(rows_y.len(), 1);
        assert_eq!(rows_y[0].payload, b"y-1");
    }
}
