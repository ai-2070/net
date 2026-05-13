//! Node bindings for MeshDB — federated query layer.
//!
//! # Slice 1 scope
//!
//! Mirrors the Python SDK's slice 1 with Node-native async:
//!
//! - [`MeshQuery`] — 1:1-with-AST factory surface. Slice 1
//!   exposes the three atomic operators (`at` / `between` /
//!   `latest`); composite variants land in follow-up slices.
//! - [`InMemoryChainReader`] — `append(originHash, seq, payload)`
//!   populator that implements the substrate's `ChainReader`
//!   trait. Phase B+ adds a Redex-backed adapter.
//! - [`MeshQueryRunner`] — owns a `LocalMeshQueryExecutor`. Its
//!   `execute(query, options?)` is `async`, returning a
//!   [`MeshQueryStream`] handle.
//! - [`MeshQueryStream`] — `async next() -> ResultRow | null`.
//!   The JS wrapper layered on top (slice 1 + TS shim) makes
//!   this `AsyncIterable<ResultRow>` for `for await` ergonomics.
//! - [`ResultRow`] — `{ originHash: BigInt, seq: BigInt,
//!   payload: Buffer }`.
//! - [`CachePolicy`] — static factory class (`permanent()` /
//!   `timeBound(seconds)`).
//! - [`ExecuteOptions`] — `{ bypassCache?: boolean, cachePolicy?:
//!   CachePolicy }`.
//! - [`MeshDbError`] — error type with stable `kind` discriminator.
//!
//! # Async story
//!
//! Locked decision: Node = `Promise<AsyncIterable<Row>>`. The
//! Rust side exposes `MeshQueryStream.next()` (async, returns
//! `Option<ResultRow>`); the slice-1 TS shim adds the
//! `Symbol.asyncIterator` so `for await (const row of stream)`
//! works. Internal impl drains the executor's row stream into
//! a `tokio::sync::Mutex<Vec<ResultRow>>` at `execute()` time
//! and pops in `next()` — true streaming (mpsc-backed) is a
//! follow-up if profiling justifies it.

use std::sync::Arc;

use napi::bindgen_prelude::*;
use napi_derive::napi;
use tokio::sync::Mutex as AsyncMutex;

use net::adapter::net::behavior::meshdb::{
    cache::{CachePolicy as InnerCachePolicy, LruResultCache},
    executor::{
        ChainReader as InnerChainReader, ExecuteOptions as InnerExecuteOptions,
        LocalMeshQueryExecutor, MeshQueryExecutor,
    },
    planner::{
        CostEstimate, JoinKeyMode, JoinStrategy, OperatorNode, OperatorPlan,
    },
    query::{
        AggregateRowPayload as InnerAggregateRowPayload,
        AggregateValue as InnerAggregateValue, GroupKey as InnerGroupKey,
        JoinKind as InnerJoinKind, JoinedRowPayload as InnerJoinedRowPayload,
        NumericAggregateKind, NumericReductionKind, ResultRow as InnerResultRow,
        WindowBoundary as InnerWindowBoundary, WindowSpec,
    },
    ExecutionPlan, SeqNum,
};

use crate::common::bigint_u64;

/// One row from a query result.
///
/// `originHash` is the 16-hex chain identifier; `seq` is the
/// per-chain monotonic sequence; `payload` is opaque bytes
/// (event body for plain reads, or a postcard-encoded envelope
/// for aggregate / join / window sentinel rows — see the
/// `decodeAggregate` / `decodeJoined` / `decodeWindow`
/// module-level helpers).
///
/// Plain object (not a class) so it can be nested inside
/// `WindowBoundary.rows` / `JoinedRow.left` etc. without the
/// `Vec<ResultRow>` marshalling restriction that
/// `#[napi]` classes carry.
#[napi(object)]
pub struct ResultRow {
    #[napi(js_name = "originHash")]
    pub origin_hash: BigInt,
    pub seq: BigInt,
    pub payload: Buffer,
}

/// Try to decode `row.payload` as an aggregate payload.
/// Returns `null` for rows that aren't aggregate sentinels.
#[napi(js_name = "decodeAggregate")]
pub fn decode_aggregate(row: ResultRow) -> Option<AggregateResult> {
    let p: InnerAggregateRowPayload = postcard::from_bytes(&row.payload).ok()?;
    Some(aggregate_result_from(p))
}

/// Try to decode `row.payload` as a joined-row payload.
/// Returns `null` when the bytes don't deserialize as a
/// JoinedRow.
#[napi(js_name = "decodeJoined")]
pub fn decode_joined(row: ResultRow) -> Option<JoinedRow> {
    let p: InnerJoinedRowPayload = postcard::from_bytes(&row.payload).ok()?;
    Some(joined_row_from(p))
}

/// Try to decode `row.payload` as a window bucket. Returns
/// `null` when the bytes don't deserialize as a
/// WindowBoundary.
#[napi(js_name = "decodeWindow")]
pub fn decode_window(row: ResultRow) -> Option<WindowBoundary> {
    let b: InnerWindowBoundary = postcard::from_bytes(&row.payload).ok()?;
    Some(window_boundary_from(b))
}

/// Decoded aggregate-row payload (slice 2 decoder).
///
/// `kind` is one of `"count"` / `"sum"` / `"avg"` / `"min"` /
/// `"max"` / `"distinct_count"` / `"percentile"`. `value` is the
/// numeric output (always set for count / distinct_count;
/// `null` for the others when the group held no numeric rows).
/// `count` mirrors `value` as a BigInt for the count-flavored
/// kinds so callers don't have to coerce floats.
#[napi(object)]
pub struct AggregateResult {
    pub group: Option<GroupKey>,
    pub kind: String,
    pub value: Option<f64>,
    pub count: Option<BigInt>,
}

/// Group-key identifier carried inside an `AggregateResult`.
/// `kind` is `"origin"` / `"seq"` / `"origin_seq"`; the
/// populated field(s) match the kind.
#[napi(object)]
pub struct GroupKey {
    pub kind: String,
    #[napi(js_name = "originHash")]
    pub origin_hash: Option<BigInt>,
    pub seq: Option<BigInt>,
}

/// Decoded join-row payload (slice 2 decoder). Either side is
/// `null` for outer-join unmatched rows; inner-join rows have
/// both populated.
#[napi(object)]
pub struct JoinedRow {
    pub left: Option<ResultRow>,
    pub right: Option<ResultRow>,
}

/// Decoded window-bucket payload (slice 2 decoder). `start`
/// and `end` are the bucket's seq bounds (half-open); `rows`
/// is the list of rows that fell in the bucket, in seq-asc
/// order.
#[napi(object)]
pub struct WindowBoundary {
    pub start: BigInt,
    pub end: BigInt,
    pub rows: Vec<ResultRow>,
}

fn aggregate_result_from(p: InnerAggregateRowPayload) -> AggregateResult {
    let group = p.group.map(group_key_from);
    let (kind, value, count) = match p.value {
        InnerAggregateValue::Count(c) => {
            ("count".to_string(), Some(c as f64), Some(BigInt::from(c)))
        }
        InnerAggregateValue::Sum(s) => ("sum".to_string(), Some(s), None),
        InnerAggregateValue::Avg(opt) => ("avg".to_string(), opt, None),
        InnerAggregateValue::Min(opt) => ("min".to_string(), opt, None),
        InnerAggregateValue::Max(opt) => ("max".to_string(), opt, None),
        InnerAggregateValue::DistinctCount(c) => (
            "distinct_count".to_string(),
            Some(c as f64),
            Some(BigInt::from(c)),
        ),
        InnerAggregateValue::Percentile(opt) => ("percentile".to_string(), opt, None),
        // AggregateValue is #[non_exhaustive] — future variants
        // surface as `"unknown"` so wire round-trip works.
        _ => ("unknown".to_string(), None, None),
    };
    AggregateResult {
        group,
        kind,
        value,
        count,
    }
}

fn group_key_from(g: InnerGroupKey) -> GroupKey {
    match g {
        InnerGroupKey::Origin(o) => GroupKey {
            kind: "origin".to_string(),
            origin_hash: Some(BigInt::from(o)),
            seq: None,
        },
        InnerGroupKey::Seq(s) => GroupKey {
            kind: "seq".to_string(),
            origin_hash: None,
            seq: Some(BigInt::from(s.0)),
        },
        InnerGroupKey::OriginSeq { origin, seq } => GroupKey {
            kind: "origin_seq".to_string(),
            origin_hash: Some(BigInt::from(origin)),
            seq: Some(BigInt::from(seq.0)),
        },
    }
}

fn joined_row_from(p: InnerJoinedRowPayload) -> JoinedRow {
    JoinedRow {
        left: p.left.map(ResultRow::from),
        right: p.right.map(ResultRow::from),
    }
}

fn window_boundary_from(b: InnerWindowBoundary) -> WindowBoundary {
    WindowBoundary {
        start: BigInt::from(b.start.0),
        end: BigInt::from(b.end.0),
        rows: b.rows.into_iter().map(ResultRow::from).collect(),
    }
}

impl From<InnerResultRow> for ResultRow {
    fn from(r: InnerResultRow) -> Self {
        Self {
            origin_hash: BigInt::from(r.origin),
            seq: BigInt::from(r.seq.0),
            payload: Buffer::from(r.payload),
        }
    }
}

/// Cache policy as a tagged plain-object shape. `kind` is one
/// of `"permanent"` (cache until LRU eviction; use only when
/// the query's result is immutable under substrate semantics)
/// or `"time_bound"` (TTL expiry, `ttlSeconds` defaults to
/// 5 s per the locked Phase F join-watermark mirror).
///
/// Construct via the [`cachePolicyPermanent`] /
/// [`cachePolicyTimeBound`] module-level factories for
/// type-safe defaults, or build the object literal directly
/// if you're sure about the shape.
#[napi(object)]
pub struct CachePolicy {
    /// `"permanent"` or `"time_bound"`. Unknown kinds map to
    /// the default `TimeBound(5s)`.
    pub kind: String,
    /// TTL in seconds (only meaningful when `kind ==
    /// "time_bound"`). Omitted / non-finite → 5 s.
    #[napi(js_name = "ttlSeconds")]
    pub ttl_seconds: Option<f64>,
}

/// Build a `"permanent"` cache policy object. Equivalent to
/// `{ kind: "permanent" }`.
#[napi(js_name = "cachePolicyPermanent")]
pub fn cache_policy_permanent() -> CachePolicy {
    CachePolicy {
        kind: "permanent".to_string(),
        ttl_seconds: None,
    }
}

/// Build a `"time_bound"` cache policy object. `seconds`
/// defaults to 5 s when omitted.
#[napi(js_name = "cachePolicyTimeBound")]
pub fn cache_policy_time_bound(seconds: Option<f64>) -> CachePolicy {
    CachePolicy {
        kind: "time_bound".to_string(),
        ttl_seconds: seconds,
    }
}

fn cache_policy_to_inner(p: Option<CachePolicy>) -> InnerCachePolicy {
    match p {
        None => InnerCachePolicy::default(),
        Some(p) => match p.kind.as_str() {
            "permanent" => InnerCachePolicy::Permanent,
            // Default + "time_bound" + any unknown kind →
            // TimeBound, with the ttl_seconds field if
            // present, else 5 s.
            _ => {
                let secs = p.ttl_seconds.unwrap_or(5.0);
                let secs = if secs.is_finite() && secs >= 0.0 {
                    secs
                } else {
                    5.0
                };
                InnerCachePolicy::TimeBound {
                    ttl: std::time::Duration::from_secs_f64(secs),
                }
            }
        },
    }
}

/// Per-execute options. `bypassCache` skips both lookup AND
/// writeback (Phase F decision); `cachePolicy` defaults to
/// `TimeBound(5s)` when omitted.
#[napi(object)]
pub struct ExecuteOptions {
    #[napi(js_name = "bypassCache")]
    pub bypass_cache: Option<bool>,
    #[napi(js_name = "cachePolicy")]
    pub cache_policy: Option<CachePolicy>,
}

fn execute_options_to_inner(opts: Option<ExecuteOptions>) -> InnerExecuteOptions {
    let Some(opts) = opts else {
        return InnerExecuteOptions::default();
    };
    InnerExecuteOptions {
        bypass_cache: opts.bypass_cache.unwrap_or(false),
        cache_policy: cache_policy_to_inner(opts.cache_policy),
    }
}

/// 1:1 AST factory surface. Construct via static methods that
/// mirror the Rust `OperatorPlan` variants. Internally carries
/// a fully-planned `OperatorNode`; slice 1 exposes only the
/// atomic operators that don't need planner-side resolution.
#[napi]
#[derive(Clone)]
pub struct MeshQuery {
    plan: ExecutionPlan,
}

#[napi]
impl MeshQuery {
    /// Read the event at `seq` from chain `originHash`.
    #[napi(factory)]
    pub fn at(origin_hash: BigInt, seq: BigInt) -> Result<Self> {
        let origin = bigint_u64(origin_hash)?;
        let seq = bigint_u64(seq)?;
        let op = OperatorPlan::AtRead {
            origin,
            seq: SeqNum(seq),
        };
        Ok(Self {
            plan: plan_of(op),
        })
    }

    /// Read events in the half-open seq range `[start, end)`.
    #[napi(factory)]
    pub fn between(origin_hash: BigInt, start: BigInt, end: BigInt) -> Result<Self> {
        let origin = bigint_u64(origin_hash)?;
        let start = bigint_u64(start)?;
        let end = bigint_u64(end)?;
        if start >= end {
            return Err(mesh_err(format!(
                "between: start ({start}) must be < end ({end})"
            )));
        }
        let op = OperatorPlan::BetweenRead {
            origin,
            start: SeqNum(start),
            end: SeqNum(end),
        };
        Ok(Self {
            plan: plan_of(op),
        })
    }

    /// Read the tip event from chain `originHash`.
    #[napi(factory)]
    pub fn latest(origin_hash: BigInt) -> Result<Self> {
        let origin = bigint_u64(origin_hash)?;
        Ok(Self {
            plan: plan_of(OperatorPlan::LatestRead { origin }),
        })
    }

    /// Tumbling window on `seq` with the given bucket `size`.
    /// Emits one sentinel row per non-empty bucket; decode via
    /// `ResultRow.decodeWindow()`.
    #[napi(factory)]
    pub fn window(inner: &MeshQuery, size: BigInt) -> Result<Self> {
        let size = bigint_u64(size)?;
        if size == 0 {
            return Err(mesh_err("window: size must be >= 1".to_string()));
        }
        Ok(Self {
            plan: plan_of(OperatorPlan::Window {
                input: Box::new(inner.plan.root.clone()),
                spec: WindowSpec::TumblingSeq { size },
            }),
        })
    }

    /// Count rows. `groupBy` is an optional list of row-
    /// intrinsic field names: `null` / `[]` = single bucket;
    /// `["origin"]`, `["seq"]`, or `["origin", "seq"]` for
    /// per-group counts.
    #[napi(factory)]
    pub fn count(inner: &MeshQuery, group_by: Option<Vec<String>>) -> Result<Self> {
        let group_by = parse_group_by(group_by)?;
        Ok(Self {
            plan: plan_of(OperatorPlan::AggregateCount {
                input: Box::new(inner.plan.root.clone()),
                group_by,
            }),
        })
    }

    /// Sum of a numeric field across rows. `field` is a row-
    /// intrinsic name (`origin` / `seq`) or a dotted JSON path.
    #[napi(factory)]
    pub fn sum(
        inner: &MeshQuery,
        field: String,
        group_by: Option<Vec<String>>,
    ) -> Result<Self> {
        Self::numeric_agg(inner, field, NumericAggregateKind::Sum, group_by)
    }

    /// Arithmetic mean. Rows where the field is missing /
    /// non-numeric are excluded from both numerator + denom.
    #[napi(factory)]
    pub fn avg(
        inner: &MeshQuery,
        field: String,
        group_by: Option<Vec<String>>,
    ) -> Result<Self> {
        Self::numeric_agg(inner, field, NumericAggregateKind::Avg, group_by)
    }

    /// Minimum value of a numeric field.
    #[napi(factory)]
    pub fn min(
        inner: &MeshQuery,
        field: String,
        group_by: Option<Vec<String>>,
    ) -> Result<Self> {
        Self::reduction(inner, field, NumericReductionKind::Min, group_by)
    }

    /// Maximum value of a numeric field.
    #[napi(factory)]
    pub fn max(
        inner: &MeshQuery,
        field: String,
        group_by: Option<Vec<String>>,
    ) -> Result<Self> {
        Self::reduction(inner, field, NumericReductionKind::Max, group_by)
    }

    /// Nearest-rank exact percentile at `p ∈ [0.0, 1.0]`. Same
    /// field-extraction semantics as the numeric aggregates.
    #[napi(factory)]
    pub fn percentile(
        inner: &MeshQuery,
        field: String,
        p: f64,
        group_by: Option<Vec<String>>,
    ) -> Result<Self> {
        if !p.is_finite() || !(0.0..=1.0).contains(&p) {
            return Err(mesh_err(format!(
                "percentile: p must be in [0.0, 1.0], got {p}"
            )));
        }
        Self::reduction(inner, field, NumericReductionKind::Percentile { p }, group_by)
    }

    /// Exact distinct count over the canonical string
    /// projection of a row-intrinsic / JSON field.
    #[napi(factory, js_name = "distinctCount")]
    pub fn distinct_count(
        inner: &MeshQuery,
        field: String,
        group_by: Option<Vec<String>>,
    ) -> Result<Self> {
        let group_by = parse_group_by(group_by)?;
        Ok(Self {
            plan: plan_of(OperatorPlan::AggregateDistinct {
                input: Box::new(inner.plan.root.clone()),
                group_by,
                field_path: field,
            }),
        })
    }

    /// Inner / outer hash- or sort-merge-join. `kind` is one of
    /// `"inner"` / `"left_outer"` / `"right_outer"` /
    /// `"full_outer"`. `key` is the field name both sides share
    /// — row-intrinsic names (`origin` / `seq` / `origin,seq`)
    /// map to the typed enum; anything else is treated as a
    /// JSON payload path. `strategy` defaults to
    /// `"hash_broadcast"` (alternatives: `"sort_merge"`).
    /// `watermarkSecs` is informational under snapshot
    /// semantics.
    #[napi(factory)]
    pub fn join(
        left: &MeshQuery,
        right: &MeshQuery,
        kind: String,
        key: String,
        strategy: Option<String>,
        watermark_secs: Option<f64>,
    ) -> Result<Self> {
        let kind = parse_join_kind(&kind)?;
        let strategy = parse_join_strategy(strategy.as_deref())?;
        let key_mode = match key.as_str() {
            "origin" => JoinKeyMode::Origin,
            "seq" => JoinKeyMode::Seq,
            "origin,seq" | "origin+seq" => JoinKeyMode::OriginSeq,
            other => JoinKeyMode::Field(other.to_string()),
        };
        let watermark = {
            let secs = watermark_secs.unwrap_or(5.0);
            if secs.is_finite() && secs >= 0.0 {
                std::time::Duration::from_secs_f64(secs)
            } else {
                std::time::Duration::from_secs(5)
            }
        };
        Ok(Self {
            plan: plan_of(OperatorPlan::HashJoin {
                left: Box::new(left.plan.root.clone()),
                right: Box::new(right.plan.root.clone()),
                key_mode,
                kind,
                strategy,
                watermark,
            }),
        })
    }
}

impl MeshQuery {
    fn numeric_agg(
        inner: &MeshQuery,
        field: String,
        kind: NumericAggregateKind,
        group_by: Option<Vec<String>>,
    ) -> Result<Self> {
        let group_by = parse_group_by(group_by)?;
        Ok(Self {
            plan: plan_of(OperatorPlan::AggregateNumeric {
                input: Box::new(inner.plan.root.clone()),
                group_by,
                field_path: field,
                kind,
            }),
        })
    }

    fn reduction(
        inner: &MeshQuery,
        field: String,
        kind: NumericReductionKind,
        group_by: Option<Vec<String>>,
    ) -> Result<Self> {
        let group_by = parse_group_by(group_by)?;
        Ok(Self {
            plan: plan_of(OperatorPlan::AggregateReduction {
                input: Box::new(inner.plan.root.clone()),
                group_by,
                field_path: field,
                kind,
            }),
        })
    }
}

fn parse_group_by(group_by: Option<Vec<String>>) -> Result<Option<JoinKeyMode>> {
    let Some(group_by) = group_by else {
        return Ok(None);
    };
    if group_by.is_empty() {
        return Ok(None);
    }
    if group_by.len() == 1 {
        return match group_by[0].as_str() {
            "origin" => Ok(Some(JoinKeyMode::Origin)),
            "seq" => Ok(Some(JoinKeyMode::Seq)),
            other => Err(mesh_err(format!(
                "groupBy field '{other}' is not a row-intrinsic key; only 'origin' / 'seq' supported"
            ))),
        };
    }
    if group_by.len() == 2 {
        let mut pair = [group_by[0].as_str(), group_by[1].as_str()];
        pair.sort();
        if pair == ["origin", "seq"] {
            return Ok(Some(JoinKeyMode::OriginSeq));
        }
    }
    Err(mesh_err(format!(
        "groupBy shape {group_by:?} not supported; use [], ['origin'], ['seq'], or ['origin', 'seq']"
    )))
}

fn parse_join_kind(s: &str) -> Result<InnerJoinKind> {
    match s {
        "inner" => Ok(InnerJoinKind::Inner),
        "left_outer" => Ok(InnerJoinKind::LeftOuter),
        "right_outer" => Ok(InnerJoinKind::RightOuter),
        "full_outer" => Ok(InnerJoinKind::FullOuter),
        other => Err(mesh_err(format!(
            "join kind '{other}' not recognised; expected inner / left_outer / right_outer / full_outer"
        ))),
    }
}

fn parse_join_strategy(s: Option<&str>) -> Result<JoinStrategy> {
    match s {
        None | Some("hash_broadcast") => Ok(JoinStrategy::HashBroadcast),
        Some("sort_merge") => Ok(JoinStrategy::SortMerge),
        Some(other) => Err(mesh_err(format!(
            "join strategy '{other}' not recognised; expected hash_broadcast / sort_merge"
        ))),
    }
}

fn plan_of(op: OperatorPlan) -> ExecutionPlan {
    ExecutionPlan {
        root: OperatorNode {
            operator: op,
            target_nodes: vec![],
            cost: CostEstimate::default(),
        },
        total_cost: CostEstimate::default(),
    }
}

/// In-process `ChainReader` Node wrapper. Slice 1 ships the
/// in-memory variant; populate via `.append(originHash, seq,
/// payload)` then hand to `MeshQueryRunner`. Phase B+ will
/// expose a `fromRedex(...)` adapter.
#[napi]
pub struct InMemoryChainReader {
    inner: Arc<InMemoryStore>,
}

#[derive(Default)]
struct InMemoryStore {
    chains: std::sync::Mutex<
        std::collections::BTreeMap<u64, std::collections::BTreeMap<SeqNum, Vec<u8>>>,
    >,
}

impl InnerChainReader for InMemoryStore {
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

#[napi]
impl InMemoryChainReader {
    #[napi(constructor)]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(InMemoryStore::default()),
        }
    }

    /// Append a single event to the in-memory store.
    #[napi]
    pub fn append(&self, origin_hash: BigInt, seq: BigInt, payload: Buffer) -> Result<()> {
        let origin = bigint_u64(origin_hash)?;
        let seq = bigint_u64(seq)?;
        self.inner
            .chains
            .lock()
            .unwrap()
            .entry(origin)
            .or_default()
            .insert(SeqNum(seq), payload.to_vec());
        Ok(())
    }

    /// Tip of chain `originHash`, or `null` if unknown.
    #[napi(js_name = "latestSeq")]
    pub fn latest_seq(&self, origin_hash: BigInt) -> Result<Option<BigInt>> {
        let origin = bigint_u64(origin_hash)?;
        Ok(self.inner.latest_seq(origin).map(|s| BigInt::from(s.0)))
    }
}

/// Runs queries against a [`InMemoryChainReader`] via the
/// substrate's `LocalMeshQueryExecutor`. Async by design —
/// `execute()` returns a `MeshQueryStream` whose `next()` is
/// async. The TS-side wrapper layers `Symbol.asyncIterator`
/// so callers can `for await (const row of stream)`.
#[napi]
pub struct MeshQueryRunner {
    executor: Arc<LocalMeshQueryExecutor<InMemoryStore>>,
}

#[napi]
impl MeshQueryRunner {
    /// Build a runner. `enableCache` wires the Phase F LRU
    /// (default: false). Capability-version source is hard-
    /// wired to `0` while there's no `CapabilityIndex` plumbed
    /// (slice 1 is local-executor-only).
    #[napi(constructor)]
    pub fn new(reader: &InMemoryChainReader, enable_cache: Option<bool>) -> Self {
        let store = reader.inner.clone();
        let executor = if enable_cache.unwrap_or(false) {
            let cache: Arc<dyn net::adapter::net::behavior::meshdb::cache::ResultCache> =
                Arc::new(LruResultCache::default());
            let version_fn: Arc<dyn Fn() -> u64 + Send + Sync> = Arc::new(|| 0);
            LocalMeshQueryExecutor::with_cache(store, cache, version_fn)
        } else {
            LocalMeshQueryExecutor::new(store)
        };
        Self {
            executor: Arc::new(executor),
        }
    }

    /// Execute `query`. Returns a stream whose `next()` yields
    /// the next [`ResultRow`] (or `null` on EOF). The full row
    /// list is drained at execute time and buffered inside the
    /// stream; true row-by-row streaming lands when a consumer
    /// needs it.
    #[napi]
    pub async fn execute(
        &self,
        query: &MeshQuery,
        options: Option<ExecuteOptions>,
    ) -> Result<MeshQueryStream> {
        use futures::StreamExt;
        let plan = query.plan.clone();
        let opts = execute_options_to_inner(options);
        let running = self
            .executor
            .execute_with(plan, opts)
            .await
            .map_err(|e| mesh_err(format!("{e}")))?;
        let mut stream = running.rows;
        let mut out: Vec<ResultRow> = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(row) => out.push(row.into()),
                Err(e) => return Err(mesh_err(format!("{e}"))),
            }
        }
        Ok(MeshQueryStream {
            // Reverse so `.pop()` returns rows in original order.
            rows: Arc::new(AsyncMutex::new({
                out.reverse();
                out
            })),
        })
    }
}

/// Pull-based row stream. The JS-side TS shim adds
/// `Symbol.asyncIterator` over this; raw callers can use
/// `await stream.next()` in a loop themselves.
#[napi]
pub struct MeshQueryStream {
    rows: Arc<AsyncMutex<Vec<ResultRow>>>,
}

#[napi]
impl MeshQueryStream {
    /// The next row, or `null` on end-of-stream. Idempotent
    /// post-EOF — repeated calls keep returning `null`.
    #[napi]
    pub async fn next(&self) -> Result<Option<ResultRow>> {
        Ok(self.rows.lock().await.pop())
    }

    /// Drain the remaining rows into a list. Convenience for
    /// callers that don't want to write the `await next()`
    /// loop. Subsequent `.next()` calls return `null`.
    #[napi(js_name = "toArray")]
    pub async fn to_array(&self) -> Result<Vec<ResultRow>> {
        let mut g = self.rows.lock().await;
        let mut out: Vec<ResultRow> = std::mem::take(&mut *g);
        // We stored reversed; un-reverse on drain so callers
        // get original insertion order.
        out.reverse();
        Ok(out)
    }
}

fn mesh_err(msg: String) -> Error {
    Error::new(Status::GenericFailure, msg)
}
