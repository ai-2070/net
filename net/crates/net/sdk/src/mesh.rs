//! Multi-peer mesh handle for the SDK.
//!
//! `Mesh` wraps [`MeshNode`] with ergonomic methods for connecting to
//! peers, sending events, and polling received events. Unlike
//! [`crate::Net`], which is backed by an `EventBus` + adapter, `Mesh`
//! manages its own encrypted peer sessions and routing.
//!
//! # Example
//!
//! ```rust,no_run
//! use net_sdk::mesh::{Mesh, MeshBuilder};
//!
//! # async fn example() -> net_sdk::error::Result<()> {
//! let mut node = Mesh::builder("127.0.0.1:9000", b"my-32-byte-preshared-key-here!!!")?
//!     .heartbeat_ms(200)
//!     .session_timeout_ms(5000)
//!     .build()
//!     .await?;
//!
//! // Connect to a peer
//! let peer_pubkey = [0u8; 32]; // get from peer.public_key()
//! node.connect("127.0.0.1:9001", &peer_pubkey, 0x2222).await?;
//! node.start();
//!
//! // Send events
//! node.send(0x2222, &serde_json::json!({"token": "hello"})).await?;
//!
//! // Poll received events
//! let events = node.recv(100).await?;
//!
//! node.shutdown().await?;
//! # Ok(())
//! # }
//! ```

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU16, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde::Serialize;

use net::adapter::net::{
    AckReason, ChannelConfig, ChannelConfigRegistry, ChannelName, ChannelPublisher, EntityKeypair,
    MeshNode, MeshNodeConfig, MigrationSubprotocolHandler, PublishConfig, PublishReport, Stream,
    StreamConfig, StreamStats,
};
use net::adapter::Adapter;
use net::event::StoredEvent;

use crate::error::{Result, SdkError};

/// Options passed to [`Mesh::subscribe_channel_with`].
///
/// Today the only knob is a presented
/// [`PermissionToken`](crate::identity::PermissionToken) — the
/// shape is a struct rather than a bare `Option<Token>` so future
/// additions (request-side timeout override, subscribe priority,
/// etc.) don't break callers.
///
/// # Round-trip shape
///
/// ```no_run
/// # use std::time::Duration;
/// # use net_sdk::{ChannelName, Identity, SubscribeOptions, TokenScope};
/// # use net_sdk::mesh::MeshBuilder;
/// # async fn example(
/// #     publisher: &net_sdk::Mesh,
/// #     publisher_identity: &Identity,
/// #     subscriber_entity_id: net::adapter::net::identity::EntityId,
/// # ) -> net_sdk::error::Result<()> {
/// // Publisher issues a SUBSCRIBE-scope token for the subscriber.
/// // The publisher's own `Mesh` is bound to an `Identity`, so the
/// // token lands in its local cache when `issue_token` is called.
/// let channel = ChannelName::new("events/trades").unwrap();
/// let token = publisher_identity.issue_token(
///     subscriber_entity_id,
///     TokenScope::SUBSCRIBE,
///     &channel,
///     Duration::from_secs(600),
///     0, // no further delegation
/// );
///
/// // Subscriber (another `Mesh`) calls `subscribe_channel_with`,
/// // attaching the same token bytes they received from the
/// // publisher out of band. The publisher verifies the signature,
/// // checks `subject == subscriber_entity_id`, installs it in its
/// // cache, then runs `can_subscribe`.
/// let subscriber: &net_sdk::Mesh = unimplemented!();
/// subscriber
///     .subscribe_channel_with(
///         publisher.node_id(),
///         &channel,
///         SubscribeOptions { token: Some(token) },
///     )
///     .await?;
/// # Ok(())
/// # }
/// ```
#[derive(Default, Debug, Clone)]
pub struct SubscribeOptions {
    /// Token to attach to the subscribe request. The publisher
    /// verifies + installs it before running
    /// `ChannelConfig::can_subscribe`, so a matching token
    /// satisfies `require_token` channels end-to-end.
    pub token: Option<net::adapter::net::PermissionToken>,
}

/// Builder for configuring a [`Mesh`] node.
pub struct MeshBuilder {
    bind_addr: SocketAddr,
    psk: [u8; 32],
    heartbeat_interval: Duration,
    session_timeout: Duration,
    num_shards: u16,
    identity: Option<crate::identity::Identity>,
    subnet: Option<net::adapter::net::SubnetId>,
    subnet_policy: Option<Arc<net::adapter::net::SubnetPolicy>>,
    subnet_authorities: Vec<crate::subnet::SubnetAuthorityConfig>,
    subnet_attachment: Option<crate::subnet::TopologySubnetId>,
    subnet_control_channel: Option<net::adapter::net::ChannelName>,
    subnet_exports: Vec<crate::subnet::NamedSubnetExport>,
    enable_sensing: bool,
    sensing_incarnation: Option<net::adapter::net::behavior::sensing::Incarnation>,
    #[cfg(feature = "nat-traversal")]
    reflex_override: Option<SocketAddr>,
    #[cfg(feature = "port-mapping")]
    try_port_mapping: bool,
    #[cfg(feature = "nat-traversal")]
    auto_direct_upgrade: bool,
}

impl MeshBuilder {
    /// Create a new builder.
    pub fn new(bind_addr: &str, psk: &[u8; 32]) -> Result<Self> {
        let addr: SocketAddr = bind_addr
            .parse()
            .map_err(|e| SdkError::Config(format!("invalid bind address: {}", e)))?;
        Ok(Self {
            bind_addr: addr,
            psk: *psk,
            heartbeat_interval: Duration::from_secs(5),
            session_timeout: Duration::from_secs(30),
            num_shards: 4,
            identity: None,
            subnet: None,
            subnet_policy: None,
            subnet_authorities: Vec::new(),
            subnet_attachment: None,
            subnet_control_channel: None,
            subnet_exports: Vec::new(),
            enable_sensing: false,
            sensing_incarnation: None,
            #[cfg(feature = "nat-traversal")]
            reflex_override: None,
            #[cfg(feature = "port-mapping")]
            try_port_mapping: false,
            #[cfg(feature = "nat-traversal")]
            auto_direct_upgrade: true,
        })
    }

    /// Pin this node to a caller-owned [`Identity`](crate::Identity).
    ///
    /// Without this call, `build()` generates an ephemeral keypair —
    /// fine for one-off sessions, but every restart produces a new
    /// entity id (and therefore a new node id). Provide an identity
    /// loaded from disk / vault / enclave to keep stable addressing.
    ///
    /// The identity's [`TokenCache`](crate::identity::TokenCache) is
    /// also bound to this mesh; tokens installed via
    /// [`Identity::install_token`](crate::identity::Identity::install_token)
    /// become available to the channel auth path at subscribe time.
    pub fn identity(mut self, identity: crate::identity::Identity) -> Self {
        self.identity = Some(identity);
        self
    }

    /// Set heartbeat interval in milliseconds.
    pub fn heartbeat_ms(mut self, ms: u64) -> Self {
        self.heartbeat_interval = Duration::from_millis(ms);
        self
    }

    /// Set session timeout in milliseconds.
    pub fn session_timeout_ms(mut self, ms: u64) -> Self {
        self.session_timeout = Duration::from_millis(ms);
        self
    }

    /// Set number of inbound shards.
    pub fn shards(mut self, n: u16) -> Self {
        self.num_shards = n;
        self
    }

    /// Pin this node to a specific subnet. Defaults to
    /// [`SubnetId::GLOBAL`](crate::subnets::SubnetId) — no
    /// restriction. Visibility checks on the publish + subscribe
    /// paths compare against this value.
    pub fn subnet(mut self, id: net::adapter::net::SubnetId) -> Self {
        self.subnet = Some(id);
        self
    }

    /// Install a subnet policy that derives each peer's subnet from
    /// their capability announcement. Mesh-wide policy consistency
    /// is assumed — mismatched policies across nodes lead to
    /// asymmetric views of peer subnets.
    ///
    /// Accepts either an owned `SubnetPolicy` or an `Arc<SubnetPolicy>`
    /// via blanket `Into` support — useful when several builders
    /// share one policy at node construction time.
    pub fn subnet_policy(
        mut self,
        policy: impl Into<Arc<net::adapter::net::SubnetPolicy>>,
    ) -> Self {
        self.subnet_policy = Some(policy.into());
        self
    }

    /// Trust a subnet AUTHORITY (SUBNET_AUTH_SDK_PLAN.md §3.3).
    /// Repeatable — one config per authority; duplicates, empty root
    /// sets, duplicate roots, and zero lifetimes fail at `build()`,
    /// before any node exists. An empty overall set means every
    /// protected subnet assertion fails closed.
    pub fn subnet_authority(mut self, config: crate::subnet::SubnetAuthorityConfig) -> Self {
        self.subnet_authorities.push(config);
        self
    }

    /// This node's own SECURITY attachment point — the local topology
    /// coordinate credentials are checked against. Distinct from
    /// [`subnet`](Self::subnet), which remains unauthenticated routing
    /// state; omitting this preserves the core compatibility fallback
    /// (attachment = topology subnet), which protected deployments
    /// should not rely on.
    pub fn subnet_attachment(mut self, path: crate::subnet::TopologySubnetId) -> Self {
        self.subnet_attachment = Some(path);
        self
    }

    /// Treat an ordinary configured channel as a subnet control-fact
    /// ARRIVAL PATH. Confers no authority: facts verify by signature
    /// regardless of how they arrive.
    pub fn subnet_control_channel(mut self, channel: net::adapter::net::ChannelName) -> Self {
        self.subnet_control_channel = Some(channel);
        self
    }

    /// Configure a NAMED subnet export (SUBNET_AUTH_SDK_PLAN.md §3.3):
    /// a provider-local label an application later serves against with
    /// [`Mesh::serve_subnet_exported`], resolved once here into a
    /// checked access mode + export binding. The name is neither
    /// announced nor accepted from callers. Repeatable; empty or
    /// duplicate names fail at `build()`.
    pub fn subnet_export(
        mut self,
        name: impl Into<String>,
        access: crate::subnet::SubnetExportAccess,
        subnet: crate::subnet::SubnetRef,
        topology_epoch: u32,
    ) -> Self {
        self.subnet_exports.push(crate::subnet::NamedSubnetExport {
            name: name.into(),
            access,
            subnet,
            topology_epoch,
        });
        self
    }

    /// Turn on the capability-sensing plane
    /// (`docs/internal/plans/CAPABILITY_SENSING_SDK_INTEGRATION_PLAN.md`
    /// §4.5). Off by default — sensing ships dark, and
    /// [`Mesh::sensing`] refuses loudly until this is called.
    ///
    /// This alone makes the node a sensing consumer/relay. SERVING
    /// readiness additionally needs
    /// [`sensing_incarnation`](Self::sensing_incarnation).
    pub fn enable_sensing(mut self) -> Self {
        self.enable_sensing = true;
        self
    }

    /// Supply this node's persisted sensing
    /// [`Incarnation`](crate::sensing::Incarnation) — the provider boot
    /// epoch its signed readiness sequence is ordered under.
    ///
    /// Required to SERVE readiness, and fail-closed without it:
    /// [`SensingClient::provide`](crate::sensing::SensingClient::provide)
    /// refuses rather than installing an evaluator on a node that can
    /// never sign. Derive the value with
    /// [`next_incarnation`](crate::sensing::next_incarnation) over real
    /// durable storage BEFORE building the mesh; a per-boot random
    /// value cannot be ordered and would be poisoned as equivocation
    /// after a restart.
    pub fn sensing_incarnation(
        mut self,
        incarnation: net::adapter::net::behavior::sensing::Incarnation,
    ) -> Self {
        self.sensing_incarnation = Some(incarnation);
        self
    }

    /// Pin this mesh's publicly-advertised reflex `SocketAddr` to
    /// the supplied external address. The classifier's background
    /// sweep is skipped; the node starts in `NatClass::Open` with
    /// `reflex_addr = Some(external)` on outbound capability
    /// announcements.
    ///
    /// Intended for:
    ///
    /// - **Port-forwarded servers.** An operator who has manually
    ///   configured a port forward knows the external address and
    ///   shouldn't wait on peer-probing to discover it.
    /// - **Stage-4 port mapping (UPnP / NAT-PMP / PCP).** A
    ///   successful mapping installation will set this on behalf
    ///   of the caller.
    ///
    /// **Optimization, not correctness.** Nodes without an
    /// override still reach every peer through the routed-
    /// handshake path — the override just cuts the classifier
    /// round-trip when the answer is already known.
    ///
    /// Requires the `nat-traversal` cargo feature.
    #[cfg(feature = "nat-traversal")]
    pub fn reflex_override(mut self, external: SocketAddr) -> Self {
        self.reflex_override = Some(external);
        self
    }

    /// Opt into opportunistic UPnP-IGD / NAT-PMP / PCP port
    /// mapping at startup. When set, the mesh spawns a
    /// [`PortMapperTask`](net::adapter::net::traversal::portmap)
    /// during `start()` that:
    ///
    /// 1. Probes NAT-PMP against the OS-discovered default
    ///    gateway (1 s deadline).
    /// 2. Falls back to UPnP via SSDP discovery (2 s deadline).
    /// 3. On install success, pins the reflex override to the
    ///    mapped external address — the mesh advertises itself
    ///    as `Open` to peers without a classifier round-trip.
    /// 4. Renews every 30 min; revokes on shutdown; falls back
    ///    to the classifier on three consecutive renewal
    ///    failures.
    ///
    /// **Optimization, not correctness.** A node whose router
    /// doesn't speak UPnP / NAT-PMP still reaches every peer
    /// through the routed-handshake path. Off by default
    /// because port mapping modifies state on the operator's
    /// router.
    ///
    /// Requires the `port-mapping` cargo feature. The flag is
    /// silently ignored when the feature is off.
    #[cfg(feature = "port-mapping")]
    pub fn try_port_mapping(mut self, enabled: bool) -> Self {
        self.try_port_mapping = enabled;
        self
    }

    /// Enable the background direct-path upgrade: once a session
    /// to a peer is established via a relay, the mesh
    /// opportunistically re-handshakes over a direct path and
    /// migrates the session, cutting relay hops out of the data
    /// plane. The swap is guarded by the migration contract
    /// (lower-id initiator, compare-and-swap install, busy-session
    /// deferral) so in-flight work is never dropped.
    ///
    /// **Optimization, not correctness** — traffic rides the relay
    /// until (and unless) a direct path lands. On by default; pass
    /// `false` to pin traffic to the relay path.
    ///
    /// Requires the `nat-traversal` cargo feature.
    #[cfg(feature = "nat-traversal")]
    pub fn auto_direct_upgrade(mut self, enabled: bool) -> Self {
        self.auto_direct_upgrade = enabled;
        self
    }

    /// Build the mesh node.
    pub async fn build(self) -> Result<Mesh> {
        // Use the caller's identity if one was set, otherwise mint an
        // ephemeral one. `MeshNode::new` takes the keypair by value,
        // so clone it out of the Arc when we have a shared identity.
        let (keypair, sdk_identity) = match self.identity {
            Some(id) => (id.keypair().as_ref().clone(), Some(id)),
            None => (EntityKeypair::generate(), None),
        };

        let mut config = MeshNodeConfig::new(self.bind_addr, self.psk)
            .with_heartbeat_interval(self.heartbeat_interval)
            .with_session_timeout(self.session_timeout)
            .with_num_shards(self.num_shards)
            .with_handshake(3, Duration::from_secs(5));
        // OSDK-L N: record whether the CALLER supplied the identity. The org
        // facade refuses to bind credentials to a generated fallback, and a
        // binding holding only `Arc<MeshNode>` cannot otherwise tell.
        config.configured_identity = sdk_identity.is_some();
        if let Some(id) = self.subnet {
            config = config.with_subnet(id);
        }
        if let Some(policy) = self.subnet_policy {
            config = config.with_subnet_policy(policy);
        }
        // SSDK §3.3 — the authority plane. Every configuration mistake
        // fails HERE, before any node exists; the named-export map is
        // resolved once and frozen beside the handle.
        crate::subnet::validate_subnet_authorities(&self.subnet_authorities)
            .map_err(|e| SdkError::Config(e.to_string()))?;
        for authority in self.subnet_authorities {
            config = config.with_subnet_authority(authority);
        }
        if let Some(attachment) = self.subnet_attachment {
            // Direct field write: the core deliberately has no
            // `with_subnet_attachment` (the `configured_identity`
            // precedent).
            config.subnet_attachment = Some(attachment);
        }
        if let Some(channel) = self.subnet_control_channel {
            config = config.with_subnet_control_channel(channel);
        }
        // Review-10 P1-6: the checked map is the NODE's, resolved and
        // frozen by `MeshNode::new`. Pushing the entries into the config
        // rather than building a second map here is what keeps Rust,
        // Node, Python, Go, and C resolving one name against one map.
        // Empty and duplicate labels still fail before the node exists —
        // just one layer down.
        for export in self.subnet_exports {
            config = config.with_subnet_export(export);
        }
        // Sensing (§4.5). The incarnation is passed through
        // independently of the master switch: core keeps the origin
        // role fail-closed on the pair, and `Mesh::sensing()` /
        // `SensingClient::provide` report the missing half by name
        // rather than leaving the plane silently dark.
        if self.enable_sensing {
            config = config.with_sensing_coalescing(true);
        }
        if let Some(incarnation) = self.sensing_incarnation {
            config = config.with_sensing_incarnation(incarnation);
        }
        #[cfg(feature = "nat-traversal")]
        if let Some(external) = self.reflex_override {
            config = config.with_reflex_override(external);
        }
        #[cfg(feature = "port-mapping")]
        if self.try_port_mapping {
            config = config.with_try_port_mapping(true);
        }
        // Unconditional pass-through, not `if self.auto_direct_upgrade`:
        // the flag now defaults on, so an opt-in-only plumbing would make
        // `.auto_direct_upgrade(false)` a silent no-op.
        #[cfg(feature = "nat-traversal")]
        {
            config = config.with_auto_direct_upgrade(self.auto_direct_upgrade);
        }

        let mut node = MeshNode::new(keypair, config).await?;
        // Install a shared ChannelConfigRegistry so `register_channel`
        // can add entries without needing `&mut Mesh` — the registry
        // itself uses interior mutability (DashMap).
        let channel_configs = Arc::new(ChannelConfigRegistry::new());
        node.set_channel_configs(channel_configs.clone());
        // Hand the caller's TokenCache to the mesh so channel auth
        // (`require_token` / `can_subscribe` / `can_publish`) has one
        // to consult: it supplies the RevocationRegistry, the
        // clock-skew tolerance, and the local node's own credentials
        // (the PUBLISH token the publish path resolves via
        // `get_for_action`). Incoming subscriber tokens are NOT
        // installed into it — they are verified inline against the
        // channel's `token_roots` and retained as chains.
        //
        // Without an identity no cache is installed, and
        // `require_token` channels reject — at subscribe, not silently
        // at first publish (M4).
        if let Some(id) = sdk_identity.as_ref() {
            node.set_token_cache(id.token_cache().clone());
        }
        Ok(Mesh {
            node: Arc::new(node),
            channel_configs,
            recv_cursor: Arc::new(AtomicU16::new(0)),
            identity: sdk_identity,
            #[cfg(feature = "tool")]
            tool_metadata_fetch: Arc::new(parking_lot::Mutex::new(None)),
            #[cfg(feature = "tool")]
            tool_watch: Arc::new(parking_lot::Mutex::new(None)),
        })
    }
}

/// A multi-peer mesh node.
///
/// Manages encrypted connections to multiple peers over a single UDP
/// socket. Supports direct peer-to-peer sends, routed multi-hop sends,
/// automatic failure detection, and rerouting.
pub struct Mesh {
    /// Shared `MeshNode`. `Arc` rather than by-value so NAPI /
    /// FFI bindings can hand the same live node to multiple
    /// wrappers (e.g. a `DaemonRuntime` alongside the existing
    /// `NetMesh` class) without double-owning the underlying
    /// socket. All public methods go through `.inner()` (Arc
    /// deref), so holding the `Arc` changes no existing call
    /// sites.
    node: Arc<MeshNode>,
    /// Channel config registry shared with the underlying `MeshNode`
    /// so `register_channel` / subscriber ACL checks operate on the
    /// same live data.
    channel_configs: Arc<ChannelConfigRegistry>,
    /// Which shard [`Mesh::recv`] starts its sweep from, advanced once
    /// per call.
    ///
    /// Without it, `recv` always starts at shard 0 and stops once it
    /// has `limit` events — so a continuously-fed shard 0 means shards
    /// 1..n are never reached. That is the same starvation as the
    /// shard-0-only bug this replaced, just needing sustained load to
    /// show up. Rotating the start makes every shard the head of the
    /// sweep within `num_shards` calls.
    ///
    /// `Relaxed`: a fairness hint, not a synchronization point. Two
    /// concurrent `recv`s racing to the same start offset costs one
    /// round of fairness, nothing more.
    recv_cursor: Arc<AtomicU16>,
    /// Held onto so the caller's `TokenCache` (and future capability
    /// announcement state) stays alive for the mesh's lifetime —
    /// `MeshNode` was already handed a clone of the keypair, so this
    /// is purely for the auxiliary state that rides alongside.
    identity: Option<crate::identity::Identity>,
    /// Lazy auto-install state for the `tool.metadata.fetch` nRPC
    /// service. The first `Mesh::serve_tool` call locks this
    /// mutex, sees `None`, installs the handler, and stores
    /// `Some(handle)`. Subsequent `serve_tool` calls see `Some(_)`
    /// and skip the install. The handle lives for the lifetime of
    /// the `Mesh`; the service stays registered even after every
    /// individual tool ServeHandle is dropped (low cost — the
    /// handler just answers `NotFound` for every name once the
    /// registry is empty again).
    ///
    /// `pub(crate)` so the SDK's `tool` module — which lives in a
    /// separate file but the same crate — can reach this slot
    /// without an accessor stub.
    #[cfg(feature = "tool")]
    pub(crate) tool_metadata_fetch: Arc<parking_lot::Mutex<Option<crate::mesh_rpc::ServeHandle>>>,
    /// Lazy auto-install state for the `tool.watch` streaming nRPC
    /// service (RT-6 remote watch). Same lifecycle as
    /// `tool_metadata_fetch`: first installer through the mutex
    /// wins, the handle lives for the lifetime of the `Mesh`, and
    /// the service is harmless while no tools are registered (a
    /// subscriber just sees no `Change` frames until one appears).
    /// Installed by every `serve_tool*` path and by the explicit
    /// [`Mesh::serve_tool_watch`].
    #[cfg(feature = "tool")]
    pub(crate) tool_watch: Arc<parking_lot::Mutex<Option<crate::mesh_rpc::ServeHandle>>>,
}

impl Mesh {
    /// Create a builder.
    pub fn builder(bind_addr: &str, psk: &[u8; 32]) -> Result<MeshBuilder> {
        MeshBuilder::new(bind_addr, psk)
    }

    /// Get this node's Noise public key.
    ///
    /// Share this with peers so they can connect to this node.
    pub fn public_key(&self) -> &[u8; 32] {
        self.node.public_key()
    }

    /// Get this node's ID (derived from ed25519 identity).
    pub fn node_id(&self) -> u64 {
        self.node.node_id()
    }

    /// This node's `origin_hash` — the value the bus stamps on outbound
    /// nRPC calls, which a remote handler reads as
    /// `RpcContext::caller_origin`. Use it to admit *this* node in a
    /// caller-scoped authorization check (it is NOT the same as
    /// [`node_id`](Self::node_id)); e.g. `net wrap`'s owner-only default
    /// admits the wrapping node's own `origin_hash`.
    pub fn origin_hash(&self) -> u64 {
        self.node.origin_hash()
    }

    /// Get the local bind address.
    pub fn local_addr(&self) -> SocketAddr {
        self.node.local_addr()
    }

    /// Install (or clear with `None`) the caller-side nRPC
    /// observer for this `Mesh`. Fires on every `call_typed`
    /// completion (success / server error / timeout / transport
    /// error) with a typed [`crate::mesh_rpc::RpcCallEvent`].
    /// See `DECK_DEMO_HARNESS_PLAN.md` Missing Item D for the
    /// design rationale.
    ///
    /// Replaces any previously-installed observer.
    /// Observers run inline on the dispatch task; implementations
    /// must be cheap (push into a bounded ring / mpsc, not
    /// block).
    ///
    /// v1 fires only `RpcDirection::Outbound`; server-side
    /// (inbound) firing is a follow-up.
    #[cfg(feature = "cortex")]
    pub fn set_rpc_observer(&self, observer: Option<crate::mesh_rpc::RpcObserverHandle>) {
        self.node.set_rpc_observer(observer);
    }

    /// Crate-internal accessor for the underlying `MeshNode`.
    /// Used by `mesh_rpc` to delegate the typed RPC API; not
    /// intended for downstream consumers (the public surface
    /// stays on `Mesh` itself). Gated on the same feature
    /// combination as its sole consumer (`mesh_rpc` /
    /// `mesh_rpc_resilience`) so feature combinations that
    /// exclude either don't trip dead-code lints.
    pub(crate) fn node(&self) -> &Arc<MeshNode> {
        &self.node
    }

    /// Connect to a peer as initiator.
    ///
    /// The peer must be listening (call `accept()` on their side).
    /// `peer_pubkey` is the peer's Noise public key from `public_key()`.
    pub async fn connect(
        &self,
        peer_addr: &str,
        peer_pubkey: &[u8; 32],
        peer_node_id: u64,
    ) -> Result<()> {
        let addr: SocketAddr = peer_addr
            .parse()
            .map_err(|e| SdkError::Config(format!("invalid peer address: {}", e)))?;
        self.node.connect(addr, peer_pubkey, peer_node_id).await?;
        Ok(())
    }

    /// Accept an incoming connection as responder.
    ///
    /// Waits for a peer to initiate a Noise handshake.
    /// Returns the peer's address.
    pub async fn accept(&self, peer_node_id: u64) -> Result<SocketAddr> {
        let (addr, _) = self.node.accept(peer_node_id).await?;
        Ok(addr)
    }

    /// Connect to a peer when the responder is already
    /// `start()`ed and hasn't pre-`accept()`'d this initiator's
    /// `node_id` — the standard "remote-attach against a running
    /// daemon" case. Mirror of [`Self::connect`] for that
    /// scenario; the local side must also have `start()` called
    /// before this method (the dispatch loop is what completes
    /// the handshake).
    ///
    /// `relay_addr` is the wire address to send msg1 to. The
    /// degenerate single-hop case (relay == final destination)
    /// is the CLI remote-attach pattern; the multi-hop case
    /// (relay forwards to dest) is the same call signature.
    /// Either way the destination's running dispatch loop
    /// receives msg1 via the routed-handshake protocol and
    /// replies with msg2.
    ///
    /// # Why a separate method from `connect`?
    ///
    /// `connect` uses the direct-handshake protocol, where the
    /// responder must pre-register the initiator's `node_id`
    /// via `accept()` before its `start()`. `connect_via` uses
    /// the routed-handshake protocol — the initiator's full
    /// `node_id` rides inside the Noise msg1 payload, so the
    /// responder learns it on demand. No pre-`accept` needed.
    pub async fn connect_via(
        &self,
        relay_addr: &str,
        peer_pubkey: &[u8; 32],
        peer_node_id: u64,
    ) -> Result<()> {
        let addr: SocketAddr = relay_addr
            .parse()
            .map_err(|e| SdkError::Config(format!("invalid relay address: {}", e)))?;
        self.node
            .connect_via(addr, peer_pubkey, peer_node_id)
            .await?;
        Ok(())
    }

    /// Start the receive loop, heartbeat sender, and router.
    ///
    /// Call this after connecting to peers. Events won't be received
    /// until `start()` is called.
    pub fn start(&self) {
        // `start_arc` (vs bare `start`) enables the periodic capability
        // re-announce, keeping this node's entry alive in its own and
        // peers' folds past one announcement TTL.
        self.node.start_arc();
    }

    /// Number of connected peers.
    pub fn peer_count(&self) -> usize {
        self.node.peer_count()
    }

    // ---- Sending ----

    /// Send a serializable event to a direct peer.
    pub async fn send_to(&self, peer_addr: &str, event: &impl Serialize) -> Result<()> {
        let addr: SocketAddr = peer_addr
            .parse()
            .map_err(|e| SdkError::Config(format!("invalid address: {}", e)))?;
        let json = serde_json::to_vec(event)?;
        let batch = net::event::Batch {
            shard_id: 0,
            events: vec![net::event::InternalEvent::new(Bytes::from(json), 0, 0)],
            sequence_start: 0,
            process_nonce: net::event::batch_process_nonce(),
        };
        self.node.send_to_peer(addr, &batch).await?;
        Ok(())
    }

    /// Send a serializable event via the routing table.
    ///
    /// The event is encrypted for the destination and forwarded through
    /// intermediate nodes if needed. Requires a route to `dest_node_id`
    /// in the routing table and a session with the destination.
    pub async fn send(&self, dest_node_id: u64, event: &impl Serialize) -> Result<()> {
        let json = serde_json::to_vec(event)?;
        let batch = net::event::Batch {
            shard_id: 0,
            events: vec![net::event::InternalEvent::new(Bytes::from(json), 0, 0)],
            sequence_start: 0,
            process_nonce: net::event::batch_process_nonce(),
        };
        self.node.send_routed(dest_node_id, &batch).await?;
        Ok(())
    }

    /// Send raw bytes to a direct peer.
    pub async fn send_raw_to(&self, peer_addr: &str, data: &[u8]) -> Result<()> {
        let addr: SocketAddr = peer_addr
            .parse()
            .map_err(|e| SdkError::Config(format!("invalid address: {}", e)))?;
        let batch = net::event::Batch {
            shard_id: 0,
            events: vec![net::event::InternalEvent::new(
                Bytes::copy_from_slice(data),
                0,
                0,
            )],
            sequence_start: 0,
            process_nonce: net::event::batch_process_nonce(),
        };
        self.node.send_to_peer(addr, &batch).await?;
        Ok(())
    }

    // ---- Receiving ----

    /// Poll for received events across **every** shard.
    ///
    /// Returns up to `limit` events in total, drawing from each shard
    /// in turn.
    ///
    /// This previously polled shard 0 only, on the reasoning that
    /// "most events land here for single-stream sends". They do not:
    /// inbound traffic is placed on `stream_id % num_shards`, so at
    /// the default of four shards a caller saw only the stream ids
    /// congruent to 0 and silently missed three quarters of them. The
    /// doc comment already claimed all shards; the body now matches
    /// it.
    ///
    /// **The sweep starts from a rotating shard**, advanced once per
    /// call. Starting at 0 every time and stopping at `limit` is the
    /// same starvation in a thinner disguise: a continuously-fed shard
    /// 0 that can fill the limit alone means shards 1..n are never
    /// reached, so a quiet stream on shard 3 stays invisible for as
    /// long as shard 0 keeps up. Rotating bounds that wait at
    /// `num_shards` calls.
    ///
    /// A consequence worth knowing: events are not returned in shard
    /// order, and the shard a sweep starts from changes between calls.
    /// Use [`Self::recv_shard`] with [`Self::shard_for_stream`] when
    /// you want one specific stream and not a merge.
    pub async fn recv(&self, limit: usize) -> Result<Vec<StoredEvent>> {
        let shards = self.node.num_shards().max(1);
        // `fetch_add` wraps at u16::MAX; `start` is reduced modulo
        // `shards` at use, so a wrap costs at most one round of
        // fairness — the same as the concurrent-caller race above.
        let start = self.recv_cursor.fetch_add(1, AtomicOrdering::Relaxed) % shards;
        let mut out = Vec::new();
        for offset in 0..shards {
            if out.len() >= limit {
                break;
            }
            let shard = (start + offset) % shards;
            let remaining = limit - out.len();
            let result = self.node.poll_shard(shard, None, remaining).await?;
            out.extend(result.events);
        }
        Ok(out)
    }

    /// Poll a specific shard for events.
    ///
    /// Pair with [`Self::shard_for_stream`]: polling an arbitrary
    /// shard for a known stream id is how the shard-0 assumption went
    /// unnoticed.
    pub async fn recv_shard(&self, shard_id: u16, limit: usize) -> Result<Vec<StoredEvent>> {
        let result = self.node.poll_shard(shard_id, None, limit).await?;
        Ok(result.events)
    }

    /// Number of shards inbound stream traffic is spread across.
    pub fn num_shards(&self) -> u16 {
        self.node.num_shards()
    }

    /// The inbound shard `stream_id`'s events land on.
    ///
    /// `mesh.recv_shard(mesh.shard_for_stream(id), limit)` is the
    /// targeted read; [`Self::recv`] is the merge.
    pub fn shard_for_stream(&self, stream_id: u64) -> u16 {
        self.node.shard_for_stream(stream_id)
    }

    // ---- Channels (distributed pub/sub) ----

    /// Register a channel on this publisher. Subscribers who ask to
    /// join are validated against `config` before being added to the
    /// subscriber roster.
    ///
    /// `config.channel_id` must be built from the same canonical name
    /// subscribers pass to `subscribe_channel`. The registry keys on
    /// the canonical name (not the u16 hash) to avoid ACL bypass via
    /// hash collision.
    ///
    /// Idempotent: re-registering the same channel replaces the prior
    /// config.
    pub fn register_channel(&self, config: ChannelConfig) {
        self.channel_configs.insert(config);
    }

    /// Register a **prefix-matched** channel config. Any channel name
    /// starting with `prefix` that has no exact-match entry resolves
    /// to `config`; when several prefixes match, the longest wins.
    ///
    /// Use this to install an ACL over a family of dynamically-named
    /// channels — notably nRPC's per-caller reply channels, where
    /// `<service>.replies.` covers every
    /// `<service>.replies.<caller_origin>` without pre-registering
    /// each one.
    ///
    /// Registering a prefix ACL **before** `serve_rpc*` is the
    /// supported way to override nRPC's permissive defaults: auto-
    /// registration installs its default only when the prefix is
    /// otherwise unclaimed, so an entry made here survives.
    ///
    /// Token gates on a prefix entry are evaluated against the
    /// **requested concrete channel**, not the sentinel
    /// `config.channel_id`. So a grant stays bound per channel: a token
    /// minted for `<prefix><a>` does not authorize `<prefix><b>`, and a
    /// token minted for the sentinel authorizes nothing real. Mint
    /// per-channel tokens; a sentinel-scoped one is not a broader key,
    /// it is a useless one.
    ///
    /// Idempotent: re-registering the same prefix replaces the prior
    /// config.
    pub fn register_channel_prefix(&self, prefix: impl Into<String>, config: ChannelConfig) {
        self.channel_configs.insert_prefix(prefix, config);
    }

    /// Ask `publisher_node_id` to add this node to `channel`'s
    /// subscriber set. Blocks until the publisher's `Ack` arrives or
    /// the mesh's membership-ack timeout elapses.
    ///
    /// Returns `Ok(())` on acceptance; rejection (unauthorized /
    /// unknown channel / rate-limited / too-many-channels) surfaces
    /// as `SdkError::ChannelRejected(reason)`. Network-level failures
    /// surface as `SdkError::Adapter(...)`.
    ///
    /// This bare form presents no credential. On a **token-gated**
    /// channel it is always rejected — the publisher requires a token
    /// chain on *every* subscribe and does not honor a credential
    /// presented on a previous subscribe (e.g. before a reconnect or
    /// roster eviction). Re-subscribe with
    /// [`Self::subscribe_channel_with`] carrying the token each time.
    pub async fn subscribe_channel(
        &self,
        publisher_node_id: u64,
        channel: &ChannelName,
    ) -> Result<()> {
        self.subscribe_channel_with(publisher_node_id, channel, SubscribeOptions::default())
            .await
    }

    /// Subscribe with options — optionally presenting a
    /// [`PermissionToken`](crate::identity::PermissionToken).
    ///
    /// Use this when the publisher registered the channel with
    /// `token_roots` (token enforcement) and/or a `subscribe_caps`
    /// filter that your node's capabilities alone don't satisfy. The
    /// publisher verifies the presented token chain on arrival — it
    /// must root at one of the channel's `token_roots`, bind at its
    /// leaf to the subscribing peer's `EntityId`, and authorize
    /// `SUBSCRIBE` at every link — then retains it to re-check expiry
    /// and revocation while the subscription lives.
    ///
    /// The credential must be presented on **every** subscribe: a
    /// previously-accepted chain is not reused for a later bare
    /// [`Self::subscribe_channel`], so after a reconnect or roster
    /// eviction you must call this again with the token.
    pub async fn subscribe_channel_with(
        &self,
        publisher_node_id: u64,
        channel: &ChannelName,
        opts: SubscribeOptions,
    ) -> Result<()> {
        let result = match opts.token {
            Some(token) => {
                self.node
                    .subscribe_channel_with_token(publisher_node_id, channel.clone(), token)
                    .await
            }
            None => {
                self.node
                    .subscribe_channel(publisher_node_id, channel.clone())
                    .await
            }
        };
        match result {
            Ok(()) => Ok(()),
            Err(e) => Err(adapter_to_channel_error(e)),
        }
    }

    /// Mirror of [`Self::subscribe_channel`]. Idempotent on the
    /// publisher side — unsubscribing a non-subscriber still returns
    /// `Ok(())`.
    pub async fn unsubscribe_channel(
        &self,
        publisher_node_id: u64,
        channel: &ChannelName,
    ) -> Result<()> {
        match self
            .node
            .unsubscribe_channel(publisher_node_id, channel.clone())
            .await
        {
            Ok(()) => Ok(()),
            Err(e) => Err(adapter_to_channel_error(e)),
        }
    }

    /// Publish one payload to every subscriber of `channel`.
    /// `config.on_failure` controls whether per-peer errors
    /// short-circuit the fan-out. Returns a [`PublishReport`]
    /// describing per-peer outcomes.
    pub async fn publish(
        &self,
        channel: &ChannelName,
        payload: Bytes,
        config: PublishConfig,
    ) -> Result<PublishReport> {
        let publisher = ChannelPublisher::new(channel.clone(), config);
        Ok(self.node.publish(&publisher, payload).await?)
    }

    /// Fan multiple payloads to every subscriber of `channel` as one
    /// batch per subscriber. Semantics match [`Self::publish`].
    pub async fn publish_many(
        &self,
        channel: &ChannelName,
        payloads: &[Bytes],
        config: PublishConfig,
    ) -> Result<PublishReport> {
        let publisher = ChannelPublisher::new(channel.clone(), config);
        Ok(self.node.publish_many(&publisher, payloads).await?)
    }

    // ---- Routing ----

    /// Add an ORDINARY route to a destination node.
    ///
    /// This installs a *candidate*, not an override. Each destination
    /// holds at most one ordinary candidate (manual installs like this
    /// one, plus pingwave-learned routes) and at most one protected
    /// candidate (identity-bound, learned from an authenticated
    /// adjacent session). Forwarding selects the lowest-metric live
    /// candidate, and on an equal metric the protected one wins.
    ///
    /// So packets to `dest_node_id` follow `next_hop_addr` only while
    /// no better — or equally good, identity-bound — candidate exists.
    /// A manual route cannot displace an authenticated direct
    /// adjacency, and cannot make protected forwarding resolve at all:
    /// only an authenticated learning path installs a protected
    /// candidate. Calling this with a route the mesh already knows
    /// better is a no-op in effect, not an error.
    ///
    /// Remove it with [`Self::remove_route`], which removes the
    /// ordinary candidate this installed and leaves authenticated
    /// state alone.
    pub fn add_route(&self, dest_node_id: u64, next_hop_addr: &str) -> Result<()> {
        let addr: SocketAddr = next_hop_addr
            .parse()
            .map_err(|e| SdkError::Config(format!("invalid address: {}", e)))?;
        self.node.router().add_route(dest_node_id, addr);
        Ok(())
    }

    /// Remove the ordinary route added by [`Self::add_route`].
    ///
    /// Removes only the manual/ordinary candidate. Routes the mesh
    /// learned for itself — and any authenticated (protected)
    /// candidate — are unaffected, so this cannot be used to strip a
    /// destination's authenticated state.
    pub fn remove_route(&self, dest_node_id: u64) {
        self.node.router().remove_route(dest_node_id);
    }

    // ---- Mesh topology ----

    /// Block a peer (simulate network partition).
    pub fn block_peer(&self, peer_addr: &str) -> Result<()> {
        let addr: SocketAddr = peer_addr
            .parse()
            .map_err(|e| SdkError::Config(format!("invalid address: {}", e)))?;
        self.node.block_peer(addr);
        Ok(())
    }

    /// Unblock a peer.
    pub fn unblock_peer(&self, peer_addr: &str) -> Result<()> {
        let addr: SocketAddr = peer_addr
            .parse()
            .map_err(|e| SdkError::Config(format!("invalid address: {}", e)))?;
        self.node.unblock_peer(&addr);
        Ok(())
    }

    /// Number of nodes discovered via pingwave propagation.
    pub fn discovered_nodes(&self) -> usize {
        self.node.proximity_graph().node_count()
    }

    // ---- Streams ----

    /// Open (or look up) a logical stream to a peer. See
    /// [`net::adapter::net::MeshNode::open_stream`] for the full contract.
    /// Repeated calls for the same `(peer, stream_id)` are idempotent;
    /// the first open wins and subsequent configs are logged and
    /// ignored.
    pub fn open_stream(
        &self,
        peer_node_id: u64,
        stream_id: u64,
        config: StreamConfig,
    ) -> Result<Stream> {
        self.node
            .open_stream(peer_node_id, stream_id, config)
            .map_err(SdkError::from)
    }

    /// Close a stream: drop its `StreamState` and free the window. Idempotent.
    pub fn close_stream(&self, peer_node_id: u64, stream_id: u64) {
        self.node.close_stream(peer_node_id, stream_id);
    }

    /// Send a batch of events on an explicit stream.
    ///
    /// Returns [`SdkError::Backpressure`] when the stream's per-stream
    /// in-flight window is full (no events were sent — the caller
    /// decides whether to drop, retry, or buffer). [`SdkError::NotConnected`]
    /// when the peer session is gone. All other failures surface as
    /// [`SdkError::Adapter`].
    pub async fn send_on_stream(&self, stream: &Stream, events: &[Bytes]) -> Result<()> {
        self.node
            .send_on_stream(stream, events)
            .await
            .map_err(SdkError::from)
    }

    /// Send events, retrying on `Backpressure` with exponential backoff
    /// (5 ms → 200 ms, doubling) up to `max_retries` times. Transport
    /// errors and `NotConnected` are returned immediately.
    pub async fn send_with_retry(
        &self,
        stream: &Stream,
        events: &[Bytes],
        max_retries: usize,
    ) -> Result<()> {
        self.node
            .send_with_retry(stream, events, max_retries)
            .await
            .map_err(SdkError::from)
    }

    /// Block the calling task until the send succeeds or a transport
    /// error occurs. See [`Mesh::send_with_retry`] for finer control.
    pub async fn send_blocking(&self, stream: &Stream, events: &[Bytes]) -> Result<()> {
        self.node
            .send_blocking(stream, events)
            .await
            .map_err(SdkError::from)
    }

    /// Snapshot of per-stream stats (tx/rx seq, window, in-flight,
    /// backpressure count, activity).
    pub fn stream_stats(&self, peer_node_id: u64, stream_id: u64) -> Option<StreamStats> {
        self.node.stream_stats(peer_node_id, stream_id)
    }

    /// Snapshot stats for every stream in the session to `peer_node_id`.
    pub fn all_stream_stats(&self, peer_node_id: u64) -> Vec<(u64, StreamStats)> {
        self.node.all_stream_stats(peer_node_id)
    }

    // ---- Capability announcements ----

    /// Announce this node's capabilities to every directly-connected
    /// peer. Self-indexes too, so `find_nodes` called from this same
    /// node matches on the announcement. Multi-hop propagation is
    /// deferred — peers more than one hop away will not see the
    /// announcement.
    ///
    /// Default TTL is 5 minutes; use
    /// [`Self::announce_capabilities_with`] to override.
    pub async fn announce_capabilities(
        &self,
        caps: crate::capabilities::CapabilitySet,
    ) -> Result<()> {
        self.node.announce_capabilities(caps).await?;
        Ok(())
    }

    /// Extended announce with explicit TTL and signing opt-in.
    /// `sign = true` is accepted but currently a no-op; signatures
    /// tie in with Stage E (channel auth), once `node_id` →
    /// `EntityId` binding is wired.
    pub async fn announce_capabilities_with(
        &self,
        caps: crate::capabilities::CapabilitySet,
        ttl: std::time::Duration,
        sign: bool,
    ) -> Result<()> {
        self.node
            .announce_capabilities_with(caps, ttl, sign)
            .await?;
        Ok(())
    }

    /// Query the capability index. Returns node ids whose latest
    /// announcement matches `filter`; includes our own `node_id` if
    /// our own announcement matches.
    pub fn find_nodes(&self, filter: &crate::capabilities::CapabilityFilter) -> Vec<u64> {
        self.node.find_nodes_by_filter(filter)
    }

    // ---- Gang-claim resource-island scheduler ------------------------
    //
    // The peer-aware Thunderdome surface; value types live in
    // [`crate::gang`]. `Reserved` is optimistic/AP — the CP `→ Active`
    // commit is a separate (currently Rust-only) primitive.

    /// Publish this node's island-topology record — the host
    /// self-announcing one island's unit set, resident capabilities, and
    /// live load / p50-latency axes. `record.host` is forced to this node.
    /// Self-indexed locally so this node's own scheduler sees it, then
    /// broadcast to peers; returns the peer fan-out count. Re-publish
    /// each heartbeat to refresh the live axes.
    pub async fn publish_island_topology(
        &self,
        record: crate::gang::IslandRecord,
    ) -> Result<usize> {
        Ok(self.node.publish_island_topology(record).await?)
    }

    /// Match islands against `criteria` over this node's capability +
    /// island folds (read-only; no claim). Best island first per the
    /// selection policy. Empty when nothing matched.
    pub fn match_islands(
        &self,
        criteria: &crate::gang::MatchCriteria,
    ) -> Vec<crate::gang::IslandId> {
        self.node.match_islands(criteria)
    }

    /// Reserve `island` (optimistic AP CAS on the reservation fold)
    /// until `until_unix_us` (wall-clock micros). [`ClaimOutcome::Won`]
    /// if this node now holds it, [`ClaimOutcome::Lost`] if already
    /// held by someone with a live reservation.
    ///
    /// [`ClaimOutcome::Won`]: crate::gang::ClaimOutcome::Won
    /// [`ClaimOutcome::Lost`]: crate::gang::ClaimOutcome::Lost
    pub async fn reserve_island(
        &self,
        island: crate::gang::IslandId,
        until_unix_us: u64,
    ) -> Result<crate::gang::ClaimOutcome> {
        Ok(self.node.reserve_island(island, until_unix_us).await?)
    }

    /// Release `island` this node holds back to `Free`.
    /// [`ClaimOutcome::Lost`](crate::gang::ClaimOutcome::Lost) if this
    /// node wasn't the holder.
    pub async fn release_island(
        &self,
        island: crate::gang::IslandId,
    ) -> Result<crate::gang::ClaimOutcome> {
        Ok(self.node.release_island(island).await?)
    }

    /// Match and reserve the first available island in one call — the
    /// node-level "schedule a single-island gang against my own folds"
    /// loop. Returns the claimed island, or `None` when nothing matched
    /// or every match was contended in this node's view.
    pub async fn claim_island(
        &self,
        criteria: &crate::gang::MatchCriteria,
        until_unix_us: u64,
    ) -> Result<Option<crate::gang::IslandId>> {
        Ok(self.node.claim_island(criteria, until_unix_us).await?)
    }

    /// Scoped variant of [`Self::find_nodes`]. Filters candidates
    /// through a [`crate::capabilities::ScopeFilter`] derived from
    /// each node's `scope:*` reserved tags. Untagged nodes resolve
    /// to `Global` and remain visible under most filters by design;
    /// nodes tagged `scope:subnet-local` only show up under
    /// [`crate::capabilities::ScopeFilter::SameSubnet`]. See
    /// `docs/internal/plans/SCOPED_CAPABILITIES_PLAN.md` for the full table.
    pub fn find_nodes_scoped(
        &self,
        filter: &crate::capabilities::CapabilityFilter,
        scope: &crate::capabilities::ScopeFilter<'_>,
    ) -> Vec<u64> {
        self.node.find_nodes_by_filter_scoped(filter, scope)
    }

    /// Pick the single best-scoring node for a placement
    /// requirement. Returns the winning node's id, or `None` if no
    /// node matches.
    pub fn find_best_node(&self, req: &crate::capabilities::CapabilityRequirement) -> Option<u64> {
        self.node.find_best_node(req)
    }

    /// Scoped variant of [`Self::find_best_node`]. Picks the highest-
    /// scoring node within the scope-filtered candidate set.
    pub fn find_best_node_scoped(
        &self,
        req: &crate::capabilities::CapabilityRequirement,
        scope: &crate::capabilities::ScopeFilter<'_>,
    ) -> Option<u64> {
        self.node.find_best_node_scoped(req, scope)
    }

    /// Bucketed aggregation over the local capability fold. Composes
    /// [`TagMatcher`](crate::capabilities::TagMatcher) ×
    /// [`GroupBy`](crate::capabilities::GroupBy) ×
    /// [`Aggregation`](crate::capabilities::Aggregation) into a
    /// `Vec<(bucket, value)>` sorted lex by bucket key. Phase 6c-A
    /// of `MULTIFOLD_PHASE_6C_CAPACITY_AGGREGATION.md`.
    ///
    /// `matcher = None` walks every entry. Returns an empty vec when
    /// no entries match.
    pub fn capability_aggregate(
        &self,
        matcher: Option<crate::capabilities::TagMatcher>,
        group_by: crate::capabilities::GroupBy,
        agg: crate::capabilities::Aggregation,
    ) -> Vec<(String, u64)> {
        self.node
            .capability_fold()
            .aggregate(matcher, group_by, agg)
    }

    /// Capacity-ranked materialized view. Wraps
    /// [`Self::capability_aggregate`] with per-bucket state
    /// breakdown (`idle` / `busy` / `reserved`), an RTT gate, and
    /// optional summed numeric capacity. Phase 6c-B of
    /// `MULTIFOLD_PHASE_6C_CAPACITY_AGGREGATION.md`.
    ///
    /// `rtt_lookup` maps a publisher's `node_id` to current RTT in
    /// milliseconds. When `query.max_rtt_ms` is `None`, the closure
    /// is never invoked; when set, publishers whose lookup returns
    /// `None` are dropped (fail-closed — never-pinged nodes don't
    /// ride a "fastest available" filter as zero).
    ///
    /// Faulty entries are always excluded. Rows return sorted by
    /// `available` descending; ties broken by bucket key ascending.
    /// Truncated to `query.limit` (0 = no truncation).
    ///
    /// # Example
    ///
    /// ```
    /// # async fn doc() -> net_sdk::error::Result<()> {
    /// use net_sdk::capabilities::{
    ///     CapabilitySet, CapacityQuery, GroupBy, TagMatcher,
    /// };
    /// use net_sdk::mesh::MeshBuilder;
    ///
    /// let node = MeshBuilder::new("127.0.0.1:0", &[0x42u8; 32])?
    ///     .build()
    ///     .await?;
    /// node.announce_capabilities(
    ///     CapabilitySet::new()
    ///         .add_tag("hardware.gpu")
    ///         .add_tag("hardware.gpu.h100")
    ///         .add_tag("hardware.gpu.count=8"),
    /// )
    /// .await?;
    ///
    /// // Top GPU types by available capacity, no RTT filter,
    /// // summed count column populated.
    /// let view = node.capability_capacity_ranking(
    ///     CapacityQuery {
    ///         matcher: Some(TagMatcher::Prefix { value: "hardware.gpu".into() }),
    ///         group_by: GroupBy::TagStem { prefix: "hardware.gpu".into() },
    ///         max_rtt_ms: None,
    ///         sum_axis_key: Some("hardware.gpu.count".into()),
    ///         limit: 5,
    ///     },
    ///     |_node_id| None,
    /// );
    /// // Self-match: one bucket per stem this node carries.
    /// assert!(view.iter().any(|row| row.bucket == "h100"));
    /// # Ok(())
    /// # }
    /// ```
    pub fn capability_capacity_ranking<R>(
        &self,
        query: crate::capabilities::CapacityQuery,
        rtt_lookup: R,
    ) -> Vec<crate::capabilities::CapacityRow>
    where
        R: Fn(u64) -> Option<u32>,
    {
        self.node
            .capability_fold()
            .capacity_ranking(query, rtt_lookup)
    }

    // ---- Lifecycle ----

    /// Set a migration handler (for Mikoshi daemon migration).
    pub fn set_migration_handler(&mut self, handler: Arc<MigrationSubprotocolHandler>) {
        self.node.set_migration_handler(handler);
    }

    /// Gracefully shut down.
    pub async fn shutdown(self) -> Result<()> {
        self.node.shutdown().await?;
        Ok(())
    }

    /// Get a reference to the underlying `MeshNode`.
    pub fn inner(&self) -> &MeshNode {
        &self.node
    }

    /// Clone the `Arc`-shared `MeshNode` handle out of the mesh.
    ///
    /// Used by FFI bindings (currently: NAPI) that need to hand
    /// the same live node to the `net-sdk::compute::DaemonRuntime`
    /// **and** to their own wrapper class without constructing a
    /// second UDP socket. All public `MeshNode` operations go
    /// through `&MeshNode`, so two Arc holders observe exactly
    /// the same state.
    pub fn node_arc(&self) -> Arc<MeshNode> {
        self.node.clone()
    }

    /// Construct a `Mesh` that shares an existing `MeshNode` with
    /// another owner. Used by FFI bindings that already hold an
    /// `Arc<MeshNode>` (e.g. NAPI's `NetMesh`) and need a `Mesh`
    /// wrapper so the SDK's `DaemonRuntime` can be built against
    /// the same live node.
    ///
    /// Does not re-install `channel_configs` or a `TokenCache` —
    /// the owner of the original `MeshNode` is responsible for
    /// that wiring. Supplied `channel_configs` / `identity`
    /// arguments are held onto here purely so the `Mesh`'s own
    /// helpers (channel registration lookup, identity getter)
    /// have data to return.
    pub fn from_node_arc(
        node: Arc<MeshNode>,
        channel_configs: Arc<ChannelConfigRegistry>,
        identity: Option<crate::identity::Identity>,
    ) -> Self {
        Self {
            node,
            channel_configs,
            recv_cursor: Arc::new(AtomicU16::new(0)),
            identity,
            #[cfg(feature = "tool")]
            tool_metadata_fetch: Arc::new(parking_lot::Mutex::new(None)),
            #[cfg(feature = "tool")]
            tool_watch: Arc::new(parking_lot::Mutex::new(None)),
        }
    }

    /// The named subnet exports this mesh was built with
    /// (SUBNET_AUTH_SDK_PLAN.md §3.3). Immutable after construction.
    ///
    /// Delegates to the NODE's map (review-10 P1-6). The SDK used to
    /// keep a second one, which meant a mesh built through
    /// [`Mesh::from_node_arc`] reported an empty map while the node it
    /// wrapped held a full one.
    pub fn subnet_exports(&self) -> &crate::subnet::NamedSubnetExports {
        self.node.subnet_exports()
    }

    /// Caller-owned identity bound to this mesh, if any. Returns
    /// `None` for meshes built without `.identity(...)` (ephemeral
    /// keypair).
    pub fn identity(&self) -> Option<&crate::identity::Identity> {
        self.identity.as_ref()
    }

    /// This node's ed25519 identity keypair — always present (the
    /// node minted an ephemeral one if `.identity(...)` was never
    /// called). For in-process surfaces that sign *as the node*,
    /// e.g. the payments caller flow binding quotes and invocation
    /// proofs to the same identity peers see on the mesh. The
    /// keypair stays in Rust; bindings never surface it.
    pub fn entity_keypair(&self) -> &EntityKeypair {
        self.node.entity_keypair()
    }

    // ── NAT traversal ──────────────────────────────────────────
    //
    // Framing (load-bearing — see `docs/internal/plans/NAT_TRAVERSAL_PLAN.md`
    // stage 5): every user-visible docstring here must position
    // NAT traversal as **optimization, not correctness**. Nodes
    // behind NAT can always talk through the mesh's routed-
    // handshake path. These APIs let the mesh upgrade to a
    // *direct* path when the underlying NATs allow it, cutting
    // relay hops out of the data plane. A `nat_type` of
    // `symmetric` or a `PunchFailed` error is not a
    // connectivity failure — it just means traffic keeps
    // riding the relay.
    //
    // Anti-phrasings to avoid: "required for NATed peers",
    // "enables cross-NAT connectivity", "fixes NAT issues."
    // Each of these implies the mesh otherwise can't reach
    // NATed peers, which is false.

    /// Current NAT classification for this mesh's public face,
    /// as observed against other peers during the classification
    /// sweep. One of `Open`, `Cone`, `Symmetric`, or `Unknown`
    /// (pre-sweep or insufficient data).
    ///
    /// **Optimization, not correctness.** A `Symmetric`
    /// classification doesn't prevent this mesh from
    /// communicating with any peer — it just means the direct-
    /// punch optimization is unlikely to succeed against some
    /// peers, and traffic will keep riding the routed path.
    ///
    /// Requires the `nat-traversal` cargo feature.
    #[cfg(feature = "nat-traversal")]
    pub fn nat_type(&self) -> net::adapter::net::traversal::classify::NatClass {
        self.node.nat_class()
    }

    /// This mesh's public-facing `SocketAddr` as observed by a
    /// remote peer, or `None` before the first classification
    /// sweep has produced an observation.
    ///
    /// Piggybacks on outbound `CapabilityAnnouncement`s so peers
    /// can attempt a direct-connect without a separate
    /// discovery round-trip. Read by peers implementing the
    /// `connect_direct` rendezvous path.
    ///
    /// Requires the `nat-traversal` cargo feature.
    #[cfg(feature = "nat-traversal")]
    pub fn reflex_addr(&self) -> Option<SocketAddr> {
        self.node.reflex_addr()
    }

    /// The NAT classification most recently advertised by
    /// `peer_node_id` (parsed from the `nat:*` tag on their
    /// capability announcement). Returns `NatClass::Unknown`
    /// when the peer hasn't announced or was compiled without
    /// NAT traversal — the pair-type matrix treats Unknown as
    /// "attempt direct, fall back on failure," not as
    /// "don't attempt."
    ///
    /// Requires the `nat-traversal` cargo feature.
    #[cfg(feature = "nat-traversal")]
    pub fn peer_nat_type(
        &self,
        peer_node_id: u64,
    ) -> net::adapter::net::traversal::classify::NatClass {
        self.node.peer_nat_class(peer_node_id)
    }

    /// Send one reflex probe to `peer_node_id` and return the
    /// public `SocketAddr` the peer observed on the probe's UDP
    /// envelope. Useful for tests and for operators diagnosing a
    /// NAT-type classification that seems off.
    ///
    /// Times out after `TraversalConfig::reflex_timeout` (3 s
    /// default) on network delays, and fast-fails with
    /// `peer-not-reachable` on an unknown peer.
    ///
    /// Requires the `nat-traversal` cargo feature.
    #[cfg(feature = "nat-traversal")]
    pub async fn probe_reflex(&self, peer_node_id: u64) -> Result<SocketAddr> {
        Ok(self.node.probe_reflex(peer_node_id).await?)
    }

    /// Explicitly re-run the NAT classification sweep against
    /// this node's currently-connected peers. Normally the
    /// background loop (spawned by `start()`) takes care of
    /// this; call this after a suspected NAT rebind (e.g. a
    /// gateway reboot) to accelerate the re-classification.
    ///
    /// No-op when fewer than 2 peers are connected — the
    /// two-probe rule needs two distinct targets to produce a
    /// classification. Never returns an error; a failed sweep
    /// leaves the previous classification intact.
    ///
    /// Requires the `nat-traversal` cargo feature.
    #[cfg(feature = "nat-traversal")]
    pub async fn reclassify_nat(&self) {
        self.node.reclassify_nat().await
    }

    /// Establish a session to `peer_node_id` via the rendezvous
    /// path, using the pair-type matrix to decide between a
    /// direct handshake and a relay-coordinated punch. The
    /// returned session is equivalent in correctness to
    /// `connect()` — the *only* difference is that a
    /// `connect_direct` that lands on the punched path cuts
    /// relay hops out of the data plane.
    ///
    /// **Optimization, not correctness.** `connect_direct`
    /// always resolves: on a punch-failed outcome, the session
    /// is established via the routed-handshake fallback.
    /// Inspect `traversal_stats()` afterward to distinguish a
    /// successful punch from a relay fallback.
    ///
    /// `coordinator` names a peer we already have a session
    /// with — typically a stable relay-capable node. The
    /// coordinator mediates the introduction; it doesn't carry
    /// user-plane traffic once the punch succeeds.
    ///
    /// Fails with an `SdkError::Traversal` variant whose `kind`
    /// is `peer-not-reachable` (no cached reflex for `peer`),
    /// `transport` (socket-level error on the final handshake),
    /// or (internal, retried on fallback) `punch-failed`.
    ///
    /// Requires the `nat-traversal` cargo feature.
    #[cfg(feature = "nat-traversal")]
    pub async fn connect_direct(
        &self,
        peer_node_id: u64,
        peer_pubkey: &[u8; 32],
        coordinator: u64,
    ) -> Result<()> {
        self.node
            .connect_direct(peer_node_id, peer_pubkey, coordinator)
            .await?;
        Ok(())
    }

    /// Like [`Self::connect_direct`], but auto-selects the rendezvous
    /// coordinator instead of taking one — the ergonomic entry point.
    /// Prefers the relay currently forwarding to `peer_node_id`, then a
    /// `relay-capable` mutual peer, then any mutual peer.
    ///
    /// **Optimization, not correctness.** If no coordinator is
    /// available for a pair that needs a punch, fails with an
    /// `SdkError::Traversal` whose `kind` is `rendezvous-no-relay` —
    /// the caller simply stays on the routed-handshake path, which is
    /// always available. `Direct` pairs need no coordinator and always
    /// proceed.
    ///
    /// Requires the `nat-traversal` cargo feature.
    #[cfg(feature = "nat-traversal")]
    pub async fn connect_direct_auto(
        &self,
        peer_node_id: u64,
        peer_pubkey: &[u8; 32],
    ) -> Result<()> {
        self.node
            .connect_direct_auto(peer_node_id, peer_pubkey)
            .await?;
        Ok(())
    }

    /// Cumulative counters for this mesh's NAT-traversal
    /// activity: punch attempts, successful punches, and relay
    /// fallbacks. Base counters are monotonic and never reset
    /// (`punches_failed` is derived and can decrease while a punch
    /// is in flight; `port_mapping_renewals` resets on each fresh
    /// install — difference only the base counters). Useful for
    /// diagnostics + telemetry (success rate, relay load
    /// trends).
    ///
    /// Requires the `nat-traversal` cargo feature.
    #[cfg(feature = "nat-traversal")]
    pub fn traversal_stats(&self) -> net::adapter::net::traversal::TraversalStatsSnapshot {
        self.node.traversal_stats()
    }

    /// Install a runtime reflex override. Forces `nat_type() =
    /// "open"` and `reflex_addr() = Some(external)` immediately,
    /// short-circuiting any further classifier sweeps.
    ///
    /// Intended for operator-driven updates — a port-forward
    /// that went live mid-session, or a stage-4 port-mapping
    /// task that just installed a UPnP / NAT-PMP mapping.
    /// Builder-level [`MeshBuilder::reflex_override`] covers the
    /// startup-time case; this is the runtime equivalent.
    ///
    /// **Optimization, not correctness.** Nodes without an
    /// override still reach every peer via the routed-handshake
    /// path. The override pins the publicly-advertised address
    /// when it's already known.
    ///
    /// Requires the `nat-traversal` cargo feature.
    #[cfg(feature = "nat-traversal")]
    pub fn set_reflex_override(&self, external: SocketAddr) {
        self.node.set_reflex_override(external);
    }

    /// Drop a previously-installed reflex override. The
    /// classifier resumes on its normal cadence; the next sweep
    /// repopulates `reflex_addr` and `nat_type` from real probe
    /// observations. `reflex_addr` clears to `None` immediately
    /// so a between-sweep read doesn't return a stale override.
    ///
    /// No-op when no override is active — safe to call
    /// unconditionally during shutdown or a port-mapper revoke
    /// path.
    ///
    /// Requires the `nat-traversal` cargo feature.
    #[cfg(feature = "nat-traversal")]
    pub fn clear_reflex_override(&self) {
        self.node.clear_reflex_override();
    }
}

/// Map an `AdapterError` from a subscribe / unsubscribe / publish
/// call into the channel-aware `SdkError` variant. Rejection acks
/// come through as `AdapterError::Connection("membership request
/// rejected: Some(<reason>)")`; parse that into
/// [`SdkError::ChannelRejected`].
fn adapter_to_channel_error(err: net::error::AdapterError) -> SdkError {
    use net::error::AdapterError;
    if let AdapterError::Connection(ref msg) = err {
        let prefix = "membership request rejected: ";
        if let Some(tail) = msg.strip_prefix(prefix) {
            let reason = parse_ack_reason(tail);
            return SdkError::ChannelRejected(reason);
        }
    }
    SdkError::from(err)
}

fn parse_ack_reason(s: &str) -> Option<AckReason> {
    // `{:?}` of `Option<AckReason>` produces `Some(Unauthorized)` etc.
    let inside = s.trim().strip_prefix("Some(")?.strip_suffix(')')?;
    match inside {
        "Unauthorized" => Some(AckReason::Unauthorized),
        "UnknownChannel" => Some(AckReason::UnknownChannel),
        "RateLimited" => Some(AckReason::RateLimited),
        "TooManyChannels" => Some(AckReason::TooManyChannels),
        _ => None,
    }
}

#[cfg(all(test, feature = "net"))]
mod rpc_service_channel_registration_tests {
    //! H2 and H3, as seen through a real `Mesh`.
    //!
    //! The aggregator module used to carry a hand-copied version of the
    //! auto-registration helper. The copy drifted: it kept replacing
    //! inserts (so it discarded operator ACLs, H2) and never gained the
    //! reply-channel origin binding (so aggregator reply channels stayed
    //! world-subscribable, H3) — both long after those were fixed for
    //! `serve_rpc`.
    //!
    //! The implementation now lives on `ChannelConfigRegistry` in the
    //! `net` crate and has its own tests there. What these add is the
    //! `Mesh` end of it: that a `MeshBuilder`-built mesh hands its node
    //! the same registry the policy lands in, so a serve through this
    //! mesh is governed by the entries below. Testing that needs a
    //! `Mesh`, which is why they stay here.

    use super::*;
    use net::adapter::net::identity::EntityKeypair;
    use net::adapter::net::{ChannelId, ChannelName};

    async fn mesh() -> Mesh {
        MeshBuilder::new("127.0.0.1:0", &[0x5Au8; 32])
            .unwrap()
            .build()
            .await
            .unwrap()
    }

    /// The registry the mesh installed on its node at build time.
    fn registry(mesh: &Mesh) -> &ChannelConfigRegistry {
        &mesh.channel_configs
    }

    /// An operator ACL registered first must survive auto-registration.
    #[tokio::test]
    async fn shared_registration_preserves_an_operator_acl() {
        let mesh = mesh().await;
        let root = EntityKeypair::generate();
        let channel = ChannelName::new("agg.svc.requests").unwrap();
        mesh.register_channel(
            ChannelConfig::new(ChannelId::new(channel))
                .with_token_roots(vec![root.entity_id().clone()]),
        );

        registry(&mesh)
            .install_rpc_service_defaults("agg.svc")
            .unwrap();

        let cfg = mesh
            .channel_configs
            .get_by_name("agg.svc.requests")
            .expect("request channel must exist")
            .clone();
        assert!(
            cfg.token_required(),
            "shared registration must not clobber an operator ACL (H2)"
        );
        assert_eq!(cfg.token_roots[0], *root.entity_id());
    }

    /// The reply prefix it installs must be origin-bound, or any peer
    /// can subscribe to another caller's reply channel.
    #[tokio::test]
    async fn shared_registration_binds_the_reply_prefix_to_the_caller_origin() {
        let mesh = mesh().await;
        registry(&mesh)
            .install_rpc_service_defaults("agg.svc2")
            .unwrap();

        let resolved = mesh
            .channel_configs
            .resolve_by_name("agg.svc2.replies.00112233445566aa")
            .expect("reply prefix must resolve for a per-caller channel");
        assert!(
            resolved.config.subscriber_origin_binding.is_some(),
            "the shared registration must bind reply channels to the \
             subscriber's own origin (H3)"
        );
        assert_eq!(
            resolved.matched_prefix.as_deref(),
            Some("agg.svc2.replies.")
        );
    }

    /// Defaults still land when the operator configured nothing —
    /// guards against "fix the clobbering by registering nothing", which
    /// would break nRPC's dynamic per-caller reply subscriptions.
    #[tokio::test]
    async fn shared_registration_still_installs_defaults() {
        let mesh = mesh().await;
        registry(&mesh)
            .install_rpc_service_defaults("agg.svc3")
            .unwrap();

        assert!(mesh
            .channel_configs
            .get_by_name("agg.svc3.requests")
            .is_some());
        assert!(mesh
            .channel_configs
            .get_by_name("agg.svc3.replies.00112233445566aa")
            .is_some());
    }
}
