//! `fold.query` RPC service — cross-subnet detail-on-demand
//! surface (Phase C of `SCALING_SUBNET_SPEC.md`).
//!
//! Receivers of a [`super::SummaryAnnouncement`] (via the
//! aggregator's summary-publish path) often want the next level
//! of detail without subscribing to the source subnet's full
//! detail channel — e.g. "summary says this subnet has 4 idle
//! GPUs; which specific publishers?". The query service exposes
//! a typed `query(kind, op)` RPC that the aggregator daemon
//! answers from its local fold state.
//!
//! # Wire shape
//!
//! Request / response are postcard-encoded — same convention as
//! the substrate's other RPC payloads
//! (`cortex/memories/adapter.rs`, `cortex/adapter.rs`, etc.).
//!
//! # Handler installation
//!
//! `MeshNode::serve_rpc(service_name, handler)` registers the
//! handler under a service name. The convention adopted here is
//! `fold.query` (matching the plan); operators that run multiple
//! aggregators on one node can supply distinct service names
//! per aggregator instance to avoid collisions.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use serde::{Deserialize, Serialize};

use super::summarizer::SummaryAnnouncement;
use super::AggregatorDaemon;
use crate::adapter::net::cortex::rpc::{
    RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
};

/// Canonical service name the aggregator registers `serve_rpc`
/// under. Clients construct `format!("{}.requests", FOLD_QUERY_SERVICE)`
/// (via the substrate's request-channel derivation) implicitly.
pub const FOLD_QUERY_SERVICE: &str = "fold.query";

/// Wire-shaped request. Postcard-encoded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoldQueryRequest {
    /// `FoldKind::KIND_ID` of the fold the client wants
    /// information about. Aggregators that aren't configured for
    /// this kind reply with [`FoldQueryError::UnknownKind`].
    pub kind: u16,
    /// Operation discriminator.
    pub op: FoldQueryOp,
}

/// Operations the query service supports today. Designed as an
/// open-coded enum so future ops (e.g. per-class detail, full
/// fold snapshots) can extend without breaking the wire shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FoldQueryOp {
    /// Return the aggregator's most recent batch of summaries
    /// for `kind`. Cheap — reads the daemon's in-memory ring.
    LatestSummary,
    /// Force the aggregator to summarize-now (synchronously) and
    /// return the fresh result. Used when staleness tolerance is
    /// tighter than the daemon's `summary_interval`.
    ///
    /// Operators should reach for `LatestSummary` first;
    /// `SummarizeNow` is the "I need a tight read RIGHT NOW"
    /// fallback. Both arms ultimately return the same shape.
    SummarizeNow,
}

/// Wire-shaped response. Postcard-encoded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FoldQueryResponse {
    /// Successful read. Includes the kind the client requested
    /// (echoed for diagnostic clarity) and the matching
    /// summaries.
    Summaries {
        /// Echo of the request's `kind` field.
        kind: u16,
        /// Per-subnet summaries the aggregator's summarizer
        /// produced. May be empty when the aggregator's source
        /// fold is empty.
        summaries: Vec<SummaryAnnouncement>,
    },
    /// Handler-level rejection. Aggregator-as-server doesn't
    /// distinguish transport-level errors here — those surface
    /// to the client as the substrate's `RpcError`.
    Error(FoldQueryError),
}

/// Handler-level error returned in [`FoldQueryResponse::Error`].
/// Distinct from `RpcHandlerError` so the wire payload doesn't
/// leak substrate-internal error shapes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FoldQueryError {
    /// `request.kind` doesn't match any of the daemon's
    /// configured `fold_kinds`.
    UnknownKind {
        /// Echo of the requested kind for client-side
        /// diagnostics.
        kind: u16,
    },
    /// Request payload failed to decode. Carries the postcard
    /// error message as a `String` so the wire type stays free
    /// of cross-crate error dependencies.
    DecodeFailed(String),
}

/// `RpcHandler` implementation that answers `fold.query`
/// requests from a hosted [`AggregatorDaemon`]. Construct via
/// [`AggregatorDaemon::query_handler`] and pass to
/// [`crate::adapter::net::MeshNode::serve_rpc`] under
/// [`FOLD_QUERY_SERVICE`].
pub struct FoldQueryHandler {
    aggregator: Arc<AggregatorDaemon>,
}

impl FoldQueryHandler {
    /// Wrap an aggregator daemon as an RPC handler.
    pub fn new(aggregator: Arc<AggregatorDaemon>) -> Self {
        Self { aggregator }
    }
}

#[async_trait]
impl RpcHandler for FoldQueryHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        let request: FoldQueryRequest = match postcard::from_bytes(&ctx.payload.body) {
            Ok(req) => req,
            Err(e) => {
                let response =
                    FoldQueryResponse::Error(FoldQueryError::DecodeFailed(e.to_string()));
                return Ok(encode_response(&response));
            }
        };
        let response = answer(&self.aggregator, &request);
        Ok(encode_response(&response))
    }
}

/// Pure-function answer logic, broken out for direct unit
/// testing without going through the RPC plumbing.
pub(crate) fn answer(
    aggregator: &Arc<AggregatorDaemon>,
    request: &FoldQueryRequest,
) -> FoldQueryResponse {
    let configured = aggregator.config().fold_kinds.contains(&request.kind);
    if !configured {
        return FoldQueryResponse::Error(FoldQueryError::UnknownKind {
            kind: request.kind,
        });
    }
    let summaries: Vec<SummaryAnnouncement> = match request.op {
        FoldQueryOp::LatestSummary => aggregator
            .latest_summaries()
            .into_iter()
            .filter(|s| s.fold_kind == request.kind)
            .collect(),
        FoldQueryOp::SummarizeNow => {
            // Run one fresh tick synchronously so the response
            // reflects the moment-of-call fold state, not the
            // last background-loop tick.
            let fresh: Vec<SummaryAnnouncement> = aggregator
                .tick_once()
                .into_iter()
                .filter(|s| s.fold_kind == request.kind)
                .collect();
            // If the tick was a no-op (buckets unchanged →
            // change-detection guard), fall back to the latest
            // buffer so the operator gets the most recent value.
            if fresh.is_empty() {
                aggregator
                    .latest_summaries()
                    .into_iter()
                    .filter(|s| s.fold_kind == request.kind)
                    .collect()
            } else {
                fresh
            }
        }
    };
    FoldQueryResponse::Summaries {
        kind: request.kind,
        summaries,
    }
}

fn encode_response(response: &FoldQueryResponse) -> RpcResponsePayload {
    // Postcard can fail on serializer-level OOM / IO; not expected
    // for these small payloads. Log a warning so an unexpected
    // encode failure shows up in operator output, then fall back
    // to an empty body — the client surfaces the empty decode as
    // a decode error and retries.
    let body = match postcard::to_allocvec(response) {
        Ok(b) => Bytes::from(b),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "aggregator: fold.query response encode failed; replying with empty body",
            );
            Bytes::new()
        }
    };
    RpcResponsePayload {
        status: RpcStatus::Ok,
        headers: Vec::new(),
        body,
    }
}

impl AggregatorDaemon {
    /// Build a [`FoldQueryHandler`] wrapping `self`. Pass the
    /// returned handler to
    /// [`crate::adapter::net::MeshNode::serve_rpc`] under
    /// [`FOLD_QUERY_SERVICE`] to expose the daemon to remote
    /// queriers.
    ///
    /// Phase-C minimum viable surface: the aggregator's own
    /// summaries are reachable via RPC. Future ops (per-class
    /// detail, full fold snapshots) extend `FoldQueryOp` without
    /// breaking this handler.
    pub fn query_handler(self: &Arc<Self>) -> FoldQueryHandler {
        FoldQueryHandler::new(self.clone())
    }

    /// One-call installation: build the query handler, register
    /// it on `mesh` under [`FOLD_QUERY_SERVICE`], and return the
    /// resulting `ServeHandle`. Dropping the handle tears down
    /// the registration.
    ///
    /// Operators that need to register under a non-default
    /// service name (e.g. running multiple aggregators on one
    /// node) call `mesh.serve_rpc(name, Arc::new(daemon.query_handler()))`
    /// directly with their chosen name.
    pub fn install_query_service(
        self: &Arc<Self>,
        mesh: &Arc<crate::adapter::net::MeshNode>,
    ) -> Result<crate::adapter::net::mesh_rpc::ServeHandle, crate::adapter::net::mesh_rpc::ServeError>
    {
        mesh.serve_rpc(FOLD_QUERY_SERVICE, Arc::new(self.query_handler()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::aggregator::AggregatorConfig;
    use crate::adapter::net::behavior::fold::capability::{CapabilityFold, CapabilityMembership};
    use crate::adapter::net::behavior::fold::wire::SignedAnnouncement;
    use crate::adapter::net::behavior::fold::{EnvelopeMeta, FoldKind, NodeState};
    use crate::adapter::net::identity::EntityKeypair;
    use crate::adapter::net::{MeshNode, MeshNodeConfig, SubnetId};
    use std::collections::BTreeMap;
    use std::net::SocketAddr;
    use std::time::Duration;

    async fn build_mesh() -> Arc<MeshNode> {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let cfg = MeshNodeConfig::new(addr, [0x17u8; 32]);
        Arc::new(
            MeshNode::new(EntityKeypair::generate(), cfg)
                .await
                .expect("MeshNode::new"),
        )
    }

    fn sign_cap(
        kp: &EntityKeypair,
        publisher: u64,
        class: u64,
        state: NodeState,
    ) -> SignedAnnouncement<CapabilityMembership> {
        SignedAnnouncement::sign(
            kp,
            CapabilityFold::KIND_ID,
            class,
            publisher,
            1,
            EnvelopeMeta::default(),
            CapabilityMembership {
                class_hash: class,
                tags: Vec::new(),
                hardware: None,
                state,
                region: None,
                price_quote: None,
                reflex_addr: None,
                allowed_nodes: Vec::new(),
                allowed_subnets: Vec::new(),
                allowed_groups: Vec::new(),
                metadata: BTreeMap::new(),
            },
        )
        .expect("sign")
    }

    async fn aggregator_with_idle_publisher() -> Arc<AggregatorDaemon> {
        let mesh = build_mesh().await;
        let kp = EntityKeypair::generate();
        let fold = mesh.capability_fold();
        fold.apply(sign_cap(&kp, 0xA, 1, NodeState::Idle)).unwrap();
        let cfg = AggregatorConfig::new(SubnetId::new(&[3]))
            .with_fold_kind(CapabilityFold::KIND_ID)
            .with_interval(Duration::from_secs(60));
        Arc::new(AggregatorDaemon::new(cfg, mesh).expect("new"))
    }

    #[tokio::test]
    async fn answer_returns_summaries_for_known_kind() {
        let agg = aggregator_with_idle_publisher().await;
        // Prime the latest-summaries buffer.
        agg.tick_once();

        let req = FoldQueryRequest {
            kind: CapabilityFold::KIND_ID,
            op: FoldQueryOp::LatestSummary,
        };
        let resp = answer(&agg, &req);
        match resp {
            FoldQueryResponse::Summaries { kind, summaries } => {
                assert_eq!(kind, CapabilityFold::KIND_ID);
                assert_eq!(summaries.len(), 1);
                let idle = summaries[0]
                    .buckets
                    .iter()
                    .find(|(n, _)| n == "idle")
                    .map(|(_, c)| *c)
                    .unwrap_or(0);
                assert_eq!(idle, 1);
            }
            other => panic!("expected Summaries, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn answer_rejects_unknown_kind() {
        let agg = aggregator_with_idle_publisher().await;
        let req = FoldQueryRequest {
            kind: 0xDEAD,
            op: FoldQueryOp::LatestSummary,
        };
        let resp = answer(&agg, &req);
        match resp {
            FoldQueryResponse::Error(FoldQueryError::UnknownKind { kind }) => {
                assert_eq!(kind, 0xDEAD);
            }
            other => panic!("expected UnknownKind, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn summarize_now_forces_a_fresh_tick_before_answering() {
        // Pin that `SummarizeNow` runs a tick (generation
        // advances) before reading the latest-summaries buffer.
        let agg = aggregator_with_idle_publisher().await;
        let gen_before = agg.generation();
        let req = FoldQueryRequest {
            kind: CapabilityFold::KIND_ID,
            op: FoldQueryOp::SummarizeNow,
        };
        let _ = answer(&agg, &req);
        assert_eq!(agg.generation(), gen_before + 1);
    }

    #[tokio::test]
    async fn summarize_now_falls_back_to_latest_when_tick_is_a_noop() {
        // Pin the no-op path: a second SummarizeNow against an
        // unchanged fold returns the cached latest entry rather
        // than an empty Vec — the change-detection guard in
        // tick_once produces no novel summary, and the RPC handler
        // falls back to the latest buffer.
        let agg = aggregator_with_idle_publisher().await;
        let req = FoldQueryRequest {
            kind: CapabilityFold::KIND_ID,
            op: FoldQueryOp::SummarizeNow,
        };
        let first = answer(&agg, &req);
        match first {
            FoldQueryResponse::Summaries { ref summaries, .. } => {
                assert_eq!(summaries.len(), 1, "first tick produces a novel summary");
            }
            other => panic!("expected Summaries, got {other:?}"),
        }
        // Second call — fold state unchanged → tick_once returns
        // empty → handler falls back to latest_summaries.
        let second = answer(&agg, &req);
        match second {
            FoldQueryResponse::Summaries { summaries, .. } => {
                assert_eq!(
                    summaries.len(),
                    1,
                    "no-op tick must still return the cached latest summary"
                );
            }
            other => panic!("expected Summaries, got {other:?}"),
        }
    }

    #[test]
    fn request_response_round_trips_through_postcard() {
        // Pin the wire encoding — the postcard variant tags are
        // load-bearing for cross-version compatibility. Round-trip
        // every shape the substrate uses.
        let req = FoldQueryRequest {
            kind: CapabilityFold::KIND_ID,
            op: FoldQueryOp::SummarizeNow,
        };
        let bytes = postcard::to_allocvec(&req).expect("encode");
        let back: FoldQueryRequest = postcard::from_bytes(&bytes).expect("decode");
        assert_eq!(back, req);

        let resp = FoldQueryResponse::Summaries {
            kind: 0x0001,
            summaries: vec![SummaryAnnouncement {
                source_subnet: SubnetId::new(&[3, 7]),
                fold_kind: 0x0001,
                generation: 42,
                buckets: vec![("idle".to_string(), 4), ("busy".to_string(), 1)],
            }],
        };
        let bytes = postcard::to_allocvec(&resp).expect("encode");
        let back: FoldQueryResponse = postcard::from_bytes(&bytes).expect("decode");
        assert_eq!(back, resp);

        let err = FoldQueryResponse::Error(FoldQueryError::UnknownKind { kind: 0xDEAD });
        let bytes = postcard::to_allocvec(&err).expect("encode");
        let back: FoldQueryResponse = postcard::from_bytes(&bytes).expect("decode");
        assert_eq!(back, err);
    }
}
