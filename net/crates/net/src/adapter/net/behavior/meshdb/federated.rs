//! `FederatedMeshQueryExecutor` — fans out atomic operators to
//! remote `target_nodes` over a pluggable [`MeshDbTransport`].
//!
//! Phase B-4 lands the cross-node executor + a [`LoopbackTransport`]
//! for in-process integration testing. The real subprotocol
//! wiring (registering the `SUBPROTOCOL_MESHDB` handler on the
//! mesh, framing requests + responses on the wire) lands when a
//! concrete consumer (Hermes telemetry / Deck metrics) drives
//! the final shape.
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
//! Out-of-band cancellation to the remote executor (so the
//! remote can free its resources) lands in a later slice;
//! Phase B-4 ships the local-side cancellation only.

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
pub struct FederatedMeshQueryExecutor<T: MeshDbTransport> {
    transport: Arc<T>,
    next_id: AtomicU64,
}

impl<T: MeshDbTransport> FederatedMeshQueryExecutor<T> {
    /// Construct a federated executor over the given transport.
    pub fn new(transport: Arc<T>) -> Self {
        Self {
            transport,
            next_id: AtomicU64::new(1),
        }
    }

    fn allocate_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }
}

#[async_trait]
impl<T: MeshDbTransport + 'static> MeshQueryExecutor for FederatedMeshQueryExecutor<T> {
    async fn execute(&self, plan: ExecutionPlan) -> Result<RunningQuery, MeshError> {
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
                let handle = QueryHandle::new(self.allocate_id());
                let rows: Vec<Result<ResultRow, MeshError>> = entries
                    .iter()
                    .map(|entry| {
                        Ok(ResultRow {
                            origin: entry.origin,
                            seq: entry.tip_seq.unwrap_or(SeqNum(0)),
                            payload: Vec::new(),
                        })
                    })
                    .collect();
                let stream: ResultStream = Box::pin(futures::stream::iter(rows));
                return Ok(RunningQuery {
                    handle,
                    rows: stream,
                });
            }
            OperatorPlan::HashJoin { .. } => {
                // Phase D-1 federated joins: fetch left + right
                // via the transport, hash join locally.
                return self.execute_hash_join_federated(plan).await;
            }
            OperatorPlan::Filter { .. } => {
                return Err(MeshError::PlannerError {
                    detail: "Filter executor not yet implemented (Phase E)".to_string(),
                });
            }
            OperatorPlan::NotYetImplemented { detail, .. } => {
                return Err(MeshError::PlannerError {
                    detail: format!("operator not yet implemented: {detail}"),
                });
            }
        }

        let handle = QueryHandle::new(self.allocate_id());
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
            last_attempted = target;
            match self.transport.send(target, request.clone()).await {
                Ok(s) => {
                    response_stream = Some(s);
                    break;
                }
                Err(TransportError::NoRoute(_)) => {
                    last_err = Some(TransportError::NoRoute(target));
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
    /// build a hash table on left, probe with right, emit
    /// [`super::query::JoinedRowPayload`] inside sentinel
    /// rows. Inner-join only — outer kinds surface
    /// `PlannerError`.
    async fn execute_hash_join_federated(
        &self,
        plan: ExecutionPlan,
    ) -> Result<RunningQuery, MeshError> {
        use super::planner::CostEstimate;
        use super::query::{JoinKind, JoinedRowPayload, SeqNum};

        let OperatorPlan::HashJoin {
            left,
            right,
            key_mode,
            kind,
            ..
        } = plan.root.operator
        else {
            unreachable!("execute_hash_join_federated dispatched on non-HashJoin");
        };
        if kind != JoinKind::Inner {
            return Err(MeshError::PlannerError {
                detail: format!(
                    "join kind {kind:?} not yet implemented in Phase D-1; only Inner supported (outer joins land in D-2)"
                ),
            });
        }

        // Reuse the federated execute path for each side by
        // wrapping the sub-plan in a fresh ExecutionPlan.
        let left_running = Box::pin(self.execute(ExecutionPlan {
            root: *left,
            total_cost: CostEstimate::default(),
        }))
        .await?;
        let right_running = Box::pin(self.execute(ExecutionPlan {
            root: *right,
            total_cost: CostEstimate::default(),
        }))
        .await?;

        let left_rows = drain_rows(left_running.rows).await?;
        let right_rows = drain_rows(right_running.rows).await?;

        // Build on left, probe with right; same memory bound
        // as the local executor (a Phase D-2 refinement will
        // make this configurable per-query).
        use std::collections::HashMap;
        let mut build: HashMap<Vec<u8>, Vec<ResultRow>> = HashMap::new();
        let mut build_bytes: u64 = 0;
        for row in left_rows {
            let key = encode_join_key_federated(&row, key_mode);
            let approx = (row.payload.len() + key.len() + 64) as u64;
            build_bytes = build_bytes.saturating_add(approx);
            if build_bytes > super::executor::HASH_JOIN_MEMORY_BYTES {
                return Err(MeshError::JoinMemoryExceeded {
                    strategy: "broadcast-hash-federated".to_string(),
                    threshold_bytes: super::executor::HASH_JOIN_MEMORY_BYTES,
                });
            }
            build.entry(key).or_default().push(row);
        }

        let mut out: Vec<Result<ResultRow, MeshError>> = Vec::new();
        for r in right_rows {
            let key = encode_join_key_federated(&r, key_mode);
            if let Some(lefts) = build.get(&key) {
                for l in lefts {
                    let payload = postcard::to_allocvec(&JoinedRowPayload {
                        left: l.clone(),
                        right: Some(r.clone()),
                    })
                    .map_err(|e| MeshError::ExecutorError {
                        node: 0,
                        detail: format!("encode JoinedRowPayload: {e}"),
                    })?;
                    out.push(Ok(ResultRow {
                        origin: 0,
                        seq: SeqNum(0),
                        payload,
                    }));
                }
            }
        }

        let handle = QueryHandle::new(self.allocate_id());
        let stream: ResultStream = Box::pin(futures::stream::iter(out));
        Ok(RunningQuery {
            handle,
            rows: stream,
        })
    }
}

/// Drain a [`ResultStream`] into a `Vec<ResultRow>`. Errors
/// short-circuit the drain with the first encountered error.
async fn drain_rows(mut s: ResultStream) -> Result<Vec<ResultRow>, MeshError> {
    let mut out = Vec::new();
    while let Some(item) = s.next().await {
        out.push(item?);
    }
    Ok(out)
}

/// Federated mirror of `executor::encode_join_key`. Duplicated
/// to avoid a public crate-export of a Phase D-1 implementation
/// detail; the two stay in lockstep by construction (key bytes
/// are intentionally the same).
fn encode_join_key_federated(
    row: &ResultRow,
    mode: super::planner::JoinKeyMode,
) -> Vec<u8> {
    use super::planner::JoinKeyMode;
    match mode {
        JoinKeyMode::Origin => row.origin.to_le_bytes().to_vec(),
        JoinKeyMode::Seq => row.seq.0.to_le_bytes().to_vec(),
        JoinKeyMode::OriginSeq => {
            let mut v = Vec::with_capacity(16);
            v.extend_from_slice(&row.origin.to_le_bytes());
            v.extend_from_slice(&row.seq.0.to_le_bytes());
            v
        }
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
    nodes: parking_lot::RwLock<
        std::collections::HashMap<u64, LoopbackNode>,
    >,
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
                let running = exec.execute(plan).await.map_err(|e| {
                    TransportError::Other(format!("remote execute failed: {e}"))
                })?;
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
                    let _ = tx
                        .send(MeshDbResponse::Error { call_id, error })
                        .await;
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

        fn read_range(
            &self,
            origin: u64,
            start: SeqNum,
            end: SeqNum,
        ) -> Vec<(SeqNum, Vec<u8>)> {
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

    fn local_executor_with(rows: &[(u64, u64, &[u8])]) -> Arc<LocalMeshQueryExecutor<InMemoryChainReader>> {
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

    fn plan_between(
        origin: u64,
        start: u64,
        end: u64,
        target_nodes: Vec<u64>,
    ) -> ExecutionPlan {
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
        let node_a = local_executor_with(&[
            (chain, 1, b"a-1"),
            (chain, 2, b"a-2"),
            (chain, 3, b"a-3"),
        ]);
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
        let node = local_executor_with(&[
            (chain, 1, b"p-1"),
            (chain, 2, b"p-2"),
            (chain, 3, b"p-3"),
        ]);
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
        let node_a = local_executor_with(&[
            (a, 1, b"a-1"),
            (a, 2, b"a-2"),
            (a, 5, b"a-5"),
        ]);
        let node_b = local_executor_with(&[
            (b, 2, b"b-2"),
            (b, 3, b"b-3"),
            (b, 5, b"b-5"),
        ]);

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
        decoded.sort_by_key(|j| j.left.seq);
        assert_eq!(decoded[0].left.payload, b"a-2");
        assert_eq!(decoded[0].right.as_ref().unwrap().payload, b"b-2");
        assert_eq!(decoded[1].left.payload, b"a-5");
        assert_eq!(decoded[1].right.as_ref().unwrap().payload, b"b-5");
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

        let rows_x: Vec<_> =
            collect_rows(fed.execute(plan_latest(chain_x, vec![0xA])).await.unwrap().rows)
                .await
                .into_iter()
                .map(|r| r.unwrap())
                .collect();
        assert_eq!(rows_x.len(), 1);
        assert_eq!(rows_x[0].payload, b"x-1");

        let rows_y: Vec<_> =
            collect_rows(fed.execute(plan_latest(chain_y, vec![0xB])).await.unwrap().rows)
                .await
                .into_iter()
                .map(|r| r.unwrap())
                .collect();
        assert_eq!(rows_y.len(), 1);
        assert_eq!(rows_y[0].payload, b"y-1");
    }
}
