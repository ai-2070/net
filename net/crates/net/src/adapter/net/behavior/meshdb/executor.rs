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
use super::planner::{ExecutionPlan, JoinKeyMode, OperatorNode, OperatorPlan};
use super::query::{
    AggregateRowPayload, AggregateValue, GroupKey, JoinKind, JoinedRowPayload,
    NumericAggregateKind, ResultRow, SeqNum,
};

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
    let rows = collect_operator_rows(node, reader.as_ref())?;
    Ok(stream_from_vec(rows, handle))
}

/// Synchronously walk an [`OperatorNode`] and collect all rows
/// into a `Vec<ResultRow>`. Used by [`walk_operator`] (which
/// then wraps the vec in a cancellation-aware stream) and by
/// [`execute_hash_join`] (which materializes both sides for
/// hashing). Every operator the local executor handles for
/// Phase D-1 is finite + synchronous, so a single pass is
/// fine.
fn collect_operator_rows<R: ChainReader + ?Sized>(
    node: &OperatorNode,
    reader: &R,
) -> Result<Vec<ResultRow>, MeshError> {
    match &node.operator {
        OperatorPlan::AtRead { origin, seq } => Ok(reader
            .read_one(*origin, *seq)
            .map(|payload| ResultRow {
                origin: *origin,
                seq: *seq,
                payload,
            })
            .into_iter()
            .collect()),
        OperatorPlan::BetweenRead { origin, start, end } => Ok(reader
            .read_range(*origin, *start, *end)
            .into_iter()
            .map(|(seq, payload)| ResultRow {
                origin: *origin,
                seq,
                payload,
            })
            .collect()),
        OperatorPlan::LatestRead { origin } => Ok(reader
            .latest_seq(*origin)
            .and_then(|tip| {
                reader.read_one(*origin, tip).map(|payload| ResultRow {
                    origin: *origin,
                    seq: tip,
                    payload,
                })
            })
            .into_iter()
            .collect()),
        OperatorPlan::LineageEmit { entries, .. } => Ok(entries
            .iter()
            .map(|entry| ResultRow {
                origin: entry.origin,
                seq: entry.tip_seq.unwrap_or(SeqNum(0)),
                payload: Vec::new(),
            })
            .collect()),
        OperatorPlan::HashJoin {
            left,
            right,
            key_mode,
            kind,
            ..
        } => execute_hash_join(left, right, *key_mode, *kind, reader),
        OperatorPlan::AggregateCount { input, group_by } => {
            let rows = collect_operator_rows(input, reader)?;
            execute_aggregate_count(&rows, *group_by)
        }
        OperatorPlan::AggregateNumeric {
            input,
            group_by,
            field_path,
            kind,
        } => {
            let rows = collect_operator_rows(input, reader)?;
            execute_aggregate_numeric(&rows, *group_by, field_path, *kind)
        }
        OperatorPlan::Filter { input, predicate } => {
            let rows = collect_operator_rows(input, reader)?;
            execute_filter(rows, predicate)
        }
        OperatorPlan::NotYetImplemented { detail, .. } => Err(MeshError::PlannerError {
            detail: format!("operator not yet implemented: {detail}"),
        }),
    }
}

/// Hash-join: build on `left`, probe with `right`, emit one
/// [`JoinedRowPayload`] per match (and per unmatched row for
/// outer kinds). Phase D-1 shipped `Inner`; Phase D-2 adds the
/// three outer variants. Memory is bounded by
/// [`HASH_JOIN_MEMORY_BYTES`]; the bound checks fire on the
/// build side (left for Inner/LeftOuter/FullOuter, right for
/// RightOuter).
fn execute_hash_join<R: ChainReader + ?Sized>(
    left: &OperatorNode,
    right: &OperatorNode,
    key_mode: JoinKeyMode,
    kind: JoinKind,
    reader: &R,
) -> Result<Vec<ResultRow>, MeshError> {
    let left_rows = collect_operator_rows(left, reader)?;
    let right_rows = collect_operator_rows(right, reader)?;

    match kind {
        JoinKind::Inner => hash_join_one_sided(left_rows, right_rows, key_mode, false, false),
        JoinKind::LeftOuter => hash_join_one_sided(left_rows, right_rows, key_mode, true, false),
        // RightOuter is symmetric: swap sides, build on right,
        // probe with left, and remember to swap back when
        // encoding the payload so `left` / `right` semantics
        // stay correct from the caller's perspective.
        JoinKind::RightOuter => hash_join_one_sided(right_rows, left_rows, key_mode, true, true),
        JoinKind::FullOuter => hash_join_full_outer(left_rows, right_rows, key_mode),
    }
}

/// Hash-join body. Builds on `build_rows`, probes with
/// `probe_rows`. `emit_unmatched_build` controls whether build-
/// side rows that never matched are emitted with the other side
/// `None` (i.e. LeftOuter / RightOuter behavior). `swap` flips
/// the (left, right) labelling in the emitted
/// [`JoinedRowPayload`] — used by RightOuter so that even after
/// "swap roles", callers see `right` as the canonical right.
fn hash_join_one_sided(
    build_rows: Vec<ResultRow>,
    probe_rows: Vec<ResultRow>,
    key_mode: JoinKeyMode,
    emit_unmatched_build: bool,
    swap: bool,
) -> Result<Vec<ResultRow>, MeshError> {
    use std::collections::HashMap;
    // Build phase: track memory + remember whether each build
    // entry has been matched.
    let mut build_bytes: u64 = 0;
    let mut build: HashMap<Vec<u8>, Vec<(ResultRow, bool)>> = HashMap::new();
    for row in build_rows {
        let key = encode_join_key(&row, key_mode);
        let approx = (row.payload.len() + key.len() + 64) as u64;
        build_bytes = build_bytes.saturating_add(approx);
        if build_bytes > HASH_JOIN_MEMORY_BYTES {
            return Err(MeshError::JoinMemoryExceeded {
                strategy: "broadcast-hash".to_string(),
                threshold_bytes: HASH_JOIN_MEMORY_BYTES,
            });
        }
        build.entry(key).or_default().push((row, false));
    }

    let mut out = Vec::new();
    // Probe phase: for each probe row, emit one joined row per
    // matching build entry and mark that build row as matched.
    for p in probe_rows {
        let key = encode_join_key(&p, key_mode);
        if let Some(entries) = build.get_mut(&key) {
            for (b, matched) in entries.iter_mut() {
                *matched = true;
                let (left, right) = if swap {
                    (Some(p.clone()), Some(b.clone()))
                } else {
                    (Some(b.clone()), Some(p.clone()))
                };
                out.push(encode_joined_row(left, right)?);
            }
        }
    }
    if emit_unmatched_build {
        for entries in build.into_values() {
            for (b, matched) in entries {
                if !matched {
                    let (left, right) = if swap {
                        (None, Some(b))
                    } else {
                        (Some(b), None)
                    };
                    out.push(encode_joined_row(left, right)?);
                }
            }
        }
    }
    Ok(out)
}

/// Full-outer hash-join: emits matched pairs + unmatched rows
/// from both sides.
fn hash_join_full_outer(
    left_rows: Vec<ResultRow>,
    right_rows: Vec<ResultRow>,
    key_mode: JoinKeyMode,
) -> Result<Vec<ResultRow>, MeshError> {
    use std::collections::HashMap;
    // Build a map of right rows so the left scan can emit
    // matched pairs + carry unmatched left rows; afterward,
    // sweep the right map for unmatched right rows.
    let mut build_bytes: u64 = 0;
    let mut right_map: HashMap<Vec<u8>, Vec<(ResultRow, bool)>> = HashMap::new();
    for row in right_rows {
        let key = encode_join_key(&row, key_mode);
        let approx = (row.payload.len() + key.len() + 64) as u64;
        build_bytes = build_bytes.saturating_add(approx);
        if build_bytes > HASH_JOIN_MEMORY_BYTES {
            return Err(MeshError::JoinMemoryExceeded {
                strategy: "broadcast-hash".to_string(),
                threshold_bytes: HASH_JOIN_MEMORY_BYTES,
            });
        }
        right_map.entry(key).or_default().push((row, false));
    }

    let mut out = Vec::new();
    for l in left_rows {
        let key = encode_join_key(&l, key_mode);
        match right_map.get_mut(&key) {
            Some(entries) => {
                for (r, matched) in entries.iter_mut() {
                    *matched = true;
                    out.push(encode_joined_row(Some(l.clone()), Some(r.clone()))?);
                }
            }
            None => {
                // Unmatched left.
                out.push(encode_joined_row(Some(l), None)?);
            }
        }
    }
    for entries in right_map.into_values() {
        for (r, matched) in entries {
            if !matched {
                out.push(encode_joined_row(None, Some(r))?);
            }
        }
    }
    Ok(out)
}

/// Phase E-2 row filter. Decodes the [`PredicateWire`] back to
/// a typed `Predicate`, builds a synthetic per-row view via
/// [`super::row::synthetic_row_view`], and evaluates the
/// predicate against each row's view. Rows whose evaluation
/// returns `true` pass through unchanged.
fn execute_filter(
    rows: Vec<ResultRow>,
    wire: &crate::adapter::net::behavior::predicate::PredicateWire,
) -> Result<Vec<ResultRow>, MeshError> {
    use crate::adapter::net::behavior::predicate::EvalContext;

    let predicate = wire
        .clone()
        .into_predicate()
        .map_err(|e| MeshError::PlannerError {
            detail: format!("Filter predicate rebuild failed: {e:?}"),
        })?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let (tags, metadata) = super::row::synthetic_row_view(&row);
        let ctx = EvalContext::new(&tags, &metadata);
        if predicate.evaluate(&ctx) {
            out.push(row);
        }
    }
    Ok(out)
}

/// Phase E-1 count aggregate. Groups `rows` by the row-
/// intrinsic key (or single bucket when `group_by` is `None`),
/// then emits one sentinel row per group whose `payload` is a
/// postcard-encoded [`AggregateRowPayload`].
///
/// Ordering: ungrouped emits exactly one row. Grouped emits
/// rows in deterministic order (lex on the encoded group key)
/// so the cache-key contract from locked decision #4 holds.
fn execute_aggregate_count(
    rows: &[ResultRow],
    group_by: Option<JoinKeyMode>,
) -> Result<Vec<ResultRow>, MeshError> {
    use std::collections::BTreeMap;

    match group_by {
        None => {
            let payload = encode_aggregate_payload(None, AggregateValue::Count(rows.len() as u64))?;
            Ok(vec![ResultRow {
                origin: 0,
                seq: SeqNum(0),
                payload,
            }])
        }
        Some(mode) => {
            // BTreeMap so iteration order is deterministic
            // (lex on encoded key bytes). Per-group count
            // accumulates as u64 — saturating to guard
            // against pathological row counts.
            let mut counts: BTreeMap<Vec<u8>, (GroupKey, u64)> = BTreeMap::new();
            for row in rows {
                let key_bytes = encode_join_key(row, mode);
                let key = match mode {
                    JoinKeyMode::Origin => GroupKey::Origin(row.origin),
                    JoinKeyMode::Seq => GroupKey::Seq(row.seq),
                    JoinKeyMode::OriginSeq => GroupKey::OriginSeq {
                        origin: row.origin,
                        seq: row.seq,
                    },
                };
                let entry = counts.entry(key_bytes).or_insert((key, 0));
                entry.1 = entry.1.saturating_add(1);
            }
            let mut out = Vec::with_capacity(counts.len());
            for (_, (group, count)) in counts {
                let payload = encode_aggregate_payload(Some(group), AggregateValue::Count(count))?;
                out.push(ResultRow {
                    origin: 0,
                    seq: SeqNum(0),
                    payload,
                });
            }
            Ok(out)
        }
    }
}

/// Phase E-3 numeric aggregate (Sum / Avg) over `rows`,
/// extracting the field at `field_path` per row.
///
/// Rows whose field is missing / non-coercible are skipped
/// silently — they neither contribute to the numerator nor the
/// denominator. An ungrouped query over zero rows yields one
/// row carrying `Sum(0.0)` or `Avg(None)` respectively.
fn execute_aggregate_numeric(
    rows: &[ResultRow],
    group_by: Option<JoinKeyMode>,
    field_path: &str,
    kind: NumericAggregateKind,
) -> Result<Vec<ResultRow>, MeshError> {
    use std::collections::BTreeMap;

    // Per-group accumulator: (sum, count_of_numeric_rows).
    let mut acc: BTreeMap<Vec<u8>, (Option<GroupKey>, f64, u64)> = BTreeMap::new();

    for row in rows {
        let Some(value) = super::row::extract_numeric(row, field_path) else {
            continue;
        };
        let (key_bytes, group) = match group_by {
            None => (Vec::new(), None),
            Some(mode) => (
                encode_join_key(row, mode),
                Some(match mode {
                    JoinKeyMode::Origin => GroupKey::Origin(row.origin),
                    JoinKeyMode::Seq => GroupKey::Seq(row.seq),
                    JoinKeyMode::OriginSeq => GroupKey::OriginSeq {
                        origin: row.origin,
                        seq: row.seq,
                    },
                }),
            ),
        };
        let entry = acc.entry(key_bytes).or_insert((group, 0.0, 0));
        entry.1 += value;
        entry.2 = entry.2.saturating_add(1);
    }

    // Ungrouped queries always emit exactly one row, even on
    // empty input. Grouped queries skip empty buckets.
    if group_by.is_none() {
        let (sum, count) = acc
            .get(&Vec::<u8>::new())
            .map(|(_, s, c)| (*s, *c))
            .unwrap_or((0.0, 0));
        let value = match kind {
            NumericAggregateKind::Sum => AggregateValue::Sum(sum),
            NumericAggregateKind::Avg => {
                if count == 0 {
                    AggregateValue::Avg(None)
                } else {
                    AggregateValue::Avg(Some(sum / count as f64))
                }
            }
        };
        let payload = encode_aggregate_payload(None, value)?;
        return Ok(vec![ResultRow {
            origin: 0,
            seq: SeqNum(0),
            payload,
        }]);
    }

    let mut out = Vec::with_capacity(acc.len());
    for (_, (group, sum, count)) in acc {
        let value = match kind {
            NumericAggregateKind::Sum => AggregateValue::Sum(sum),
            NumericAggregateKind::Avg => {
                if count == 0 {
                    AggregateValue::Avg(None)
                } else {
                    AggregateValue::Avg(Some(sum / count as f64))
                }
            }
        };
        let payload = encode_aggregate_payload(group, value)?;
        out.push(ResultRow {
            origin: 0,
            seq: SeqNum(0),
            payload,
        });
    }
    Ok(out)
}

/// Wrap `group` + `value` in an [`AggregateRowPayload`] and
/// postcard-encode it.
fn encode_aggregate_payload(
    group: Option<GroupKey>,
    value: AggregateValue,
) -> Result<Vec<u8>, MeshError> {
    postcard::to_allocvec(&AggregateRowPayload { group, value }).map_err(|e| {
        MeshError::ExecutorError {
            node: 0,
            detail: format!("encode AggregateRowPayload: {e}"),
        }
    })
}

/// Wrap `(left, right)` in a [`JoinedRowPayload`], postcard-
/// encode it, and pack into a sentinel [`ResultRow`].
fn encode_joined_row(
    left: Option<ResultRow>,
    right: Option<ResultRow>,
) -> Result<ResultRow, MeshError> {
    let payload = postcard::to_allocvec(&JoinedRowPayload { left, right }).map_err(|e| {
        MeshError::ExecutorError {
            node: 0,
            detail: format!("encode JoinedRowPayload: {e}"),
        }
    })?;
    Ok(ResultRow {
        origin: 0,
        seq: SeqNum(0),
        payload,
    })
}

/// Phase D-1 hash-join memory bound. Per-query, build-side.
/// Tunable in Phase D-2 once a consumer drives the value.
pub const HASH_JOIN_MEMORY_BYTES: u64 = 256 * 1024 * 1024;

/// Extract the join key from a [`ResultRow`] under the given
/// mode. Returns a `Vec<u8>` so the build map can key on
/// arbitrary-length composites.
fn encode_join_key(row: &ResultRow, mode: JoinKeyMode) -> Vec<u8> {
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
    async fn lineage_emit_yields_one_row_per_entry_in_walk_order() {
        use crate::adapter::net::behavior::meshdb::planner::{
            LineageDirection, LineageEntry,
        };

        // Reader is unused for LineageEmit (walk happens at
        // plan time, executor just translates entries).
        let reader = Arc::new(InMemoryChainReader::default());
        let executor = LocalMeshQueryExecutor::new(reader);
        let plan = atomic_plan(OperatorPlan::LineageEmit {
            origin: 0xAA,
            direction: LineageDirection::Back,
            entries: vec![
                LineageEntry {
                    origin: 0xAA,
                    depth: 0,
                    tip_seq: Some(SeqNum(7)),
                },
                LineageEntry {
                    origin: 0xBB,
                    depth: 1,
                    tip_seq: None,
                },
                LineageEntry {
                    origin: 0xCC,
                    depth: 2,
                    tip_seq: Some(SeqNum(42)),
                },
            ],
        });

        let running = executor.execute(plan).await.unwrap();
        let rows: Vec<ResultRow> = collect_rows(running.rows)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows.len(), 3);
        assert_eq!((rows[0].origin, rows[0].seq), (0xAA, SeqNum(7)));
        // tip_seq=None → seq sentinel 0
        assert_eq!((rows[1].origin, rows[1].seq), (0xBB, SeqNum(0)));
        assert_eq!((rows[2].origin, rows[2].seq), (0xCC, SeqNum(42)));
        // payload empty by Phase C convention.
        assert!(rows.iter().all(|r| r.payload.is_empty()));
    }

    #[tokio::test]
    async fn hash_join_inner_on_origin_matches_pairs() {
        use crate::adapter::net::behavior::meshdb::planner::{
            CostEstimate, JoinKeyMode,
        };
        use crate::adapter::net::behavior::meshdb::query::{JoinKind, JoinedRowPayload};
        use std::time::Duration;

        // Left chain X with 3 events; right chain X has 2.
        // (Same origin → both sides hash to the same key.)
        let chain = 0xABCD;
        let reader = Arc::new(InMemoryChainReader::default());
        reader.append(chain, SeqNum(1), b"L-1".to_vec());
        reader.append(chain, SeqNum(2), b"L-2".to_vec());
        let executor = LocalMeshQueryExecutor::new(reader);

        // Both sides read the same chain; with origin-keyed
        // join, every left-row hashes to the same key, so
        // each right row matches every left row.
        let leaf = |o: u64, s: u64| OperatorNode {
            operator: OperatorPlan::AtRead {
                origin: o,
                seq: SeqNum(s),
            },
            target_nodes: vec![],
            cost: CostEstimate::default(),
        };
        let plan = ExecutionPlan {
            root: OperatorNode {
                operator: OperatorPlan::HashJoin {
                    left: Box::new(leaf(chain, 1)),
                    right: Box::new(leaf(chain, 2)),
                    key_mode: JoinKeyMode::Origin,
                    kind: JoinKind::Inner,
                    watermark: Duration::from_secs(5),
                },
                target_nodes: vec![],
                cost: CostEstimate::default(),
            },
            total_cost: CostEstimate::default(),
        };

        let running = executor.execute(plan).await.unwrap();
        let rows: Vec<ResultRow> = collect_rows(running.rows)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        // 1 left row * 1 right row = 1 pair.
        assert_eq!(rows.len(), 1);
        // Sentinel row markers.
        assert_eq!(rows[0].origin, 0);
        assert_eq!(rows[0].seq, SeqNum(0));
        let decoded: JoinedRowPayload =
            postcard::from_bytes(&rows[0].payload).expect("decode JoinedRowPayload");
        assert_eq!(decoded.left.as_ref().unwrap().payload, b"L-1");
        assert_eq!(decoded.right.as_ref().unwrap().payload, b"L-2");
    }

    #[tokio::test]
    async fn hash_join_seq_key_only_matches_equal_seqs() {
        use crate::adapter::net::behavior::meshdb::planner::{
            CostEstimate, JoinKeyMode,
        };
        use crate::adapter::net::behavior::meshdb::query::{JoinKind, JoinedRowPayload};
        use std::time::Duration;

        // Two different chains; join on seq. Only rows that
        // share seq numbers across the chains match.
        let a = 0x111;
        let b = 0x222;
        let reader = Arc::new(InMemoryChainReader::default());
        // Chain A: seqs 1, 2, 3
        reader.append(a, SeqNum(1), b"a-1".to_vec());
        reader.append(a, SeqNum(2), b"a-2".to_vec());
        reader.append(a, SeqNum(3), b"a-3".to_vec());
        // Chain B: seqs 2, 4
        reader.append(b, SeqNum(2), b"b-2".to_vec());
        reader.append(b, SeqNum(4), b"b-4".to_vec());

        let executor = LocalMeshQueryExecutor::new(reader);
        let plan = ExecutionPlan {
            root: OperatorNode {
                operator: OperatorPlan::HashJoin {
                    left: Box::new(OperatorNode {
                        operator: OperatorPlan::BetweenRead {
                            origin: a,
                            start: SeqNum(1),
                            end: SeqNum(10),
                        },
                        target_nodes: vec![],
                        cost: CostEstimate::default(),
                    }),
                    right: Box::new(OperatorNode {
                        operator: OperatorPlan::BetweenRead {
                            origin: b,
                            start: SeqNum(1),
                            end: SeqNum(10),
                        },
                        target_nodes: vec![],
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
        let running = executor.execute(plan).await.unwrap();
        let rows: Vec<ResultRow> = collect_rows(running.rows)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        // Only seq=2 is in both chains → one matched pair.
        assert_eq!(rows.len(), 1);
        let decoded: JoinedRowPayload =
            postcard::from_bytes(&rows[0].payload).unwrap();
        assert_eq!(decoded.left.unwrap().payload, b"a-2");
        assert_eq!(decoded.right.unwrap().payload, b"b-2");
    }

    #[tokio::test]
    async fn hash_join_left_outer_emits_unmatched_lefts() {
        use crate::adapter::net::behavior::meshdb::planner::{CostEstimate, JoinKeyMode};
        use crate::adapter::net::behavior::meshdb::query::{JoinKind, JoinedRowPayload};
        use std::time::Duration;

        // Left chain a: seqs 1,2,3. Right chain b: seqs 2.
        // LeftOuter on seq → seq=2 matches, seqs 1 & 3 emit
        // with right=None.
        let a = 0x100;
        let b = 0x200;
        let reader = Arc::new(InMemoryChainReader::default());
        reader.append(a, SeqNum(1), b"a-1".to_vec());
        reader.append(a, SeqNum(2), b"a-2".to_vec());
        reader.append(a, SeqNum(3), b"a-3".to_vec());
        reader.append(b, SeqNum(2), b"b-2".to_vec());

        let executor = LocalMeshQueryExecutor::new(reader);
        let plan = ExecutionPlan {
            root: OperatorNode {
                operator: OperatorPlan::HashJoin {
                    left: Box::new(OperatorNode {
                        operator: OperatorPlan::BetweenRead {
                            origin: a,
                            start: SeqNum(1),
                            end: SeqNum(10),
                        },
                        target_nodes: vec![],
                        cost: CostEstimate::default(),
                    }),
                    right: Box::new(OperatorNode {
                        operator: OperatorPlan::BetweenRead {
                            origin: b,
                            start: SeqNum(1),
                            end: SeqNum(10),
                        },
                        target_nodes: vec![],
                        cost: CostEstimate::default(),
                    }),
                    key_mode: JoinKeyMode::Seq,
                    kind: JoinKind::LeftOuter,
                    watermark: Duration::from_secs(5),
                },
                target_nodes: vec![],
                cost: CostEstimate::default(),
            },
            total_cost: CostEstimate::default(),
        };

        let running = executor.execute(plan).await.unwrap();
        let rows: Vec<ResultRow> = collect_rows(running.rows)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        // 3 left rows → 3 output rows (one matched, two with right=None).
        assert_eq!(rows.len(), 3);
        let mut decoded: Vec<JoinedRowPayload> = rows
            .iter()
            .map(|r| postcard::from_bytes(&r.payload).unwrap())
            .collect();
        decoded.sort_by_key(|j| j.left.as_ref().unwrap().seq);
        // seq=1 left, right=None
        assert_eq!(decoded[0].left.as_ref().unwrap().seq, SeqNum(1));
        assert!(decoded[0].right.is_none());
        // seq=2 left, right=b-2
        assert_eq!(decoded[1].left.as_ref().unwrap().seq, SeqNum(2));
        assert_eq!(decoded[1].right.as_ref().unwrap().payload, b"b-2");
        // seq=3 left, right=None
        assert_eq!(decoded[2].left.as_ref().unwrap().seq, SeqNum(3));
        assert!(decoded[2].right.is_none());
    }

    #[tokio::test]
    async fn hash_join_right_outer_emits_unmatched_rights() {
        use crate::adapter::net::behavior::meshdb::planner::{CostEstimate, JoinKeyMode};
        use crate::adapter::net::behavior::meshdb::query::{JoinKind, JoinedRowPayload};
        use std::time::Duration;

        // Left chain a: seqs 1,2. Right chain b: seqs 2,3,4.
        // RightOuter on seq → seq=2 matches, seqs 3 & 4 emit
        // with left=None.
        let a = 0x100;
        let b = 0x200;
        let reader = Arc::new(InMemoryChainReader::default());
        reader.append(a, SeqNum(1), b"a-1".to_vec());
        reader.append(a, SeqNum(2), b"a-2".to_vec());
        reader.append(b, SeqNum(2), b"b-2".to_vec());
        reader.append(b, SeqNum(3), b"b-3".to_vec());
        reader.append(b, SeqNum(4), b"b-4".to_vec());

        let executor = LocalMeshQueryExecutor::new(reader);
        let plan = ExecutionPlan {
            root: OperatorNode {
                operator: OperatorPlan::HashJoin {
                    left: Box::new(OperatorNode {
                        operator: OperatorPlan::BetweenRead {
                            origin: a,
                            start: SeqNum(1),
                            end: SeqNum(10),
                        },
                        target_nodes: vec![],
                        cost: CostEstimate::default(),
                    }),
                    right: Box::new(OperatorNode {
                        operator: OperatorPlan::BetweenRead {
                            origin: b,
                            start: SeqNum(1),
                            end: SeqNum(10),
                        },
                        target_nodes: vec![],
                        cost: CostEstimate::default(),
                    }),
                    key_mode: JoinKeyMode::Seq,
                    kind: JoinKind::RightOuter,
                    watermark: Duration::from_secs(5),
                },
                target_nodes: vec![],
                cost: CostEstimate::default(),
            },
            total_cost: CostEstimate::default(),
        };

        let running = executor.execute(plan).await.unwrap();
        let rows: Vec<ResultRow> = collect_rows(running.rows)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows.len(), 3);
        let mut decoded: Vec<JoinedRowPayload> = rows
            .iter()
            .map(|r| postcard::from_bytes(&r.payload).unwrap())
            .collect();
        decoded.sort_by_key(|j| j.right.as_ref().unwrap().seq);
        // seq=2 right, left=a-2
        assert_eq!(decoded[0].right.as_ref().unwrap().seq, SeqNum(2));
        assert_eq!(decoded[0].left.as_ref().unwrap().payload, b"a-2");
        // seq=3 right, left=None
        assert_eq!(decoded[1].right.as_ref().unwrap().seq, SeqNum(3));
        assert!(decoded[1].left.is_none());
        // seq=4 right, left=None
        assert_eq!(decoded[2].right.as_ref().unwrap().seq, SeqNum(4));
        assert!(decoded[2].left.is_none());
    }

    #[tokio::test]
    async fn hash_join_full_outer_emits_unmatched_on_both_sides() {
        use crate::adapter::net::behavior::meshdb::planner::{CostEstimate, JoinKeyMode};
        use crate::adapter::net::behavior::meshdb::query::{JoinKind, JoinedRowPayload};
        use std::time::Duration;

        // Left a: 1,2. Right b: 2,3. FullOuter on seq:
        //   seq=1 → (a-1, None)
        //   seq=2 → (a-2, b-2)
        //   seq=3 → (None, b-3)
        let a = 0x100;
        let b = 0x200;
        let reader = Arc::new(InMemoryChainReader::default());
        reader.append(a, SeqNum(1), b"a-1".to_vec());
        reader.append(a, SeqNum(2), b"a-2".to_vec());
        reader.append(b, SeqNum(2), b"b-2".to_vec());
        reader.append(b, SeqNum(3), b"b-3".to_vec());

        let executor = LocalMeshQueryExecutor::new(reader);
        let plan = ExecutionPlan {
            root: OperatorNode {
                operator: OperatorPlan::HashJoin {
                    left: Box::new(OperatorNode {
                        operator: OperatorPlan::BetweenRead {
                            origin: a,
                            start: SeqNum(1),
                            end: SeqNum(10),
                        },
                        target_nodes: vec![],
                        cost: CostEstimate::default(),
                    }),
                    right: Box::new(OperatorNode {
                        operator: OperatorPlan::BetweenRead {
                            origin: b,
                            start: SeqNum(1),
                            end: SeqNum(10),
                        },
                        target_nodes: vec![],
                        cost: CostEstimate::default(),
                    }),
                    key_mode: JoinKeyMode::Seq,
                    kind: JoinKind::FullOuter,
                    watermark: Duration::from_secs(5),
                },
                target_nodes: vec![],
                cost: CostEstimate::default(),
            },
            total_cost: CostEstimate::default(),
        };

        let running = executor.execute(plan).await.unwrap();
        let rows: Vec<ResultRow> = collect_rows(running.rows)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows.len(), 3);
        let decoded: Vec<JoinedRowPayload> = rows
            .iter()
            .map(|r| postcard::from_bytes(&r.payload).unwrap())
            .collect();
        // Three buckets: (left-only), (matched), (right-only).
        let left_only = decoded.iter().filter(|j| j.left.is_some() && j.right.is_none()).count();
        let right_only = decoded.iter().filter(|j| j.left.is_none() && j.right.is_some()).count();
        let matched = decoded.iter().filter(|j| j.left.is_some() && j.right.is_some()).count();
        assert_eq!(left_only, 1, "decoded = {decoded:?}");
        assert_eq!(right_only, 1, "decoded = {decoded:?}");
        assert_eq!(matched, 1, "decoded = {decoded:?}");
        // None-None is illegal — defensive check.
        assert_eq!(
            decoded
                .iter()
                .filter(|j| j.left.is_none() && j.right.is_none())
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn aggregate_count_no_group_by_returns_single_row_with_total() {
        use crate::adapter::net::behavior::meshdb::planner::CostEstimate;
        use crate::adapter::net::behavior::meshdb::query::{
            AggregateRowPayload, AggregateValue,
        };

        let chain = 0xABCD;
        let reader = Arc::new(InMemoryChainReader::default());
        for s in 1..=5u64 {
            reader.append(chain, SeqNum(s), format!("p-{s}").into_bytes());
        }
        let executor = LocalMeshQueryExecutor::new(reader);
        let plan = ExecutionPlan {
            root: OperatorNode {
                operator: OperatorPlan::AggregateCount {
                    input: Box::new(OperatorNode {
                        operator: OperatorPlan::BetweenRead {
                            origin: chain,
                            start: SeqNum(1),
                            end: SeqNum(10),
                        },
                        target_nodes: vec![],
                        cost: CostEstimate::default(),
                    }),
                    group_by: None,
                },
                target_nodes: vec![],
                cost: CostEstimate::default(),
            },
            total_cost: CostEstimate::default(),
        };

        let running = executor.execute(plan).await.unwrap();
        let rows: Vec<ResultRow> = collect_rows(running.rows)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows.len(), 1);
        let decoded: AggregateRowPayload =
            postcard::from_bytes(&rows[0].payload).unwrap();
        assert_eq!(decoded.group, None);
        assert_eq!(decoded.value, AggregateValue::Count(5));
    }

    #[tokio::test]
    async fn aggregate_count_group_by_origin_returns_per_chain_count() {
        use crate::adapter::net::behavior::meshdb::planner::{CostEstimate, JoinKeyMode};
        use crate::adapter::net::behavior::meshdb::query::{
            AggregateRowPayload, AggregateValue, GroupKey,
        };
        use std::collections::HashMap;

        // The local executor's HashJoin reuses both chains as
        // input rows; we drive a similar cross-chain pattern
        // via HashJoin output rows... actually simpler: emit a
        // sequence of rows from two BetweenReads stitched via a
        // joinless leaf source. For Phase E-1 we only need
        // multiple rows from one chain; group_by Origin then
        // yields one bucket. To exercise two buckets, use a
        // HashJoin with origin-mode then group by origin =
        // overkill. Instead, use two AtReads via a contrived
        // composite — not possible without a Union operator.
        //
        // Workaround: feed rows from a HashJoin Inner whose
        // sentinel output rows all share origin=0; group_by
        // Origin then puts everything in one bucket. Useful as
        // a sanity check.
        //
        // Better: directly call execute_aggregate_count over a
        // hand-crafted row Vec — that exercises the grouping
        // logic without composing operators we don't yet have.
        // Phase E-2's Union operator (or a richer test
        // harness) is the right place to test multi-bucket
        // origin-grouped aggregates over executor-emitted rows.
        let rows = vec![
            ResultRow {
                origin: 0xAA,
                seq: SeqNum(1),
                payload: vec![],
            },
            ResultRow {
                origin: 0xAA,
                seq: SeqNum(2),
                payload: vec![],
            },
            ResultRow {
                origin: 0xBB,
                seq: SeqNum(1),
                payload: vec![],
            },
            ResultRow {
                origin: 0xCC,
                seq: SeqNum(1),
                payload: vec![],
            },
            ResultRow {
                origin: 0xCC,
                seq: SeqNum(2),
                payload: vec![],
            },
            ResultRow {
                origin: 0xCC,
                seq: SeqNum(3),
                payload: vec![],
            },
        ];
        let out = super::execute_aggregate_count(&rows, Some(JoinKeyMode::Origin)).unwrap();
        assert_eq!(out.len(), 3);
        let mut by_origin: HashMap<u64, u64> = HashMap::new();
        for row in &out {
            let decoded: AggregateRowPayload =
                postcard::from_bytes(&row.payload).unwrap();
            if let Some(GroupKey::Origin(o)) = decoded.group {
                if let AggregateValue::Count(c) = decoded.value {
                    by_origin.insert(o, c);
                }
            }
        }
        assert_eq!(by_origin.get(&0xAA), Some(&2));
        assert_eq!(by_origin.get(&0xBB), Some(&1));
        assert_eq!(by_origin.get(&0xCC), Some(&3));

        let _ = CostEstimate::default(); // silence unused-import lint
    }

    #[tokio::test]
    async fn aggregate_count_group_by_seq_buckets_by_seq() {
        use crate::adapter::net::behavior::meshdb::planner::JoinKeyMode;
        use crate::adapter::net::behavior::meshdb::query::{
            AggregateRowPayload, AggregateValue, GroupKey,
        };
        use std::collections::HashMap;

        let rows = vec![
            ResultRow {
                origin: 0xAA,
                seq: SeqNum(1),
                payload: vec![],
            },
            ResultRow {
                origin: 0xBB,
                seq: SeqNum(1),
                payload: vec![],
            },
            ResultRow {
                origin: 0xCC,
                seq: SeqNum(7),
                payload: vec![],
            },
        ];
        let out = super::execute_aggregate_count(&rows, Some(JoinKeyMode::Seq)).unwrap();
        assert_eq!(out.len(), 2);
        let mut by_seq: HashMap<u64, u64> = HashMap::new();
        for row in &out {
            let decoded: AggregateRowPayload =
                postcard::from_bytes(&row.payload).unwrap();
            if let Some(GroupKey::Seq(SeqNum(s))) = decoded.group {
                if let AggregateValue::Count(c) = decoded.value {
                    by_seq.insert(s, c);
                }
            }
        }
        assert_eq!(by_seq.get(&1), Some(&2));
        assert_eq!(by_seq.get(&7), Some(&1));
    }

    #[tokio::test]
    async fn aggregate_count_empty_input_returns_zero() {
        use crate::adapter::net::behavior::meshdb::planner::CostEstimate;
        use crate::adapter::net::behavior::meshdb::query::{
            AggregateRowPayload, AggregateValue,
        };

        let reader = Arc::new(InMemoryChainReader::default());
        let executor = LocalMeshQueryExecutor::new(reader);
        let plan = ExecutionPlan {
            root: OperatorNode {
                operator: OperatorPlan::AggregateCount {
                    input: Box::new(OperatorNode {
                        operator: OperatorPlan::LatestRead { origin: 0xDEAD },
                        target_nodes: vec![],
                        cost: CostEstimate::default(),
                    }),
                    group_by: None,
                },
                target_nodes: vec![],
                cost: CostEstimate::default(),
            },
            total_cost: CostEstimate::default(),
        };
        let running = executor.execute(plan).await.unwrap();
        let rows: Vec<ResultRow> = collect_rows(running.rows)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        // Ungrouped aggregate always emits one row, even on
        // empty input (Count = 0).
        assert_eq!(rows.len(), 1);
        let decoded: AggregateRowPayload =
            postcard::from_bytes(&rows[0].payload).unwrap();
        assert_eq!(decoded.value, AggregateValue::Count(0));
    }

    #[tokio::test]
    async fn aggregate_sum_on_seq_returns_total() {
        use crate::adapter::net::behavior::meshdb::planner::CostEstimate;
        use crate::adapter::net::behavior::meshdb::query::{
            AggregateRowPayload, AggregateValue, NumericAggregateKind,
        };

        let chain = 0xAA;
        let reader = Arc::new(InMemoryChainReader::default());
        for s in [1u64, 3, 7, 11] {
            reader.append(chain, SeqNum(s), vec![]);
        }
        let executor = LocalMeshQueryExecutor::new(reader);
        let plan = ExecutionPlan {
            root: OperatorNode {
                operator: OperatorPlan::AggregateNumeric {
                    input: Box::new(OperatorNode {
                        operator: OperatorPlan::BetweenRead {
                            origin: chain,
                            start: SeqNum(1),
                            end: SeqNum(20),
                        },
                        target_nodes: vec![],
                        cost: CostEstimate::default(),
                    }),
                    group_by: None,
                    field_path: "seq".to_string(),
                    kind: NumericAggregateKind::Sum,
                },
                target_nodes: vec![],
                cost: CostEstimate::default(),
            },
            total_cost: CostEstimate::default(),
        };

        let rows: Vec<ResultRow> = collect_rows(executor.execute(plan).await.unwrap().rows)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows.len(), 1);
        let decoded: AggregateRowPayload =
            postcard::from_bytes(&rows[0].payload).unwrap();
        assert_eq!(decoded.value, AggregateValue::Sum(22.0));
    }

    #[tokio::test]
    async fn aggregate_avg_on_json_field_returns_mean() {
        use crate::adapter::net::behavior::meshdb::planner::CostEstimate;
        use crate::adapter::net::behavior::meshdb::query::{
            AggregateRowPayload, AggregateValue, NumericAggregateKind,
        };

        let chain = 0xBB;
        let reader = Arc::new(InMemoryChainReader::default());
        reader.append(chain, SeqNum(1), br#"{"latency_ms": 10}"#.to_vec());
        reader.append(chain, SeqNum(2), br#"{"latency_ms": 30}"#.to_vec());
        reader.append(chain, SeqNum(3), br#"{"latency_ms": 50}"#.to_vec());
        reader.append(chain, SeqNum(4), b"not-json".to_vec()); // skipped
        let executor = LocalMeshQueryExecutor::new(reader);

        let plan = ExecutionPlan {
            root: OperatorNode {
                operator: OperatorPlan::AggregateNumeric {
                    input: Box::new(OperatorNode {
                        operator: OperatorPlan::BetweenRead {
                            origin: chain,
                            start: SeqNum(1),
                            end: SeqNum(10),
                        },
                        target_nodes: vec![],
                        cost: CostEstimate::default(),
                    }),
                    group_by: None,
                    field_path: "latency_ms".to_string(),
                    kind: NumericAggregateKind::Avg,
                },
                target_nodes: vec![],
                cost: CostEstimate::default(),
            },
            total_cost: CostEstimate::default(),
        };
        let rows: Vec<ResultRow> = collect_rows(executor.execute(plan).await.unwrap().rows)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows.len(), 1);
        let decoded: AggregateRowPayload =
            postcard::from_bytes(&rows[0].payload).unwrap();
        assert_eq!(decoded.value, AggregateValue::Avg(Some(30.0)));
    }

    #[tokio::test]
    async fn aggregate_avg_empty_input_returns_avg_none() {
        use crate::adapter::net::behavior::meshdb::planner::CostEstimate;
        use crate::adapter::net::behavior::meshdb::query::{
            AggregateRowPayload, AggregateValue, NumericAggregateKind,
        };

        let reader = Arc::new(InMemoryChainReader::default());
        let executor = LocalMeshQueryExecutor::new(reader);
        let plan = ExecutionPlan {
            root: OperatorNode {
                operator: OperatorPlan::AggregateNumeric {
                    input: Box::new(OperatorNode {
                        operator: OperatorPlan::LatestRead { origin: 0xDEAD },
                        target_nodes: vec![],
                        cost: CostEstimate::default(),
                    }),
                    group_by: None,
                    field_path: "seq".to_string(),
                    kind: NumericAggregateKind::Avg,
                },
                target_nodes: vec![],
                cost: CostEstimate::default(),
            },
            total_cost: CostEstimate::default(),
        };
        let rows: Vec<ResultRow> = collect_rows(executor.execute(plan).await.unwrap().rows)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows.len(), 1);
        let decoded: AggregateRowPayload =
            postcard::from_bytes(&rows[0].payload).unwrap();
        assert_eq!(decoded.value, AggregateValue::Avg(None));
    }

    #[tokio::test]
    async fn filter_keeps_rows_whose_synthetic_seq_matches() {
        use crate::adapter::net::behavior::predicate::Predicate;
        use crate::adapter::net::behavior::tag::{TagKey, TaxonomyAxis};

        let chain = 0xCAFE;
        let reader = Arc::new(InMemoryChainReader::default());
        reader.append(chain, SeqNum(1), b"p-1".to_vec());
        reader.append(chain, SeqNum(2), b"p-2".to_vec());
        reader.append(chain, SeqNum(3), b"p-3".to_vec());
        let executor = LocalMeshQueryExecutor::new(reader);

        // Predicate: seq == "2" (string match via synthetic tag).
        let predicate = Predicate::Equals {
            key: TagKey {
                axis: TaxonomyAxis::Dataforts,
                key: "seq".to_string(),
            },
            value: "2".to_string(),
        }
        .to_wire();
        let plan = atomic_plan(OperatorPlan::Filter {
            input: Box::new(OperatorNode {
                operator: OperatorPlan::BetweenRead {
                    origin: chain,
                    start: SeqNum(1),
                    end: SeqNum(10),
                },
                target_nodes: vec![],
                cost: CostEstimate::default(),
            }),
            predicate,
        });

        let running = executor.execute(plan).await.unwrap();
        let rows: Vec<ResultRow> = collect_rows(running.rows)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].seq, SeqNum(2));
        assert_eq!(rows[0].payload, b"p-2");
    }

    #[tokio::test]
    async fn filter_numeric_at_least_on_seq_keeps_upper_rows() {
        use crate::adapter::net::behavior::predicate::Predicate;
        use crate::adapter::net::behavior::tag::{TagKey, TaxonomyAxis};

        let chain = 0xCAFE;
        let reader = Arc::new(InMemoryChainReader::default());
        for s in 1..=5u64 {
            reader.append(chain, SeqNum(s), format!("p-{s}").into_bytes());
        }
        let executor = LocalMeshQueryExecutor::new(reader);

        let predicate = Predicate::NumericAtLeast {
            key: TagKey {
                axis: TaxonomyAxis::Dataforts,
                key: "seq".to_string(),
            },
            threshold: 3.0,
        }
        .to_wire();
        let plan = atomic_plan(OperatorPlan::Filter {
            input: Box::new(OperatorNode {
                operator: OperatorPlan::BetweenRead {
                    origin: chain,
                    start: SeqNum(1),
                    end: SeqNum(10),
                },
                target_nodes: vec![],
                cost: CostEstimate::default(),
            }),
            predicate,
        });

        let rows: Vec<ResultRow> = collect_rows(executor.execute(plan).await.unwrap().rows)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows.len(), 3);
        let seqs: Vec<u64> = rows.iter().map(|r| r.seq.0).collect();
        assert_eq!(seqs, vec![3, 4, 5]);
    }

    #[tokio::test]
    async fn filter_on_flat_json_payload_field_keeps_matching_rows() {
        use crate::adapter::net::behavior::predicate::Predicate;
        use crate::adapter::net::behavior::tag::{TagKey, TaxonomyAxis};

        let chain = 0xC0DE;
        let reader = Arc::new(InMemoryChainReader::default());
        // Rows carry JSON payloads with a "severity" field.
        reader.append(
            chain,
            SeqNum(1),
            br#"{"severity":"low"}"#.to_vec(),
        );
        reader.append(
            chain,
            SeqNum(2),
            br#"{"severity":"high"}"#.to_vec(),
        );
        reader.append(
            chain,
            SeqNum(3),
            br#"{"severity":"high","other":"x"}"#.to_vec(),
        );
        reader.append(chain, SeqNum(4), b"not-json".to_vec());
        let executor = LocalMeshQueryExecutor::new(reader);

        let predicate = Predicate::Equals {
            key: TagKey {
                axis: TaxonomyAxis::Dataforts,
                key: "severity".to_string(),
            },
            value: "high".to_string(),
        }
        .to_wire();
        let plan = atomic_plan(OperatorPlan::Filter {
            input: Box::new(OperatorNode {
                operator: OperatorPlan::BetweenRead {
                    origin: chain,
                    start: SeqNum(1),
                    end: SeqNum(10),
                },
                target_nodes: vec![],
                cost: CostEstimate::default(),
            }),
            predicate,
        });
        let rows: Vec<ResultRow> = collect_rows(executor.execute(plan).await.unwrap().rows)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        // Rows 2 + 3 match; row 1 has severity=low (no), row 4
        // is non-JSON (predicate fails silently).
        let seqs: Vec<u64> = rows.iter().map(|r| r.seq.0).collect();
        assert_eq!(seqs, vec![2, 3]);
    }
}
