//! Node bindings for the `aggregator.registry` RPC client +
//! the `fold.query` RPC client. Stage 2 of
//! `SDK_AGGREGATOR_SUBNET_PLAN.md`.
//!
//! Client surfaces only — daemon-author types
//! (`AggregatorDaemon`, `LifecycleGroup`, `HealthMonitor`)
//! stay Rust-only. Operators wanting to host aggregators in
//! Node should run the `net-aggregator-daemon` binary
//! alongside their app and RPC into it via the clients here.
//!
//! # Wire-error mapping
//!
//! Both clients translate substrate errors into napi `Error`s
//! with a stable `agg:` prefix discriminator, mirroring the
//! `nrpc:` pattern in `mesh_rpc.rs`. The JS shim layer
//! (`sdk-ts/src/aggregator.ts`) matches the prefix to re-throw
//! typed `RegistryClientError` / `FoldQueryClientError`
//! instances with `.kind` and `.serverDetail` fields.

use std::sync::Arc;
use std::time::Duration;

use napi::bindgen_prelude::*;
use napi_derive::napi;

use ::net::adapter::net::MeshNode;
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

/// Per-replica row inside a [`RegistryGroupSummaryJs`].
#[napi(object)]
pub struct RegistryReplicaRowJs {
    /// Replica's monotonic tick counter.
    pub generation: BigInt,
    /// `true` when the replica reported healthy at snapshot.
    pub healthy: bool,
    /// Daemon-specific diagnostic when `healthy == false`.
    pub diagnostic: Option<String>,
    /// Placement decision recorded at spawn time (only present
    /// when the group was spawned via `spawn_with_placement`).
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

/// Per-group entry returned by `RegistryClient::list` /
/// `spawn`. Locked across SDK languages — see
/// `SDK_AGGREGATOR_SUBNET_PLAN.md` § "Cross-language wire
/// contract."
#[napi(object)]
pub struct RegistryGroupSummaryJs {
    /// Operator-chosen group name (the registry key).
    pub name: String,
    /// 64-char lowercase hex rendering of the 32-byte group
    /// seed. JS BigInt is awkward at 32 bytes; hex matches what
    /// every other binding emits.
    pub group_seed_hex: String,
    /// Per-replica rows in declaration order.
    pub replicas: Vec<RegistryReplicaRowJs>,
}

impl From<&RegistryGroupSummary> for RegistryGroupSummaryJs {
    fn from(g: &RegistryGroupSummary) -> Self {
        let group_seed_hex: String = g.group_seed.iter().map(|b| format!("{b:02x}")).collect();
        Self {
            name: g.name.clone(),
            group_seed_hex,
            replicas: g.replicas.iter().map(Into::into).collect(),
        }
    }
}

/// Per-summary row returned by `FoldQueryClient::queryLatest`
/// / `querySummarizeNow`.
#[napi(object)]
pub struct SummaryAnnouncementJs {
    /// `FoldKind::KIND_ID` (decimal; render as 0x%04x in
    /// operator surfaces if needed).
    pub fold_kind: u32,
    /// `SubnetId` rendered as dotted notation (e.g. `"3.7"`)
    /// or `"global"`.
    pub source_subnet: String,
    /// Aggregator's monotonic tick counter when this summary
    /// was produced.
    pub generation: BigInt,
    /// Per-bucket counts as `[name, count]` pairs.
    pub buckets: Vec<SummaryBucketJs>,
}

#[napi(object)]
pub struct SummaryBucketJs {
    pub name: String,
    pub count: BigInt,
}

impl From<&::net::adapter::net::behavior::aggregator::SummaryAnnouncement> for SummaryAnnouncementJs {
    fn from(s: &::net::adapter::net::behavior::aggregator::SummaryAnnouncement) -> Self {
        Self {
            fold_kind: s.fold_kind as u32,
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

/// Wire-shaped client for `aggregator.registry` RPC. Construct
/// via [`RegistryClient::create`] against a live `NetMesh`.
/// Every op resolves to a JS Promise.
#[napi]
pub struct RegistryClient {
    inner: SdkRegistryClient,
    mesh: Arc<MeshNode>,
}

#[napi]
impl RegistryClient {
    /// Construct against a live NetMesh. Errors if the mesh has
    /// been shut down.
    #[napi(factory)]
    pub fn create(mesh: &crate::NetMesh) -> Result<Self> {
        let mesh_arc = mesh.node_arc_clone()?;
        Ok(Self {
            inner: SdkRegistryClient::new(mesh_arc.clone()),
            mesh: mesh_arc,
        })
    }

    /// Override the per-call deadline in milliseconds. Returns
    /// a fresh client; the original stays at its prior
    /// deadline. Mirrors the builder pattern on the substrate
    /// `RegistryClient::with_deadline`.
    #[napi]
    pub fn with_deadline(&self, millis: u32) -> Self {
        Self {
            inner: SdkRegistryClient::new(self.mesh.clone())
                .with_deadline(Duration::from_millis(u64::from(millis))),
            mesh: self.mesh.clone(),
        }
    }

    /// Enumerate groups on `target_node_id`.
    #[napi]
    pub async fn list(&self, target_node_id: BigInt) -> Result<Vec<RegistryGroupSummaryJs>> {
        let target = bigint_u64("targetNodeId", target_node_id)?;
        let groups = self
            .inner
            .list(target)
            .await
            .map_err(registry_err)?;
        Ok(groups.iter().map(Into::into).collect())
    }

    /// Spawn a new group by referencing a daemon-side template.
    #[napi]
    pub async fn spawn(
        &self,
        target_node_id: BigInt,
        template_name: String,
        group_name: String,
        replica_count: u32,
    ) -> Result<RegistryGroupSummaryJs> {
        let target = bigint_u64("targetNodeId", target_node_id)?;
        let count = u8::try_from(replica_count).map_err(|_| {
            agg_err(
                "invalid-args",
                format!("replicaCount must be 1..=255, got {replica_count}"),
            )
        })?;
        let summary = self
            .inner
            .spawn(target, template_name, group_name, count)
            .await
            .map_err(registry_err)?;
        Ok((&summary).into())
    }

    /// Tear down a registered group by name.
    #[napi]
    pub async fn unregister(
        &self,
        target_node_id: BigInt,
        group_name: String,
    ) -> Result<bool> {
        let target = bigint_u64("targetNodeId", target_node_id)?;
        self.inner
            .unregister(target, group_name)
            .await
            .map_err(registry_err)
    }
}

// ============================================================================
// FoldQueryClient
// ============================================================================

#[napi]
pub struct FoldQueryClient {
    inner: SdkFoldQueryClient,
    mesh: Arc<MeshNode>,
}

#[napi]
impl FoldQueryClient {
    /// Construct against a live NetMesh.
    #[napi(factory)]
    pub fn create(mesh: &crate::NetMesh) -> Result<Self> {
        let mesh_arc = mesh.node_arc_clone()?;
        Ok(Self {
            inner: SdkFoldQueryClient::new(mesh_arc.clone()),
            mesh: mesh_arc,
        })
    }

    /// Override the cache TTL in milliseconds. `0` disables
    /// the cache entirely.
    #[napi]
    pub fn with_ttl(&self, millis: u32) -> Self {
        Self {
            inner: SdkFoldQueryClient::new(self.mesh.clone())
                .with_ttl(Duration::from_millis(u64::from(millis))),
            mesh: self.mesh.clone(),
        }
    }

    /// Override the per-call deadline in milliseconds.
    #[napi]
    pub fn with_deadline(&self, millis: u32) -> Self {
        Self {
            inner: SdkFoldQueryClient::new(self.mesh.clone())
                .with_deadline(Duration::from_millis(u64::from(millis))),
            mesh: self.mesh.clone(),
        }
    }

    /// Query the aggregator's latest cached summaries. Cache
    /// hit → returned directly; miss → wire RPC, cached, returned.
    #[napi]
    pub async fn query_latest(
        &self,
        target_node_id: BigInt,
        kind: u32,
    ) -> Result<Vec<SummaryAnnouncementJs>> {
        let target = bigint_u64("targetNodeId", target_node_id)?;
        let kind_u16 = u16::try_from(kind).map_err(|_| {
            agg_err(
                "invalid-args",
                format!("kind must fit in u16, got {kind}"),
            )
        })?;
        let summaries = self
            .inner
            .query_latest(target, kind_u16)
            .await
            .map_err(fold_query_err)?;
        Ok(summaries.iter().map(Into::into).collect())
    }

    /// Force a fresh `SummarizeNow` query — never cached.
    #[napi]
    pub async fn query_summarize_now(
        &self,
        target_node_id: BigInt,
        kind: u32,
    ) -> Result<Vec<SummaryAnnouncementJs>> {
        let target = bigint_u64("targetNodeId", target_node_id)?;
        let kind_u16 = u16::try_from(kind).map_err(|_| {
            agg_err(
                "invalid-args",
                format!("kind must fit in u16, got {kind}"),
            )
        })?;
        let summaries = self
            .inner
            .query_summarize_now(target, kind_u16)
            .await
            .map_err(fold_query_err)?;
        Ok(summaries.iter().map(Into::into).collect())
    }

    /// Drop every cached entry. Call after a topology change
    /// (e.g. a placement migration) so the next query re-resolves.
    #[napi]
    pub fn invalidate_cache(&self) {
        self.inner.invalidate_cache();
    }

    /// Drop only entries matching `targetNodeId`. Useful when
    /// a single replica is known stale but the rest of the
    /// cache is warm.
    #[napi]
    pub fn invalidate_target(&self, target_node_id: BigInt) -> Result<()> {
        let target = bigint_u64("targetNodeId", target_node_id)?;
        self.inner.invalidate_target(target);
        Ok(())
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Coerce a JS BigInt to a Rust u64, surfacing a typed
/// invalid-args error on overflow.
fn bigint_u64(name: &str, value: BigInt) -> Result<u64> {
    let (signed, words, _) = value.get_u64();
    if signed {
        return Err(agg_err(
            "invalid-args",
            format!("{name} must be a non-negative integer that fits in u64"),
        ));
    }
    Ok(words)
}
