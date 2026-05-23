//! `RegistryClient` — client-side helper that wraps
//! [`MeshNode::call`] with typed `aggregator.registry`
//! serialization.
//!
//! Slice 7 of `docs/plans/AGGREGATOR_LIFECYCLE_DEFERRED_2026_05_23.md`.
//! Mirrors the [`FoldQueryClient`](super::query_client::FoldQueryClient)
//! shape: take an `Arc<MeshNode>`, expose typed methods, marshal
//! requests + replies via postcard.
//!
//! # No cache (yet)
//!
//! `fold.query` caches by `(target, service, kind)` because
//! summaries change at the daemon's tick cadence. Registry
//! membership changes at deployment events (spawn / scale /
//! unregister), which are operator-driven and rare — a cache
//! would hide live state from the operator. Operators reaching
//! for "give me a stable read" can call once and reuse the Vec.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;

use super::registry_service::{
    RegistryGroupSummary, RegistryRequest, RegistryResponse, RegistryRpcError, REGISTRY_SERVICE,
};
use crate::adapter::net::mesh_rpc::{CallOptions, RpcError};
use crate::adapter::net::MeshNode;

/// Default RPC call deadline. Mirrors `FoldQueryClient`'s
/// shape — long enough to absorb cross-subnet latency, short
/// enough that a wedged daemon surfaces quickly.
pub const DEFAULT_REGISTRY_DEADLINE: Duration = Duration::from_secs(3);

/// Client-side errors. Distinct from `RpcError` (transport),
/// `postcard::Error` (codec), and `RegistryRpcError`
/// (server-side handler) so the caller can match on the failure
/// shape they care about.
#[derive(Debug, thiserror::Error)]
pub enum RegistryClientError {
    /// Transport-level failure — no route, timeout, server
    /// returned a non-Ok status before invoking the handler.
    #[error("transport: {0}")]
    Transport(RpcError),
    /// Request serialization or response deserialization failed.
    #[error("codec: {0}")]
    Codec(String),
    /// Server handler rejected the request.
    #[error("server: {0:?}")]
    Server(RegistryRpcError),
}

impl From<RpcError> for RegistryClientError {
    fn from(e: RpcError) -> Self {
        Self::Transport(e)
    }
}

impl From<postcard::Error> for RegistryClientError {
    fn from(e: postcard::Error) -> Self {
        Self::Codec(e.to_string())
    }
}

/// Typed `aggregator.registry` client. Cheap to clone.
#[derive(Clone)]
pub struct RegistryClient {
    mesh: Arc<MeshNode>,
    deadline: Duration,
}

impl RegistryClient {
    /// Build a client backed by `mesh` with the default
    /// deadline.
    pub fn new(mesh: Arc<MeshNode>) -> Self {
        Self {
            mesh,
            deadline: DEFAULT_REGISTRY_DEADLINE,
        }
    }

    /// Override the per-call deadline.
    pub fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }

    /// Enumerate every aggregator group registered on the target
    /// node. The reply ordering matches the server-side
    /// [`super::registry::AggregatorRegistry::entries`] ordering
    /// (sorted by group name).
    pub async fn list(
        &self,
        target_node_id: u64,
    ) -> Result<Vec<RegistryGroupSummary>, RegistryClientError> {
        self.list_with_service(target_node_id, REGISTRY_SERVICE)
            .await
    }

    /// Same as [`Self::list`] but with a caller-supplied service
    /// name. Useful when a node hosts multiple registries (e.g.
    /// staging + prod aggregators isolated by service name).
    pub async fn list_with_service(
        &self,
        target_node_id: u64,
        service: &str,
    ) -> Result<Vec<RegistryGroupSummary>, RegistryClientError> {
        let response = self
            .send(target_node_id, service, RegistryRequest::List)
            .await?;
        match response {
            RegistryResponse::Groups(groups) => Ok(groups),
            RegistryResponse::Error(e) => Err(RegistryClientError::Server(e)),
            other => Err(RegistryClientError::Codec(format!(
                "unexpected response for List: {other:?}"
            ))),
        }
    }

    /// Deploy a new aggregator group by referencing a daemon-
    /// side template by name. The daemon resolves `template_name`
    /// against its config-time `[[template]]` registry, builds
    /// the group with the operator-chosen `group_name`, and
    /// returns its initial snapshot.
    pub async fn spawn(
        &self,
        target_node_id: u64,
        template_name: impl Into<String>,
        group_name: impl Into<String>,
        replica_count: u8,
    ) -> Result<RegistryGroupSummary, RegistryClientError> {
        self.spawn_with_service(
            target_node_id,
            REGISTRY_SERVICE,
            template_name,
            group_name,
            replica_count,
        )
        .await
    }

    /// `Spawn` against a non-default service name.
    pub async fn spawn_with_service(
        &self,
        target_node_id: u64,
        service: &str,
        template_name: impl Into<String>,
        group_name: impl Into<String>,
        replica_count: u8,
    ) -> Result<RegistryGroupSummary, RegistryClientError> {
        let request = RegistryRequest::Spawn {
            template_name: template_name.into(),
            group_name: group_name.into(),
            replica_count,
        };
        let response = self.send(target_node_id, service, request).await?;
        match response {
            RegistryResponse::Spawned(summary) => Ok(summary),
            RegistryResponse::Error(e) => Err(RegistryClientError::Server(e)),
            other => Err(RegistryClientError::Codec(format!(
                "unexpected response for Spawn: {other:?}"
            ))),
        }
    }

    /// Tear down a registered group by name. Returns `true`
    /// when the group existed and was stopped, `false` when no
    /// such group was registered on the target node.
    pub async fn unregister(
        &self,
        target_node_id: u64,
        group_name: impl Into<String>,
    ) -> Result<bool, RegistryClientError> {
        self.unregister_with_service(target_node_id, REGISTRY_SERVICE, group_name)
            .await
    }

    /// `Unregister` against a non-default service name.
    pub async fn unregister_with_service(
        &self,
        target_node_id: u64,
        service: &str,
        group_name: impl Into<String>,
    ) -> Result<bool, RegistryClientError> {
        let request = RegistryRequest::Unregister {
            group_name: group_name.into(),
        };
        let response = self.send(target_node_id, service, request).await?;
        match response {
            RegistryResponse::Unregistered { existed } => Ok(existed),
            RegistryResponse::Error(e) => Err(RegistryClientError::Server(e)),
            other => Err(RegistryClientError::Codec(format!(
                "unexpected response for Unregister: {other:?}"
            ))),
        }
    }

    /// Shared marshalling helper. Encodes the request, fires the
    /// RPC, decodes the response.
    async fn send(
        &self,
        target_node_id: u64,
        service: &str,
        request: RegistryRequest,
    ) -> Result<RegistryResponse, RegistryClientError> {
        let body = postcard::to_allocvec(&request)?;
        let opts = CallOptions {
            deadline: Some(Instant::now() + self.deadline),
            ..Default::default()
        };
        let reply = self
            .mesh
            .call(target_node_id, service, Bytes::from(body), opts)
            .await?;
        let response: RegistryResponse = postcard::from_bytes(&reply.body)?;
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::identity::EntityKeypair;
    use crate::adapter::net::MeshNodeConfig;
    use std::net::SocketAddr;

    async fn build_mesh() -> Arc<MeshNode> {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let cfg = MeshNodeConfig::new(addr, [0x17u8; 32]);
        Arc::new(
            MeshNode::new(EntityKeypair::generate(), cfg)
                .await
                .expect("MeshNode::new"),
        )
    }

    #[tokio::test]
    async fn new_carries_default_deadline() {
        let mesh = build_mesh().await;
        let client = RegistryClient::new(mesh);
        assert_eq!(client.deadline, DEFAULT_REGISTRY_DEADLINE);
    }

    #[tokio::test]
    async fn with_deadline_overrides_default() {
        let mesh = build_mesh().await;
        let client = RegistryClient::new(mesh).with_deadline(Duration::from_secs(7));
        assert_eq!(client.deadline, Duration::from_secs(7));
    }
}
