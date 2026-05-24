//! Node bindings for the `aggregator.registry` + `fold.query`
//! RPC clients. Client surfaces only.
//!
//! Errors surface as napi `Error` with the stable `agg:<kind>:
//! <detail>` prefix (`aggregator.ts` re-throws as a typed
//! `RegistryClientError` / `FoldQueryClientError`).
//!
//! Inner client lives behind `Arc<RwLock<...>>`; `with_deadline`
//! / `with_ttl` mutate in place and return a fresh wrapper that
//! shares inner state — adjustments are observed by every alias
//! and the warmed cache survives.

use std::sync::Arc;
use std::time::Duration;

use napi::bindgen_prelude::*;
use napi_derive::napi;
use parking_lot::RwLock as ParkingRwLock;

use net_sdk::aggregator::{
    FoldQueryClient as SdkFoldQueryClient, FoldQueryClientError, FoldQueryError,
    RegistryClient as SdkRegistryClient, RegistryClientError, RegistryGroupSummary,
    RegistryReplicaSummary, RegistryRpcError,
};

// ============================================================================
// Error mapping — stable `agg:` prefix kind:detail.
// ============================================================================

pub(crate) const ERR_AGG_PREFIX: &str = "agg:";

#[inline]
fn agg_err(kind: &str, detail: impl std::fmt::Display) -> Error {
    Error::from_reason(format!("{ERR_AGG_PREFIX}{kind}: {detail}"))
}

fn registry_err(e: RegistryClientError) -> Error {
    match e {
        RegistryClientError::Transport(t) => agg_err("transport", t),
        RegistryClientError::Codec(c) => agg_err("codec", c),
        RegistryClientError::Server(rpc) => match rpc {
            RegistryRpcError::DecodeFailed(s) => agg_err("codec", format!("server-decode: {s}")),
            RegistryRpcError::UnknownTemplate(t) => agg_err("unknown-template", t),
            RegistryRpcError::DuplicateGroupName(n) => agg_err("duplicate-group-name", n),
            RegistryRpcError::SpawnRejected(d) => agg_err("spawn-rejected", d),
            RegistryRpcError::SpawnNotSupported => {
                agg_err("spawn-not-supported", "daemon is read-only")
            }
            RegistryRpcError::UnknownGroup(g) => agg_err("unknown-group", g),
            RegistryRpcError::ScaleRejected(d) => agg_err("scale-rejected", d),
            RegistryRpcError::ScaleNotSupported => {
                agg_err("scale-not-supported", "daemon doesn't accept dynamic scale")
            }
        },
    }
}

fn fold_query_err(e: FoldQueryClientError) -> Error {
    match e {
        FoldQueryClientError::Transport(t) => agg_err("transport", t),
        FoldQueryClientError::Codec(c) => agg_err("codec", c),
        FoldQueryClientError::Server(srv) => match srv {
            FoldQueryError::UnknownKind { kind } => {
                agg_err("unknown-kind", format!("0x{kind:04x}"))
            }
            FoldQueryError::DecodeFailed(s) => agg_err("codec", format!("server-decode: {s}")),
        },
    }
}

// ============================================================================
// POJO shapes — wire-contract-locked across all language SDKs.
// ============================================================================

#[napi(object)]
pub struct RegistryReplicaRowJs {
    pub generation: BigInt,
    pub healthy: bool,
    pub diagnostic: Option<String>,
    pub placement_node_id: Option<BigInt>,
}

impl From<&RegistryReplicaSummary> for RegistryReplicaRowJs {
    fn from(r: &RegistryReplicaSummary) -> Self {
        Self {
            generation: BigInt::from(r.generation),
            healthy: r.healthy,
            diagnostic: r.diagnostic.clone(),
            placement_node_id: r.placement_node_id.map(BigInt::from),
        }
    }
}

#[napi(object)]
pub struct RegistryGroupSummaryJs {
    pub name: String,
    /// 64-char lowercase hex rendering of the 32-byte group
    /// seed. JS BigInt is awkward at 32 bytes; hex matches what
    /// every other binding emits.
    pub group_seed_hex: String,
    pub replicas: Vec<RegistryReplicaRowJs>,
}

impl From<&RegistryGroupSummary> for RegistryGroupSummaryJs {
    fn from(g: &RegistryGroupSummary) -> Self {
        Self {
            name: g.name.clone(),
            group_seed_hex: hex::encode(g.group_seed),
            replicas: g.replicas.iter().map(Into::into).collect(),
        }
    }
}

#[napi(object)]
pub struct SummaryAnnouncementJs {
    pub fold_kind: u32,
    pub source_subnet: String,
    pub generation: BigInt,
    pub buckets: Vec<SummaryBucketJs>,
}

#[napi(object)]
pub struct SummaryBucketJs {
    pub name: String,
    pub count: BigInt,
}

impl From<&::net::adapter::net::behavior::aggregator::SummaryAnnouncement>
    for SummaryAnnouncementJs
{
    fn from(s: &::net::adapter::net::behavior::aggregator::SummaryAnnouncement) -> Self {
        Self {
            fold_kind: u32::from(s.fold_kind),
            source_subnet: format!("{}", s.source_subnet),
            generation: BigInt::from(s.generation),
            buckets: s
                .buckets
                .iter()
                .map(|(n, c)| SummaryBucketJs {
                    name: n.clone(),
                    count: BigInt::from(*c),
                })
                .collect(),
        }
    }
}

// ============================================================================
// RegistryClient
// ============================================================================

/// Wire-shaped client for `aggregator.registry` RPC.
///
/// `with_deadline` mutates the inner client in place and
/// returns a fresh wrapper that **shares the inner state** —
/// both the new and original wrapper observe the new deadline.
/// Matches the C-FFI / Go shape.
#[napi]
pub struct RegistryClient {
    inner: Arc<ParkingRwLock<SdkRegistryClient>>,
}

#[napi]
impl RegistryClient {
    #[napi(factory)]
    pub fn create(mesh: &crate::NetMesh) -> Result<Self> {
        let mesh_arc = mesh.node_arc_clone()?;
        Ok(Self {
            inner: Arc::new(ParkingRwLock::new(SdkRegistryClient::new(mesh_arc))),
        })
    }

    #[napi]
    pub fn with_deadline(&self, millis: u32) -> Self {
        self.inner
            .write()
            .set_deadline_mut(Duration::from_millis(u64::from(millis)));
        Self {
            inner: Arc::clone(&self.inner),
        }
    }

    #[napi]
    pub async fn list(&self, target_node_id: BigInt) -> Result<Vec<RegistryGroupSummaryJs>> {
        let target = crate::common::bigint_u64(target_node_id)?;
        let client = self.inner.read().clone();
        let groups = client.list(target).await.map_err(registry_err)?;
        Ok(groups.iter().map(Into::into).collect())
    }

    #[napi]
    pub async fn spawn(
        &self,
        target_node_id: BigInt,
        template_name: String,
        group_name: String,
        replica_count: u32,
    ) -> Result<RegistryGroupSummaryJs> {
        let target = crate::common::bigint_u64(target_node_id)?;
        let count = u8::try_from(replica_count).map_err(|_| {
            agg_err(
                "invalid-args",
                format!("replicaCount must be 1..=255, got {replica_count}"),
            )
        })?;
        let client = self.inner.read().clone();
        let summary = client
            .spawn(target, template_name, group_name, count)
            .await
            .map_err(registry_err)?;
        Ok((&summary).into())
    }

    #[napi]
    pub async fn unregister(&self, target_node_id: BigInt, group_name: String) -> Result<bool> {
        let target = crate::common::bigint_u64(target_node_id)?;
        let client = self.inner.read().clone();
        client
            .unregister(target, group_name)
            .await
            .map_err(registry_err)
    }
}

// ============================================================================
// FoldQueryClient
// ============================================================================

/// Wire-shaped client for `fold.query` RPC. Same in-place
/// builder shape as [`RegistryClient`]; the warmed cache
/// survives `with_ttl` / `with_deadline` adjustments.
#[napi]
pub struct FoldQueryClient {
    inner: Arc<ParkingRwLock<SdkFoldQueryClient>>,
}

#[napi]
impl FoldQueryClient {
    #[napi(factory)]
    pub fn create(mesh: &crate::NetMesh) -> Result<Self> {
        let mesh_arc = mesh.node_arc_clone()?;
        Ok(Self {
            inner: Arc::new(ParkingRwLock::new(SdkFoldQueryClient::new(mesh_arc))),
        })
    }

    #[napi]
    pub fn with_ttl(&self, millis: u32) -> Self {
        self.inner
            .write()
            .set_ttl_mut(Duration::from_millis(u64::from(millis)));
        Self {
            inner: Arc::clone(&self.inner),
        }
    }

    #[napi]
    pub fn with_deadline(&self, millis: u32) -> Self {
        self.inner
            .write()
            .set_deadline_mut(Duration::from_millis(u64::from(millis)));
        Self {
            inner: Arc::clone(&self.inner),
        }
    }

    #[napi]
    pub async fn query_latest(
        &self,
        target_node_id: BigInt,
        kind: u32,
    ) -> Result<Vec<SummaryAnnouncementJs>> {
        let target = crate::common::bigint_u64(target_node_id)?;
        let kind_u16 = u16::try_from(kind)
            .map_err(|_| agg_err("invalid-args", format!("kind must fit in u16, got {kind}")))?;
        let client = self.inner.read().clone();
        let summaries = client
            .query_latest(target, kind_u16)
            .await
            .map_err(fold_query_err)?;
        Ok(summaries.iter().map(Into::into).collect())
    }

    #[napi]
    pub async fn query_summarize_now(
        &self,
        target_node_id: BigInt,
        kind: u32,
    ) -> Result<Vec<SummaryAnnouncementJs>> {
        let target = crate::common::bigint_u64(target_node_id)?;
        let kind_u16 = u16::try_from(kind)
            .map_err(|_| agg_err("invalid-args", format!("kind must fit in u16, got {kind}")))?;
        let client = self.inner.read().clone();
        let summaries = client
            .query_summarize_now(target, kind_u16)
            .await
            .map_err(fold_query_err)?;
        Ok(summaries.iter().map(Into::into).collect())
    }

    #[napi]
    pub fn invalidate_cache(&self) {
        self.inner.read().invalidate_cache();
    }

    #[napi]
    pub fn invalidate_target(&self, target_node_id: BigInt) -> Result<()> {
        let target = crate::common::bigint_u64(target_node_id)?;
        self.inner.read().invalidate_target(target);
        Ok(())
    }
}
