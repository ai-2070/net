//! Aggregator + lifecycle surfaces.
//!
//! This module is the SDK's entry-point into the substrate's
//! aggregator-daemon infrastructure: subnet-tier roll-up daemons
//! that summarize fine-grained fold state and republish on
//! broader-visibility channels, plus the lifecycle primitives
//! ([`LifecycleDaemon`], [`LifecycleGroup`], [`HealthMonitor`])
//! that host them.
//!
//! Two flavours of surface ride this module:
//!
//! ## Client surfaces (read + control)
//!
//! [`RegistryClient`] talks to a remote `net-aggregator-daemon`
//! over the `aggregator.registry` RPC service: list registered
//! groups, spawn new ones by referencing a daemon-side template,
//! unregister a group. [`FoldQueryClient`] queries an aggregator
//! for its latest summaries (with a 5 s TTL cache on `LatestSummary`
//! results) or forces a fresh `SummarizeNow` tick.
//!
//! Both clients wrap [`crate::Mesh`] — the SDK's `MeshNode`
//! handle — and run from any process that has one.
//!
//! ## Daemon-author surfaces
//!
//! Embedders that want to host aggregators inside their own
//! process (rather than running the turnkey `net-aggregator-daemon`
//! binary) reach for the substrate types directly:
//! [`AggregatorConfig`], [`AggregatorDaemon`], [`AggregatorRegistry`],
//! [`LifecycleGroup`], [`HealthMonitor`]. These are all re-exported
//! from this module so a single `use net_sdk::aggregator::*` picks
//! up everything.
//!
//! Non-Rust bindings (Node / Python / Go / C) get *client-only*
//! surfaces — the async-trait-heavy daemon-author types don't
//! cross those FFI boundaries cleanly. Operators who want a
//! non-Rust process to host aggregators run the binary alongside
//! their app and RPC into it.
//!
//! # Example: list groups on a remote daemon
//!
//! ```no_run
//! # async fn doc() -> Result<(), Box<dyn std::error::Error>> {
//! use net_sdk::aggregator::RegistryClient;
//! use net_sdk::mesh::MeshBuilder;
//!
//! let mesh = MeshBuilder::new("127.0.0.1:0", &[0x42u8; 32])?
//!     .build()
//!     .await?;
//! // Caller's responsibility: handshake against the daemon first
//! // (see `Mesh::connect`). Once connected, the registry client
//! // talks via the standard RPC plumbing.
//! let client = RegistryClient::new(mesh.node_arc());
//! let target_daemon_node_id: u64 = 0xABCD;
//! let groups = client.list(target_daemon_node_id).await?;
//! for g in groups {
//!     println!("group {} ({} replicas)", g.name, g.replicas.len());
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Wire shape contract
//!
//! Cross-language SDKs marshal the same `RegistryGroupSummary`
//! shape — see `SDK_AGGREGATOR_SUBNET_PLAN.md` § "Cross-language
//! wire contract" for the bytes-and-types table that every
//! binding honors.

// ─── Client surfaces (every binding can re-export these) ───
pub use net::adapter::net::behavior::aggregator::{
    FoldQueryClient, FoldQueryClientError, FoldQueryError, FoldQueryOp, FoldQueryRequest,
    FoldQueryResponse, RegistryClient, RegistryClientError, RegistryGroupSummary,
    RegistryReplicaSummary, RegistryRequest, RegistryResponse, RegistryRpcError, ScaleFn,
    ScaleRequest, SpawnFn, SpawnRequest, DEFAULT_QUERY_CACHE_TTL, DEFAULT_QUERY_DEADLINE,
    DEFAULT_REGISTRY_DEADLINE, FOLD_QUERY_SERVICE, REGISTRY_SERVICE,
};

// ─── Daemon-author surfaces (Rust-only re-exports) ───
pub use net::adapter::net::behavior::aggregator::{
    snapshot_group, AggregatorConfig, AggregatorDaemon, AggregatorError, AggregatorGroupEntry,
    AggregatorPublishError, AggregatorRegistry, AggregatorRegistryError, CapabilityFoldSummarizer,
    EntrySnapshot, RegistryHandler, RegistryReadHandler, ReservationFoldSummarizer, Summarizer,
    SummaryAnnouncement,
};

// ─── Lifecycle primitives ───
pub use net::adapter::net::behavior::lifecycle::{
    HealthMonitor, HealthMonitorStats, LifecycleDaemon, LifecycleError, LifecycleGroup,
    LifecycleGroupError, LifecycleHandle, ReplicaContext, ReplicaHealth,
};

// ─── SDK ergonomic wrappers ───

use std::sync::Arc;
use std::time::Duration;

use ::net::adapter::net::mesh_rpc::{ServeError, ServeHandle};

use ::net::adapter::net::MeshNode;

use crate::mesh::Mesh;

/// Install the `aggregator.registry` RPC service on a [`Mesh`]
/// — including auto-registering the request + reply-prefix
/// channels in the mesh's `ChannelConfigRegistry`. The
/// substrate's `AggregatorRegistry::install_registry_service`
/// alone doesn't touch the channel registry; for SDK-built
/// meshes (which install an empty registry by default) the
/// channels must be permissive or RPC calls reject with
/// `UnknownChannel`. This helper closes that gap.
///
/// Read-only handler — Spawn requests reply with
/// `SpawnNotSupported`. Use
/// [`install_aggregator_registry_service_with_spawner`] for
/// dynamic deployment.
pub fn install_aggregator_registry_service(
    mesh: &Mesh,
    registry: &Arc<AggregatorRegistry>,
) -> Result<ServeHandle, ServeError> {
    auto_register_rpc_channels(mesh, REGISTRY_SERVICE);
    registry.install_registry_service(&mesh.node_arc())
}

/// Same as [`install_aggregator_registry_service`] but with a
/// `SpawnFn`. Accepts dynamic deployment via `Spawn` RPC.
pub fn install_aggregator_registry_service_with_spawner(
    mesh: &Mesh,
    registry: &Arc<AggregatorRegistry>,
    spawner: SpawnFn,
) -> Result<ServeHandle, ServeError> {
    auto_register_rpc_channels(mesh, REGISTRY_SERVICE);
    registry.install_registry_service_with_spawner(&mesh.node_arc(), spawner)
}

/// Install the `fold.query` RPC service on a [`Mesh`],
/// auto-registering the request + reply-prefix channels.
/// Same rationale as
/// [`install_aggregator_registry_service`] — SDK-built meshes
/// require explicit channel registration; the substrate's
/// raw `install_query_service` doesn't do it.
pub fn install_fold_query_service(
    aggregator: &Arc<AggregatorDaemon>,
    mesh: &Mesh,
) -> Result<ServeHandle, ServeError> {
    auto_register_rpc_channels(mesh, FOLD_QUERY_SERVICE);
    aggregator.install_query_service(&mesh.node_arc())
}

/// Internal: install the standard RPC channel policy for `service`.
///
/// Installs nothing, on purpose — core does it.
///
/// This function once carried its own copy of the policy, mirroring
/// `serve_rpc`'s pattern by hand because the SDK's registry accessor is
/// `cortex`-gated while this module is not. The copy drifted: it kept
/// using replacing inserts (so it discarded operator ACLs, H2) and
/// never gained the reply-channel origin binding (so aggregator reply
/// channels stayed world-subscribable, H3), both long after those were
/// fixed for `serve_rpc`. The implementation now lives on
/// [`ChannelConfigRegistry::install_rpc_service_defaults`], and core's
/// serve seams call it, so there is nothing left for this hop to do.
fn auto_register_rpc_channels(_mesh: &Mesh, _service: &str) {
    // Deliberately empty. `install_query_service` reaches
    // `MeshNode::serve_rpc`, and core installs the policy from that
    // seam now — so pre-registering here would be a second owner of a
    // requirement that already has one. That duplication is what let
    // this copy drift in the first place.
}

#[cfg(test)]
mod aggregator_channel_registration_tests {
    //! The aggregator's own entry points, checked on the aggregator's own
    //! service names.
    //!
    //! The shared helper is pinned by tests next to it, and the
    //! delegation chain by a scan in `mesh_rpc.rs`. Neither reaches
    //! *these* three functions with *these* two service names, and this
    //! is the module whose hand-rolled copy stayed unbound through both
    //! H3 and the aggregator-specific follow-up. Assert the property
    //! where the drift actually happened.

    use super::*;
    use crate::mesh::MeshBuilder;
    use ::net::adapter::net::channel::{ChannelConfigRegistry, OriginBinding};

    async fn mesh() -> Mesh {
        MeshBuilder::new("127.0.0.1:0", &[0xA6u8; 32])
            .unwrap()
            .build()
            .await
            .unwrap()
    }

    /// `Mesh::channel_configs` is private to its module, so reach the
    /// same registry through the public node accessor.
    fn registry(mesh: &Mesh) -> Arc<ChannelConfigRegistry> {
        mesh.node_arc()
            .channel_configs()
            .expect("an SDK-built mesh always installs a registry")
            .clone()
    }

    /// Both aggregator services must end up with an origin-bound reply
    /// prefix. Unbound, any peer sharing a session with the server can
    /// subscribe to a victim's `<service>.replies.<victim_origin>` and
    /// receive that victim's response bodies whenever direct delivery
    /// misses and the reply falls back to roster fan-out.
    #[tokio::test]
    async fn both_aggregator_services_bind_their_reply_prefix() {
        let mesh = mesh().await;

        for service in [REGISTRY_SERVICE, FOLD_QUERY_SERVICE] {
            // Install through the registry primitive, which is what the
            // core serve seams now call. `auto_register_rpc_channels`
            // is deliberately empty — pre-registering here would make
            // the SDK a second owner of a requirement core already
            // enforces. What this test is really for is the aggregator
            // SERVICE NAMES: they are the longest in the tree, and a
            // name that cannot form a valid reply channel installs
            // nothing at all.
            registry(&mesh)
                .install_rpc_service_defaults(service)
                .unwrap_or_else(|e| panic!("{service}: must install: {e}"));

            let caller = format!("{service}.replies.00112233445566aa");
            let reg = registry(&mesh);
            let resolved = reg
                .resolve_by_name(&caller)
                .unwrap_or_else(|| panic!("{service}: the reply prefix must resolve for {caller}"));
            assert_eq!(
                resolved.matched_prefix.as_deref(),
                Some(format!("{service}.replies.").as_str()),
                "{service}: resolved through the wrong entry"
            );
            assert_eq!(
                resolved.config.subscriber_origin_binding,
                Some(OriginBinding::OriginHashHex16),
                "{service}: reply channels are world-subscribable (H3)"
            );
            assert!(
                reg.get_by_name(&format!("{service}.requests")).is_some(),
                "{service}: the request channel must still be installed — \
                 'fix the clobbering by registering nothing' would break RPC \
                 with UnknownChannel instead"
            );
        }
    }

    /// An operator ACL registered before the aggregator is installed
    /// must survive it (H2).
    #[tokio::test]
    async fn installing_an_aggregator_service_preserves_an_operator_acl() {
        use ::net::adapter::net::identity::EntityKeypair;
        use ::net::adapter::net::{ChannelConfig, ChannelId, ChannelName};

        let mesh = mesh().await;
        let root = EntityKeypair::generate();
        let requests = format!("{REGISTRY_SERVICE}.requests");
        mesh.register_channel(
            ChannelConfig::new(ChannelId::new(ChannelName::new(&requests).unwrap()))
                .with_token_roots(vec![root.entity_id().clone()]),
        );

        auto_register_rpc_channels(&mesh, REGISTRY_SERVICE);

        let cfg = registry(&mesh)
            .get_by_name(&requests)
            .expect("request channel must exist")
            .clone();
        assert!(
            cfg.token_required(),
            "installing the aggregator discarded the operator's ACL (H2)"
        );
        assert_eq!(cfg.token_roots[0], *root.entity_id());
    }
}

/// Ergonomic wrapper that binds a [`RegistryClient`] to a
/// specific `target_node_id` once at construction. Removes the
/// repetition of passing the same `u64` to every call.
///
/// ```no_run
/// # async fn doc() -> Result<(), Box<dyn std::error::Error>> {
/// use net_sdk::aggregator::BoundRegistryClient;
/// use net_sdk::mesh::MeshBuilder;
///
/// let mesh = MeshBuilder::new("127.0.0.1:0", &[0x42u8; 32])?
///     .build()
///     .await?;
/// let client = BoundRegistryClient::new(mesh.node_arc(), 0xABCDu64);
/// let groups = client.list().await?;
/// let spawned = client.spawn("primary", "newgrp", 3).await?;
/// client.unregister("newgrp").await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct BoundRegistryClient {
    inner: RegistryClient,
    target_node_id: u64,
}

impl BoundRegistryClient {
    /// Build a client bound to `target_node_id`. Uses
    /// [`DEFAULT_REGISTRY_DEADLINE`] for the per-call deadline;
    /// override via [`Self::with_deadline`].
    pub fn new(mesh: Arc<MeshNode>, target_node_id: u64) -> Self {
        Self {
            inner: RegistryClient::new(mesh),
            target_node_id,
        }
    }

    /// Override the per-call deadline. Builder-style — returns
    /// `Self` so calls chain.
    pub fn with_deadline(mut self, deadline: Duration) -> Self {
        self.inner = self.inner.with_deadline(deadline);
        self
    }

    /// `target_node_id` this client was bound to.
    pub fn target_node_id(&self) -> u64 {
        self.target_node_id
    }

    /// Borrow the underlying [`RegistryClient`] for operators
    /// who need to talk to multiple targets through the same
    /// underlying mesh handle.
    pub fn unbound(&self) -> &RegistryClient {
        &self.inner
    }

    /// List groups on the bound target.
    pub async fn list(&self) -> Result<Vec<RegistryGroupSummary>, RegistryClientError> {
        self.inner.list(self.target_node_id).await
    }

    /// Spawn a group on the bound target.
    pub async fn spawn(
        &self,
        template_name: impl Into<String>,
        group_name: impl Into<String>,
        replica_count: u8,
    ) -> Result<RegistryGroupSummary, RegistryClientError> {
        self.inner
            .spawn(
                self.target_node_id,
                template_name,
                group_name,
                replica_count,
            )
            .await
    }

    /// Unregister a group on the bound target.
    pub async fn unregister(
        &self,
        group_name: impl Into<String>,
    ) -> Result<bool, RegistryClientError> {
        self.inner.unregister(self.target_node_id, group_name).await
    }

    /// Resize a group on the bound target. See
    /// [`RegistryClient::scale`].
    pub async fn scale(
        &self,
        group_name: impl Into<String>,
        template_name: impl Into<String>,
        target_replica_count: u8,
    ) -> Result<RegistryGroupSummary, RegistryClientError> {
        self.inner
            .scale(
                self.target_node_id,
                group_name,
                template_name,
                target_replica_count,
            )
            .await
    }
}

/// Same shape as [`BoundRegistryClient`] for [`FoldQueryClient`].
/// Binds the `target_node_id` so callers don't repeat it.
#[derive(Clone)]
pub struct BoundFoldQueryClient {
    inner: FoldQueryClient,
    target_node_id: u64,
}

impl BoundFoldQueryClient {
    /// Build a query client bound to `target_node_id`. Uses
    /// the substrate defaults ([`DEFAULT_QUERY_CACHE_TTL`],
    /// [`DEFAULT_QUERY_DEADLINE`]); override via builders.
    pub fn new(mesh: Arc<MeshNode>, target_node_id: u64) -> Self {
        Self {
            inner: FoldQueryClient::new(mesh),
            target_node_id,
        }
    }

    /// Override the cache TTL.
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.inner = self.inner.with_ttl(ttl);
        self
    }

    /// Override the per-call deadline.
    pub fn with_deadline(mut self, deadline: Duration) -> Self {
        self.inner = self.inner.with_deadline(deadline);
        self
    }

    /// `target_node_id` this client was bound to.
    pub fn target_node_id(&self) -> u64 {
        self.target_node_id
    }

    /// Query the bound aggregator's latest cached summaries.
    pub async fn query_latest(
        &self,
        kind: u16,
    ) -> Result<Vec<SummaryAnnouncement>, FoldQueryClientError> {
        self.inner.query_latest(self.target_node_id, kind).await
    }

    /// Force a fresh `SummarizeNow` against the bound aggregator.
    pub async fn query_summarize_now(
        &self,
        kind: u16,
    ) -> Result<Vec<SummaryAnnouncement>, FoldQueryClientError> {
        self.inner
            .query_summarize_now(self.target_node_id, kind)
            .await
    }

    /// Invalidate the entire query cache.
    pub fn invalidate_cache(&self) {
        self.inner.invalidate_cache();
    }
}
