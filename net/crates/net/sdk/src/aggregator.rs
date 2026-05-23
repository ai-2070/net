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
    DEFAULT_QUERY_CACHE_TTL, DEFAULT_QUERY_DEADLINE, DEFAULT_REGISTRY_DEADLINE, FOLD_QUERY_SERVICE,
    FoldQueryClient, FoldQueryClientError, FoldQueryError, FoldQueryOp, FoldQueryRequest,
    FoldQueryResponse, REGISTRY_SERVICE, RegistryClient, RegistryClientError, RegistryGroupSummary,
    RegistryReplicaSummary, RegistryRequest, RegistryResponse, RegistryRpcError, SpawnFn,
    SpawnRequest,
};

// ─── Daemon-author surfaces (Rust-only re-exports) ───
pub use net::adapter::net::behavior::aggregator::{
    AggregatorConfig, AggregatorDaemon, AggregatorError, AggregatorGroupEntry,
    AggregatorPublishError, AggregatorRegistry, AggregatorRegistryError, CapabilityFoldSummarizer,
    EntrySnapshot, RegistryHandler, RegistryReadHandler, ReservationFoldSummarizer,
    SummaryAnnouncement, Summarizer, snapshot_group,
};

// ─── Lifecycle primitives ───
pub use net::adapter::net::behavior::lifecycle::{
    HealthMonitor, HealthMonitorStats, LifecycleDaemon, LifecycleError, LifecycleGroup,
    LifecycleGroupError, LifecycleHandle, ReplicaContext, ReplicaHealth,
};

// ─── SDK ergonomic wrappers ───

use std::sync::Arc;
use std::time::Duration;

use ::net::adapter::net::channel::{ChannelId, ChannelName};
use ::net::adapter::net::mesh_rpc::{ServeError, ServeHandle};
use ::net::adapter::net::ChannelConfig;
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

/// Internal: mirror the SDK's `mesh_rpc::Mesh::serve_rpc`
/// auto-register pattern — register the `<service>.requests`
/// channel exactly + the `<service>.replies.` prefix entry
/// permissively. Idempotent.
fn auto_register_rpc_channels(mesh: &Mesh, service: &str) {
    if let Ok(req_channel) = ChannelName::new(&format!("{service}.requests")) {
        mesh.register_channel(ChannelConfig::new(ChannelId::new(req_channel)));
    }
    if let Ok(sentinel_name) = ChannelName::new(&format!("{service}.replies.prefix")) {
        mesh.channel_configs_arc().insert_prefix(
            format!("{service}.replies."),
            ChannelConfig::new(ChannelId::new(sentinel_name)),
        );
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
            .spawn(self.target_node_id, template_name, group_name, replica_count)
            .await
    }

    /// Unregister a group on the bound target.
    pub async fn unregister(
        &self,
        group_name: impl Into<String>,
    ) -> Result<bool, RegistryClientError> {
        self.inner
            .unregister(self.target_node_id, group_name)
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
