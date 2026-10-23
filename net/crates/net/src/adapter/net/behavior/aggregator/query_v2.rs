//! `fold.query.v2` and the daemon-level `fold.query` handlers.
//!
//! Slice Q2 of `docs/internal/plans/AGGREGATOR_MULTI_HOST_PLAN.md`
//! (decisions D0, D8, D9).
//!
//! # Why a v2
//!
//! v1 (`fold.query`, [`super::query_service`]) answers for one
//! [`AggregatorDaemon`](super::AggregatorDaemon). A daemon can host several groups, and v1's request
//! has no way to name one. Postcard is positional, and `postcard::from_bytes`
//! ignores trailing input. A field appended to v1 would be **silently
//! dropped** by an old server instead of refused. So changed shapes go to a
//! new service name, and v1 stays frozen (D9).
//!
//! # What v2 adds
//!
//! - **A group selector:** `group: None` answers from every group that
//!   aggregates the requested kind.
//! - **One [`SummaryRecord`] per summary.** It names its group and carries
//!   the stream metadata receivers need to order and age summaries (D3, D8).
//!   Those fields are `Option`s, filled in as the daemon gains the matching
//!   behavior, so the layout does not need a v3.
//! - **Strict decoding.** [`decode_strict`] rejects trailing bytes on both
//!   ends.
//! - **An access check (D0).** Queries see exactly what the summary
//!   channel's visibility would deliver. [`FoldQueryAccess`] applies
//!   `MeshNode::subnet_visible`, the same rule the channel fan-out uses.
//!   Operators are always admitted.
//!
//! # Frozen layouts
//!
//! The types below are wire contracts. Changing a field's order or type
//! changes the bytes. The golden-byte tests in `tests/aggregator_fold_query.rs`
//! pin them, and a change needs a new service name, not an edit.

use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::Instant;

use async_trait::async_trait;
use bytes::Bytes;
use parking_lot::RwLock;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::query_client::FoldQueryClient;
use super::query_service::{
    answer, FoldQueryError, FoldQueryOp, FoldQueryRequest, FoldQueryResponse,
};
use super::registry::AggregatorRegistry;
use super::registry_service::RegistryAdminPolicy;
use super::summarizer::SummaryAnnouncement;
use crate::adapter::net::cortex::rpc::{
    RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
};
use crate::adapter::net::mesh_rpc::{CallOptions, RpcError, ServeError, ServeHandle};
use crate::adapter::net::{MeshNode, SubnetId, Visibility};

/// Service name `fold.query.v2` is served under.
pub const FOLD_QUERY_V2_SERVICE: &str = "fold.query.v2";

/// Application status a daemon-level **v1** `fold.query` handler returns
/// when it refuses a caller. v1's body has no "not authorized" variant, and
/// its layout is frozen, so the refusal travels as an application-band
/// status instead (`RpcError::ServerError { status: 0x8001, .. }`).
pub const FOLD_QUERY_V1_NOT_AUTHORIZED: u16 = 0x8001;

/// `fold.query.v2` request. **Frozen layout.**
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoldQueryV2Request {
    /// `FoldKind::KIND_ID` to query.
    pub kind: u16,
    /// What to read.
    pub op: FoldQueryOp,
    /// Only this group, or `None` for every group aggregating `kind`.
    pub group: Option<String>,
}

/// One summary, with the group it came from and its stream metadata.
/// **Frozen layout.**
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryRecord {
    /// The registry group that produced the summary.
    pub group: String,
    /// The summary itself, in its frozen v1 shape (which carries the
    /// group's per-tick `generation`).
    pub summary: SummaryAnnouncement,
    /// How often the publisher republishes an unchanged summary, once it
    /// does (D8 bounded refresh). `None` until then: today a summary is
    /// republished only when it changes, so there is no interval to state.
    pub refresh_ms: Option<u32>,
    /// The publisher's persisted boot counter (D3). `None` until the
    /// daemon persists one.
    pub boot_epoch: Option<u64>,
    /// The stream's start sequence within the boot (D3). `None` until the
    /// daemon assigns one.
    pub stream_seq: Option<u32>,
}

/// `fold.query.v2` response. **Frozen layout.**
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FoldQueryV2Response {
    /// The matching summaries. May be empty when the groups have
    /// produced none yet.
    Summaries {
        /// Echo of the requested kind.
        kind: u16,
        /// One record per summary, grouped by registry group.
        records: Vec<SummaryRecord>,
    },
    /// The handler refused the request.
    Error(FoldQueryV2Error),
}

/// Handler-level refusals. **Frozen layout.**
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum FoldQueryV2Error {
    /// No queried group aggregates this kind.
    #[error("no group aggregates fold kind {kind:#06x}")]
    UnknownKind {
        /// Echo of the requested kind.
        kind: u16,
    },
    /// `request.group` names no group on this daemon.
    #[error("no group named {0:?}")]
    UnknownGroup(String),
    /// The caller is neither an operator nor inside the summary channel's
    /// visibility.
    #[error("not authorized to query this daemon's summaries")]
    NotAuthorized,
    /// The request did not decode, strictly.
    #[error("request did not decode: {0}")]
    DecodeFailed(String),
}

/// Why [`decode_strict`] refused its input.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StrictDecodeError {
    /// Postcard could not decode a value.
    #[error("{0}")]
    Codec(String),
    /// A value decoded, but bytes remained after it.
    #[error("{0} trailing byte(s) after a complete value")]
    TrailingBytes(usize),
}

/// Decode exactly one `T` from `bytes`, refusing any trailing input.
///
/// `postcard::from_bytes` accepts and ignores trailing bytes. That is how a
/// field appended to a v1 request would be silently dropped by an old
/// server (D9). Every v2 decode goes through this function instead.
pub fn decode_strict<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, StrictDecodeError> {
    let (value, rest) =
        postcard::take_from_bytes(bytes).map_err(|e| StrictDecodeError::Codec(e.to_string()))?;
    if !rest.is_empty() {
        return Err(StrictDecodeError::TrailingBytes(rest.len()));
    }
    Ok(value)
}

/// Who may query a daemon's summaries (D0).
#[derive(Debug, Clone)]
pub struct FoldQueryAccess {
    /// The summary channels' visibility. A caller is admitted when a
    /// publish on those channels would reach it.
    pub visibility: Visibility,
    /// Operators, who are always admitted (the registry's own policy).
    pub operators: RegistryAdminPolicy,
}

impl FoldQueryAccess {
    /// Admit `caller` (the authenticated session peer) if it is an operator
    /// or if `visibility` delivers from `local` to `caller_subnet`.
    ///
    /// `caller_subnet` is `None` when the mesh has no subnet recorded for the
    /// caller (no direct, signed announcement, or no policy). The fan-out
    /// rule then admits it only for `Global` visibility or a global
    /// publisher, and so does this.
    pub fn admits(&self, local: SubnetId, caller: u64, caller_subnet: Option<SubnetId>) -> bool {
        self.operators.permits(caller)
            || MeshNode::subnet_visible(local, caller_subnet, self.visibility, None)
    }

    fn check(&self, mesh: &MeshNode, ctx: &RpcContext, service: &'static str) -> bool {
        let caller = ctx.session_peer;
        let caller_subnet = mesh.peer_subnet(caller);
        let admitted = self.admits(mesh.local_subnet(), caller, caller_subnet);
        if !admitted {
            tracing::warn!(
                session_peer = format!("{caller:#x}"),
                caller_subnet = ?caller_subnet,
                local_subnet = %mesh.local_subnet(),
                visibility = ?self.visibility,
                service,
                "refused a fold query outside the summary channel's visibility",
            );
        }
        admitted
    }
}

/// Answer a v2 request from every matching group in `registry`.
pub(crate) async fn answer_v2(
    registry: &AggregatorRegistry,
    request: &FoldQueryV2Request,
) -> FoldQueryV2Response {
    let names = match &request.group {
        Some(group) if registry.get(group).is_none() => {
            return FoldQueryV2Response::Error(FoldQueryV2Error::UnknownGroup(group.clone()));
        }
        Some(group) => vec![group.clone()],
        None => registry.names(),
    };
    let v1 = FoldQueryRequest {
        kind: request.kind,
        op: request.op.clone(),
    };
    let mut records = Vec::new();
    let mut served = false;
    for name in names {
        let Some(entry) = registry.get(&name) else {
            continue; // unregistered since `names()`
        };
        let snap = entry.snapshot().await;
        // Replicas of one group share their daemon's fold, so any one
        // answers for the group.
        let Some(aggregator) = snap.replicas.first() else {
            continue;
        };
        if let FoldQueryResponse::Summaries { summaries, .. } = answer(aggregator, &v1) {
            served = true;
            records.extend(summaries.into_iter().map(|summary| SummaryRecord {
                group: name.clone(),
                summary,
                refresh_ms: None,
                boot_epoch: None,
                stream_seq: None,
            }));
        }
    }
    if !served {
        return FoldQueryV2Response::Error(FoldQueryV2Error::UnknownKind { kind: request.kind });
    }
    FoldQueryV2Response::Summaries {
        kind: request.kind,
        records,
    }
}

/// `fold.query.v2` handler over a daemon's whole registry.
pub struct FoldQueryV2Handler {
    // Weak: the mesh owns its served handlers, so a strong reference
    // here would be a cycle.
    mesh: Weak<MeshNode>,
    registry: Arc<AggregatorRegistry>,
    access: FoldQueryAccess,
}

#[async_trait]
impl RpcHandler for FoldQueryV2Handler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        let mesh = self
            .mesh
            .upgrade()
            .ok_or_else(|| RpcHandlerError::Internal("mesh is shutting down".into()))?;
        // Authorize before decoding: the body is attacker-controlled.
        if !self.access.check(&mesh, &ctx, FOLD_QUERY_V2_SERVICE) {
            return Ok(encode(&FoldQueryV2Response::Error(
                FoldQueryV2Error::NotAuthorized,
            )));
        }
        let request: FoldQueryV2Request = match decode_strict(&ctx.payload.body) {
            Ok(r) => r,
            Err(e) => {
                return Ok(encode(&FoldQueryV2Response::Error(
                    FoldQueryV2Error::DecodeFailed(e.to_string()),
                )))
            }
        };
        Ok(encode(&answer_v2(&self.registry, &request).await))
    }
}

/// Daemon-level **v1** `fold.query` handler: answers v1's frozen shape from
/// every group aggregating the requested kind. With one group per kind per
/// daemon (D0), that is at most one group. It applies the same access
/// check as v2, and refuses with [`FOLD_QUERY_V1_NOT_AUTHORIZED`].
pub struct FoldQueryRegistryV1Handler {
    mesh: Weak<MeshNode>,
    registry: Arc<AggregatorRegistry>,
    access: FoldQueryAccess,
}

#[async_trait]
impl RpcHandler for FoldQueryRegistryV1Handler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        let mesh = self
            .mesh
            .upgrade()
            .ok_or_else(|| RpcHandlerError::Internal("mesh is shutting down".into()))?;
        if !self
            .access
            .check(&mesh, &ctx, super::query_service::FOLD_QUERY_SERVICE)
        {
            return Err(RpcHandlerError::Application {
                code: FOLD_QUERY_V1_NOT_AUTHORIZED,
                message: "not authorized to query this daemon's summaries".into(),
            });
        }
        // v1's decode stays as it always was (frozen behavior).
        let request: FoldQueryRequest = match postcard::from_bytes(&ctx.payload.body) {
            Ok(r) => r,
            Err(e) => {
                return Ok(encode(&FoldQueryResponse::Error(
                    FoldQueryError::DecodeFailed(e.to_string()),
                )))
            }
        };
        let v2 = FoldQueryV2Request {
            kind: request.kind,
            op: request.op,
            group: None,
        };
        let response = match answer_v2(&self.registry, &v2).await {
            FoldQueryV2Response::Summaries { kind, records } => FoldQueryResponse::Summaries {
                kind,
                summaries: records.into_iter().map(|r| r.summary).collect(),
            },
            FoldQueryV2Response::Error(_) => {
                FoldQueryResponse::Error(FoldQueryError::UnknownKind { kind: v2.kind })
            }
        };
        Ok(encode(&response))
    }
}

/// Serve `fold.query` (v1, daemon-level) and `fold.query.v2` on `mesh` for
/// every group in `registry`. Returns both handles; dropping one
/// un-serves that service.
///
/// Install after the mesh's channel registry, if any, so `serve_rpc` lists
/// both services' RPC channels in it.
pub fn install_fold_query_services(
    mesh: &Arc<MeshNode>,
    registry: Arc<AggregatorRegistry>,
    access: FoldQueryAccess,
) -> Result<(ServeHandle, ServeHandle), ServeError> {
    let v1 = mesh.serve_rpc(
        super::query_service::FOLD_QUERY_SERVICE,
        Arc::new(FoldQueryRegistryV1Handler {
            mesh: Arc::downgrade(mesh),
            registry: registry.clone(),
            access: access.clone(),
        }),
    )?;
    let v2 = mesh.serve_rpc(
        FOLD_QUERY_V2_SERVICE,
        Arc::new(FoldQueryV2Handler {
            mesh: Arc::downgrade(mesh),
            registry,
            access,
        }),
    )?;
    Ok((v1, v2))
}

fn encode<T: Serialize>(response: &T) -> RpcResponsePayload {
    let body = match postcard::to_allocvec(response) {
        Ok(b) => Bytes::from(b),
        Err(e) => {
            tracing::warn!(error = %e, "aggregator: fold query response encode failed");
            Bytes::new()
        }
    };
    RpcResponsePayload {
        status: RpcStatus::Ok,
        headers: Vec::new(),
        body,
    }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Errors from the v2 client methods on [`FoldQueryClient`].
///
/// A separate type from `FoldQueryClientError`, so adding v2 does not
/// change the v1 client's error surface that bindings match on.
#[derive(Debug, thiserror::Error)]
pub enum FoldQueryV2ClientError {
    /// The target serves no `fold.query.v2`: it answered `NotFound` for
    /// the service. Mapped from that one observable result only. A timeout
    /// or any other failure keeps its own kind, because it says nothing
    /// about the server's version.
    #[error("the target does not serve {FOLD_QUERY_V2_SERVICE} (server too old)")]
    ServerTooOld,
    /// Any other transport failure (no route, timeout, other status).
    #[error("transport: {0}")]
    Transport(RpcError),
    /// Request encoding or strict response decoding failed.
    #[error("codec: {0}")]
    Codec(String),
    /// The handler refused the request.
    #[error("server: {0}")]
    Server(FoldQueryV2Error),
}

impl From<RpcError> for FoldQueryV2ClientError {
    fn from(e: RpcError) -> Self {
        match e {
            RpcError::ServerError { status, .. } if status == RpcStatus::NotFound.to_wire() => {
                Self::ServerTooOld
            }
            other => Self::Transport(other),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct V2CacheKey {
    target: u64,
    kind: u16,
    group: Option<String>,
}

impl V2CacheKey {
    pub(crate) fn is_for_target(&self, target: u64) -> bool {
        self.target == target
    }
}

pub(crate) struct V2CacheEntry {
    records: Vec<SummaryRecord>,
    fetched_at: Instant,
}

/// The v2 half of [`FoldQueryClient`]'s cache.
pub(crate) type V2Cache = Arc<RwLock<HashMap<V2CacheKey, V2CacheEntry>>>;

impl FoldQueryClient {
    /// `fold.query.v2` `LatestSummary` for `kind`, from one `group` or
    /// (`None`) every group aggregating it. Cached for the client's TTL, per
    /// `(target, kind, group)`.
    pub async fn query_group_latest(
        &self,
        target_node_id: u64,
        kind: u16,
        group: Option<&str>,
    ) -> Result<Vec<SummaryRecord>, FoldQueryV2ClientError> {
        let key = V2CacheKey {
            target: target_node_id,
            kind,
            group: group.map(str::to_owned),
        };
        let ttl = self.ttl();
        if !ttl.is_zero() {
            if let Some(entry) = self.v2_cache().read().get(&key) {
                if entry.fetched_at.elapsed() < ttl {
                    return Ok(entry.records.clone());
                }
            }
        }
        let records = self
            .issue_v2(target_node_id, kind, group, FoldQueryOp::LatestSummary)
            .await?;
        if !ttl.is_zero() {
            let mut cache = self.v2_cache().write();
            cache.retain(|_, e| e.fetched_at.elapsed() < ttl);
            cache.insert(
                key,
                V2CacheEntry {
                    records: records.clone(),
                    fetched_at: Instant::now(),
                },
            );
        }
        Ok(records)
    }

    /// `fold.query.v2` `SummarizeNow`. Never cached.
    pub async fn query_group_summarize_now(
        &self,
        target_node_id: u64,
        kind: u16,
        group: Option<&str>,
    ) -> Result<Vec<SummaryRecord>, FoldQueryV2ClientError> {
        self.issue_v2(target_node_id, kind, group, FoldQueryOp::SummarizeNow)
            .await
    }

    async fn issue_v2(
        &self,
        target_node_id: u64,
        kind: u16,
        group: Option<&str>,
        op: FoldQueryOp,
    ) -> Result<Vec<SummaryRecord>, FoldQueryV2ClientError> {
        let request = FoldQueryV2Request {
            kind,
            op,
            group: group.map(str::to_owned),
        };
        let body = postcard::to_allocvec(&request)
            .map_err(|e| FoldQueryV2ClientError::Codec(e.to_string()))?;
        let opts = CallOptions {
            deadline: Some(Instant::now() + self.deadline()),
            ..Default::default()
        };
        let reply = self
            .mesh()
            .call(
                target_node_id,
                FOLD_QUERY_V2_SERVICE,
                Bytes::from(body),
                opts,
            )
            .await?;
        let response: FoldQueryV2Response =
            decode_strict(&reply.body).map_err(|e| FoldQueryV2ClientError::Codec(e.to_string()))?;
        match response {
            FoldQueryV2Response::Summaries { records, .. } => Ok(records),
            FoldQueryV2Response::Error(e) => Err(FoldQueryV2ClientError::Server(e)),
        }
    }
}
