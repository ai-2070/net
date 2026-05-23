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
//! # Spawn / Unregister
//!
//! Operators deploy + remove aggregators dynamically via the
//! `Spawn { template_name, group_name, replica_count }` and
//! `Unregister { group_name }` requests. The daemon side
//! resolves `template_name` against its config-time template
//! registry (see `aggregator-daemon::TemplateRegistry`) — this
//! avoids marshalling full `AggregatorConfig` over the wire and
//! keeps the trust boundary at the daemon's operator-controlled
//! config file.
//!
//! # Scale (deferred)
//!
//! Scale is implementable today as Unregister + Spawn with a
//! different `replica_count`. A dedicated Scale op (grow/shrink
//! the live `LifecycleGroup` without tearing it down) needs
//! `LifecycleGroup::add_replica` / `remove_last`, which we
//! haven't shipped yet — tracked as a follow-up.

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
    /// Deploy a new aggregator group by referencing a
    /// daemon-side template by name. The daemon resolves
    /// `template_name` against its operator-supplied config
    /// (`[[template]]` sections) and registers the resulting
    /// group under `group_name`. Returns the spawned group's
    /// snapshot.
    Spawn {
        /// Name of a `[[template]]` block in the daemon's
        /// config file.
        template_name: String,
        /// Operator-chosen name for the new group (registry
        /// key). Must be unique within the daemon process.
        group_name: String,
        /// Number of replicas to spawn. `1..=255`.
        replica_count: u8,
    },
    /// Tear down a registered group by name. Returns `Ok(true)`
    /// when the group existed and was stopped, `Ok(false)`
    /// when no such group was registered.
    Unregister {
        /// Name of the group to remove.
        group_name: String,
    },
}

/// Wire-shaped response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegistryResponse {
    /// Successful `List` reply.
    Groups(Vec<RegistryGroupSummary>),
    /// Successful `Spawn` reply — carries the newly-registered
    /// group's snapshot so the client doesn't need a follow-up
    /// `List` to read its initial state.
    Spawned(RegistryGroupSummary),
    /// `Unregister` reply: `true` when the group existed and
    /// was stopped, `false` when no such group was registered.
    Unregistered {
        /// True iff a group by that name was present.
        existed: bool,
    },
    /// Handler-level error (decode failure, op-specific errors,
    /// template/factory rejections).
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
    /// `Spawn` rejected: no template by that name in the
    /// daemon's config.
    UnknownTemplate(String),
    /// `Spawn` rejected: a group by `group_name` is already
    /// registered.
    DuplicateGroupName(String),
    /// `Spawn` rejected for a daemon-defined reason
    /// (config validation, replica spawn failed, etc.).
    /// Carries an operator-facing diagnostic.
    SpawnRejected(String),
    /// The daemon refuses dynamic `Spawn` (no spawn factory
    /// installed via `RegistryHandler::with_spawner`). Read-only
    /// daemons surface this rather than silently failing.
    SpawnNotSupported,
}

/// Async callback the [`RegistryHandler`] invokes when a
/// `Spawn` request arrives. The daemon's template-resolution
/// layer plugs in here: given `(template_name, group_name,
/// replica_count)`, build + register the group. The returned
/// summary populates `RegistryResponse::Spawned`.
///
/// Boxed so the handler stays `Sync` without leaking the
/// closure's concrete type. `'static` so the handler can move
/// the callback into the spawned `RpcHandler::call` future.
pub type SpawnFn = Box<
    dyn Fn(
            SpawnRequest,
        )
            -> futures::future::BoxFuture<'static, Result<RegistryGroupSummary, RegistryRpcError>>
        + Send
        + Sync
        + 'static,
>;

/// Argument bundle passed to a [`SpawnFn`]. Lifted into its own
/// struct so future fields (placement requirements, soft caps,
/// etc.) don't break the callback signature.
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    /// Name of a `[[template]]` block in the daemon's config.
    pub template_name: String,
    /// Operator-chosen name for the new group (registry key).
    pub group_name: String,
    /// Number of replicas to spawn.
    pub replica_count: u8,
}

/// `RpcHandler` implementation backed by a shared
/// [`AggregatorRegistry`]. Construct via
/// [`RegistryHandler::new`] and pass to
/// [`crate::adapter::net::MeshNode::serve_rpc`] under
/// [`REGISTRY_SERVICE`].
///
/// `Spawn` requests are dispatched to an optional [`SpawnFn`]
/// installed via [`RegistryHandler::with_spawner`]. Handlers
/// without a spawner reject `Spawn` requests with
/// [`RegistryRpcError::SpawnNotSupported`].
pub struct RegistryHandler {
    registry: Arc<AggregatorRegistry>,
    spawner: Option<Arc<SpawnFn>>,
}

impl RegistryHandler {
    /// Wrap a shared registry as a read-only RPC handler.
    /// `List` and `Unregister` work; `Spawn` is rejected with
    /// [`RegistryRpcError::SpawnNotSupported`].
    pub fn new(registry: Arc<AggregatorRegistry>) -> Self {
        Self {
            registry,
            spawner: None,
        }
    }

    /// Install a spawn callback so the handler accepts
    /// `Spawn` requests. Typically the daemon's template
    /// resolver — see `aggregator-daemon`'s `make_spawner`.
    pub fn with_spawner(mut self, spawner: SpawnFn) -> Self {
        self.spawner = Some(Arc::new(spawner));
        self
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
        let response = answer(&self.registry, self.spawner.as_deref(), &request).await;
        Ok(encode_response(&response))
    }
}

/// Pure-function answer logic, broken out for direct unit
/// testing without going through the RPC plumbing.
pub(crate) async fn answer(
    registry: &Arc<AggregatorRegistry>,
    spawner: Option<&SpawnFn>,
    request: &RegistryRequest,
) -> RegistryResponse {
    match request {
        RegistryRequest::List => {
            let entries = registry.entries();
            let mut groups = Vec::with_capacity(entries.len());
            for entry in entries {
                groups.push(snapshot_group(&entry).await);
            }
            RegistryResponse::Groups(groups)
        }
        RegistryRequest::Spawn {
            template_name,
            group_name,
            replica_count,
        } => {
            let Some(spawner) = spawner else {
                return RegistryResponse::Error(RegistryRpcError::SpawnNotSupported);
            };
            // Fail-fast on the duplicate name before invoking
            // the (potentially expensive) spawn callback.
            if registry.get(group_name).is_some() {
                return RegistryResponse::Error(RegistryRpcError::DuplicateGroupName(
                    group_name.clone(),
                ));
            }
            let req = SpawnRequest {
                template_name: template_name.clone(),
                group_name: group_name.clone(),
                replica_count: *replica_count,
            };
            match (spawner)(req).await {
                Ok(summary) => RegistryResponse::Spawned(summary),
                Err(e) => RegistryResponse::Error(e),
            }
        }
        RegistryRequest::Unregister { group_name } => match registry.unregister(group_name).await {
            Ok(group) => {
                group.stop().await;
                RegistryResponse::Unregistered { existed: true }
            }
            Err(_) => RegistryResponse::Unregistered { existed: false },
        },
    }
}

/// Build a wire-shaped per-group snapshot from a live
/// registry entry. Used by the `answer` path internally and
/// by daemon-side `SpawnFn` implementations to build the
/// `RegistryResponse::Spawned` payload after registration.
pub async fn snapshot_group(entry: &Arc<super::AggregatorGroupEntry>) -> RegistryGroupSummary {
    let snap = entry.snapshot().await;
    let rows = build_rows(&snap);
    RegistryGroupSummary {
        name: entry.name.clone(),
        group_seed: entry.group_seed,
        replicas: rows,
    }
}

/// Map an [`EntrySnapshot`](super::EntrySnapshot) to the wire's
/// per-replica row Vec. Pulled out so `snapshot_group` and the
/// deck-side accessor produce byte-identical replica metadata.
fn build_rows(snap: &super::EntrySnapshot) -> Vec<RegistryReplicaSummary> {
    snap.replicas
        .iter()
        .enumerate()
        .map(|(idx, replica)| {
            let health = snap.healths.get(idx).cloned().unwrap_or(
                crate::adapter::net::behavior::lifecycle::ReplicaHealth {
                    healthy: true,
                    diagnostic: None,
                },
            );
            let placement_node_id = snap.placements.get(idx).map(|p| p.node_id);
            RegistryReplicaSummary {
                generation: replica.generation(),
                healthy: health.healthy,
                diagnostic: health.diagnostic,
                placement_node_id,
            }
        })
        .collect()
}

impl AggregatorRegistry {
    /// Wrap `self` in a read-only [`RegistryHandler`] and
    /// register it against `mesh` under [`REGISTRY_SERVICE`].
    /// Returns the `ServeHandle` — drop it to tear down the
    /// registration. `Spawn` requests reject with
    /// [`RegistryRpcError::SpawnNotSupported`]; use
    /// [`Self::install_registry_service_with_spawner`] to
    /// accept dynamic deployment.
    pub fn install_registry_service(
        self: &Arc<Self>,
        mesh: &Arc<crate::adapter::net::MeshNode>,
    ) -> Result<crate::adapter::net::mesh_rpc::ServeHandle, crate::adapter::net::mesh_rpc::ServeError>
    {
        mesh.serve_rpc(
            REGISTRY_SERVICE,
            Arc::new(RegistryHandler::new(self.clone())),
        )
    }

    /// Same as [`Self::install_registry_service`] but with a
    /// dynamic-spawn callback. The daemon's template-resolution
    /// layer plugs in here so `Spawn` RPCs can deploy new
    /// groups.
    pub fn install_registry_service_with_spawner(
        self: &Arc<Self>,
        mesh: &Arc<crate::adapter::net::MeshNode>,
        spawner: SpawnFn,
    ) -> Result<crate::adapter::net::mesh_rpc::ServeHandle, crate::adapter::net::mesh_rpc::ServeError>
    {
        mesh.serve_rpc(
            REGISTRY_SERVICE,
            Arc::new(RegistryHandler::new(self.clone()).with_spawner(spawner)),
        )
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

        let response = answer(&registry, None, &RegistryRequest::List).await;
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
            other => panic!("expected Groups, got {other:?}"),
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
        let response = answer(&registry, None, &RegistryRequest::List).await;
        match response {
            RegistryResponse::Groups(groups) => assert!(groups.is_empty()),
            other => panic!("expected empty Groups, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unregister_drives_group_shutdown_and_returns_existed_true() {
        let registry = Arc::new(AggregatorRegistry::new());
        registry
            .register("agg", spawn_group("agg", 50).await)
            .expect("register");
        let response = answer(
            &registry,
            None,
            &RegistryRequest::Unregister {
                group_name: "agg".into(),
            },
        )
        .await;
        match response {
            RegistryResponse::Unregistered { existed } => assert!(existed),
            other => panic!("expected Unregistered, got {other:?}"),
        }
        assert!(registry.is_empty());
    }

    #[tokio::test]
    async fn unregister_unknown_group_returns_existed_false() {
        let registry = Arc::new(AggregatorRegistry::new());
        let response = answer(
            &registry,
            None,
            &RegistryRequest::Unregister {
                group_name: "missing".into(),
            },
        )
        .await;
        match response {
            RegistryResponse::Unregistered { existed } => assert!(!existed),
            other => panic!("expected Unregistered, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn spawn_without_spawner_returns_spawn_not_supported() {
        let registry = Arc::new(AggregatorRegistry::new());
        let response = answer(
            &registry,
            None,
            &RegistryRequest::Spawn {
                template_name: "primary".into(),
                group_name: "newgrp".into(),
                replica_count: 2,
            },
        )
        .await;
        match response {
            RegistryResponse::Error(RegistryRpcError::SpawnNotSupported) => {}
            other => panic!("expected SpawnNotSupported, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn spawn_with_spawner_round_trips_a_new_group() {
        // Test spawner ignores `template_name` and just spawns
        // a fixed-config group, then registers it. Pin that the
        // wire shape carries the freshly-registered group's
        // snapshot back to the caller.
        let registry: Arc<AggregatorRegistry> = Arc::new(AggregatorRegistry::new());
        let registry_for_spawner = registry.clone();
        let spawner: SpawnFn = Box::new(move |req: SpawnRequest| {
            let registry = registry_for_spawner.clone();
            Box::pin(async move {
                if req.template_name != "primary" {
                    return Err(RegistryRpcError::UnknownTemplate(req.template_name));
                }
                let mesh = {
                    let addr: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();
                    let cfg = crate::adapter::net::MeshNodeConfig::new(addr, [0x17u8; 32]);
                    Arc::new(
                        crate::adapter::net::MeshNode::new(
                            crate::adapter::net::identity::EntityKeypair::generate(),
                            cfg,
                        )
                        .await
                        .map_err(|e| RegistryRpcError::SpawnRejected(format!("{e:?}")))?,
                    )
                };
                let cfg = crate::adapter::net::behavior::aggregator::AggregatorConfig::new(
                    crate::adapter::net::SubnetId::GLOBAL,
                )
                .with_fold_kind(
                    crate::adapter::net::behavior::fold::capability::CapabilityFold::KIND_ID,
                )
                .with_interval(std::time::Duration::from_millis(50));
                let cfg_clone = cfg.clone();
                let mesh_clone = mesh.clone();
                let group = crate::adapter::net::behavior::lifecycle::LifecycleGroup::<
                    crate::adapter::net::behavior::aggregator::AggregatorDaemon,
                >::spawn(req.replica_count, [0xCDu8; 32], move |_idx| {
                    Arc::new(
                        crate::adapter::net::behavior::aggregator::AggregatorDaemon::new(
                            cfg_clone.clone(),
                            mesh_clone.clone(),
                        )
                        .expect("new"),
                    )
                })
                .await
                .map_err(|e| RegistryRpcError::SpawnRejected(format!("{e}")))?;
                let entry = registry
                    .register(req.group_name.clone(), group)
                    .map_err(|e| RegistryRpcError::SpawnRejected(format!("{e}")))?;
                Ok(snapshot_group(&entry).await)
            })
        });

        let response = answer(
            &registry,
            Some(&spawner),
            &RegistryRequest::Spawn {
                template_name: "primary".into(),
                group_name: "dynamic".into(),
                replica_count: 2,
            },
        )
        .await;
        match response {
            RegistryResponse::Spawned(summary) => {
                assert_eq!(summary.name, "dynamic");
                assert_eq!(summary.replicas.len(), 2);
            }
            other => panic!("expected Spawned, got {other:?}"),
        }
        // The group is now in the registry.
        assert_eq!(registry.len(), 1);
        // Cleanup via the Unregister RPC.
        let _ = answer(
            &registry,
            None,
            &RegistryRequest::Unregister {
                group_name: "dynamic".into(),
            },
        )
        .await;
    }

    #[tokio::test]
    async fn spawn_with_unknown_template_surfaces_typed_error() {
        let registry: Arc<AggregatorRegistry> = Arc::new(AggregatorRegistry::new());
        let spawner: SpawnFn = Box::new(|req: SpawnRequest| {
            Box::pin(async move { Err(RegistryRpcError::UnknownTemplate(req.template_name)) })
        });
        let response = answer(
            &registry,
            Some(&spawner),
            &RegistryRequest::Spawn {
                template_name: "nope".into(),
                group_name: "x".into(),
                replica_count: 1,
            },
        )
        .await;
        match response {
            RegistryResponse::Error(RegistryRpcError::UnknownTemplate(t)) => {
                assert_eq!(t, "nope");
            }
            other => panic!("expected UnknownTemplate, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn spawn_rejects_duplicate_group_name_before_invoking_spawner() {
        // Pre-register a group; Spawn against the same name must
        // surface DuplicateGroupName without invoking the
        // spawner (operator typo shouldn't burn an aggregator).
        let registry = Arc::new(AggregatorRegistry::new());
        registry
            .register("existing", spawn_group("existing", 50).await)
            .expect("register existing");
        let invoked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let invoked_clone = invoked.clone();
        let spawner: SpawnFn = Box::new(move |_req: SpawnRequest| {
            invoked_clone.store(true, std::sync::atomic::Ordering::Release);
            Box::pin(async { Err(RegistryRpcError::SpawnRejected("should not run".into())) })
        });
        let response = answer(
            &registry,
            Some(&spawner),
            &RegistryRequest::Spawn {
                template_name: "anything".into(),
                group_name: "existing".into(),
                replica_count: 1,
            },
        )
        .await;
        match response {
            RegistryResponse::Error(RegistryRpcError::DuplicateGroupName(n)) => {
                assert_eq!(n, "existing");
            }
            other => panic!("expected DuplicateGroupName, got {other:?}"),
        }
        assert!(
            !invoked.load(std::sync::atomic::Ordering::Acquire),
            "spawner must not be invoked on duplicate-name short-circuit"
        );
        // Cleanup.
        let g = registry.unregister("existing").await.expect("unregister");
        g.stop().await;
    }

    #[test]
    fn registry_request_response_round_trip_through_postcard() {
        // Pin the wire shape — postcard encode/decode round-trip
        // for every variant we ship.
        for req in [
            RegistryRequest::List,
            RegistryRequest::Spawn {
                template_name: "primary".into(),
                group_name: "newgrp".into(),
                replica_count: 3,
            },
            RegistryRequest::Unregister {
                group_name: "old".into(),
            },
        ] {
            let bytes = postcard::to_allocvec(&req).expect("encode req");
            let decoded: RegistryRequest = postcard::from_bytes(&bytes).expect("decode req");
            assert_eq!(req, decoded);
        }

        let group_summary = RegistryGroupSummary {
            name: "test".into(),
            group_seed: [0xCDu8; 32],
            replicas: vec![RegistryReplicaSummary {
                generation: 42,
                healthy: false,
                diagnostic: Some("stuck".into()),
                placement_node_id: Some(0xBEEF),
            }],
        };
        for resp in [
            RegistryResponse::Groups(vec![group_summary.clone()]),
            RegistryResponse::Spawned(group_summary),
            RegistryResponse::Unregistered { existed: true },
            RegistryResponse::Unregistered { existed: false },
            RegistryResponse::Error(RegistryRpcError::DecodeFailed("bad bytes".into())),
            RegistryResponse::Error(RegistryRpcError::UnknownTemplate("missing".into())),
            RegistryResponse::Error(RegistryRpcError::DuplicateGroupName("dup".into())),
            RegistryResponse::Error(RegistryRpcError::SpawnRejected("oops".into())),
            RegistryResponse::Error(RegistryRpcError::SpawnNotSupported),
        ] {
            let bytes = postcard::to_allocvec(&resp).expect("encode resp");
            let decoded: RegistryResponse = postcard::from_bytes(&bytes).expect("decode resp");
            assert_eq!(resp, decoded);
        }
    }
}
