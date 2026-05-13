//! MeshDB wire protocol — request / response envelopes for
//! cross-node query execution.
//!
//! Phase B-3 lands the wire format. Phase B-4 plugs it into the
//! mesh's subprotocol dispatch + a `FederatedMeshQueryExecutor`
//! that fans out atomic operators to remote `target_nodes`.
//!
//! # Subprotocol slot
//!
//! [`SUBPROTOCOL_MESHDB`] (`0x0F00`) is the next slot after the
//! existing 0x0E00 (RedEX) per the subprotocol numbering in
//! `subprotocol/mod.rs`.
//!
//! # Envelope shape
//!
//! The wire is request / streaming-response — mirrors nRPC's
//! streaming pattern (see `cortex/rpc.rs`). One [`MeshDbRequest`]
//! flows caller → executor; zero or more [`MeshDbResponse`]
//! messages flow back. End-of-stream is signalled by either
//! [`MeshDbResponse::End`] or [`MeshDbResponse::Error`]; a
//! [`MeshDbResponse::Batch`] with `r#final = true` is a valid
//! coalesced "last batch + end" optimisation.
//!
//! All envelopes carry a `call_id: u64` so an out-of-band
//! [`MeshDbRequest::Cancel`] can be addressed to the right
//! in-flight call. `call_id` is opaque to the wire — the caller
//! assigns it.
//!
//! # Serialization
//!
//! Postcard is the canonical wire codec (matches the rest of
//! the substrate). The envelopes are also JSON-round-trippable
//! for tooling. Locked decision #1 (AST stability) implies that
//! response payloads riding inside [`ResultBatch::rows`] stay
//! postcard-compatible — the [`ResultRow`] type is `#[derive]`
//! Serialize so the wire format is stable.

use serde::{Deserialize, Serialize};

use super::error::MeshError;
use super::planner::ExecutionPlan;
use super::query::ResultRow;

/// Subprotocol identifier for MeshDB query traffic. Sits in the
/// next free slot after RedEX replication (`0x0E00`).
pub const SUBPROTOCOL_MESHDB: u16 = 0x0F00;

/// Opaque continuation token. Returned by an executor that
/// surfaced [`MeshError::PartialResult`]; the caller hands it
/// back via [`MeshDbRequest::Resume`] to pick up where the
/// previous call left off.
///
/// The bytes are executor-private — callers MUST treat them
/// as opaque. The wrapper exists to keep the wire type
/// distinct from "any old `Vec<u8>`" in API signatures.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContinuationToken(pub Vec<u8>);

impl ContinuationToken {
    /// Construct from raw bytes.
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Borrow the underlying bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Whether the token carries any state. An empty
    /// continuation token is conventionally "start from the
    /// beginning" — useful for the initial call.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// One chunk of rows from a streaming response.
///
/// `r#final = true` signals that this is the last batch in
/// the stream — the executor may optionally elide the trailing
/// [`MeshDbResponse::End`] when piggybacking is convenient.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResultBatch {
    /// Rows in this batch. Empty Vec is legal (e.g. a final
    /// batch that just signals end-of-stream).
    pub rows: Vec<ResultRow>,
    /// Whether this is the last batch in the response stream.
    /// `r#` escapes the Rust keyword.
    pub r#final: bool,
}

impl ResultBatch {
    /// Construct a non-terminal batch.
    pub fn chunk(rows: Vec<ResultRow>) -> Self {
        Self {
            rows,
            r#final: false,
        }
    }

    /// Construct a terminal batch (optionally empty).
    pub fn last(rows: Vec<ResultRow>) -> Self {
        Self {
            rows,
            r#final: true,
        }
    }
}

/// Caller → executor request.
///
/// Three shapes:
/// - [`Execute`] — start a new query.
/// - [`Resume`] — continue from a previously-surfaced
///   [`ContinuationToken`].
/// - [`Cancel`] — request cancellation of an in-flight call.
///   Empty payload beyond the `call_id`, matching nRPC's
///   cancellation convention.
///
/// `#[non_exhaustive]` so phases C–F can add variants
/// (e.g. a `Subscribe` for continuous queries) without
/// breaking source-side users.
///
/// [`Execute`]: MeshDbRequest::Execute
/// [`Resume`]: MeshDbRequest::Resume
/// [`Cancel`]: MeshDbRequest::Cancel
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum MeshDbRequest {
    /// Start a new query. The executor responds with a stream
    /// of [`MeshDbResponse`] messages all carrying the same
    /// `call_id`.
    Execute {
        /// Caller-assigned id. Must be unique per (caller,
        /// executor) pair while in-flight.
        call_id: u64,
        /// Plan to execute.
        plan: ExecutionPlan,
    },
    /// Resume a previously-paused query. The executor either
    /// continues streaming rows or surfaces a fresh
    /// [`MeshDbResponse::Error`].
    Resume {
        /// Caller-assigned id. May reuse a finished call's id;
        /// the executor disambiguates by the token, not the id.
        call_id: u64,
        /// Continuation token returned by the prior call's
        /// `PartialResult`.
        token: ContinuationToken,
    },
    /// Cancel an in-flight call. Matches nRPC's cooperative
    /// cancellation pattern: the executor flips its handle's
    /// cancel flag; the next row boundary terminates the
    /// stream with [`MeshError::QueryCancelled`] (delivered as
    /// [`MeshDbResponse::Error`]).
    Cancel {
        /// The `call_id` to cancel. No-op if no matching call
        /// is in flight.
        call_id: u64,
    },
}

/// Executor → caller streaming response.
///
/// Per `call_id`, the executor emits zero or more
/// [`Batch`] messages followed by exactly one terminal
/// ([`End`] or [`Error`]) — unless the terminal batch is
/// coalesced via [`ResultBatch::last`].
///
/// `#[non_exhaustive]` so phases C–F can add variants
/// (e.g. a `Progress` heartbeat for long-running queries).
///
/// [`Batch`]: MeshDbResponse::Batch
/// [`End`]: MeshDbResponse::End
/// [`Error`]: MeshDbResponse::Error
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum MeshDbResponse {
    /// One chunk of rows. May be the last (see the `final`
    /// field on [`ResultBatch`]).
    Batch {
        /// Matches the originating request's `call_id`.
        call_id: u64,
        /// The chunk.
        batch: ResultBatch,
    },
    /// Clean end of the response stream.
    End {
        /// Matches the originating request's `call_id`.
        call_id: u64,
    },
    /// Terminal error. Carries the full [`MeshError`] so the
    /// caller can decide whether to resume (via the
    /// continuation token in `PartialResult`) or fail.
    Error {
        /// Matches the originating request's `call_id`.
        call_id: u64,
        /// The error.
        error: MeshError,
    },
}

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use super::super::query::SeqNum;
    use super::*;

    fn sample_row() -> ResultRow {
        ResultRow {
            origin: 0xABCD_EF01_2345_6789,
            seq: SeqNum(42),
            payload: b"hello".to_vec(),
        }
    }

    fn sample_plan() -> ExecutionPlan {
        use super::super::planner::{CostEstimate, OperatorNode, OperatorPlan};
        ExecutionPlan {
            root: OperatorNode {
                operator: OperatorPlan::LatestRead {
                    origin: 0xABCD_EF01_2345_6789,
                },
                target_nodes: vec![0xDEAD, 0xBEEF],
                cost: CostEstimate::default(),
            },
            total_cost: CostEstimate::default(),
        }
    }

    #[test]
    fn subprotocol_slot_is_stable() {
        // Pinned. Bumping this is a wire-incompat change and
        // requires a Phase note in MESHDB_PLAN.md.
        assert_eq!(SUBPROTOCOL_MESHDB, 0x0F00);
    }

    #[test]
    fn continuation_token_round_trips_through_postcard() {
        let t = ContinuationToken::new(vec![1, 2, 3, 4, 5]);
        let bytes = postcard::to_allocvec(&t).expect("encode");
        let decoded: ContinuationToken = postcard::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded, t);
        assert_eq!(decoded.as_bytes(), &[1, 2, 3, 4, 5]);
        assert!(!decoded.is_empty());
    }

    #[test]
    fn continuation_token_empty_round_trips() {
        let t = ContinuationToken::default();
        assert!(t.is_empty());
        let bytes = postcard::to_allocvec(&t).expect("encode");
        let decoded: ContinuationToken = postcard::from_bytes(&bytes).expect("decode");
        assert!(decoded.is_empty());
    }

    #[test]
    fn result_batch_chunk_vs_last_flags() {
        let chunk = ResultBatch::chunk(vec![sample_row()]);
        assert!(!chunk.r#final);
        let last = ResultBatch::last(vec![sample_row()]);
        assert!(last.r#final);
        // Empty terminal batch is legal.
        let empty_last = ResultBatch::last(vec![]);
        assert!(empty_last.r#final);
        assert!(empty_last.rows.is_empty());
    }

    #[test]
    fn result_batch_round_trips_through_postcard() {
        let b = ResultBatch::chunk(vec![sample_row(), sample_row()]);
        let bytes = postcard::to_allocvec(&b).expect("encode");
        let decoded: ResultBatch = postcard::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded, b);
    }

    #[test]
    fn request_execute_round_trips() {
        let req = MeshDbRequest::Execute {
            call_id: 0x1234_5678_9ABC_DEF0,
            plan: sample_plan(),
        };
        let bytes = postcard::to_allocvec(&req).expect("encode");
        let decoded: MeshDbRequest = postcard::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded, req);
    }

    #[test]
    fn request_resume_round_trips() {
        let req = MeshDbRequest::Resume {
            call_id: 7,
            token: ContinuationToken::new(b"opaque-cursor-bytes".to_vec()),
        };
        let bytes = postcard::to_allocvec(&req).expect("encode");
        let decoded: MeshDbRequest = postcard::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded, req);
    }

    #[test]
    fn request_cancel_round_trips_with_no_extra_payload() {
        // Cancel mirrors nRPC: empty beyond the call_id.
        let req = MeshDbRequest::Cancel { call_id: 99 };
        let bytes = postcard::to_allocvec(&req).expect("encode");
        let decoded: MeshDbRequest = postcard::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded, req);
    }

    #[test]
    fn response_batch_round_trips() {
        let resp = MeshDbResponse::Batch {
            call_id: 1,
            batch: ResultBatch::chunk(vec![sample_row()]),
        };
        let bytes = postcard::to_allocvec(&resp).expect("encode");
        let decoded: MeshDbResponse = postcard::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded, resp);
    }

    #[test]
    fn response_end_round_trips() {
        let resp = MeshDbResponse::End { call_id: 1 };
        let bytes = postcard::to_allocvec(&resp).expect("encode");
        let decoded: MeshDbResponse = postcard::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded, resp);
    }

    #[test]
    fn response_error_round_trips_all_mesh_error_variants() {
        // Pin the wire-friendliness of every MeshError variant
        // that the executor can surface. If a future variant
        // breaks postcard, this test fails loudly.
        let cases = vec![
            MeshError::HistoricalRangeUnavailable {
                origin: 0xAA,
                requested: SeqNum(10)..SeqNum(20),
                available: vec![SeqNum(0)..SeqNum(5)],
            },
            MeshError::LineageMaxDepthExceeded {
                origin: 0xBB,
                depth: 16,
            },
            MeshError::LineageCycleDetected {
                origin: 0xCC,
                cycle: vec![0xCC, 0xDD, 0xCC],
            },
            MeshError::JoinMemoryExceeded {
                strategy: "broadcast".to_string(),
                threshold_bytes: 1 << 20,
            },
            MeshError::QueryBudgetExceeded {
                metric: super::super::error::BudgetMetric::MaxRows,
                used: 1001,
                limit: 1000,
            },
            MeshError::PartialResult {
                rows: vec![sample_row()],
                continuation: b"cursor".to_vec(),
                reason: "watermark expired".to_string(),
            },
            MeshError::PlannerError {
                detail: "test".to_string(),
            },
            MeshError::ExecutorError {
                node: 0xEE,
                detail: "boom".to_string(),
            },
            MeshError::NoCapableHolder {
                origin: 0xFF,
                requirement: "causal:abc".to_string(),
            },
            MeshError::QueryCancelled,
        ];
        for err in cases {
            let resp = MeshDbResponse::Error {
                call_id: 42,
                error: err,
            };
            let bytes = postcard::to_allocvec(&resp).expect("encode");
            let decoded: MeshDbResponse = postcard::from_bytes(&bytes).expect("decode");
            // Can't assert equality on Range<SeqNum> via
            // PartialEq derive? It does derive PartialEq on
            // MeshError. Smoke-test via Debug equality.
            assert_eq!(format!("{decoded:?}"), format!("{resp:?}"));
        }
    }

    #[test]
    fn full_session_round_trips_through_postcard() {
        // A realistic session: Execute, two Batches, an End.
        let session = vec![MeshDbRequest::Execute {
            call_id: 1,
            plan: sample_plan(),
        }];
        let responses = vec![
            MeshDbResponse::Batch {
                call_id: 1,
                batch: ResultBatch::chunk(vec![sample_row()]),
            },
            MeshDbResponse::Batch {
                call_id: 1,
                batch: ResultBatch::last(vec![sample_row()]),
            },
            MeshDbResponse::End { call_id: 1 },
        ];
        for r in &session {
            let bytes = postcard::to_allocvec(r).expect("encode");
            let _: MeshDbRequest = postcard::from_bytes(&bytes).expect("decode");
        }
        for r in &responses {
            let bytes = postcard::to_allocvec(r).expect("encode");
            let _: MeshDbResponse = postcard::from_bytes(&bytes).expect("decode");
        }
    }

    #[test]
    fn envelopes_round_trip_through_json() {
        // JSON is the tooling form per the plan. Just ensure
        // they all encode + decode round-trip without panic.
        let req = MeshDbRequest::Execute {
            call_id: 1,
            plan: sample_plan(),
        };
        let json = serde_json::to_string(&req).expect("json encode");
        let _: MeshDbRequest = serde_json::from_str(&json).expect("json decode");

        let resp = MeshDbResponse::Batch {
            call_id: 1,
            batch: ResultBatch::chunk(vec![sample_row()]),
        };
        let json = serde_json::to_string(&resp).expect("json encode");
        let _: MeshDbResponse = serde_json::from_str(&json).expect("json decode");
    }

    // Suppress unused-import warning when no error variant
    // actually pulls Range into scope.
    #[allow(dead_code)]
    fn _range_in_scope() -> Range<SeqNum> {
        SeqNum(0)..SeqNum(1)
    }
}
