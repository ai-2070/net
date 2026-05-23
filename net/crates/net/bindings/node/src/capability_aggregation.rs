// `#[napi]` exports to JS leave items "unused" from Rust's POV, so
// clippy's dead-code analysis doesn't apply to this module. Suppress
// at file scope.
#![allow(dead_code)]

//! NAPI surface for the Phase 6c capability-aggregation API.
//!
//! The Rust core exposes
//! [`Fold::aggregate`](::net::adapter::net::behavior::fold::Fold::aggregate)
//! and
//! [`Fold::capacity_ranking`](::net::adapter::net::behavior::fold::Fold::capacity_ranking)
//! against `Fold<CapabilityFold>`. This module wires those through to
//! JS as `capabilityAggregate(matcher, groupBy, aggregation)` and
//! `capabilityCapacityRanking(query, rttMap)`.
//!
//! Wire shape per the plan: `TagMatcher` / `GroupBy` / `Aggregation`
//! cross the FFI boundary as JSON-encoded tagged unions because
//! napi-rs can't express tagged-union enums natively. The TS SDK
//! ships ergonomic constructors that wrap the JSON encoding.
//!
//! For the RTT lookup we accept a materialized map (`Map<bigint,
//! number>` on the JS side, projected to a `HashMap<NodeId, u32>`
//! here). The plan flags a `ThreadsafeFunction` closure variant as
//! the natural shape for TS — adding it is a follow-up; the map
//! variant matches what operators typically have cached from the
//! proximity graph anyway.

use std::collections::HashMap;

use napi::bindgen_prelude::*;
use napi_derive::napi;

use net::adapter::net::behavior::fold::CapabilityFold;
use net::adapter::net::behavior::fold::{
    Aggregation, CapacityQuery, CapacityRow, Fold, GroupBy, TagMatcher,
};

/// One row of an `aggregate` result. Mirrors the
/// `Vec<(String, u64)>` shape the Rust core returns but JS-friendly
/// (BigInt for u64 values).
#[napi(object)]
pub struct AggregateRowJs {
    /// Bucket key (the stem / value / state-name / region / etc.).
    pub bucket: String,
    /// Numeric value the aggregation produced for this bucket.
    pub value: BigInt,
}

/// One row of a `capacity_ranking` result. Mirrors
/// [`CapacityRow`](::net::adapter::net::behavior::fold::CapacityRow).
#[napi(object)]
pub struct CapacityRowJs {
    /// Bucket key.
    pub bucket: String,
    /// Entries in `Idle` that pass the matcher + RTT gates.
    pub idle: BigInt,
    /// Entries in `Busy` that pass.
    pub busy: BigInt,
    /// Entries in `Reserved` that pass.
    pub reserved: BigInt,
    /// `idle + busy + reserved`. Faulty entries are excluded.
    pub available: BigInt,
    /// Sum of the `sumAxisKey` numeric tag across the bucket;
    /// `null` when no `sumAxisKey` was requested.
    pub summed_capacity: Option<BigInt>,
}

/// JSON entry of the materialized RTT map. Operators pass a
/// `[{ nodeId: bigint, rttMs: number }, ...]` array as the second
/// argument to `capabilityCapacityRanking`; we project it to a
/// `HashMap<NodeId, u32>` once per call.
#[napi(object)]
pub struct RttEntryJs {
    /// Publisher `node_id` the RTT applies to.
    pub node_id: BigInt,
    /// RTT in milliseconds.
    pub rtt_ms: u32,
}

/// Run [`Fold::aggregate`] against the live capability fold.
///
/// Arguments are JSON-encoded tagged unions matching the Rust core's
/// `TagMatcher`, `GroupBy`, and `Aggregation` shapes (see
/// `MULTIFOLD_PHASE_6C_CAPACITY_AGGREGATION.md` §"Core API"). The
/// TS SDK builds the JSON; pass-through callers can construct it
/// manually.
///
/// `matcher_json = null` means "no pre-filter" (every entry walked).
pub(crate) fn aggregate(
    fold: &Fold<CapabilityFold>,
    matcher_json: Option<String>,
    group_by_json: String,
    aggregation_json: String,
) -> Result<Vec<AggregateRowJs>> {
    let matcher: Option<TagMatcher> = match matcher_json {
        Some(s) => Some(
            serde_json::from_str(&s)
                .map_err(|e| Error::from_reason(format!("matcher JSON: {e}")))?,
        ),
        None => None,
    };
    let group_by: GroupBy = serde_json::from_str(&group_by_json)
        .map_err(|e| Error::from_reason(format!("groupBy JSON: {e}")))?;
    let agg: Aggregation = serde_json::from_str(&aggregation_json)
        .map_err(|e| Error::from_reason(format!("aggregation JSON: {e}")))?;

    let rows = fold.aggregate(matcher, group_by, agg);
    Ok(rows
        .into_iter()
        .map(|(bucket, value)| AggregateRowJs {
            bucket,
            value: BigInt::from(value),
        })
        .collect())
}

/// Run [`Fold::capacity_ranking`] against the live capability fold.
///
/// `query_json` is a JSON-encoded [`CapacityQuery`]. `rtt_entries`
/// is the materialized RTT map; `null`/empty disables the RTT
/// filter regardless of `query.max_rtt_ms`. (Per the plan, the RTT
/// closure variant — `ThreadsafeFunction<u64, u32>` — is a follow-
/// up; the materialized-map shape matches the Go / C wrappers and
/// the value operators typically pull from `ProximityGraph`.)
pub(crate) fn capacity_ranking(
    fold: &Fold<CapabilityFold>,
    query_json: String,
    rtt_entries: Option<Vec<RttEntryJs>>,
) -> Result<Vec<CapacityRowJs>> {
    let query: CapacityQuery = serde_json::from_str(&query_json)
        .map_err(|e| Error::from_reason(format!("CapacityQuery JSON: {e}")))?;

    let rtt_map: HashMap<u64, u32> = rtt_entries
        .map(|entries| {
            entries
                .into_iter()
                .map(|e| {
                    let (_signed, words, _lossless) = e.node_id.get_u64();
                    (words, e.rtt_ms)
                })
                .collect()
        })
        .unwrap_or_default();

    let rows: Vec<CapacityRow> =
        fold.capacity_ranking(query, |node_id| rtt_map.get(&node_id).copied());
    Ok(rows
        .into_iter()
        .map(|r| CapacityRowJs {
            bucket: r.bucket,
            idle: BigInt::from(r.idle),
            busy: BigInt::from(r.busy),
            reserved: BigInt::from(r.reserved),
            available: BigInt::from(r.available),
            summed_capacity: r.summed_capacity.map(BigInt::from),
        })
        .collect())
}
