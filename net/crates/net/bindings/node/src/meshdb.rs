// `#[napi]` exports functions / structs to JS but leaves them "unused"
// from Rust's POV, so clippy's dead-code analysis fires when the
// node binding is linted with `--all-targets` (which compiles the
// `lib test` configuration). Suppress at file scope — matches the
// pattern in `meshos.rs`.
#![allow(dead_code)]

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

use net::adapter::net::behavior::predicate::Predicate as InnerPredicate;
use net::adapter::net::behavior::tag::{TagKey, TaxonomyAxis};

use net::adapter::net::behavior::meshdb::{
    cache::{CachePolicy as InnerCachePolicy, LruResultCache},
    error::MeshError,
    executor::{
        ChainReader as InnerChainReader, ExecuteOptions as InnerExecuteOptions,
        LocalMeshQueryExecutor, MeshQueryExecutor,
    },
    planner::{
        CostEstimate, JoinKeyMode, JoinStrategy, LineageDirection,
        LineageEntry as InnerLineageEntry, OperatorNode, OperatorPlan,
    },
    query::{
        clamp_join_watermark_secs, AggregateRowPayload as InnerAggregateRowPayload,
        AggregateValue as InnerAggregateValue, GroupKey as InnerGroupKey,
        JoinKind as InnerJoinKind, JoinedRowPayload as InnerJoinedRowPayload, NumericAggregateKind,
        NumericReductionKind, ResultRow as InnerResultRow, WindowBoundary as InnerWindowBoundary,
        WindowSpec,
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

/// One chain reached during a lineage walk. Pre-walked by the
/// caller and handed to `MeshQuery.lineageEmit(...)`. The SDK
/// doesn't itself walk the `fork-of:` graph — that needs a
/// `CapabilityIndex`, which isn't plumbed through the Node
/// runner yet. Callers maintain their own graph view and emit
/// entries in walk order: index 0 is the start origin with
/// `depth = 0n`; ancestors / descendants follow.
#[napi(object)]
pub struct LineageEntry {
    /// Chain origin hash (substrate `u64`).
    #[napi(js_name = "originHash")]
    pub origin_hash: BigInt,
    /// Hops from the walk's start. `0n` for the start origin.
    /// Substrate-side this is a `u32`; values outside that range
    /// are rejected at the `lineageEmit` factory. BigInt for
    /// shape parity with the other id-like fields on this
    /// struct.
    pub depth: BigInt,
    /// Best-known tip seq for this chain, if any. Surfaces in
    /// the emitted row's `seq` field (defaults to `0` when
    /// absent).
    #[napi(js_name = "tipSeq")]
    pub tip_seq: Option<BigInt>,
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
/// defaults to 5 s when omitted. Negative or non-finite
/// values are rejected at the factory (Python / Go behave the
/// same way); the converter at execute-time no longer rewrites
/// silently.
#[napi(js_name = "cachePolicyTimeBound")]
pub fn cache_policy_time_bound(seconds: Option<f64>) -> Result<CachePolicy> {
    let secs = seconds.unwrap_or(5.0);
    if !secs.is_finite() || secs < 0.0 {
        return Err(mesh_err(format!(
            "cachePolicyTimeBound: ttlSeconds must be a non-negative finite number; got {secs}"
        )));
    }
    Ok(CachePolicy {
        kind: "time_bound".to_string(),
        ttl_seconds: Some(secs),
    })
}

fn cache_policy_to_inner(p: Option<CachePolicy>) -> Result<InnerCachePolicy> {
    let Some(p) = p else {
        return Ok(InnerCachePolicy::default());
    };
    match p.kind.as_str() {
        "permanent" => Ok(InnerCachePolicy::Permanent),
        "time_bound" => {
            let secs = p.ttl_seconds.unwrap_or(5.0);
            if !secs.is_finite() || secs < 0.0 {
                return Err(mesh_err(format!(
                    "cachePolicy.ttlSeconds must be a non-negative finite number; got {secs}"
                )));
            }
            Ok(InnerCachePolicy::TimeBound {
                ttl: std::time::Duration::from_secs_f64(secs),
            })
        }
        other => Err(mesh_err(format!(
            "cachePolicy.kind must be 'permanent' or 'time_bound'; got {other:?}"
        ))),
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

fn execute_options_to_inner(opts: Option<ExecuteOptions>) -> Result<InnerExecuteOptions> {
    let Some(opts) = opts else {
        return Ok(InnerExecuteOptions::default());
    };
    Ok(InnerExecuteOptions {
        bypass_cache: opts.bypass_cache.unwrap_or(false),
        cache_policy: cache_policy_to_inner(opts.cache_policy)?,
    })
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
        Ok(Self { plan: plan_of(op) })
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
        Ok(Self { plan: plan_of(op) })
    }

    /// Read the tip event from chain `originHash`.
    #[napi(factory)]
    pub fn latest(origin_hash: BigInt) -> Result<Self> {
        let origin = bigint_u64(origin_hash)?;
        Ok(Self {
            plan: plan_of(OperatorPlan::LatestRead { origin }),
        })
    }

    /// Start a fluent builder. Equivalent to constructing a
    /// fresh [`QueryBuilder`]; chainable methods (`at`,
    /// `between`, `latest`, `filter`, `count`, `sum`, `avg`,
    /// `min`, `max`, `percentile`, `distinctCount`, `window`,
    /// `join`) compose into a final `MeshQuery` via `.build()`.
    ///
    /// Annotated as a plain `#[napi]` static method rather than
    /// `#[napi(factory)]` because the return type is a different
    /// class (`QueryBuilder`) — `factory` always wraps the return
    /// value as `Self`, which would mis-construct the JS instance
    /// as a `MeshQuery` and strip the `QueryBuilder` methods.
    #[napi]
    pub fn builder() -> QueryBuilder {
        QueryBuilder { state: None }
    }

    /// Filter `inner`'s rows by `predicate`. The executor builds
    /// a synthetic per-row tag view (origin / seq / flat JSON
    /// payload fields) and evaluates the predicate; rows whose
    /// evaluation returns `true` pass through unchanged. Rows
    /// whose payload isn't JSON are still filterable by their
    /// row-intrinsic fields (`origin`, `seq`); payload field
    /// references against a non-JSON payload simply don't match.
    #[napi(factory)]
    pub fn filter(inner: &MeshQuery, predicate: Predicate) -> Result<Self> {
        let typed = predicate_to_inner(predicate)?;
        Ok(Self {
            plan: plan_of(OperatorPlan::Filter {
                input: Box::new(inner.plan.root.clone()),
                predicate: typed.to_wire(),
            }),
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
    pub fn sum(inner: &MeshQuery, field: String, group_by: Option<Vec<String>>) -> Result<Self> {
        Self::numeric_agg(inner, field, NumericAggregateKind::Sum, group_by)
    }

    /// Arithmetic mean. Rows where the field is missing /
    /// non-numeric are excluded from both numerator + denom.
    #[napi(factory)]
    pub fn avg(inner: &MeshQuery, field: String, group_by: Option<Vec<String>>) -> Result<Self> {
        Self::numeric_agg(inner, field, NumericAggregateKind::Avg, group_by)
    }

    /// Minimum value of a numeric field.
    #[napi(factory)]
    pub fn min(inner: &MeshQuery, field: String, group_by: Option<Vec<String>>) -> Result<Self> {
        Self::reduction(inner, field, NumericReductionKind::Min, group_by)
    }

    /// Maximum value of a numeric field.
    #[napi(factory)]
    pub fn max(inner: &MeshQuery, field: String, group_by: Option<Vec<String>>) -> Result<Self> {
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
        Self::reduction(
            inner,
            field,
            NumericReductionKind::Percentile { p },
            group_by,
        )
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
        // Canonical join-key keywords across all shims:
        // "origin", "seq", "origin,seq". Anything else is
        // treated as a dotted JSON field path. The variant
        // "origin+seq" was tolerated in earlier slices but is
        // now rejected — cross-language conformance tests need
        // one canonical encoding.
        let key_mode = match key.as_str() {
            "origin" => JoinKeyMode::Origin,
            "seq" => JoinKeyMode::Seq,
            "origin,seq" => JoinKeyMode::OriginSeq,
            other => JoinKeyMode::Field(other.to_string()),
        };
        let watermark = clamp_join_watermark_secs(watermark_secs);
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

    /// Emit a pre-walked lineage as one row per entry. `entries`
    /// is a list of `LineageEntry` in walk order; `direction` is
    /// `"back"` or `"forward"`. Each entry emits a `ResultRow`
    /// with `originHash = entry.originHash`, `seq = entry.tipSeq
    /// ?? 0`, payload empty. Compose with `at` / `between` to
    /// fetch event content for each chain.
    #[napi(factory, js_name = "lineageEmit")]
    pub fn lineage_emit(
        origin_hash: BigInt,
        entries: Vec<LineageEntry>,
        direction: String,
    ) -> Result<Self> {
        let origin = bigint_u64(origin_hash)?;
        let direction = parse_lineage_direction(&direction)?;
        let entries = entries
            .into_iter()
            .map(|e| -> Result<InnerLineageEntry> {
                let entry_origin = bigint_u64(e.origin_hash)?;
                let depth_u64 = bigint_u64(e.depth)?;
                let depth = u32::try_from(depth_u64).map_err(|_| {
                    mesh_err(format!(
                        "lineageEmit: depth {depth_u64} exceeds u32::MAX ({})",
                        u32::MAX
                    ))
                })?;
                let tip_seq = e.tip_seq.map(bigint_u64).transpose()?.map(SeqNum);
                Ok(InnerLineageEntry {
                    origin: entry_origin,
                    depth,
                    tip_seq,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            plan: plan_of(OperatorPlan::LineageEmit {
                origin,
                direction,
                entries,
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
    if group_by.len() == 2 && group_by[0].as_str() == "origin" && group_by[1].as_str() == "seq" {
        return Ok(Some(JoinKeyMode::OriginSeq));
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

/// Fluent builder for the common-ops query shape. Each
/// chainable method returns a fresh builder so callers can
/// write
///
/// ```ts
/// const q = MeshQuery.builder()
///   .between(0xCDn, 0n, 100n)
///   .filter(predicateEquals('severity', 'high'))
///   .count(null)
///   .build();
/// ```
///
/// Source operators (`at` / `between` / `latest`) seed the
/// pipeline; pipeline operators require a seeded state and
/// surface a `MeshDbError` when called on an empty builder.
/// `.build()` consumes the builder into a `MeshQuery`.
#[napi]
#[derive(Clone)]
pub struct QueryBuilder {
    state: Option<MeshQuery>,
}

#[napi]
impl QueryBuilder {
    /// Source: read a single event. Resets any prior state.
    #[napi]
    pub fn at(&self, origin_hash: BigInt, seq: BigInt) -> Result<Self> {
        Ok(Self {
            state: Some(MeshQuery::at(origin_hash, seq)?),
        })
    }

    /// Source: read events in the half-open seq range. Resets
    /// any prior state.
    #[napi]
    pub fn between(&self, origin_hash: BigInt, start: BigInt, end: BigInt) -> Result<Self> {
        Ok(Self {
            state: Some(MeshQuery::between(origin_hash, start, end)?),
        })
    }

    /// Source: read the tip event. Resets any prior state.
    #[napi]
    pub fn latest(&self, origin_hash: BigInt) -> Result<Self> {
        Ok(Self {
            state: Some(MeshQuery::latest(origin_hash)?),
        })
    }

    /// Filter the current pipeline's rows.
    #[napi]
    pub fn filter(&self, predicate: Predicate) -> Result<Self> {
        let inner = self.require_state("filter")?;
        Ok(Self {
            state: Some(MeshQuery::filter(&inner, predicate)?),
        })
    }

    /// Count rows in the current pipeline.
    #[napi]
    pub fn count(&self, group_by: Option<Vec<String>>) -> Result<Self> {
        let inner = self.require_state("count")?;
        Ok(Self {
            state: Some(MeshQuery::count(&inner, group_by)?),
        })
    }

    /// Sum of a numeric field.
    #[napi]
    pub fn sum(&self, field: String, group_by: Option<Vec<String>>) -> Result<Self> {
        let inner = self.require_state("sum")?;
        Ok(Self {
            state: Some(MeshQuery::sum(&inner, field, group_by)?),
        })
    }

    /// Arithmetic mean of a numeric field.
    #[napi]
    pub fn avg(&self, field: String, group_by: Option<Vec<String>>) -> Result<Self> {
        let inner = self.require_state("avg")?;
        Ok(Self {
            state: Some(MeshQuery::avg(&inner, field, group_by)?),
        })
    }

    /// Min over a numeric field.
    #[napi]
    pub fn min(&self, field: String, group_by: Option<Vec<String>>) -> Result<Self> {
        let inner = self.require_state("min")?;
        Ok(Self {
            state: Some(MeshQuery::min(&inner, field, group_by)?),
        })
    }

    /// Max over a numeric field.
    #[napi]
    pub fn max(&self, field: String, group_by: Option<Vec<String>>) -> Result<Self> {
        let inner = self.require_state("max")?;
        Ok(Self {
            state: Some(MeshQuery::max(&inner, field, group_by)?),
        })
    }

    /// Nearest-rank percentile.
    #[napi]
    pub fn percentile(&self, field: String, p: f64, group_by: Option<Vec<String>>) -> Result<Self> {
        let inner = self.require_state("percentile")?;
        Ok(Self {
            state: Some(MeshQuery::percentile(&inner, field, p, group_by)?),
        })
    }

    /// Exact distinct count.
    #[napi(js_name = "distinctCount")]
    pub fn distinct_count(&self, field: String, group_by: Option<Vec<String>>) -> Result<Self> {
        let inner = self.require_state("distinctCount")?;
        Ok(Self {
            state: Some(MeshQuery::distinct_count(&inner, field, group_by)?),
        })
    }

    /// Tumbling window on `seq` with the given bucket `size`.
    #[napi]
    pub fn window(&self, size: BigInt) -> Result<Self> {
        let inner = self.require_state("window")?;
        Ok(Self {
            state: Some(MeshQuery::window(&inner, size)?),
        })
    }

    /// Join the current pipeline with `right`. See
    /// [`MeshQuery::join`] for full parameter docs.
    #[napi]
    pub fn join(
        &self,
        right: &MeshQuery,
        kind: String,
        key: String,
        strategy: Option<String>,
        watermark_secs: Option<f64>,
    ) -> Result<Self> {
        let inner = self.require_state("join")?;
        Ok(Self {
            state: Some(MeshQuery::join(
                &inner,
                right,
                kind,
                key,
                strategy,
                watermark_secs,
            )?),
        })
    }

    /// Terminal: consume the builder into a `MeshQuery`.
    /// Surfaces `MeshDbError` when the builder has no source.
    #[napi]
    pub fn build(&self) -> Result<MeshQuery> {
        self.require_state("build")
    }
}

impl QueryBuilder {
    fn require_state(&self, op: &str) -> Result<MeshQuery> {
        self.state.clone().ok_or_else(|| {
            mesh_err(format!(
                "{op}: builder has no source — call .at(...), .between(...), or .latest(...) first"
            ))
        })
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

fn parse_lineage_direction(s: &str) -> Result<LineageDirection> {
    match s {
        "back" => Ok(LineageDirection::Back),
        "forward" => Ok(LineageDirection::Forward),
        other => Err(mesh_err(format!(
            "lineage direction '{other}' not recognised; expected 'back' or 'forward'"
        ))),
    }
}

/// Tagged predicate object. `kind` discriminates the variant;
/// the field set is the union of all variants' inputs (each
/// variant uses a subset). Build with the module-level
/// `predicate*` helpers — manual object literals work too if
/// you're sure about the field set.
///
/// Field paths target the synthetic `Dataforts` axis: a
/// row-intrinsic name like `"origin"` / `"seq"` resolves to the
/// row's intrinsic; a JSON path like `"severity"` or `"a.b.c"`
/// resolves to the flattened JSON-object payload.
#[napi(object)]
pub struct Predicate {
    /// Variant discriminator. One of: `exists` / `equals` /
    /// `numeric_at_least` / `numeric_at_most` /
    /// `numeric_in_range` / `string_prefix` / `string_matches`
    /// / `semver_at_least` / `and` / `or` / `not`.
    pub kind: String,
    /// Field name (axis-tag predicates).
    pub field: Option<String>,
    /// String value (equals).
    pub value: Option<String>,
    /// Numeric threshold (numeric_at_least / numeric_at_most).
    pub threshold: Option<f64>,
    /// Range lower bound (numeric_in_range).
    pub min: Option<f64>,
    /// Range upper bound (numeric_in_range).
    pub max: Option<f64>,
    /// String prefix (string_prefix).
    pub prefix: Option<String>,
    /// String pattern (string_matches).
    pub pattern: Option<String>,
    /// Version literal (semver_at_least).
    pub version: Option<String>,
    /// Child predicates (and / or / not — `not` uses a 1-element
    /// list).
    pub children: Option<Vec<Predicate>>,
}

/// Field is present (any value).
#[napi(js_name = "predicateExists")]
pub fn predicate_exists(field: String) -> Predicate {
    Predicate {
        kind: "exists".to_string(),
        field: Some(field),
        ..Predicate::empty()
    }
}

/// `field == value` (string equality).
#[napi(js_name = "predicateEquals")]
pub fn predicate_equals(field: String, value: String) -> Predicate {
    Predicate {
        kind: "equals".to_string(),
        field: Some(field),
        value: Some(value),
        ..Predicate::empty()
    }
}

/// `field >= threshold` (numeric).
#[napi(js_name = "predicateNumericAtLeast")]
pub fn predicate_numeric_at_least(field: String, threshold: f64) -> Predicate {
    Predicate {
        kind: "numeric_at_least".to_string(),
        field: Some(field),
        threshold: Some(threshold),
        ..Predicate::empty()
    }
}

/// `field <= threshold` (numeric).
#[napi(js_name = "predicateNumericAtMost")]
pub fn predicate_numeric_at_most(field: String, threshold: f64) -> Predicate {
    Predicate {
        kind: "numeric_at_most".to_string(),
        field: Some(field),
        threshold: Some(threshold),
        ..Predicate::empty()
    }
}

/// `min <= field <= max`.
#[napi(js_name = "predicateNumericInRange")]
pub fn predicate_numeric_in_range(field: String, min: f64, max: f64) -> Result<Predicate> {
    if !(min.is_finite() && max.is_finite()) || min > max {
        return Err(mesh_err(format!(
            "predicateNumericInRange: requires finite min <= max (got min={min}, max={max})"
        )));
    }
    Ok(Predicate {
        kind: "numeric_in_range".to_string(),
        field: Some(field),
        min: Some(min),
        max: Some(max),
        ..Predicate::empty()
    })
}

/// `field.startsWith(prefix)`.
#[napi(js_name = "predicateStringPrefix")]
pub fn predicate_string_prefix(field: String, prefix: String) -> Predicate {
    Predicate {
        kind: "string_prefix".to_string(),
        field: Some(field),
        prefix: Some(prefix),
        ..Predicate::empty()
    }
}

/// Substring `pattern` in `field`.
#[napi(js_name = "predicateStringMatches")]
pub fn predicate_string_matches(field: String, pattern: String) -> Predicate {
    Predicate {
        kind: "string_matches".to_string(),
        field: Some(field),
        pattern: Some(pattern),
        ..Predicate::empty()
    }
}

/// `field >= version` (semver).
#[napi(js_name = "predicateSemverAtLeast")]
pub fn predicate_semver_at_least(field: String, version: String) -> Predicate {
    Predicate {
        kind: "semver_at_least".to_string(),
        field: Some(field),
        version: Some(version),
        ..Predicate::empty()
    }
}

/// Conjunction. Empty list evaluates to `true` (vacuous match).
#[napi(js_name = "predicateAnd")]
pub fn predicate_and(children: Vec<Predicate>) -> Predicate {
    Predicate {
        kind: "and".to_string(),
        children: Some(children),
        ..Predicate::empty()
    }
}

/// Disjunction. Empty list evaluates to `false`.
#[napi(js_name = "predicateOr")]
pub fn predicate_or(children: Vec<Predicate>) -> Predicate {
    Predicate {
        kind: "or".to_string(),
        children: Some(children),
        ..Predicate::empty()
    }
}

/// Negation.
#[napi(js_name = "predicateNot")]
pub fn predicate_not(child: Predicate) -> Predicate {
    Predicate {
        kind: "not".to_string(),
        children: Some(vec![child]),
        ..Predicate::empty()
    }
}

impl Predicate {
    fn empty() -> Self {
        Self {
            kind: String::new(),
            field: None,
            value: None,
            threshold: None,
            min: None,
            max: None,
            prefix: None,
            pattern: None,
            version: None,
            children: None,
        }
    }
}

fn tag_key(field: &str) -> TagKey {
    TagKey {
        axis: TaxonomyAxis::Dataforts,
        key: field.to_string(),
    }
}

fn predicate_to_inner(p: Predicate) -> Result<InnerPredicate> {
    let need_field = |p: &Predicate, kind: &str| {
        p.field
            .clone()
            .ok_or_else(|| mesh_err(format!("predicate '{kind}' requires `field`",)))
    };
    match p.kind.as_str() {
        "exists" => Ok(InnerPredicate::Exists {
            key: tag_key(&need_field(&p, "exists")?),
        }),
        "equals" => {
            let field = need_field(&p, "equals")?;
            let value = p
                .value
                .clone()
                .ok_or_else(|| mesh_err("predicate 'equals' requires `value`".to_string()))?;
            Ok(InnerPredicate::Equals {
                key: tag_key(&field),
                value,
            })
        }
        "numeric_at_least" => Ok(InnerPredicate::NumericAtLeast {
            key: tag_key(&need_field(&p, "numeric_at_least")?),
            threshold: p.threshold.ok_or_else(|| {
                mesh_err("predicate 'numeric_at_least' requires `threshold`".to_string())
            })?,
        }),
        "numeric_at_most" => Ok(InnerPredicate::NumericAtMost {
            key: tag_key(&need_field(&p, "numeric_at_most")?),
            threshold: p.threshold.ok_or_else(|| {
                mesh_err("predicate 'numeric_at_most' requires `threshold`".to_string())
            })?,
        }),
        "numeric_in_range" => Ok(InnerPredicate::NumericInRange {
            key: tag_key(&need_field(&p, "numeric_in_range")?),
            min: p.min.ok_or_else(|| {
                mesh_err("predicate 'numeric_in_range' requires `min`".to_string())
            })?,
            max: p.max.ok_or_else(|| {
                mesh_err("predicate 'numeric_in_range' requires `max`".to_string())
            })?,
        }),
        "string_prefix" => Ok(InnerPredicate::StringPrefix {
            key: tag_key(&need_field(&p, "string_prefix")?),
            prefix: p.prefix.clone().ok_or_else(|| {
                mesh_err("predicate 'string_prefix' requires `prefix`".to_string())
            })?,
        }),
        "string_matches" => Ok(InnerPredicate::StringMatches {
            key: tag_key(&need_field(&p, "string_matches")?),
            pattern: p.pattern.clone().ok_or_else(|| {
                mesh_err("predicate 'string_matches' requires `pattern`".to_string())
            })?,
        }),
        "semver_at_least" => Ok(InnerPredicate::SemverAtLeast {
            key: tag_key(&need_field(&p, "semver_at_least")?),
            version: p.version.clone().ok_or_else(|| {
                mesh_err("predicate 'semver_at_least' requires `version`".to_string())
            })?,
        }),
        "and" => {
            let children = p.children.unwrap_or_default();
            let mut converted: Vec<InnerPredicate> = Vec::with_capacity(children.len());
            for c in children {
                converted.push(predicate_to_inner(c)?);
            }
            Ok(InnerPredicate::And(converted))
        }
        "or" => {
            let children = p.children.unwrap_or_default();
            let mut converted: Vec<InnerPredicate> = Vec::with_capacity(children.len());
            for c in children {
                converted.push(predicate_to_inner(c)?);
            }
            Ok(InnerPredicate::Or(converted))
        }
        "not" => {
            let mut children = p.children.unwrap_or_default();
            if children.len() != 1 {
                return Err(mesh_err(format!(
                    "predicate 'not' requires exactly one child, got {}",
                    children.len()
                )));
            }
            Ok(InnerPredicate::Not(Box::new(predicate_to_inner(
                children.remove(0),
            )?)))
        }
        other => Err(mesh_err(format!("unknown predicate kind '{other}'"))),
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
    executor: LocalMeshQueryExecutor<InMemoryStore>,
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
        Self { executor }
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
        let opts = execute_options_to_inner(options)?;
        let running = self
            .executor
            .execute_with(plan, opts)
            .await
            .map_err(|e| mesh_err_kinded(&e))?;
        let mut stream = running.rows;
        let mut out: Vec<ResultRow> = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(row) => out.push(row.into()),
                Err(e) => return Err(mesh_err_kinded(&e)),
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

    /// Discard any remaining rows immediately, freeing the
    /// backing buffer. Used by the AsyncIterable `return()` /
    /// `throw()` hooks so a `for await (...) { if (...) break; }`
    /// loop releases the result vector promptly instead of
    /// holding it pinned on the AsyncMutex until JS GC fires.
    /// Subsequent `.next()` calls return `null`.
    #[napi]
    pub async fn release(&self) {
        let mut g = self.rows.lock().await;
        let _drop = std::mem::take(&mut *g);
    }
}

fn mesh_err(msg: String) -> Error {
    // No kind information — used for SDK-side validation
    // failures (predicate factory, group_by shape, etc.). The
    // executor / planner paths use `mesh_err_kinded` below.
    Error::new(Status::GenericFailure, msg)
}

/// Wire a `MeshError` to a napi `Error`, embedding the
/// structured kind discriminator in the reason string so the
/// JS-side SDK can recover it.
///
/// Reason format: `<<meshdb-kind:KIND>>MSG`. The SDK exposes
/// a helper that parses this back into `{kind, message}`.
/// Errors raised from non-substrate paths (factory validation
/// etc.) use plain `mesh_err`; consumers branch on whether the
/// `<<meshdb-kind:` prefix is present.
fn mesh_err_kinded(err: &MeshError) -> Error {
    Error::new(
        Status::GenericFailure,
        format!("<<meshdb-kind:{}>>{}", err.kind(), err),
    )
}
