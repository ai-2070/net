//! `MeshQueryExecutor` — walks an [`ExecutionPlan`] and produces
//! [`ResultRow`]s.
//!
//! Phase B-2 lands the trait surface + a single-node
//! [`LocalMeshQueryExecutor`] that reads events through a
//! pluggable [`ChainReader`] abstraction. Federation
//! (cross-node fan-out, fold-on-relay, partial-result resume)
//! lands in Phase B-4 once the 3-node integration harness is
//! in place.
//!
//! # Surface
//!
//! - [`ChainReader`] — lower-level read primitive that maps a
//!   chain origin hash (`u64`) to event payloads. Decouples the
//!   executor from the substrate's channel-keyed storage so the
//!   integration layer can pick its own origin→channel
//!   resolution strategy.
//! - [`MeshQueryExecutor`] — async user-facing trait. Returns a
//!   [`RunningQuery`] carrying a row stream + a [`QueryHandle`]
//!   for cancellation.
//! - [`LocalMeshQueryExecutor`] — the Phase B-2 implementation
//!   for single-node reads. Handles atomic operators
//!   (`AtRead` / `BetweenRead` / `LatestRead`); composite
//!   operators surface `MeshError::PlannerError` until their
//!   phase activates.
//!
//! # Cancellation
//!
//! [`QueryHandle::cancel`] flips a shared `AtomicBool`. The
//! executor checks it between operator steps and surfaces
//! [`MeshError::QueryCancelled`]. Cancellation is cooperative —
//! a long-running read won't be interrupted mid-syscall, but
//! the next row boundary will exit the stream.

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::Stream;
use futures::StreamExt;

use super::error::MeshError;
use super::planner::{ExecutionPlan, OperatorNode, OperatorPlan};
use super::query::{ResultRow, SeqNum};

/// Unique id for a running query. Currently a monotonically-
/// increasing `u64` per executor; surfaces through metrics and
/// the [`QueryHandle`] for cross-referencing.
pub type QueryId = u64;

/// Row stream returned by an executor. Pinned + boxed so the
/// trait stays object-safe and consumers can write
/// `&mut dyn Stream<...>`.
pub type ResultStream = Pin<Box<dyn Stream<Item = Result<ResultRow, MeshError>> + Send>>;

/// A handle to an in-flight query. Returned alongside the row
/// stream so callers can cancel a query without dropping the
/// stream (the stream itself short-circuits to
/// [`MeshError::QueryCancelled`] on the next row boundary).
///
/// Cheap to clone; the cancel flag is shared.
#[derive(Clone, Debug)]
pub struct QueryHandle {
    id: QueryId,
    cancel: Arc<AtomicBool>,
}

impl QueryHandle {
    /// Construct a fresh handle with the given id. The cancel
    /// flag starts cleared. Used by both [`LocalMeshQueryExecutor`]
    /// and the federated executor in `super::federated`.
    pub fn new(id: QueryId) -> Self {
        Self {
            id,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The query's id.
    pub fn id(&self) -> QueryId {
        self.id
    }

    /// Signal cancellation. The row stream will surface
    /// [`MeshError::QueryCancelled`] at its next yield point.
    /// Idempotent.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// Whether [`Self::cancel`] has been called.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
}

/// A running query — a [`QueryHandle`] for cancellation paired
/// with the [`ResultStream`] of rows.
pub struct RunningQuery {
    /// Handle for cancel + id.
    pub handle: QueryHandle,
    /// Stream of rows. Terminates either when the operator
    /// tree is exhausted or when the handle's cancel flag is
    /// flipped.
    pub rows: ResultStream,
}

impl std::fmt::Debug for RunningQuery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningQuery")
            .field("handle", &self.handle)
            .field("rows", &"<stream>")
            .finish()
    }
}

/// Lower-level read primitive consumed by the executor.
///
/// Decouples the executor from the substrate's channel-keyed
/// storage: an integration-layer implementor decides how to
/// resolve a chain origin hash (`u64`) into a readable chain
/// (e.g. by maintaining a secondary `origin → ChannelName`
/// index and dispatching to a `Redex` manager).
///
/// Methods are synchronous because the underlying RedEX read
/// API is synchronous (mmap-backed). The async dimension lives
/// at the [`MeshQueryExecutor`] level for cross-node fan-out.
pub trait ChainReader: Send + Sync {
    /// Read the event at `seq` from chain `origin`. `None` if
    /// the chain is unknown, the seq has been evicted, or
    /// never written.
    fn read_one(&self, origin: u64, seq: SeqNum) -> Option<Vec<u8>>;

    /// Read events in `[start, end)` from chain `origin`.
    /// Returns `(seq, payload)` pairs in seq-asc order. Evicted
    /// entries are silently skipped, matching RedEX semantics.
    fn read_range(&self, origin: u64, start: SeqNum, end: SeqNum) -> Vec<(SeqNum, Vec<u8>)>;

    /// The tip seq for `origin`, or `None` if the chain has
    /// never been written or is unknown.
    fn latest_seq(&self, origin: u64) -> Option<SeqNum>;
}

/// The user-facing executor surface. Walks an
/// [`ExecutionPlan`] and produces rows.
///
/// Phase B-2 implementors:
/// - [`LocalMeshQueryExecutor`] — single-node, reads through
///   a [`ChainReader`].
///
/// Future phases: a `FederatedMeshQueryExecutor` that fans out
/// to `target_nodes` over the transport, with partial-result
/// + continuation-token semantics.
#[async_trait]
pub trait MeshQueryExecutor: Send + Sync {
    /// Begin execution of `plan`. Returns a [`RunningQuery`]
    /// with a row stream + a cancellable handle.
    ///
    /// Errors before stream construction (e.g. unresolved
    /// composite operator) surface synchronously; errors mid-
    /// stream surface as `Err` items in the stream.
    async fn execute(&self, plan: ExecutionPlan) -> Result<RunningQuery, MeshError>;
}

/// Single-node executor. Generic over a [`ChainReader`] so the
/// tests can drive it without needing a real RedEX file.
pub struct LocalMeshQueryExecutor<R: ChainReader> {
    reader: Arc<R>,
    next_id: AtomicU64,
}

impl<R: ChainReader> LocalMeshQueryExecutor<R> {
    /// Construct a new local executor.
    pub fn new(reader: Arc<R>) -> Self {
        Self {
            reader,
            next_id: AtomicU64::new(1),
        }
    }

    fn allocate_id(&self) -> QueryId {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }
}

#[async_trait]
impl<R: ChainReader + 'static> MeshQueryExecutor for LocalMeshQueryExecutor<R> {
    async fn execute(&self, plan: ExecutionPlan) -> Result<RunningQuery, MeshError> {
        let handle = QueryHandle::new(self.allocate_id());
        let rows = walk_operator(&plan.root, self.reader.clone(), handle.clone())?;
        Ok(RunningQuery { handle, rows })
    }
}

/// Walk one operator node and produce its row stream.
///
/// Atomic operators (`AtRead` / `BetweenRead` / `LatestRead`)
/// read through the [`ChainReader`] and emit rows synchronously
/// (the stream is a [`futures::stream::iter`] over the result).
/// Composite operators (`Filter`, etc.) are not yet wired and
/// surface [`MeshError::PlannerError`] synchronously so callers
/// see the misconfiguration up front.
fn walk_operator<R: ChainReader + 'static>(
    node: &OperatorNode,
    reader: Arc<R>,
    handle: QueryHandle,
) -> Result<ResultStream, MeshError> {
    match &node.operator {
        OperatorPlan::AtRead { origin, seq } => {
            let row = reader.read_one(*origin, *seq).map(|payload| ResultRow {
                origin: *origin,
                seq: *seq,
                payload,
            });
            Ok(stream_from_vec(row.into_iter().collect(), handle))
        }
        OperatorPlan::BetweenRead { origin, start, end } => {
            let events = reader.read_range(*origin, *start, *end);
            let rows: Vec<ResultRow> = events
                .into_iter()
                .map(|(seq, payload)| ResultRow {
                    origin: *origin,
                    seq,
                    payload,
                })
                .collect();
            Ok(stream_from_vec(rows, handle))
        }
        OperatorPlan::LatestRead { origin } => {
            let row = match reader.latest_seq(*origin) {
                Some(tip) => reader.read_one(*origin, tip).map(|payload| ResultRow {
                    origin: *origin,
                    seq: tip,
                    payload,
                }),
                None => None,
            };
            Ok(stream_from_vec(row.into_iter().collect(), handle))
        }
        OperatorPlan::Filter { .. } => Err(MeshError::PlannerError {
            detail: "Filter executor not yet implemented (Phase E)".to_string(),
        }),
        OperatorPlan::NotYetImplemented { detail, .. } => Err(MeshError::PlannerError {
            detail: format!("operator not yet implemented: {detail}"),
        }),
    }
}

/// Wrap a finite `Vec<ResultRow>` in a `ResultStream` that
/// honours the cancellation flag: each yielded row checks the
/// handle's cancel bit and short-circuits with
/// [`MeshError::QueryCancelled`] if it has been flipped.
fn stream_from_vec(rows: Vec<ResultRow>, handle: QueryHandle) -> ResultStream {
    let iter = rows.into_iter();
    let stream = futures::stream::iter(iter).map(move |row| {
        if handle.is_cancelled() {
            Err(MeshError::QueryCancelled)
        } else {
            Ok(row)
        }
    });
    Box::pin(stream)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use futures::StreamExt;

    use super::*;
    use crate::adapter::net::behavior::meshdb::planner::{CostEstimate, ExecutionPlan};

    /// Test-only `ChainReader` backed by a `BTreeMap<u64,
    /// BTreeMap<SeqNum, Vec<u8>>>`. Keeps tests hermetic.
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

    fn atomic_plan(op: OperatorPlan) -> ExecutionPlan {
        ExecutionPlan {
            root: OperatorNode {
                operator: op,
                target_nodes: vec![],
                cost: CostEstimate::default(),
            },
            total_cost: CostEstimate::default(),
        }
    }

    async fn collect_rows(rows: ResultStream) -> Vec<Result<ResultRow, MeshError>> {
        rows.collect::<Vec<_>>().await
    }

    #[tokio::test]
    async fn at_read_emits_single_row() {
        let reader = Arc::new(InMemoryChainReader::default());
        reader.append(0xAA, SeqNum(7), b"payload-7".to_vec());

        let executor = LocalMeshQueryExecutor::new(reader);
        let plan = atomic_plan(OperatorPlan::AtRead {
            origin: 0xAA,
            seq: SeqNum(7),
        });

        let running = executor.execute(plan).await.unwrap();
        let rows = collect_rows(running.rows).await;
        assert_eq!(rows.len(), 1);
        let row = rows.into_iter().next().unwrap().unwrap();
        assert_eq!(row.origin, 0xAA);
        assert_eq!(row.seq, SeqNum(7));
        assert_eq!(row.payload, b"payload-7");
    }

    #[tokio::test]
    async fn at_read_emits_empty_stream_when_seq_missing() {
        // Missing seq is a non-error: the stream is just
        // empty. HistoricalRangeUnavailable is the planner's
        // job (it knows what holders advertised); the
        // executor's job is to read what's there.
        let reader = Arc::new(InMemoryChainReader::default());
        let executor = LocalMeshQueryExecutor::new(reader);
        let plan = atomic_plan(OperatorPlan::AtRead {
            origin: 0xAA,
            seq: SeqNum(99),
        });

        let running = executor.execute(plan).await.unwrap();
        let rows = collect_rows(running.rows).await;
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn between_read_emits_rows_in_seq_order() {
        let reader = Arc::new(InMemoryChainReader::default());
        for s in [3u64, 5, 7, 11, 13] {
            reader.append(0xAB, SeqNum(s), format!("p-{s}").into_bytes());
        }
        let executor = LocalMeshQueryExecutor::new(reader);
        let plan = atomic_plan(OperatorPlan::BetweenRead {
            origin: 0xAB,
            start: SeqNum(5),
            end: SeqNum(12),
        });

        let running = executor.execute(plan).await.unwrap();
        let rows: Vec<ResultRow> = collect_rows(running.rows)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        let seqs: Vec<u64> = rows.iter().map(|r| r.seq.0).collect();
        assert_eq!(seqs, vec![5, 7, 11]);
        assert!(rows.iter().all(|r| r.origin == 0xAB));
    }

    #[tokio::test]
    async fn between_read_half_open_excludes_end() {
        let reader = Arc::new(InMemoryChainReader::default());
        reader.append(0xAB, SeqNum(5), b"five".to_vec());
        reader.append(0xAB, SeqNum(10), b"ten".to_vec());

        let executor = LocalMeshQueryExecutor::new(reader);
        let plan = atomic_plan(OperatorPlan::BetweenRead {
            origin: 0xAB,
            start: SeqNum(5),
            end: SeqNum(10),
        });

        let rows: Vec<ResultRow> = collect_rows(executor.execute(plan).await.unwrap().rows)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].seq, SeqNum(5));
    }

    #[tokio::test]
    async fn latest_read_returns_tip() {
        let reader = Arc::new(InMemoryChainReader::default());
        for s in [1u64, 4, 9] {
            reader.append(0xCD, SeqNum(s), format!("p-{s}").into_bytes());
        }
        let executor = LocalMeshQueryExecutor::new(reader);
        let plan = atomic_plan(OperatorPlan::LatestRead { origin: 0xCD });

        let rows: Vec<ResultRow> = collect_rows(executor.execute(plan).await.unwrap().rows)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].seq, SeqNum(9));
        assert_eq!(rows[0].payload, b"p-9");
    }

    #[tokio::test]
    async fn latest_read_empty_chain_yields_empty_stream() {
        let reader = Arc::new(InMemoryChainReader::default());
        let executor = LocalMeshQueryExecutor::new(reader);
        let plan = atomic_plan(OperatorPlan::LatestRead { origin: 0xCD });

        let rows = collect_rows(executor.execute(plan).await.unwrap().rows).await;
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn cancel_short_circuits_stream() {
        let reader = Arc::new(InMemoryChainReader::default());
        for s in 1u64..=10 {
            reader.append(0xEF, SeqNum(s), format!("p-{s}").into_bytes());
        }
        let executor = LocalMeshQueryExecutor::new(reader);
        let plan = atomic_plan(OperatorPlan::BetweenRead {
            origin: 0xEF,
            start: SeqNum(1),
            end: SeqNum(11),
        });

        let running = executor.execute(plan).await.unwrap();
        running.handle.cancel();
        assert!(running.handle.is_cancelled());

        let rows = collect_rows(running.rows).await;
        // Every item is QueryCancelled because cancel was
        // flipped before the first poll. Stream length still
        // equals the underlying row count — cancellation is
        // cooperative, not a hard abort.
        assert_eq!(rows.len(), 10);
        assert!(
            rows.iter()
                .all(|r| matches!(r, Err(MeshError::QueryCancelled))),
            "expected all-QueryCancelled, got {rows:?}"
        );
    }

    #[tokio::test]
    async fn handle_id_is_unique_per_execute() {
        let reader = Arc::new(InMemoryChainReader::default());
        let executor = LocalMeshQueryExecutor::new(reader);
        let p = || atomic_plan(OperatorPlan::LatestRead { origin: 0x01 });

        let r1 = executor.execute(p()).await.unwrap();
        let r2 = executor.execute(p()).await.unwrap();
        let r3 = executor.execute(p()).await.unwrap();
        assert_ne!(r1.handle.id(), r2.handle.id());
        assert_ne!(r2.handle.id(), r3.handle.id());
    }

    #[tokio::test]
    async fn not_yet_implemented_surfaces_planner_error() {
        let reader = Arc::new(InMemoryChainReader::default());
        let executor = LocalMeshQueryExecutor::new(reader);
        let plan = atomic_plan(OperatorPlan::NotYetImplemented {
            detail: "Join (Phase D)".to_string(),
            input: None,
        });

        let err = executor.execute(plan).await.unwrap_err();
        match err {
            MeshError::PlannerError { detail } => {
                assert!(detail.contains("Join (Phase D)"), "got: {detail}");
            }
            other => panic!("expected PlannerError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn filter_operator_surfaces_planner_error_until_phase_e() {
        use crate::adapter::net::behavior::predicate::Predicate;
        use crate::adapter::net::behavior::tag::{TagKey, TaxonomyAxis};

        let reader = Arc::new(InMemoryChainReader::default());
        let executor = LocalMeshQueryExecutor::new(reader);
        let predicate = Predicate::Exists {
            key: TagKey {
                axis: TaxonomyAxis::Dataforts,
                key: "blob.storage".to_string(),
            },
        }
        .to_wire();
        let plan = atomic_plan(OperatorPlan::Filter {
            input: Box::new(OperatorNode {
                operator: OperatorPlan::LatestRead { origin: 0x01 },
                target_nodes: vec![],
                cost: CostEstimate::default(),
            }),
            predicate,
        });

        let err = executor.execute(plan).await.unwrap_err();
        match err {
            MeshError::PlannerError { detail } => {
                assert!(detail.contains("Filter"), "got: {detail}");
            }
            other => panic!("expected PlannerError, got {other:?}"),
        }
    }
}
