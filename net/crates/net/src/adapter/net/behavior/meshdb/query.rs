//! `MeshQuery` AST + supporting types.
//!
//! Phase A of [`MESHDB_PLAN.md`](../../../../../../docs/plans/MESHDB_PLAN.md).
//! Closed under composition: every variant nests other queries,
//! so a single typed expression encodes a full federated plan.
//!
//! # Locked decision #1 — AST stability
//!
//! [`MeshQuery`] is explicitly versioned at the enum top level.
//! The Phase A serde + postcard surfaces both honor this — a
//! v2 wire form decoded against a v1 substrate rejects cleanly
//! at the planner layer; never silently drops fields.
//!
//! Adding a new operator variant inside [`QueryV1`] is a
//! non-bump if (a) the new operator is optional and (b) old
//! planners reject unknown variants cleanly. The
//! `#[non_exhaustive]` attribute on `QueryV1` enforces (a) at
//! the source level (downstream `match` calls must catch-all);
//! the planner's "operator not yet implemented in this build"
//! [`MeshError`]`::PlannerError` enforces (b) at the runtime.
//!
//! [`MeshError`]: super::error::MeshError
//!
//! # Locked decision #7 — Window operator
//!
//! `Window { kind, duration }` ships in Phase E (per the locked
//! decision), not in Phase A. The AST below intentionally omits
//! the variant; when Phase E activates, a `Window` variant
//! lands inside `QueryV1` (non-breaking — the
//! `#[non_exhaustive]` attribute carries the contract).

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::adapter::net::behavior::predicate::PredicateWire;

/// Versioned outer query enum. Per locked decision #1, the
/// version tag is explicit at the top level so the wire / FFI
/// boundary can reject unknown versions cleanly without
/// silently partial-decoding.
///
/// Today only `V1` exists. Future versions land here as new
/// variants; old planners decoding a `V2` payload return a
/// [`MeshError`]`::PlannerError` rather than partially
/// interpreting it.
///
/// [`MeshError`]: super::error::MeshError
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MeshQuery {
    /// MeshDB v1 query shape (Phase A → F).
    V1(QueryV1),
}

/// The v1 query algebra. Closed under composition — every
/// variant either reads from a [`ChainRef`] directly or wraps
/// an inner `MeshQuery` recursively.
///
/// `#[non_exhaustive]` so downstream consumers must include a
/// catch-all `_` arm in their match expressions; this lets us
/// add new operator variants (e.g. `Window` in Phase E)
/// without breaking source-side users.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum QueryV1 {
    /// Atomic — read a chain at a specific seq.
    At {
        /// The chain to read.
        origin: ChainRef,
        /// Sequence number (inclusive).
        seq: SeqNum,
    },

    /// Atomic — read a chain across a seq range. Half-open
    /// `[start, end)`; the same convention as the rest of the
    /// substrate's range ops.
    Between {
        /// The chain to read.
        origin: ChainRef,
        /// Lower bound (inclusive).
        start: SeqNum,
        /// Upper bound (exclusive).
        end: SeqNum,
    },

    /// Atomic — read a chain's current tip event.
    Latest {
        /// The chain to read.
        origin: ChainRef,
    },

    /// Composite — walk `fork-of:` parents backward toward
    /// ancestors. Stops at `max_depth` or when no further
    /// `fork-of:` link is found. Cycle-safe via visited-set
    /// tracking inside the executor (Phase C).
    LineageBack {
        /// Starting point.
        origin: ChainRef,
        /// Maximum number of hops to traverse. Default 32 in
        /// the SDK helpers; lineage chains rarely go deeper.
        max_depth: u32,
    },

    /// Composite — walk `fork-of:` descendants forward.
    /// Implementation queries the capability index for
    /// "all chains advertising `fork-of:<this_origin>`" via
    /// [`CapabilityQuery::match_axis`]; chains form a DAG, so
    /// the depth bound is the practical guard against
    /// runaway traversal.
    ///
    /// [`CapabilityQuery::match_axis`]: super::super::query::CapabilityQuery::match_axis
    LineageForward {
        /// Starting point.
        origin: ChainRef,
        /// Maximum number of hops to traverse.
        max_depth: u32,
    },

    /// Composite — join two chain queries on a correlation
    /// key. Strategy (broadcast / hash-partitioned /
    /// sort-merge) is picked by the planner based on
    /// cardinality estimates from the capability index's
    /// `aggregate` primitive (Phase D).
    Join {
        /// Left-side sub-query.
        left: Box<MeshQuery>,
        /// Right-side sub-query.
        right: Box<MeshQuery>,
        /// Correlation key extracted from each side's rows.
        on: JoinKey,
        /// Inner / outer-join semantics.
        kind: JoinKind,
        /// Late-arrival watermark per locked decision #2.
        /// Default 5s (`Duration::from_secs(5)`); pass
        /// `Duration::MAX` for batch queries over closed
        /// seq ranges (waits forever for matches, effectively
        /// infinite).
        watermark: Duration,
    },

    /// Composite — filter inner rows by predicate. Reuses the
    /// Capability System's [`PredicateWire`] (the
    /// serializable flat-tree form) so the wire layer can
    /// round-trip filters without breaking the in-process
    /// `Predicate` AST.
    Filter {
        /// Inner sub-query whose rows are filtered.
        inner: Box<MeshQuery>,
        /// Predicate evaluated per row.
        predicate: PredicateWire,
    },

    /// Composite — aggregate inner rows. Federated push-down
    /// execution: each node returns a partial aggregate; the
    /// caller-side combiner merges. Sketches (HLL, T-Digest)
    /// use the canonical encodings locked in decision #3.
    Aggregate {
        /// Inner sub-query whose rows are aggregated.
        inner: Box<MeshQuery>,
        /// Group-by expression list. Empty = single group.
        group_by: Vec<Expr>,
        /// Aggregate function applied to each group.
        agg_fn: AggregateFn,
    },

    /// Composite — project / transform rows. Lets queries
    /// produce only the fields the caller cares about, which
    /// also helps the planner push down narrower projections
    /// closer to the data nodes.
    Project {
        /// Inner sub-query whose rows are projected.
        inner: Box<MeshQuery>,
        /// Column expressions in output order.
        columns: Vec<Expr>,
    },

    /// Composite — order + optional limit. Pushed down to the
    /// data nodes when possible (e.g. `OrderBy` on a chain's
    /// seq is free because chains are already seq-sorted).
    OrderBy {
        /// Inner sub-query whose rows are ordered.
        inner: Box<MeshQuery>,
        /// Ordering keys (lexicographic — first key dominant).
        by: Vec<OrderKey>,
        /// Optional row cap. `None` = unbounded.
        limit: Option<u64>,
    },
    /// Composite — bucket rows into tumbling windows of fixed
    /// size, then emit one [`super::query::WindowBoundary`] per
    /// bucket carrying the rows inside it. Locked-decision #6
    /// per the plan; Phase E-5 ships tumbling-on-seq
    /// (overlapping / session windows defer until a consumer
    /// drives the shape).
    Window {
        /// Inner sub-query whose rows are bucketed.
        inner: Box<MeshQuery>,
        /// Bucketing strategy.
        spec: WindowSpec,
    },
}

/// Window strategy for [`QueryV1::Window`]. Phase E-5 ships
/// only the tumbling-on-seq variant; sliding + session windows
/// extend cleanly via additional variants.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum WindowSpec {
    /// Non-overlapping fixed-size buckets keyed on
    /// [`ResultRow::seq`]. Bucket `i` contains rows whose seq
    /// falls in `[i * size, (i + 1) * size)`.
    TumblingSeq {
        /// Window size in seq units. Must be `>= 1`; the
        /// planner rejects `0`.
        size: u64,
    },
}

/// One bucket emitted by the Window operator. Postcard-
/// encoded inside each window output [`ResultRow`]'s
/// `payload` (with `origin = 0` and `seq = SeqNum(<bucket_start>)`
/// — the seq carries the bucket boundary so a downstream
/// `OrderBy` can sort on it without decoding the payload).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowBoundary {
    /// Inclusive bucket start (in seq units).
    pub start: SeqNum,
    /// Exclusive bucket end.
    pub end: SeqNum,
    /// Rows in this bucket, ordered by their original seq.
    pub rows: Vec<ResultRow>,
}

/// How a [`MeshQuery`] addresses a chain.
///
/// # Origin-hash width
///
/// The plan doc speccd `OriginHash([u8; 32])` — 32-byte BLAKE3
/// — but the substrate's chain identity is a `u64` derived
/// from the publisher's identity, and the substrate's
/// `causal:<hex16>` capability advertisement encodes that
/// `u64` as 16 lowercase hex chars. MeshDB matches the
/// substrate so the planner can look up holders against real
/// `causal:` tags without a width-translation step. The plan
/// doc is the spec-vs-reality outlier; Phase B reconciles
/// here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ChainRef {
    /// Direct addressing by a chain's `u64` origin hash. The
    /// planner looks up holders via the capability index
    /// without any discovery step.
    OriginHash(u64),
    /// Metadata-tag-driven discovery — e.g. "all chains with
    /// `intent:ml-training`". The planner resolves the
    /// predicate to concrete origin hashes via
    /// `CapabilityQuery::filter` at plan time. Time-
    /// bounded; resolution is part of the plan output.
    Discovered(PredicateWire),
}

/// Newtype around the substrate's seq counter for stronger
/// type checking. The plan uses `SeqNum` ubiquitously; we
/// alias to `u64` so existing call sites that pass `0` /
/// `1_000_000` etc. work without ceremony.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SeqNum(pub u64);

impl From<u64> for SeqNum {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl From<SeqNum> for u64 {
    fn from(value: SeqNum) -> Self {
        value.0
    }
}

/// Inner / outer join shape for [`QueryV1::Join`]. Mirrors the
/// standard SQL semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoinKind {
    /// Emit only rows where both sides have a match.
    Inner,
    /// Emit every left-side row; right is nullable when no
    /// match.
    LeftOuter,
    /// Emit every right-side row; left is nullable when no
    /// match.
    RightOuter,
    /// Emit every row from either side; the other is nullable
    /// when no match.
    FullOuter,
}

/// Correlation-key extraction for a join. Each side names the
/// expression that produces the join key; the executor
/// hash-keys on the resulting value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JoinKey {
    /// Expression evaluated against the left-side row to
    /// produce the join key.
    pub left_field: Expr,
    /// Expression evaluated against the right-side row.
    pub right_field: Expr,
}

/// Aggregate function applied to grouped rows. Per locked
/// decision #3, sketch-backed variants (`DistinctCount` /
/// `Percentile`) use canonical encodings (HLL p=14,
/// T-Digest compression=100) so cross-node merges are
/// deterministic.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum AggregateFn {
    /// Count of rows in the group.
    Count,
    /// Sum of `field` across rows.
    Sum {
        /// Field whose values are summed.
        field: Expr,
    },
    /// Arithmetic mean of `field` across rows.
    Avg {
        /// Field whose values are averaged.
        field: Expr,
    },
    /// Minimum of `field` across rows.
    Min {
        /// Field whose values are compared.
        field: Expr,
    },
    /// Maximum of `field` across rows.
    Max {
        /// Field whose values are compared.
        field: Expr,
    },
    /// Approximate distinct count via HyperLogLog. Per locked
    /// decision #3, `p = 14` (16 KB sketch, ±0.81 % error)
    /// is the canonical parameter. Cross-node merges are
    /// guaranteed deterministic.
    DistinctCountHll {
        /// Field whose distinct values are counted.
        field: Expr,
    },
    /// Exact distinct count. Bounded by the executor's
    /// per-group memory budget; falls back to
    /// `MeshError::QueryBudgetExceeded` past the threshold.
    DistinctCountExact {
        /// Field whose distinct values are counted.
        field: Expr,
    },
    /// Approximate percentile via T-Digest. Per locked
    /// decision #3, `compression = 100` is the canonical
    /// parameter (compact sketch, ±0.5 % on quantiles).
    PercentileTDigest {
        /// Field whose values are summarized.
        field: Expr,
        /// Target percentile in `[0.0, 1.0]` (e.g. `0.99`).
        p: f64,
    },
    /// Exact percentile. Sorts every value in the group and
    /// picks the nearest-rank quantile. Bounded by the
    /// executor's per-group memory budget; the approximate
    /// `PercentileTDigest` ships once a consumer's data volume
    /// justifies the fixed-memory tradeoff.
    PercentileExact {
        /// Field whose values are summarized.
        field: Expr,
        /// Target percentile in `[0.0, 1.0]` (e.g. `0.99`).
        p: f64,
    },
}

/// Row expression — a path / column reference / literal /
/// simple arithmetic. Reused by `JoinKey`, `Aggregate`,
/// `Project`, and `OrderBy`.
///
/// Phase A ships the minimum shape: dotted-path field
/// references + string / numeric literals. Phase E grows
/// arithmetic + window functions as the executor needs them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Expr {
    /// Dotted-path field reference into a row — e.g.
    /// `"event.metadata.severity"`.
    Field(String),
    /// String literal.
    LitString(String),
    /// Signed 64-bit integer literal.
    LitInt(i64),
    /// IEEE-754 double literal.
    LitFloat(f64),
    /// Boolean literal.
    LitBool(bool),
}

/// Order direction for [`OrderKey`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderDir {
    /// Ascending — smallest first.
    Asc,
    /// Descending — largest first.
    Desc,
}

/// Ordering key. The planner sorts lexicographically by the
/// list of keys — first key dominant; ties broken by the
/// next key.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OrderKey {
    /// Expression evaluated per row to produce the sort key.
    pub field: Expr,
    /// Sort direction.
    pub dir: OrderDir,
}

/// Row payload returned by a [`MeshQuery`]. Phase A ships the
/// minimal envelope — origin + seq + opaque bytes. Phase B
/// onwards introduces typed projections for specific query
/// shapes; rows still pass through this envelope for the wire
/// + cache + continuation paths.
///
/// Carries `Serialize + Deserialize` so the result-streaming
/// protocol can postcard-encode batches without needing
/// additional plumbing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResultRow {
    /// Origin hash of the chain this row came from. Substrate
    /// chain identity (`u64`), matching the `causal:<hex16>`
    /// capability-advertisement encoding.
    pub origin: u64,
    /// Sequence number within that chain.
    pub seq: SeqNum,
    /// Opaque payload bytes (typically the event payload).
    pub payload: Vec<u8>,
}

/// Row-intrinsic group key for [`OperatorPlan::AggregateCount`].
/// Mirrors the shape of [`super::planner::JoinKeyMode`] but
/// materializes the actual value (not just the mode) so the
/// aggregate row can carry the group identifier verbatim.
///
/// [`OperatorPlan::AggregateCount`]: super::planner::OperatorPlan::AggregateCount
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GroupKey {
    /// Grouped by [`ResultRow::origin`].
    Origin(u64),
    /// Grouped by [`ResultRow::seq`].
    Seq(SeqNum),
    /// Grouped by the `(origin, seq)` tuple.
    OriginSeq {
        /// Chain origin.
        origin: u64,
        /// Seq within that chain.
        seq: SeqNum,
    },
}

/// Aggregate-result envelope. The executor postcard-encodes one
/// of these into each aggregate output [`ResultRow`]'s
/// `payload` (with `origin = 0` and `seq = SeqNum(0)` as
/// sentinel-row markers; the group identifier lives in the
/// `group` field, not the row metadata, so the wire shape is
/// uniform across grouped + ungrouped queries).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AggregateRowPayload {
    /// Group identifier. `None` when the query had no
    /// `group_by` (single-bucket aggregate).
    pub group: Option<GroupKey>,
    /// Phase E-1 ships `Count` only; future aggregate functions
    /// (`Sum`, `Avg`, `DistinctCountHll`, `PercentileTDigest`)
    /// land via additional variants on this enum.
    pub value: AggregateValue,
}

/// Computed aggregate value. Phase E-1 shipped `Count`; Phase
/// E-3 adds `Sum` and `Avg`; sketches (`DistinctCount`,
/// `Percentile`) land in Phase E-4.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum AggregateValue {
    /// Row count within the group.
    Count(u64),
    /// Sum of a numeric field across rows in the group. Rows
    /// whose field is missing / non-numeric contribute `0.0`.
    Sum(f64),
    /// Arithmetic mean of a numeric field across rows in the
    /// group. Rows whose field is missing / non-numeric are
    /// excluded from both numerator and denominator (so a
    /// group with no numeric rows yields a group with an
    /// empty `Avg` — surfaced via the `Avg(None)` variant).
    Avg(Option<f64>),
    /// Minimum of a numeric field across rows in the group.
    /// `None` when the group has no numeric rows.
    Min(Option<f64>),
    /// Maximum of a numeric field across rows in the group.
    /// `None` when the group has no numeric rows.
    Max(Option<f64>),
    /// Exact distinct count over a row-intrinsic / JSON field.
    /// Counts distinct **string projections** of the leaf value
    /// (since `f64` doesn't have `Eq`, we project numerics to
    /// their canonical string form). Rows whose field is
    /// missing are skipped.
    DistinctCount(u64),
    /// Nearest-rank exact percentile over a numeric field.
    /// `None` when the group has no numeric rows.
    Percentile(Option<f64>),
}

/// Phase E-4 numeric reduction kind shared by Min / Max /
/// Percentile. Kept separate from
/// [`NumericAggregateKind`] (Sum / Avg) so the executor's
/// match arms stay narrow.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum NumericReductionKind {
    /// Minimum value in the group.
    Min,
    /// Maximum value in the group.
    Max,
    /// Nearest-rank percentile at `p ∈ [0.0, 1.0]`.
    Percentile {
        /// Target percentile in `[0.0, 1.0]`.
        p: f64,
    },
}

/// Phase E-3 numeric aggregate kind. Marks which function the
/// executor applies when materializing
/// [`super::planner::OperatorPlan::AggregateNumeric`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NumericAggregateKind {
    /// Arithmetic sum.
    Sum,
    /// Arithmetic mean.
    Avg,
}

/// Join-result envelope. The executor postcard-encodes one of
/// these into each joined [`ResultRow`]'s `payload` (with
/// `origin = 0` and `seq = SeqNum(0)` as the sentinel-row
/// markers). Callers consuming a Join operator's stream decode
/// the payload to recover the original `(left, right)` rows.
///
/// Both sides are `Option` to accommodate the four
/// [`JoinKind`]s:
///
/// - `Inner`: both `Some`.
/// - `LeftOuter`: left always `Some`, right `Some` on match
///   and `None` on miss.
/// - `RightOuter`: symmetric — right always `Some`, left
///   `Some` on match and `None` on miss.
/// - `FullOuter`: matched pairs have both `Some`; unmatched
///   rows from either side have the missing side `None`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JoinedRowPayload {
    /// Left-side row, if present.
    pub left: Option<ResultRow>,
    /// Right-side row, if present.
    pub right: Option<ResultRow>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn small_query() -> MeshQuery {
        MeshQuery::V1(QueryV1::Latest {
            origin: ChainRef::OriginHash(0xABAB_ABAB_ABAB_ABAB),
        })
    }

    fn between_query() -> MeshQuery {
        MeshQuery::V1(QueryV1::Between {
            origin: ChainRef::OriginHash(0x4242_4242_4242_4242),
            start: SeqNum(0),
            end: SeqNum(12345),
        })
    }

    /// Build a non-trivial nested query that touches most of
    /// the composable variants WITHOUT `Filter` /
    /// `ChainRef::Discovered`. Those two carry `PredicateWire`
    /// which is `#[serde(tag = "kind")]` — an internally-
    /// tagged enum, which postcard does not support
    /// (`postcard::Error::WontImplement` at decode time).
    /// JSON round-trips fine; postcard requires a wire-side
    /// predicate representation that bypasses serde's
    /// internally-tagged shape. Tracking item: ship a
    /// postcard-friendly predicate IR alongside `PredicateWire`
    /// when Phase E (predicate push-down) activates.
    fn complex_query_postcardable() -> MeshQuery {
        // Aggregate(Between, group_by=[Field], Count) — no
        // predicate inside, so postcard handles it cleanly.
        MeshQuery::V1(QueryV1::Aggregate {
            inner: Box::new(between_query()),
            group_by: vec![Expr::Field("operator_id".to_string())],
            agg_fn: AggregateFn::Count,
        })
    }

    /// Build a complex query that DOES include `Filter` so
    /// the JSON round-trip exercises the full surface.
    fn complex_query_with_filter() -> MeshQuery {
        use crate::adapter::net::behavior::predicate::Predicate;
        use crate::adapter::net::behavior::tag::{TagKey, TaxonomyAxis};
        let predicate = Predicate::Exists {
            key: TagKey {
                axis: TaxonomyAxis::Dataforts,
                key: "blob.storage".to_string(),
            },
        };
        let filter = MeshQuery::V1(QueryV1::Filter {
            inner: Box::new(between_query()),
            predicate: predicate.to_wire(),
        });
        MeshQuery::V1(QueryV1::Aggregate {
            inner: Box::new(filter),
            group_by: vec![Expr::Field("operator_id".to_string())],
            agg_fn: AggregateFn::Count,
        })
    }

    #[test]
    fn meshquery_round_trips_through_postcard() {
        // The wire form per locked decision #1 — postcard
        // encodes the version tag + the inner variant. The
        // complex case skips `Filter` because `PredicateWire`
        // is internally-tagged (see `complex_query_postcardable`
        // doc-comment); JSON exercises the full surface
        // separately.
        for q in [
            small_query(),
            between_query(),
            complex_query_postcardable(),
        ] {
            let bytes = postcard::to_allocvec(&q).expect("encode");
            let decoded: MeshQuery = postcard::from_bytes(&bytes).expect("decode");
            assert_eq!(decoded, q);
        }
    }

    #[test]
    fn meshquery_round_trips_through_json() {
        // The debug / FFI form per locked decision #5 —
        // serde_json is the canonical text encoding.
        // Exercises `Filter` + `PredicateWire` (which only
        // round-trips through JSON today).
        for q in [
            small_query(),
            between_query(),
            complex_query_postcardable(),
            complex_query_with_filter(),
        ] {
            let s = serde_json::to_string(&q).expect("encode");
            let decoded: MeshQuery = serde_json::from_str(&s).expect("decode");
            assert_eq!(decoded, q);
        }
    }

    #[test]
    fn version_tag_visible_in_json() {
        // Pin the JSON wire shape so external tools (audit
        // logs, debug dumps, query introspection) can rely on
        // the explicit version discriminant. The serde default
        // tag for an enum variant is the variant name, so
        // `MeshQuery::V1(...)` becomes a `{"V1": {...}}`
        // wrapper.
        let body = serde_json::to_string(&small_query()).unwrap();
        assert!(
            body.contains("\"V1\""),
            "JSON form must carry the V1 discriminant; got: {body}"
        );
    }

    #[test]
    fn seq_num_from_u64_round_trips() {
        let s: SeqNum = 42u64.into();
        let back: u64 = s.into();
        assert_eq!(back, 42);
    }

    #[test]
    fn chainref_originhash_round_trips() {
        let r = ChainRef::OriginHash(0xCDCD_CDCD_CDCD_CDCD);
        let bytes = postcard::to_allocvec(&r).unwrap();
        let back: ChainRef = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn aggregatefn_distinct_hll_round_trips() {
        // Sketch-backed aggregates carry no inline state in
        // the AST — the executor materializes the sketch.
        // Pin the variant shape so phase E + the wire stay in
        // sync.
        let f = AggregateFn::DistinctCountHll {
            field: Expr::Field("user_id".to_string()),
        };
        let bytes = postcard::to_allocvec(&f).unwrap();
        let back: AggregateFn = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, f);
    }

    #[test]
    fn aggregatefn_percentile_round_trips_with_p() {
        // p is f64; pin that the wire form preserves it.
        let f = AggregateFn::PercentileTDigest {
            field: Expr::Field("latency_ms".to_string()),
            p: 0.99,
        };
        let bytes = postcard::to_allocvec(&f).unwrap();
        let back: AggregateFn = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, f);
    }

    #[test]
    fn join_round_trips_with_watermark() {
        // Watermark is `Duration`; postcard encodes it as
        // (secs, nanos). Pin that the default 5s round-trips.
        let q = MeshQuery::V1(QueryV1::Join {
            left: Box::new(small_query()),
            right: Box::new(between_query()),
            on: JoinKey {
                left_field: Expr::Field("request_id".to_string()),
                right_field: Expr::Field("request_id".to_string()),
            },
            kind: JoinKind::Inner,
            watermark: Duration::from_secs(5),
        });
        let bytes = postcard::to_allocvec(&q).unwrap();
        let back: MeshQuery = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, q);
    }

    #[test]
    fn resultrow_round_trips_through_postcard() {
        let row = ResultRow {
            origin: 0x7777_7777_7777_7777,
            seq: SeqNum(1024),
            payload: b"the bytes".to_vec(),
        };
        let bytes = postcard::to_allocvec(&row).unwrap();
        let back: ResultRow = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back, row);
    }
}
