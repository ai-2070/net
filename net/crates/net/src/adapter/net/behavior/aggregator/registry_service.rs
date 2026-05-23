//! `aggregator.registry` RPC service — read-only enumeration
//! surface for the daemon process's [`AggregatorRegistry`].
//!
//! Slice 7 of `docs/plans/AGGREGATOR_LIFECYCLE_DEFERRED_2026_05_23.md`.
//! Mirrors the `fold.query` service pattern: postcard-encoded
//! wire types, RPC handler holding an `Arc<AggregatorRegistry>`,
//! pure-fn `answer` for unit testing without the RPC plumbing.
//!
//! # What's in this slice
//!
//! - `RegistryRequest::List` — return every group registered on
//!   the target node, with per-replica health snapshot inline.
//!
//! # What's NOT in this slice
//!
//! - `Spawn` / `Scale` — both need a way to receive a daemon
//!   factory + config over the wire. That requires either:
//!   1. The daemon side preregisters config templates by name
//!      (CLI just picks a template), or
//!   2. The wire carries a full `AggregatorConfig` payload.
//!   Both are bigger design surfaces; the next slice picks one.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use serde::{Deserialize, Serialize};

use super::registry::AggregatorRegistry;
use crate::adapter::net::cortex::rpc::{
    RpcContext, RpcHandler, RpcHandlerError, RpcResponsePayload, RpcStatus,
};

/// Canonical service name. Clients construct request channels
/// implicitly via the substrate's `call_typed` plumbing.
pub const REGISTRY_SERVICE: &str = "aggregator.registry";

/// Wire-shaped request. Postcard-encoded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegistryRequest {
    /// Enumerate every registered group with per-replica
    /// health. Read-only.
    List,
}

/// Wire-shaped response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegistryResponse {
    /// Successful `List` reply.
    Groups(Vec<RegistryGroupSummary>),
    /// Handler-level error (decode failure, future op-specific
    /// errors).
    Error(RegistryRpcError),
}

/// Per-group entry in a `Groups` reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryGroupSummary {
    /// Operator-chosen group name (the registry key).
    pub name: String,
    /// 32-byte group seed.
    pub group_seed: [u8; 32],
    /// Per-replica rows in declaration order.
    pub replicas: Vec<RegistryReplicaSummary>,
}

/// Per-replica row inside a [`RegistryGroupSummary`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryReplicaSummary {
    /// Replica's monotonic tick counter.
    pub generation: u64,
    /// `true` when the replica reported healthy at snapshot time.
    pub healthy: bool,
    /// Operator-facing diagnostic when `healthy == false`.
    pub diagnostic: Option<String>,
    /// Placement decision recorded at spawn time (when the group
    /// was spawned via `LifecycleGroup::spawn_with_placement`).
    pub placement_node_id: Option<u64>,
}

/// Handler-level error variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegistryRpcError {
    /// Request payload failed to decode. Carries the postcard
    /// error message as a `String`.
    DecodeFailed(String),
}

/// `RpcHandler` implementation backed by a shared
/// [`AggregatorRegistry`]. Construct via
/// [`RegistryHandler::new`] and pass to
/// [`crate::adapter::net::MeshNode::serve_rpc`] under
/// [`REGISTRY_SERVICE`].
pub struct RegistryHandler {
    registry: Arc<AggregatorRegistry>,
}

impl RegistryHandler {
    /// Wrap a shared registry as an RPC handler.
    pub fn new(registry: Arc<AggregatorRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl RpcHandler for RegistryHandler {
    async fn call(&self, ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
        let request: RegistryRequest = match postcard::from_bytes(&ctx.payload.body) {
            Ok(req) => req,
            Err(e) => {
                let response =
                    RegistryResponse::Error(RegistryRpcError::DecodeFailed(e.to_string()));
                return Ok(encode_response(&response));
            }
        };
        let response = answer(&self.registry, &request).await;
        Ok(encode_response(&response))
    }
}

/// Pure-function answer logic, broken out for direct unit
/// testing without going through the RPC plumbing.
pub(crate) async fn answer(
    registry: &Arc<AggregatorRegistry>,
    request: &RegistryRequest,
) -> RegistryResponse {
    match request {
        RegistryRequest::List => {
            let entries = registry.entries();
            let mut groups = Vec::with_capacity(entries.len());
            for entry in entries {
                let replicas = entry.replicas().await;
                let placements = entry.placements().await;
                let healths = entry.health().await;
                let mut rows = Vec::with_capacity(replicas.len());
                for (idx, replica) in replicas.iter().enumerate() {
                    let health = healths.get(idx).cloned().unwrap_or_else(|| {
                        crate::adapter::net::behavior::lifecycle::ReplicaHealth {
                            healthy: true,
                            diagnostic: None,
                        }
                    });
                    let placement_node_id = placements.get(idx).map(|p| p.node_id);
                    rows.push(RegistryReplicaSummary {
                        generation: replica.generation(),
                        healthy: health.healthy,
                        diagnostic: health.diagnostic,
                        placement_node_id,
                    });
                }
                groups.push(RegistryGroupSummary {
                    name: entry.name.clone(),
                    group_seed: entry.group_seed,
                    replicas: rows,
                });
            }
            RegistryResponse::Groups(groups)
        }
    }
}

impl AggregatorRegistry {
    /// Wrap `self` in a [`RegistryHandler`] and register it
    /// against `mesh` under [`REGISTRY_SERVICE`]. Returns the
    /// `ServeHandle` — drop it to tear down the registration.
    ///
    /// Convenience around `mesh.serve_rpc` so daemon-process
    /// startup looks like:
    ///
    /// ```ignore
    /// let registry = Arc::new(AggregatorRegistry::new());
    /// mesh.set_aggregator_registry(registry.clone());
    /// let _serve = registry.install_registry_service(&mesh)?;
    /// ```
    pub fn install_registry_service(
        self: &Arc<Self>,
        mesh: &Arc<crate::adapter::net::MeshNode>,
    ) -> Result<crate::adapter::net::mesh_rpc::ServeHandle, crate::adapter::net::mesh_rpc::ServeError>
    {
        mesh.serve_rpc(REGISTRY_SERVICE, Arc::new(RegistryHandler::new(self.clone())))
    }
}

fn encode_response(response: &RegistryResponse) -> RpcResponsePayload {
    let body = match postcard::to_allocvec(response) {
        Ok(b) => Bytes::from(b),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "aggregator: registry response encode failed; replying with empty body",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::aggregator::{
        AggregatorConfig, AggregatorDaemon, AggregatorRegistry,
    };
    use crate::adapter::net::behavior::fold::capability::CapabilityFold;
    use crate::adapter::net::behavior::fold::FoldKind;
    use crate::adapter::net::behavior::lifecycle::LifecycleGroup;
    use crate::adapter::net::identity::EntityKeypair;
    use crate::adapter::net::{MeshNode, MeshNodeConfig, SubnetId};
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

    async fn spawn_group(name: &str, interval_ms: u64) -> LifecycleGroup<AggregatorDaemon> {
        let _ = name;
        let mesh = build_mesh().await;
        let cfg = AggregatorConfig::new(SubnetId::GLOBAL)
            .with_fold_kind(CapabilityFold::KIND_ID)
            .with_interval(Duration::from_millis(interval_ms));
        let cfg_clone = cfg.clone();
        let mesh_clone = mesh.clone();
        LifecycleGroup::<AggregatorDaemon>::spawn(2, [0xABu8; 32], move |_idx| {
            Arc::new(AggregatorDaemon::new(cfg_clone.clone(), mesh_clone.clone()).expect("new"))
        })
        .await
        .expect("spawn")
    }

    #[tokio::test]
    async fn list_returns_every_registered_group() {
        let registry = Arc::new(AggregatorRegistry::new());
        registry
            .register("alpha", spawn_group("alpha", 50).await)
            .expect("register alpha");
        registry
            .register("beta", spawn_group("beta", 50).await)
            .expect("register beta");

        let response = answer(&registry, &RegistryRequest::List).await;
        match response {
            RegistryResponse::Groups(groups) => {
                assert_eq!(groups.len(), 2);
                let names: Vec<&str> = groups.iter().map(|g| g.name.as_str()).collect();
                // Registry's `entries()` sorts by name, so alpha
                // before beta.
                assert_eq!(names, vec!["alpha", "beta"]);
                for g in &groups {
                    assert_eq!(g.replicas.len(), 2);
                    for r in &g.replicas {
                        // Healthy initially (no tick has landed
                        // yet, but the daemon's health() returns
                        // healthy until the 3 × interval window
                        // expires).
                        assert!(r.healthy);
                    }
                }
            }
            RegistryResponse::Error(e) => panic!("expected Groups, got Error {e:?}"),
        }

        // Cleanup.
        for n in ["alpha", "beta"] {
            let g = registry.unregister(n).await.expect("unregister");
            g.stop().await;
        }
    }

    #[tokio::test]
    async fn list_against_empty_registry_returns_empty_groups() {
        let registry = Arc::new(AggregatorRegistry::new());
        let response = answer(&registry, &RegistryRequest::List).await;
        match response {
            RegistryResponse::Groups(groups) => assert!(groups.is_empty()),
            RegistryResponse::Error(e) => panic!("expected empty Groups, got Error {e:?}"),
        }
    }

    #[test]
    fn registry_request_response_round_trip_through_postcard() {
        // Pin the wire shape — postcard encode/decode round-trip
        // for both variants we ship.
        let req = RegistryRequest::List;
        let bytes = postcard::to_allocvec(&req).expect("encode req");
        let decoded: RegistryRequest = postcard::from_bytes(&bytes).expect("decode req");
        assert_eq!(req, decoded);

        let resp = RegistryResponse::Groups(vec![RegistryGroupSummary {
            name: "test".into(),
            group_seed: [0xCDu8; 32],
            replicas: vec![RegistryReplicaSummary {
                generation: 42,
                healthy: false,
                diagnostic: Some("stuck".into()),
                placement_node_id: Some(0xBEEF),
            }],
        }]);
        let bytes = postcard::to_allocvec(&resp).expect("encode resp");
        let decoded: RegistryResponse = postcard::from_bytes(&bytes).expect("decode resp");
        assert_eq!(resp, decoded);

        let err_resp =
            RegistryResponse::Error(RegistryRpcError::DecodeFailed("bad bytes".into()));
        let bytes = postcard::to_allocvec(&err_resp).expect("encode err resp");
        let decoded: RegistryResponse = postcard::from_bytes(&bytes).expect("decode err resp");
        assert_eq!(err_resp, decoded);
    }
}
