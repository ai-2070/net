//! MeshNode: multi-peer mesh runtime composing all protocol layers.
//!
//! `MeshNode` is the composition layer that turns independent components
//! (encrypted sessions, router, failure detector) into a functioning mesh
//! node that can communicate with multiple peers simultaneously over a
//! single UDP socket.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────┐
//! │                  MeshNode                   │
//! │                                             │
//! │  ┌──────────┐  ┌──────────┐  ┌──────────┐  │
//! │  │ Session A│  │ Session B│  │ Session C│  │
//! │  └────┬─────┘  └────┬─────┘  └────┬─────┘  │
//! │       │              │              │       │
//! │  ┌────┴──────────────┴──────────────┴────┐  │
//! │  │          Receive Loop (single)        │  │
//! │  │  demux by source_addr → session       │  │
//! │  │  local → decrypt → queue              │  │
//! │  │  forward → router (no decrypt)        │  │
//! │  └───────────────┬───────────────────────┘  │
//! │                  │                          │
//! │  ┌───────────────┴───────────────────────┐  │
//! │  │         UDP Socket (shared)           │  │
//! │  └───────────────────────────────────────┘  │
//! └─────────────────────────────────────────────┘
//! ```

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use arc_swap::ArcSwapOption;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use crossbeam_queue::SegQueue;
use dashmap::DashMap;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use super::crypto::{handshake_prologue, CryptoError, NoiseHandshake, SessionKeys, StaticKeypair};
use super::failure::{FailureDetector, FailureDetectorConfig};
use super::identity::{EntityId, EntityKeypair, PermissionToken, TokenCache, TokenScope};
use super::pool::PacketBuilder;

use super::behavior::broadcast::SUBPROTOCOL_CAPABILITY_ANN;
use super::behavior::capability::{
    CapabilityAnnouncement, CapabilityFilter, CapabilityIndex, CapabilityRequirement,
    CapabilitySet, ScopeFilter, MAX_CAPABILITY_HOPS,
};
use super::behavior::loadbalance::HealthStatus;
use super::behavior::proximity::{EnhancedPingwave, ProximityConfig, ProximityGraph};
use super::channel::membership::{self, MembershipMsg, SUBPROTOCOL_CHANNEL_MEMBERSHIP};
use super::channel::{
    AckReason, AuthGuard, AuthVerdict, ChannelConfigRegistry, ChannelId, ChannelName,
    ChannelPublisher, OnFailure, PublishConfig, PublishReport, SubscriberRoster,
};
use super::compute::SUBPROTOCOL_MIGRATION;
use super::protocol::{self, EventFrame, PacketFlags, HEADER_SIZE, MAGIC, TAG_SIZE};

/// Wire overhead added to the AEAD-encrypted payload by every Net
/// packet: the 64-byte header plus the 16-byte Poly1305 tag. Credit
/// accounting charges this against the sender's `tx_credit_remaining`
/// alongside the payload so the byte window matches the bandwidth
/// the sender actually pumps onto the link. The receiver's
/// `on_bytes_consumed` adds the same overhead, keeping sender and
/// receiver in lockstep.
const PACKET_WIRE_OVERHEAD: usize = HEADER_SIZE + TAG_SIZE;

/// Total wire bytes for a single Net packet carrying `payload_bytes`
/// of AEAD-encrypted content. Saturating at `u32::MAX` so a
/// pathological `payload_bytes` can't silently wrap the credit math.
#[inline]
fn wire_bytes_for_payload(payload_bytes: usize) -> u32 {
    payload_bytes
        .saturating_add(PACKET_WIRE_OVERHEAD)
        .min(u32::MAX as usize) as u32
}
use super::reroute::ReroutePolicy;
use super::route::{RoutingHeader, ROUTING_HEADER_SIZE, ROUTING_MAGIC};
use super::router::{NetRouter, RouterConfig};
use super::session::{NetSession, TxAdmit, CONTROL_STREAM_ID};
use super::stream::{Stream, StreamConfig, StreamError, StreamStats};
use super::subnet::{SubnetId, SubnetPolicy};
use super::subprotocol::stream_window::{StreamWindow, SUBPROTOCOL_STREAM_WINDOW};
use super::subprotocol::MigrationSubprotocolHandler;
use super::transport::{NetSocket, PacketReceiver, ParsedPacket, SocketBufferConfig};
use super::Visibility;
use tokio::sync::oneshot;

use crate::adapter::{Adapter, ShardPollResult};
use crate::error::AdapterError;
use crate::event::{Batch, StoredEvent};

/// Inbound event queues (same type as NetAdapter uses).
type InboundQueues = Arc<DashMap<u16, SegQueue<StoredEvent>>>;

/// Convert a u64 node_id to a 32-byte graph NodeId.
///
/// The proximity graph uses 32-byte ed25519 public keys as NodeId.
/// For nodes where we only have the derived u64 node_id, we zero-pad
/// it to 32 bytes. This preserves uniqueness for topology tracking
/// without requiring the full public key exchange.
fn node_id_to_graph_id(node_id: u64) -> [u8; 32] {
    let mut id = [0u8; 32];
    id[0..8].copy_from_slice(&node_id.to_le_bytes());
    id
}

/// Inverse of `node_id_to_graph_id`: read the u64 back from the first 8
/// bytes of a 32-byte proximity `NodeId`. Assumes the id was produced by
/// `node_id_to_graph_id` (which is how every peer in this codebase is
/// seeded into the graph).
fn graph_id_to_node_id(graph_id: &[u8; 32]) -> u64 {
    u64::from_le_bytes(graph_id[0..8].try_into().unwrap())
}

/// Set of peer addresses whose packets should be silently dropped.
///
/// Used by test harnesses to simulate network partitions. When a peer's
/// address is in this set, both inbound and outbound packets are dropped
/// as if the network link is severed.
pub type PartitionFilter = Arc<dashmap::DashSet<SocketAddr>>;

/// Cancellation-safe rollback for a freshly registered peer
/// session + addr map + routing entry.
///
/// `handle_routed_handshake` schedules its msg2 send on a
/// background task, so the rollback path must survive task drop.
/// A fire-and-forget `tokio::spawn` whose only rollback was inside
/// the spawned future's error arm would skip the rollback if that
/// future was cancelled (runtime shutdown, parent task abort)
/// before the send completed — the responder would keep session
/// keys the initiator never received the matching msg2 for,
/// wedged forever with no idle sweeper to reap it.
///
/// This guard moves the rollback into `Drop`, which runs
/// synchronously whenever the spawned future is dropped. The
/// successful-send arm calls `commit()` to consume the guard
/// without invoking `Drop` (`std::mem::forget`); cancellation,
/// panic, or any non-success path lets the guard drop naturally,
/// and `Drop` reverts all three registrations.
struct PeerRegistrationGuard {
    peer_node_id: u64,
    registered_next_hop: SocketAddr,
    peers: Arc<DashMap<u64, PeerInfo>>,
    peer_addrs: Arc<DashMap<u64, SocketAddr>>,
    router: Arc<NetRouter>,
}

impl PeerRegistrationGuard {
    /// Mark the registration as durable. Drops `self` *without*
    /// running the rollback — the post-handshake send completed
    /// successfully, so the registrations should stay in place.
    fn commit(self) {
        // `mem::forget` skips `Drop`. The Arc fields would
        // normally decrement on drop, but since we want them
        // alive (they're shared with the rest of the bus), we
        // need to drop them manually before forgetting the
        // wrapper. SAFETY: reading the Arc fields out of the
        // struct via `ptr::read` and forgetting the rest is the
        // standard cancel-Drop pattern.
        let me = std::mem::ManuallyDrop::new(self);
        // SAFETY: `me` is `ManuallyDrop`, so its fields won't be
        // dropped automatically. We read them out and let them
        // drop normally, which decrements the Arc strong counts
        // — exactly what would happen on a non-guarded path.
        unsafe {
            let _peers = std::ptr::read(&me.peers);
            let _peer_addrs = std::ptr::read(&me.peer_addrs);
            let _router = std::ptr::read(&me.router);
        }
    }
}

impl Drop for PeerRegistrationGuard {
    fn drop(&mut self) {
        // Match the original inline rollback's semantics: only
        // remove entries whose addr / next-hop still equals the
        // value we wrote. A concurrent retry for the same peer
        // may have already replaced them with a fresh, valid
        // registration — we must not overwrite that.
        self.peers.remove_if(&self.peer_node_id, |_, pi| {
            pi.addr == self.registered_next_hop
        });
        self.peer_addrs.remove_if(&self.peer_node_id, |_, addr| {
            *addr == self.registered_next_hop
        });
        self.router
            .routing_table()
            .remove_route_if_next_hop_is(self.peer_node_id, self.registered_next_hop);
    }
}

/// Shared context for the packet dispatch loop.
struct DispatchCtx {
    local_node_id: u64,
    peers: Arc<DashMap<u64, PeerInfo>>,
    addr_to_node: Arc<DashMap<SocketAddr, u64>>,
    /// Node-id → addr map shared with the reroute policy. Must be kept in
    /// sync with `peers` on every registration so the reroute policy can
    /// resolve failed peers.
    peer_addrs: Arc<DashMap<u64, SocketAddr>>,
    router: Arc<NetRouter>,
    failure_detector: Arc<FailureDetector>,
    inbound: InboundQueues,
    num_shards: u16,
    /// Optional subprotocol handler for migration messages.
    ///
    /// `ArcSwapOption` so [`Self::set_migration_handler`] can install
    /// at runtime via `&self` — the SDK's `DaemonRuntime::start`
    /// hands in a handler after the mesh has been constructed but
    /// before user migration traffic lands, which is otherwise
    /// awkward because a started `Mesh` is shared by `Arc`.
    migration_handler: Arc<ArcSwapOption<MigrationSubprotocolHandler>>,
    /// In-flight initiator handshakes; dispatch completes them when a
    /// matching routed msg2 arrives.
    pending_handshakes: Arc<DashMap<u64, PendingHandshake>>,
    /// In-flight DIRECT initiator handshakes, keyed by the peer's
    /// socket address. The initiator registers a oneshot here
    /// BEFORE sending msg1; the dispatch loop's direct-handshake
    /// branch looks up the source, forwards the parsed payload
    /// bytes through the oneshot, and removes the entry.
    ///
    /// Polling `socket_arc.recv_from` directly from
    /// `try_handshake_initiator` would race the dispatch receive
    /// loop spawned by `start()` — tokio dispatches each datagram
    /// to exactly one waiter, so the handshake response could be
    /// swallowed by either side. If no entry matches the source
    /// (e.g., the
    /// responder side or pre-start invocations), the dispatcher
    /// falls through to its drop-direct-handshake behaviour.
    pending_direct_initiators: Arc<DashMap<SocketAddr, oneshot::Sender<Bytes>>>,
    /// Our Noise static keypair — needed to construct responder state
    /// when a routed msg1 arrives for us.
    static_keypair: StaticKeypair,
    /// PSK shared across the mesh.
    psk: [u8; 32],
    /// Socket for sending outbound subprotocol responses.
    socket: Arc<NetSocket>,
    /// Proximity graph for topology awareness.
    proximity_graph: Arc<ProximityGraph>,
    /// Partition filter — packets from blocked addresses are dropped.
    partition_filter: PartitionFilter,
    /// Settings for sessions we create during inbound dispatch (relayed
    /// handshake responder completes here).
    packet_pool_size: usize,
    default_reliable: bool,
    /// Subscriber roster for channel fan-out.
    roster: Arc<SubscriberRoster>,
    /// Channel config registry used to authorize incoming Subscribe.
    /// `None` disables channel-level ACL checks (any caller accepted).
    channel_configs: Option<Arc<ChannelConfigRegistry>>,
    /// In-flight Subscribe/Unsubscribe requests awaiting an Ack, keyed by nonce.
    pending_membership_acks: Arc<DashMap<u64, oneshot::Sender<MembershipAck>>>,
    /// In-flight reflex probes keyed by the responder's `node_id`.
    /// Populated by `MeshNode::probe_reflex`; the dispatch branch
    /// for `SUBPROTOCOL_REFLEX` completes the oneshot with the
    /// decoded observed-address on response receipt. Only one
    /// probe per peer is in flight at a time — a second call
    /// replaces the pending entry and the previous caller sees
    /// `ReflexTimeout`.
    #[cfg(feature = "nat-traversal")]
    pending_reflex_probes:
        Arc<DashMap<u64, (u64, tokio::sync::oneshot::Sender<std::net::SocketAddr>)>>,
    /// Waiters for incoming `PunchIntroduce` messages, keyed by
    /// the counterpart endpoint's `node_id` (the `peer` field in
    /// the introduce). Mirrors `pending_reflex_probes` — the
    /// dispatcher completes the oneshot when a matching introduce
    /// lands; a message with no waiter is dropped silently.
    #[cfg(feature = "nat-traversal")]
    pending_punch_introduces: Arc<
        DashMap<
            u64,
            (
                u64,
                tokio::sync::oneshot::Sender<super::traversal::rendezvous::PunchIntroduce>,
            ),
        >,
    >,
    /// Waiters for incoming `PunchAck` messages, keyed by the
    /// sender's `node_id` (the `from_peer` field in the ack).
    /// `connect_direct`'s `SinglePunch` path awaits on this map
    /// to confirm the peer completed their side of the punch.
    #[cfg(feature = "nat-traversal")]
    pending_punch_acks: Arc<
        DashMap<
            u64,
            (
                u64,
                tokio::sync::oneshot::Sender<super::traversal::rendezvous::PunchAck>,
            ),
        >,
    >,
    /// Keep-alive observers, keyed by the `SocketAddr` of the
    /// counterpart's `peer_reflex`. Fired by the receive loop
    /// when a matching `Keepalive`-formatted packet arrives;
    /// consumed by the endpoint's punch-scheduling task to
    /// decide when to emit a `PunchAck`.
    #[cfg(feature = "nat-traversal")]
    punch_observers: Arc<
        DashMap<SocketAddr, tokio::sync::oneshot::Sender<super::traversal::rendezvous::Keepalive>>,
    >,
    /// NAT-traversal tunables (probe timeouts, punch cadence,
    /// classification deadlines). Shared with `MeshNode` by value
    /// since `TraversalConfig` is `Clone` and small. The dispatch
    /// path only reads it — no need for an Arc.
    #[cfg(feature = "nat-traversal")]
    traversal_config: super::traversal::TraversalConfig,
    /// Max distinct channels a single peer may subscribe to.
    max_channels_per_peer: usize,
    /// Capability index shared with `MeshNode`. Inbound
    /// `SUBPROTOCOL_CAPABILITY_ANN` packets are indexed here.
    capability_index: Arc<CapabilityIndex>,
    /// Dedup cache for multi-hop capability announcements, keyed by
    /// `(origin_node_id, version)`. Written by the dispatch handler
    /// before indexing + forwarding so a `(origin, version)` tuple
    /// is processed at most once per node.
    seen_announcements: Arc<DashMap<(u64, u64), std::time::Instant>>,
    /// Whether inbound `CapabilityAnnouncement` packets without a
    /// signature are dropped. Validity is not enforced yet.
    require_signed_capabilities: bool,
    /// This node's subnet (copy of `config.subnet`).
    local_subnet: SubnetId,
    /// Policy applied to each inbound `CapabilityAnnouncement` to
    /// derive the sender's subnet. `None` disables tracking.
    local_subnet_policy: Option<Arc<SubnetPolicy>>,
    /// Per-peer subnet map, written by the capability-announcement
    /// dispatch and read by the subscribe gate + publish fan-out.
    peer_subnets: Arc<DashMap<u64, SubnetId>>,
    /// Per-peer entity-id map, written by the capability-
    /// announcement dispatch after signature verification. Load-
    /// bearing for channel auth.
    peer_entity_ids: Arc<DashMap<u64, EntityId>>,
    /// Shared token cache, populated by subscriber-presented tokens
    /// plus caller-side pre-installs. `None` disables the
    /// `require_token` path — unset is equivalent to "no token is
    /// ever valid."
    token_cache: Option<Arc<TokenCache>>,
    /// Per-packet authorization fast path. `authorize_subscribe`
    /// writes on success (via `allow_channel`) so the publish
    /// fan-out can use the bloom filter + verified cache to admit
    /// or drop subscribers in constant time.
    auth_guard: Arc<AuthGuard>,
    /// Per-peer auth-failure state (for the subscribe rate limit).
    auth_failures: Arc<DashMap<u64, AuthFailureState>>,
    /// Failures-per-window threshold from the parent config.
    max_auth_failures_per_window: u16,
    /// Rolling window length for auth-failure counting.
    auth_failure_window: Duration,
    /// How long a peer stays throttled after tripping the threshold.
    auth_throttle_duration: Duration,
}

/// Result passed through the pending-ack oneshot.
#[derive(Debug, Clone)]
pub(crate) struct MembershipAck {
    pub accepted: bool,
    pub reason: Option<AckReason>,
}

/// Configuration for a MeshNode.
#[derive(Debug, Clone)]
pub struct MeshNodeConfig {
    /// Local bind address
    pub bind_addr: SocketAddr,
    /// Pre-shared key (32 bytes, shared across the mesh)
    pub psk: [u8; 32],
    /// Heartbeat interval for failure detection
    pub heartbeat_interval: Duration,
    /// Session timeout
    pub session_timeout: Duration,
    /// Number of shards for inbound event routing
    pub num_shards: u16,
    /// Packet pool size per session
    pub packet_pool_size: usize,
    /// Default reliability mode
    pub default_reliable: bool,
    /// Handshake timeout per attempt
    pub handshake_timeout: Duration,
    /// Handshake retries
    pub handshake_retries: usize,
    /// Socket buffer config
    pub socket_buffers: SocketBufferConfig,
    /// Max queue depth per stream for the fair scheduler.
    pub max_queue_depth: usize,
    /// Fair scheduling quantum (packets per stream per round).
    pub fair_quantum: usize,
    /// Idle timeout before a stream is evicted from its session. A
    /// stream with no send or receive activity for this long is dropped
    /// on the heartbeat-loop sweep. Protects against unbounded
    /// `StreamState` growth under workloads that hash into stream ids.
    pub stream_idle_timeout: Duration,
    /// Hard cap on the number of streams per session. When exceeded,
    /// the least-recently-active stream is evicted via the same path as
    /// `close_stream` (logged with `reason=cap_exceeded`).
    pub max_streams: usize,
    /// Max channels a single peer may subscribe to via
    /// `SUBPROTOCOL_CHANNEL_MEMBERSHIP`. Extra Subscribe requests are
    /// rejected with `AckReason::TooManyChannels`. Protects the roster
    /// from a peer that spams subscriptions.
    pub max_channels_per_peer: usize,
    /// Timeout for `subscribe_channel` / `unsubscribe_channel` to wait
    /// for an `Ack` before returning `AdapterError::Timeout`.
    pub membership_ack_timeout: Duration,
    /// Drop inbound `CapabilityAnnouncement` packets whose signature
    /// is missing. Defaults to `true` because the cap data feeds
    /// channel-auth (`can_publish` / `can_subscribe` cap filters)
    /// and subnet visibility — an unsigned announcement is
    /// attacker-controlled input, and accepting it silently meant a
    /// peer could claim any caps or subnet just by announcing. The
    /// dispatch path still applies a second belt-and-braces guard
    /// on individual auth-load-bearing state updates
    /// (`peer_entity_ids`, `peer_subnets`), so explicitly setting
    /// this to `false` for discovery-only deployments is
    /// defensible; flipping this on simply makes the rejection
    /// happen up-front instead of silently no-oping the state
    /// writes downstream.
    pub require_signed_capabilities: bool,
    /// How often the capability index sweeps expired entries. Low
    /// values waste CPU; high values keep stale peers queryable past
    /// their TTL.
    pub capability_gc_interval: Duration,
    /// This node's subnet. Defaults to [`SubnetId::GLOBAL`] — "no
    /// restriction." Visibility checks compare against this value on
    /// both the publish and subscribe paths.
    pub subnet: SubnetId,
    /// Policy applied to inbound [`CapabilityAnnouncement`]s to
    /// derive each peer's subnet. `None` disables per-peer subnet
    /// tracking; every peer is treated as `GLOBAL`, which in
    /// practice means `Visibility::SubnetLocal` channels ship only
    /// when both sides are `GLOBAL`.
    pub subnet_policy: Option<Arc<SubnetPolicy>>,
    /// Visibility applied on publish when a channel has **no**
    /// registered config in the local
    /// [`ChannelConfigRegistry`]. Defaults to
    /// [`Visibility::Global`] — simple deployments without a
    /// registry publish unrestricted, which is the lowest-
    /// friction default for single-subnet meshes.
    ///
    /// Security-conservative deployments (fleets where forgetting
    /// to register a channel should not silently leak messages
    /// across subnets) set this to
    /// [`Visibility::SubnetLocal`]. The publish path reads it on
    /// every fanout, so toggling it propagates without a restart.
    ///
    /// This is **only** the fallback for unregistered channels —
    /// a channel with an explicit registry entry always uses
    /// its configured visibility.
    pub default_visibility: Visibility,
    /// Minimum time between successive
    /// [`MeshNode::announce_capabilities`] broadcasts from this
    /// origin. Calls within the window coalesce: the local index
    /// and `local_announcement` are updated so self-queries + late-
    /// joiner session-open pushes reflect the latest caps, but the
    /// network broadcast is skipped. Rate-limits apps that
    /// re-announce in tight loops.
    pub min_announce_interval: Duration,
    /// Period between `TokenCache` expiry sweeps. A subscriber
    /// whose token expires mid-subscription is evicted from the
    /// [`SubscriberRoster`] and revoked from the [`AuthGuard`]
    /// within one sweep interval. Set to [`Duration::MAX`] (or any
    /// value longer than the mesh's lifetime) to disable the
    /// sweep — publishes will still re-check the guard, so this
    /// mainly affects how quickly stale tokens drop off the
    /// roster.
    pub token_sweep_interval: Duration,
    /// Authorization-failure threshold per peer per window. A peer
    /// that exceeds this count across a rolling
    /// [`Self::auth_failure_window`] gets throttled — subsequent
    /// subscribes short-circuit with `AckReason::RateLimited` for
    /// [`Self::auth_throttle_duration`] without running the
    /// cap-filter + ed25519 path. Set to `u16::MAX` to disable.
    pub max_auth_failures_per_window: u16,
    /// Rolling window over which failed subscribes are counted for
    /// the throttle check above. Default: 60 s.
    pub auth_failure_window: Duration,
    /// How long a peer stays throttled after tripping the
    /// failure threshold. Default: 30 s.
    pub auth_throttle_duration: Duration,
    /// Override the mesh's public-facing `SocketAddr` — the
    /// address peers see this node as reachable at. When `Some`,
    /// the classifier's background sweep is skipped entirely and
    /// the node immediately advertises `NatClass::Open` with the
    /// supplied `SocketAddr` on its capability announcements.
    ///
    /// Intended for:
    ///
    /// - **Port-forwarded servers.** An operator who has manually
    ///   configured a port forward knows the external address
    ///   directly; setting this short-circuits the multi-peer
    ///   classification that wouldn't discover anything new.
    /// - **Stage-4 port mapping (UPnP / NAT-PMP / PCP).** A
    ///   successful mapping installation records the mapped
    ///   external `ip:port` here, so subsequent peers see the
    ///   node as `Open` without the classifier needing to probe
    ///   for a reflex.
    ///
    /// Framing (plan §4): this is an optimization surface, not a
    /// connectivity requirement — a node with no override still
    /// reaches every peer through routed-handshake. Stored on
    /// `MeshNodeConfig` so both programmatic callers and future
    /// port-mapping runtime writers have a single site to update.
    ///
    /// Default: `None` (use classifier observations).
    #[cfg(feature = "nat-traversal")]
    pub reflex_override: Option<SocketAddr>,
    /// Attempt to install a UPnP-IGD / NAT-PMP / PCP port
    /// mapping on the operator's router at [`MeshNode::start`]
    /// time, lifting this node to `NatClass::Open` with the
    /// router's external `SocketAddr` when the mapping succeeds.
    ///
    /// Off by default because port mapping modifies state on a
    /// device the operator owns — some deployments explicitly
    /// disable router control from software, and the mesh should
    /// never silently change that.
    ///
    /// When set, `start()` spawns a `PortMapperTask` that:
    ///
    /// 1. Probes NAT-PMP (1 s), falls back to UPnP (2 s).
    /// 2. On install success: calls [`MeshNode::set_reflex_override`]
    ///    with the mapped external address.
    /// 3. Renews on [`super::traversal::TraversalConfig::port_mapping_renewal`]
    ///    cadence (default 30 min).
    /// 4. On 3 consecutive renewal failures: calls
    ///    [`MeshNode::clear_reflex_override`] and exits.
    /// 5. On mesh shutdown: removes the mapping (best-effort).
    ///
    /// **Optimization, not correctness.** Setting this to
    /// `true` on a network without UPnP / NAT-PMP support is
    /// safe — the task exits cleanly after one failed probe
    /// cycle and the classifier takes over as usual.
    ///
    /// Requires the `port-mapping` cargo feature. Reading this
    /// field on a build without the feature is always `false`
    /// at runtime.
    ///
    /// Default: `false`.
    #[cfg(feature = "port-mapping")]
    pub try_port_mapping: bool,
}

impl MeshNodeConfig {
    /// Create with minimal required fields.
    pub fn new(bind_addr: SocketAddr, psk: [u8; 32]) -> Self {
        Self {
            bind_addr,
            psk,
            heartbeat_interval: Duration::from_secs(5),
            session_timeout: Duration::from_secs(30),
            num_shards: 4,
            packet_pool_size: 64,
            default_reliable: false,
            handshake_timeout: Duration::from_secs(5),
            handshake_retries: 3,
            socket_buffers: SocketBufferConfig::for_testing(),
            max_queue_depth: 1024,
            fair_quantum: 16,
            stream_idle_timeout: Duration::from_secs(300),
            max_streams: 4096,
            max_channels_per_peer: 1024,
            membership_ack_timeout: Duration::from_secs(5),
            require_signed_capabilities: true,
            capability_gc_interval: Duration::from_secs(60),
            subnet: SubnetId::GLOBAL,
            subnet_policy: None,
            default_visibility: Visibility::Global,
            min_announce_interval: Duration::from_secs(10),
            token_sweep_interval: Duration::from_secs(30),
            max_auth_failures_per_window: 16,
            auth_failure_window: Duration::from_secs(60),
            auth_throttle_duration: Duration::from_secs(30),
            #[cfg(feature = "nat-traversal")]
            reflex_override: None,
            #[cfg(feature = "port-mapping")]
            try_port_mapping: false,
        }
    }

    /// Set the reflex override — the public `SocketAddr` this
    /// node advertises to peers. See
    /// [`MeshNodeConfig::reflex_override`] for semantics.
    ///
    /// Requires the `nat-traversal` cargo feature.
    #[cfg(feature = "nat-traversal")]
    pub fn with_reflex_override(mut self, external: SocketAddr) -> Self {
        self.reflex_override = Some(external);
        self
    }

    /// Opt into opportunistic UPnP-IGD / NAT-PMP / PCP port
    /// mapping at `start()` time. See
    /// [`MeshNodeConfig::try_port_mapping`] for lifecycle
    /// semantics.
    ///
    /// Requires the `port-mapping` cargo feature.
    #[cfg(feature = "port-mapping")]
    pub fn with_try_port_mapping(mut self, enabled: bool) -> Self {
        self.try_port_mapping = enabled;
        self
    }

    /// Set heartbeat interval.
    pub fn with_heartbeat_interval(mut self, interval: Duration) -> Self {
        self.heartbeat_interval = interval;
        self
    }

    /// Set session timeout.
    pub fn with_session_timeout(mut self, timeout: Duration) -> Self {
        self.session_timeout = timeout;
        self
    }

    /// Set number of shards.
    pub fn with_num_shards(mut self, n: u16) -> Self {
        self.num_shards = n;
        self
    }

    /// Set handshake timing.
    pub fn with_handshake(mut self, retries: usize, timeout: Duration) -> Self {
        self.handshake_retries = retries;
        self.handshake_timeout = timeout;
        self
    }

    /// Require inbound `CapabilityAnnouncement` packets to carry a
    /// signature. Unsigned announcements are dropped silently (a
    /// trace is emitted).
    pub fn with_require_signed_capabilities(mut self, require: bool) -> Self {
        self.require_signed_capabilities = require;
        self
    }

    /// Set the capability-index GC sweep interval.
    pub fn with_capability_gc_interval(mut self, interval: Duration) -> Self {
        self.capability_gc_interval = interval;
        self
    }

    /// Set the minimum interval between outbound capability-
    /// announcement broadcasts. See [`Self::min_announce_interval`].
    pub fn with_min_announce_interval(mut self, interval: Duration) -> Self {
        self.min_announce_interval = interval;
        self
    }

    /// Set the token-expiry sweep interval. See
    /// [`Self::token_sweep_interval`].
    pub fn with_token_sweep_interval(mut self, interval: Duration) -> Self {
        self.token_sweep_interval = interval;
        self
    }

    /// Tune the per-peer authorization-failure rate limit. See
    /// [`Self::max_auth_failures_per_window`].
    pub fn with_auth_failure_limit(
        mut self,
        max_per_window: u16,
        window: Duration,
        throttle: Duration,
    ) -> Self {
        self.max_auth_failures_per_window = max_per_window;
        self.auth_failure_window = window;
        self.auth_throttle_duration = throttle;
        self
    }

    /// Pin this node to a specific subnet.
    pub fn with_subnet(mut self, subnet: SubnetId) -> Self {
        self.subnet = subnet;
        self
    }

    /// Derive each peer's subnet locally by applying this policy to
    /// their inbound [`CapabilityAnnouncement`]s. Mesh-wide policy
    /// consistency is assumed; mismatched policies lead to
    /// asymmetric views of peer subnets.
    pub fn with_subnet_policy(mut self, policy: Arc<SubnetPolicy>) -> Self {
        self.subnet_policy = Some(policy);
        self
    }

    /// Override the visibility applied to publishes on channels
    /// that have **no** registered config. Defaults to
    /// [`Visibility::Global`] — messages flow unrestricted when
    /// no registry entry exists. Flip to
    /// [`Visibility::SubnetLocal`] for fail-closed deployments
    /// where forgetting to register a channel should confine
    /// messages to the local subnet rather than broadcasting
    /// them mesh-wide.
    ///
    /// No effect on channels that *do* have a registry entry —
    /// their configured visibility always wins.
    pub fn with_default_visibility(mut self, visibility: Visibility) -> Self {
        self.default_visibility = visibility;
        self
    }
}

/// Peer connection info.
struct PeerInfo {
    /// Node ID (derived from keypair or assigned)
    node_id: u64,
    /// Address used for direct sends. For peers reached via a relay, this
    /// is the relay's address — packets to the destination go there first.
    addr: SocketAddr,
    /// Encrypted session
    session: Arc<NetSession>,
    /// The peer's Noise static public key (X25519, 32 bytes). Captured
    /// during the handshake and surfaced via
    /// [`MeshNode::peer_static_x25519`] so the identity-envelope path
    /// can seal daemon keypairs to a peer that the session already
    /// trusts. Zero-filled when the session was built without a real
    /// handshake (test paths).
    remote_static_pub: [u8; 32],
}

/// In-flight initiator handshake. The dispatch loop consumes this when a
/// routed msg2 arrives for `peer_node_id`: it pulls the Noise state out,
/// runs `read_message`, derives the session keys, and signals the
/// awaiting `connect_via` caller via the oneshot.
///
/// Keyed in `pending_handshakes` by `peer_node_id as u32 as u64` because
/// the routing header's `src_id` field is only 32 bits — msg2's routing
/// header carries the truncated value, so the dispatch loop can only
/// look up by that. The full `u64` is stored here for peer registration.
struct PendingHandshake {
    noise: NoiseHandshake,
    tx: oneshot::Sender<Result<SessionKeys, CryptoError>>,
}

/// 32-bit "routing identity" projection of a `u64` node_id, used as the
/// key across the routing plane (routing header's `src_id` is `u32`).
/// Encoded back into a `u64` as the low 32 bits, high bits zero, so the
/// same projection is visible on both sides of a routed packet.
#[inline]
fn routing_id(node_id: u64) -> u64 {
    (node_id as u32) as u64
}

/// 64-bit origin-hash projection used as the `AuthGuard` key.
///
/// A prior version truncated to `u32` so the key matched the
/// routing-plane's 32-bit `src_id`, but truncating to 32 bits
/// birthday-collides at ~65 k peers — inside the practical reach of
/// a medium mesh — and lets one subscriber's grant admit a different
/// subscriber's packets. The fan-out fast path keys on the full
/// 64-bit `node_id` (the value it already has in hand), which pushes
/// the collision floor out of reach. The `src_id` field on wire
/// packets is not consulted for authorization.
#[inline]
fn subscriber_origin_hash(node_id: u64) -> u64 {
    node_id
}

/// Replace a zero `Duration` with a 1 s floor.
/// `tokio::time::interval` panics on a zero period; this is the
/// guard every `spawn_*_loop` call site applies before handing the
/// caller-configured interval to tokio. A legitimate "disable this
/// timer" sentinel is `Duration::MAX`, documented on the relevant
/// config fields — `Duration::ZERO` is just pathological input.
///
/// The floor is deliberately coarse (1 s, not 1 ms): a mis-configured
/// zero-interval used to spin the maintenance loop at 1 kHz, calling
/// `index.gc()` + `seen.retain()` a thousand times a second and
/// burning a whole core. 1 Hz keeps the loop obviously alive for
/// observability without an appreciable CPU cost, and is still
/// finer than the default intervals (capability GC ≈ announcement
/// TTL, token sweep 30 s), so it never masks a legitimate
/// fine-grained config — those values are already well above 1 s.
/// Parse an inbound migration payload just far enough to decide
/// whether it's a migration-initiating message that needs a
/// `ComputeNotSupported` response. Returns the encoded reply for
/// `TakeSnapshot` / `SnapshotReady`; `None` for decode failures or
/// mid-migration message types (which arrive only inside an
/// already-live migration and so can't reach a node with no
/// handler at all).
///
/// Used by the mesh dispatch loop when `ctx.migration_handler` is
/// `None` — a bare `Mesh` with no `DaemonRuntime` attached still
/// responds to migration attempts instead of silently dropping
/// them, so the source surfaces `MigrationFailureReason::ComputeNotSupported`
/// promptly rather than timing out.
fn synthesize_compute_not_supported_reply(payload: &[u8]) -> Option<Bytes> {
    use crate::adapter::net::compute::orchestrator::wire as mig_wire;
    use crate::adapter::net::compute::{MigrationFailureReason, MigrationMessage};

    let msg = mig_wire::decode(payload).ok()?;
    let origin = match msg {
        MigrationMessage::TakeSnapshot { daemon_origin, .. }
        | MigrationMessage::SnapshotReady { daemon_origin, .. } => daemon_origin,
        _ => return None,
    };
    let reply = MigrationMessage::MigrationFailed {
        daemon_origin: origin,
        reason: MigrationFailureReason::ComputeNotSupported,
    };
    mig_wire::encode(&reply).ok().map(Bytes::from)
}

#[inline]
fn nonzero_interval(d: Duration) -> Duration {
    if d.is_zero() {
        Duration::from_secs(1)
    } else {
        d
    }
}

/// Race a punch-observer `oneshot::Receiver` against a deadline
/// and handle cleanup of the shared `punch_observers` map with
/// the correct semantics for each outcome. Returns `true` when
/// the observer fired (caller should emit a `PunchAck`), `false`
/// otherwise.
///
/// Three outcomes, each with a distinct cleanup rule:
///
/// - **`Ok(Ok(ka))`** — a matching keep-alive arrived and the
///   receive loop fired our sender. Returns `true`; caller emits
///   the ack. The receive loop already consumed the map entry
///   via `remove` when it fired the oneshot, so no cleanup
///   needed here.
/// - **`Ok(Err(_))`** — our sender was dropped without firing.
///   This happens when a newer observer replaced ours in the
///   map (`DashMap::insert` drops the old value, which wakes
///   our `rx` with `RecvError`). Returning without removing the
///   key is load-bearing: removing would evict the replacement
///   observer that's now legitimately in the map.
/// - **`Err(_)`** — the deadline expired. Our sender is still
///   in the map — if it had been replaced we'd be in the
///   `Ok(Err)` branch above, not here. Remove our stale entry
///   so a late keep-alive doesn't find it.
///
/// History: earlier revisions collapsed `Ok(Err)` and `Err(_)`
/// into a single `remove`-in-both-cases arm, which evicted
/// replacement observers. cubic flagged this as a P2. The
/// three-arm split is tested in `await_punch_observer_outcome`'s
/// unit tests below.
#[cfg(feature = "nat-traversal")]
async fn await_punch_observer_outcome(
    obs_rx: tokio::sync::oneshot::Receiver<super::traversal::rendezvous::Keepalive>,
    deadline: Duration,
    punch_observers: &DashMap<
        SocketAddr,
        tokio::sync::oneshot::Sender<super::traversal::rendezvous::Keepalive>,
    >,
    peer_reflex: SocketAddr,
) -> bool {
    match tokio::time::timeout(deadline, obs_rx).await {
        // Observer fired with a real keep-alive.
        Ok(Ok(_ka)) => true,
        // Sender was dropped — almost certainly replaced by a
        // newer observer for the same peer_reflex. Leaving the
        // map alone is correct: the replacement's sender is the
        // current value and a remove would evict it.
        Ok(Err(_)) => false,
        // Deadline fired with our sender still in the map. Evict
        // so a late keep-alive doesn't find a stale entry.
        Err(_) => {
            punch_observers.remove(&peer_reflex);
            false
        }
    }
}

/// Rolling-window auth-failure tracker, one entry per peer.
/// Lives behind a per-key `Mutex` so updates from concurrent
/// subscribes don't race each other on the same peer's counter.
#[derive(Debug, Default)]
struct AuthFailureState {
    /// Failures accumulated inside the current window.
    failures: u16,
    /// Start of the window. Resets to `Instant::now()` once
    /// `auth_failure_window` has elapsed since the current window
    /// opened.
    window_start: Option<std::time::Instant>,
    /// If set, the peer is throttled until this instant and every
    /// subscribe short-circuits with `RateLimited`.
    throttled_until: Option<std::time::Instant>,
}

/// Evict subscribers whose tokens have expired. Walks the roster by
/// peer and, for every `require_token` channel they hold, runs the
/// full token-cache check. Expired entries are revoked in the
/// [`AuthGuard`] and removed from the [`SubscriberRoster`].
///
/// Skip conditions (short-circuits to no-op):
///
/// - No `token_cache`: `require_token` channels reject every
///   subscribe anyway, so the roster contains no token-gated
///   entries.
/// - No `channel_configs`: nothing to check `require_token`
///   against, so every roster entry is treated as open and left
///   alone.
///
/// Pulled into a free fn (not a method) so the sweep loop can
/// call it without capturing `&self` through the async closure.
fn sweep_expired_subscribers(
    roster: &SubscriberRoster,
    guard: &AuthGuard,
    token_cache: Option<&Arc<TokenCache>>,
    peer_entity_ids: &DashMap<u64, EntityId>,
    channel_configs: Option<&Arc<ChannelConfigRegistry>>,
) {
    let (Some(cache), Some(configs)) = (token_cache, channel_configs) else {
        return;
    };
    // Snapshot (node_id, entity_id) pairs so we don't hold the
    // DashMap read guard across the token checks below.
    let peers: Vec<(u64, EntityId)> = peer_entity_ids
        .iter()
        .map(|e| (*e.key(), e.value().clone()))
        .collect();
    for (node_id, entity_id) in peers {
        for channel_id in roster.channels_for(node_id) {
            let name = channel_id.name();
            let Some(cfg) = configs.get_by_name(name.as_str()) else {
                continue;
            };
            if !cfg.require_token {
                continue;
            }
            // `check` validates signature + time bounds. Any error
            // (expired, not_yet_valid, invalid_signature, not_authorized)
            // means this subscriber is no longer authorized.
            let authorized = cache
                .check(
                    &entity_id,
                    super::identity::TokenScope::SUBSCRIBE,
                    name.hash(),
                )
                .is_ok();
            if !authorized {
                guard.revoke_channel(subscriber_origin_hash(node_id), name);
                roster.remove(&channel_id, node_id);
                tracing::debug!(
                    node_id = format!("{:#x}", node_id),
                    channel = name.as_str(),
                    "auth: evicted subscriber with expired/invalid token",
                );
            }
        }
    }
}

/// Default TTL for the routing header we stamp on routed handshake
/// packets. Far above any realistic relay chain; the routing layer
/// drops at zero.
const DEFAULT_HANDSHAKE_TTL: u8 = 16;

/// Maximum hop count a pingwave may carry on receipt. Pingwaves with
/// `hop_count >= MAX_HOPS` are dropped — they install no route, no
/// graph edge, and are not re-broadcast. TTL bounds forwarding at the
/// emitter; `MAX_HOPS` is the receive-time counterpart that prevents
/// an inflated-hop-count advertisement (malicious or buggy) from
/// populating the routing table with an arbitrarily-distant entry.
/// Value sized to accommodate the largest plausibly-useful mesh depth
/// while still bounding count-to-infinity worst cases.
const MAX_HOPS: u8 = 16;

/// Multi-peer mesh node.
///
/// Composes `NetSession` (per-peer encryption), `NetRouter` (forwarding),
/// and `FailureDetector` (heartbeat monitoring) behind a single UDP socket.
pub struct MeshNode {
    /// This node's identity (ed25519, for signing and node_id derivation).
    /// Used in Phase 3 for subprotocol message signing.
    #[allow(dead_code)]
    identity: EntityKeypair,
    /// Noise static keypair (Curve25519, for handshakes)
    static_keypair: StaticKeypair,
    /// Derived node ID
    node_id: u64,
    /// Configuration
    config: MeshNodeConfig,
    /// Shared UDP socket
    socket: Arc<NetSocket>,
    /// Per-peer sessions keyed by node_id. Keying by node_id (rather than
    /// SocketAddr) is required for relayed sessions: if A connects to C via
    /// relay B, both peers share B's wire address, so a SocketAddr-keyed map
    /// would overwrite B's session with C's.
    peers: Arc<DashMap<u64, PeerInfo>>,
    /// Reverse lookup for dispatch: incoming source address → node_id. Only
    /// populated for directly-connected peers; relayed peers are resolved by
    /// session_id during dispatch.
    addr_to_node: Arc<DashMap<SocketAddr, u64>>,
    /// Router for forwarding decisions
    router: Arc<NetRouter>,
    /// Failure detector
    failure_detector: Arc<FailureDetector>,
    /// Inbound event queues (shared with receive loop)
    inbound: InboundQueues,
    /// Optional migration subprotocol handler — same `ArcSwapOption`
    /// surface as on `MeshNode`, propagated into the dispatch
    /// context so the packet-receive loop stays lock-free.
    migration_handler: Arc<ArcSwapOption<MigrationSubprotocolHandler>>,
    /// In-flight routed-handshake initiators, keyed by the responder's
    /// node_id. Populated by `connect_via`; consumed by the dispatch
    /// loop when the matching msg2 arrives.
    pending_handshakes: Arc<DashMap<u64, PendingHandshake>>,
    /// In-flight direct-handshake initiators, keyed by the peer's
    /// `SocketAddr`. Populated by `try_handshake_initiator` BEFORE
    /// sending msg1; consumed by the dispatch loop when a matching
    /// direct handshake response arrives. See the matching field
    /// on `DispatchCtx` for context.
    pending_direct_initiators: Arc<DashMap<SocketAddr, oneshot::Sender<Bytes>>>,
    /// Proximity graph — topology awareness from pingwave propagation
    proximity_graph: Arc<ProximityGraph>,
    /// Automatic reroute policy
    reroute_policy: Arc<ReroutePolicy>,
    /// Node ID → SocketAddr map (shared with reroute policy)
    peer_addrs: Arc<DashMap<u64, SocketAddr>>,
    /// Partition filter for simulating network splits
    partition_filter: PartitionFilter,
    /// Per-channel subscriber roster (daemon-layer fan-out).
    roster: Arc<SubscriberRoster>,
    /// Channel config registry consulted by incoming `Subscribe` packets
    /// for ACL decisions. When `None`, ACL is bypassed and all subscribes
    /// are accepted — used by tests and by nodes that don't run channels.
    channel_configs: Option<Arc<ChannelConfigRegistry>>,
    /// In-flight Subscribe/Unsubscribe requests keyed by nonce.
    pending_membership_acks: Arc<DashMap<u64, oneshot::Sender<MembershipAck>>>,
    /// In-flight reflex probes keyed by the responder's `node_id`.
    /// Shared with `DispatchCtx` via `Arc` clone so the dispatcher
    /// can complete oneshots without routing back through
    /// `MeshNode`. Details on the field's usage in the
    /// `DispatchCtx` docstring.
    #[cfg(feature = "nat-traversal")]
    pending_reflex_probes: Arc<DashMap<u64, (u64, oneshot::Sender<std::net::SocketAddr>)>>,
    /// In-flight rendezvous handshakes keyed by the *counterpart*
    /// endpoint's `node_id` — i.e. the `peer` field in the
    /// incoming `PunchIntroduce`. The coordinator-side fanout
    /// (stage 3b) drops `PunchIntroduce` messages silently when
    /// no waiter is installed; endpoint callers that want to
    /// observe an introduce install an entry here via the
    /// stage-3c surface before calling the coordinator.
    #[cfg(feature = "nat-traversal")]
    pending_punch_introduces: Arc<
        DashMap<
            u64,
            (
                u64,
                oneshot::Sender<super::traversal::rendezvous::PunchIntroduce>,
            ),
        >,
    >,
    /// In-flight punch acknowledgements keyed by the *sender*
    /// endpoint's `node_id` — i.e. the `from_peer` field on the
    /// arriving `PunchAck`. `connect_direct` awaits this map on
    /// the `SinglePunch` path so `punches_succeeded` only bumps
    /// when the peer actually confirmed the punch.
    #[cfg(feature = "nat-traversal")]
    pending_punch_acks:
        Arc<DashMap<u64, (u64, oneshot::Sender<super::traversal::rendezvous::PunchAck>)>>,
    /// Monotonic counter for waiter generations used by the
    /// three `pending_*` maps above. Each insert stamps its
    /// entry with a unique `gen`; removal is a `remove_if` check
    /// that the entry's gen matches ours. Without this, a
    /// timeout cleanup racing a concurrent replacement could
    /// evict the new waiter — a cubic-flagged P1 bug
    /// (`connect_direct` + `request_punch` both affected).
    #[cfg(feature = "nat-traversal")]
    next_waiter_gen: Arc<std::sync::atomic::AtomicU64>,
    /// Keep-alive observers for in-progress punches, keyed by
    /// the `SocketAddr` we're watching for inbound traffic (the
    /// counterpart's `peer_reflex`). The receive loop fires the
    /// oneshot on the first matching keep-alive and the punch-
    /// scheduler task reacts by emitting a `PunchAck`.
    #[cfg(feature = "nat-traversal")]
    punch_observers:
        Arc<DashMap<SocketAddr, oneshot::Sender<super::traversal::rendezvous::Keepalive>>>,
    /// Current NAT classification, encoded via
    /// [`super::traversal::classify::NatClass::as_u8`]. Starts as
    /// `Unknown` (`0`) and is updated by the classification sweep
    /// spawned in [`Self::start`]. Stored atomically so the
    /// announce-capabilities hot path can read without locking.
    #[cfg(feature = "nat-traversal")]
    nat_class: Arc<std::sync::atomic::AtomicU8>,
    /// Current reflex address (this node's public-facing
    /// `SocketAddr` as observed by remote peers), or `None` until
    /// the classification sweep has produced at least one reflex
    /// observation. Piggybacks on outbound `CapabilityAnnouncement`
    /// payloads so peers can attempt a direct connect without a
    /// separate discovery round-trip.
    #[cfg(feature = "nat-traversal")]
    reflex_addr: Arc<ArcSwapOption<SocketAddr>>,
    /// Runtime flag: `true` when the current `reflex_addr` came
    /// from an operator-set or port-mapper-installed override,
    /// `false` when it came from classification observations.
    /// When `true`, the classifier sweep short-circuits and
    /// `reflex_addr` stays pinned until
    /// [`Self::clear_reflex_override`] is called.
    ///
    /// Separate from `MeshNodeConfig::reflex_override` because the
    /// config is moved into `MeshNode` at construction time and
    /// can't be mutated afterward. A stage-4b `PortMapper` task
    /// installs a mapping mid-session; this atomic is what lets
    /// a `&self` setter turn the override on without racing the
    /// announce-capabilities path.
    #[cfg(feature = "nat-traversal")]
    reflex_override_active: Arc<std::sync::atomic::AtomicBool>,
    /// Publication barrier held briefly during any code path
    /// that touches more than one of `nat_class`, `reflex_addr`,
    /// and `reflex_override_active` as a group. The three atomics
    /// underneath are still lock-free for single-field readers
    /// (`nat_class()`, `reflex_addr()`, and the traversal-loop
    /// branches that only check one value), but the setters
    /// (`set_reflex_override`, `clear_reflex_override`) and the
    /// classifier commit (`commit_reclassify_observations`) write
    /// the triple under this lock, and readers that need a
    /// consistent snapshot (`announce_capabilities_with` emits
    /// `nat_class` AND `reflex_addr` into the same outbound
    /// announcement) read the triple under this lock too.
    ///
    /// A cubic P1 review flagged that reading the three atomics
    /// independently let a concurrent announce publish a torn
    /// state — e.g., a just-cleared override with reflex=None
    /// paired with the still-Open NAT class, or a just-set
    /// override's new reflex paired with the pre-override
    /// Unknown class. This mutex closes that window; writers
    /// serialize against each other and against the multi-field
    /// read in announce.
    #[cfg(feature = "nat-traversal")]
    traversal_publish_mu: Arc<parking_lot::Mutex<()>>,
    /// Traversal tunables — probe timeouts, classification
    /// deadline, punch cadence, and port-mapping renewal interval.
    /// Defaults match `docs/NAT_TRAVERSAL_PLAN.md`. Exposed via
    /// `MeshBuilder` setters in stage 5; internal-only today.
    #[cfg(feature = "nat-traversal")]
    traversal_config: super::traversal::TraversalConfig,
    /// Cumulative counters for `connect_direct` outcomes. Every
    /// punch attempt, success, and relay fallback is recorded
    /// here; read via [`Self::traversal_stats`]. Observability
    /// surface, not control — the traversal behavior doesn't read
    /// this.
    #[cfg(feature = "nat-traversal")]
    traversal_stats: Arc<super::traversal::TraversalStats>,
    /// Capability index populated by inbound
    /// `SUBPROTOCOL_CAPABILITY_ANN` packets and the local
    /// `announce_capabilities` path. Self-index so single-node
    /// queries return us too.
    capability_index: Arc<CapabilityIndex>,
    /// Dedup cache for multi-hop capability announcements. Keyed by
    /// `(origin_node_id, version)` — the same discriminator
    /// `CapabilityIndex` uses to skip stale announcements. Entries
    /// are evicted by the capability GC loop once their
    /// announcement's effective lifetime (2× `ttl_secs`) has
    /// elapsed. Mirrors the `seen_pingwaves` cache in
    /// [`ProximityGraph`].
    seen_announcements: Arc<DashMap<(u64, u64), std::time::Instant>>,
    /// Timestamp of the most recent outbound capability-announcement
    /// broadcast from this origin. Compared against
    /// `config.min_announce_interval` on every `announce_capabilities_with`
    /// call; within-window calls coalesce to a local self-index
    /// update without a network broadcast.
    last_announce_at: Arc<parking_lot::Mutex<Option<std::time::Instant>>>,
    /// Most recent `CapabilityAnnouncement` this node published.
    /// Pushed to new peers right after `accept` / `connect`
    /// completes, so late joiners pick up our caps without waiting
    /// for a re-announce. `None` until the first `announce_*` call.
    local_announcement: Arc<ArcSwapOption<CapabilityAnnouncement>>,
    /// Monotonic version counter used when stamping our own
    /// announcements. `CapabilityIndex::index` skips older versions,
    /// so this must move forward across restarts if the caller wants
    /// their announcements accepted.
    capability_version: Arc<AtomicU64>,
    /// This node's subnet. Copy of `config.subnet`, hoisted to the
    /// top level because the publish + subscribe hot paths read it
    /// without going through the config struct.
    local_subnet: SubnetId,
    /// Subnet policy applied to inbound `CapabilityAnnouncement`s.
    /// `None` disables per-peer subnet tracking.
    local_subnet_policy: Option<Arc<SubnetPolicy>>,
    /// Per-peer subnet map. Keys are `node_id`; values are the
    /// subnet derived from each peer's most recent announcement via
    /// `local_subnet_policy`. Unknown peers default to
    /// [`SubnetId::GLOBAL`] at read time.
    peer_subnets: Arc<DashMap<u64, SubnetId>>,
    /// Per-peer entity-id map. Keys are `node_id`; values are the
    /// 32-byte ed25519 public key carried on the peer's most recent
    /// `CapabilityAnnouncement`. Load-bearing for channel auth —
    /// without it, `require_token` channels can't match a token's
    /// `subject` to the subscribing peer.
    peer_entity_ids: Arc<DashMap<u64, EntityId>>,
    /// Shared token cache used by the channel-auth path. When
    /// `None`, `can_publish` / `can_subscribe` fall back to a
    /// fresh empty cache — which means `require_token` channels
    /// always reject. SDK builders wire this up from the caller's
    /// `Identity`.
    token_cache: Option<Arc<TokenCache>>,
    /// Per-packet authorization fast path. Populated when a
    /// subscribe clears `authorize_subscribe`; consulted on every
    /// publish fan-out via `check_fast`. The bloom filter + verified
    /// cache keep authorization at O(1) without per-packet
    /// signature verification. See
    /// [`docs/CHANNEL_AUTH_GUARD_PLAN.md`](../../../../docs/CHANNEL_AUTH_GUARD_PLAN.md).
    auth_guard: Arc<AuthGuard>,
    /// Per-peer auth-failure tracker. Counts failed
    /// `authorize_subscribe` attempts per `auth_failure_window` and
    /// throttles bursts — peers that exceed
    /// `max_auth_failures_per_window` short-circuit with
    /// `RateLimited` for `auth_throttle_duration` without running
    /// the cap-filter + ed25519 verify path. Successful subscribes
    /// clear the counter for that peer.
    auth_failures: Arc<DashMap<u64, AuthFailureState>>,
    /// Background tasks
    tasks: Arc<tokio::sync::Mutex<Vec<JoinHandle<()>>>>,
    /// Shutdown flag
    shutdown: Arc<AtomicBool>,
    /// Shutdown notifier
    shutdown_notify: Arc<Notify>,
    /// Whether the node has been started
    started: AtomicBool,
    /// Number of `accept()` calls currently awaiting
    /// `handshake_responder`. A simple `started.load(Acquire)`
    /// guard at `accept` entry is a TOCTOU — `start()` could fire
    /// between the check and the `handshake_responder` poll, after
    /// which the dispatcher would race the responder for inbound
    /// msg1 packets and silently swallow them. The counter closes
    /// the race: `accept()` increments on entry, decrements on
    /// exit, and `start()` refuses while any `accept()` is in
    /// flight.
    accept_in_flight: std::sync::atomic::AtomicUsize,
}

impl MeshNode {
    /// Get the Noise static public key (for peers to connect to this node).
    pub fn public_key(&self) -> &[u8; 32] {
        &self.static_keypair.public
    }

    /// Whether [`Self::shutdown`] has been invoked on this node.
    ///
    /// Exposed for tests and for FFI callers that want to verify a
    /// shutdown actually landed (rather than being a silent no-op
    /// because extra `Arc` references were outstanding, as an earlier
    /// `net_mesh_shutdown` variant did).
    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }

    /// Create a new mesh node.
    ///
    /// Binds a UDP socket but does not connect to any peers yet.
    /// Call `connect()` to establish sessions with peers, then
    /// `start()` to begin the receive loop.
    pub async fn new(
        identity: EntityKeypair,
        config: MeshNodeConfig,
    ) -> Result<Self, AdapterError> {
        let node_id = identity.node_id();
        let static_keypair = StaticKeypair::generate();

        let socket = NetSocket::with_config(config.bind_addr, config.socket_buffers)
            .await
            .map_err(|e| AdapterError::Connection(format!("bind failed: {}", e)))?;
        let socket = Arc::new(socket);

        let router_config = RouterConfig {
            local_id: node_id,
            // Router binds to an ephemeral port for its send loop. It uses
            // this socket only for forwarding packets — the main socket
            // handles all receives.
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            max_queue_depth: config.max_queue_depth,
            fair_quantum: config.fair_quantum,
            ..Default::default()
        };
        let router = NetRouter::new(router_config)
            .await
            .map_err(|e| AdapterError::Connection(format!("router bind failed: {}", e)))?;

        let router = Arc::new(router);

        // Configure route staleness. Routes learned from pingwaves age
        // out if a fresh pingwave hasn't refreshed them in this window;
        // direct routes are refreshed by the heartbeat loop, so they
        // stay fresh as long as the session is alive.
        router
            .routing_table()
            .set_max_route_age(config.session_timeout.saturating_mul(3));

        let peer_addrs: Arc<DashMap<u64, SocketAddr>> = Arc::new(DashMap::new());

        // Hoist `peers` and `addr_to_node` out of the struct literal so
        // the failure-detector `on_failure` callback below can evict
        // dead peers from them. A previous implementation removed the
        // failed peer from the reroute policy, roster, subnet map,
        // entity-id map, and capability index — but left the PeerInfo
        // (including its session) in `peers` indefinitely. Subsequent
        // `send_to_peer` calls would then route through a dead session
        // and silently drop packets via UDP until an application-layer
        // timeout fired.
        let peers: Arc<DashMap<u64, PeerInfo>> = Arc::new(DashMap::new());
        let addr_to_node: Arc<DashMap<SocketAddr, u64>> = Arc::new(DashMap::new());

        // Create proximity graph for topology awareness.
        //
        // Peers are seeded into the graph via `node_id_to_graph_id(peer_node_id)`
        // (see `connect`/`accept`). The local node must use the *same*
        // encoding or path lookups between local and peers would miss —
        // `entity_id().as_bytes()` would put this node under a different
        // key than what peers see for it.
        let graph_node_id = node_id_to_graph_id(node_id);
        let proximity_graph = Arc::new(ProximityGraph::new(
            graph_node_id,
            ProximityConfig::default(),
        ));

        // Create reroute policy with proximity graph for topology-aware alternates
        let reroute_policy = Arc::new(
            ReroutePolicy::new(router.routing_table().clone(), peer_addrs.clone())
                .with_proximity_graph(proximity_graph.clone()),
        );

        // Subscriber roster for channel fan-out; also wired into the
        // failure-detector `on_failure` callback so that a peer going
        // Failed is removed from every channel it was subscribed to.
        let roster: Arc<SubscriberRoster> = Arc::new(SubscriberRoster::new());

        // Peer-subnet map (Stage D). Populated when inbound
        // `SUBPROTOCOL_CAPABILITY_ANN` packets arrive and the local
        // `SubnetPolicy` derives a subnet for the sender. Created
        // here so the failure callback can evict stale entries on
        // session loss — otherwise reconnects would silently reuse
        // the old subnet until the next announcement arrived.
        let peer_subnets: Arc<DashMap<u64, SubnetId>> = Arc::new(DashMap::new());
        // Peer entity-id map (Stage E). Populated from each inbound
        // `CapabilityAnnouncement`. Evicted alongside `peer_subnets`
        // on session failure so a reconnect doesn't silently reuse
        // the old identity.
        let peer_entity_ids: Arc<DashMap<u64, EntityId>> = Arc::new(DashMap::new());

        // Capability index — hoisted out of the struct literal so
        // the failure-detector `on_failure` callback can hold a
        // clone. Without this eviction, a failed peer's advertised
        // reflex would linger in the index and the rendezvous
        // coordinator could hand it to a PunchRequest initiator
        // even though the peer is known dead (TEST_COVERAGE_PLAN
        // §P1-5 / TRANSPORT-adjacent bug: three-way agreement
        // between the failure detector, the routing table, and
        // the capability index on peer-death).
        let capability_index: Arc<CapabilityIndex> = Arc::new(CapabilityIndex::new());

        // Wire failure detector with reroute callbacks + roster eviction.
        //
        // Note: the `peers` / `addr_to_node` / `peer_addrs` maps are
        // *not* evicted here. Keeping the session entry lets a
        // transient-partition recovery work — once `b`'s heartbeats
        // resume, the packet-dispatch path matches them against the
        // retained session_id, calls `failure_detector.heartbeat`,
        // and the detector's `on_recovery` callback undoes the
        // reroute. Permanent failures are swept separately by the
        // heartbeat loop (see `spawn_heartbeat` — once a peer has
        // been `Failed` for longer than the cleanup window and has
        // not produced any traffic, the loop drops the entry from
        // `peers` / `addr_to_node` / `peer_addrs`).
        let rp_failure = reroute_policy.clone();
        let rp_recovery = reroute_policy.clone();
        let roster_failure = roster.clone();
        let peer_subnets_failure = peer_subnets.clone();
        let peer_entity_ids_failure = peer_entity_ids.clone();
        let capability_index_failure = capability_index.clone();
        let failure_detector = FailureDetector::with_config(FailureDetectorConfig {
            timeout: config.session_timeout,
            miss_threshold: 3,
            suspicion_threshold: 2,
            cleanup_interval: Duration::from_secs(60),
        })
        .on_failure(move |node_id| {
            rp_failure.on_failure(node_id);
            let removed = roster_failure.remove_peer(node_id);
            if !removed.is_empty() {
                tracing::debug!(
                    node_id = format!("{:#x}", node_id),
                    channels = removed.len(),
                    "roster: evicted failed peer from channels"
                );
            }
            peer_subnets_failure.remove(&node_id);
            peer_entity_ids_failure.remove(&node_id);
            // Drop the dead peer's cached capabilities + reflex.
            // Without this, a rendezvous coordinator could still
            // hand a PunchRequest initiator the failed peer's
            // (stale) reflex, leading to wasted punch attempts
            // against a dead address. The three maps above
            // (routes, subnets, entity-ids) are all cleared on
            // failure; the capability index is now consistent
            // with them.
            capability_index_failure.remove(node_id);
        })
        .on_recovery(move |node_id| rp_recovery.on_recovery(node_id));

        let pending_handshakes: Arc<DashMap<u64, PendingHandshake>> = Arc::new(DashMap::new());
        let pending_direct_initiators: Arc<DashMap<SocketAddr, oneshot::Sender<Bytes>>> =
            Arc::new(DashMap::new());

        // Hoist the subnet knobs before `config` is moved into the
        // struct literal; the publish + subscribe paths read these
        // without going back through `config`.
        let local_subnet = config.subnet;
        let local_subnet_policy = config.subnet_policy.clone();
        // Pre-apply the reflex override so the node starts in
        // `Open` + `reflex_addr = Some(override)` state before
        // the first capability announcement leaves the box. Skips
        // the classifier sweep — operators with manually-forwarded
        // ports (or stage-4 port mapping) don't need multi-peer
        // probing to discover what they already know.
        #[cfg(feature = "nat-traversal")]
        let initial_reflex_override = config.reflex_override;

        Ok(Self {
            identity,
            static_keypair,
            node_id,
            config,
            socket,
            peers,
            addr_to_node,
            router,
            failure_detector: Arc::new(failure_detector),
            inbound: Arc::new(DashMap::new()),
            migration_handler: Arc::new(ArcSwapOption::empty()),
            pending_handshakes,
            pending_direct_initiators,
            proximity_graph,
            reroute_policy,
            peer_addrs,
            partition_filter: Arc::new(dashmap::DashSet::new()),
            roster,
            channel_configs: None,
            pending_membership_acks: Arc::new(DashMap::new()),
            #[cfg(feature = "nat-traversal")]
            pending_reflex_probes: Arc::new(DashMap::new()),
            #[cfg(feature = "nat-traversal")]
            pending_punch_introduces: Arc::new(DashMap::new()),
            #[cfg(feature = "nat-traversal")]
            pending_punch_acks: Arc::new(DashMap::new()),
            #[cfg(feature = "nat-traversal")]
            next_waiter_gen: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            #[cfg(feature = "nat-traversal")]
            punch_observers: Arc::new(DashMap::new()),
            #[cfg(feature = "nat-traversal")]
            nat_class: Arc::new(std::sync::atomic::AtomicU8::new(
                if initial_reflex_override.is_some() {
                    super::traversal::classify::NatClass::Open.as_u8()
                } else {
                    super::traversal::classify::NatClass::Unknown.as_u8()
                },
            )),
            #[cfg(feature = "nat-traversal")]
            reflex_addr: Arc::new(match initial_reflex_override {
                Some(addr) => ArcSwapOption::from_pointee(addr),
                None => ArcSwapOption::empty(),
            }),
            #[cfg(feature = "nat-traversal")]
            reflex_override_active: Arc::new(std::sync::atomic::AtomicBool::new(
                initial_reflex_override.is_some(),
            )),
            #[cfg(feature = "nat-traversal")]
            traversal_publish_mu: Arc::new(parking_lot::Mutex::new(())),
            #[cfg(feature = "nat-traversal")]
            traversal_config: super::traversal::TraversalConfig::default(),
            #[cfg(feature = "nat-traversal")]
            traversal_stats: Arc::new(super::traversal::TraversalStats::new()),
            capability_index,
            seen_announcements: Arc::new(DashMap::new()),
            last_announce_at: Arc::new(parking_lot::Mutex::new(None)),
            local_announcement: Arc::new(ArcSwapOption::empty()),
            capability_version: Arc::new(AtomicU64::new(0)),
            local_subnet,
            local_subnet_policy,
            peer_subnets,
            peer_entity_ids,
            token_cache: None,
            auth_guard: Arc::new(AuthGuard::new()),
            auth_failures: Arc::new(DashMap::new()),
            tasks: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            shutdown: Arc::new(AtomicBool::new(false)),
            shutdown_notify: Arc::new(Notify::new()),
            started: AtomicBool::new(false),
            accept_in_flight: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// Get this node's ID.
    pub fn node_id(&self) -> u64 {
        self.node_id
    }

    /// The per-packet authorization fast path. Writes land here on
    /// successful subscribe (via `AuthGuard::allow_channel`) and
    /// reads happen on every publish fan-out. Exposed primarily for
    /// tests + operator observability; production code should reach
    /// for `register_channel` / `subscribe_channel` instead.
    pub fn auth_guard(&self) -> &Arc<AuthGuard> {
        &self.auth_guard
    }

    /// The shared `TokenCache` installed on this node, if any. Only
    /// populated when a caller registered one via
    /// [`Self::set_token_cache`]. Exposed for tests that need to
    /// assert the cache is *not* populated as a side effect of a
    /// rejected subscribe.
    pub fn token_cache(&self) -> Option<&Arc<TokenCache>> {
        self.token_cache.as_ref()
    }

    /// Get this node's ed25519 entity id (derived from the
    /// keypair handed to `MeshNode::new`). 32 bytes. Used by
    /// `CapabilityAnnouncement` + channel-auth path.
    pub fn entity_id(&self) -> &EntityId {
        self.identity.entity_id()
    }

    /// Look up a peer's pinned `entity_id`, if the TOFU binding
    /// has been established. Returns `None` before we've received
    /// a signature-verified `CapabilityAnnouncement` from the peer.
    /// Exposed primarily for tests + operator observability; the
    /// channel-auth subscribe gate consults this map internally.
    pub fn peer_entity_id(&self, node_id: u64) -> Option<EntityId> {
        self.peer_entity_ids
            .get(&node_id)
            .map(|e| e.value().clone())
    }

    /// The peer's socket address, if we have an active session
    /// with them. Used by the migration subprotocol to route
    /// orchestrator-originated messages (e.g. `TakeSnapshot`) to
    /// the source node by its `node_id`.
    pub fn peer_addr(&self, node_id: u64) -> Option<SocketAddr> {
        self.peers.get(&node_id).map(|e| e.value().addr)
    }

    /// Build a [`MigrationIdentityContext`](crate::adapter::net::subprotocol::MigrationIdentityContext)
    /// bound to this node.
    ///
    /// The context's closures capture this node's long-term Noise
    /// static private key (for the envelope-open path) and an
    /// `Arc`-clone of the peer map (for the peer-static lookup used
    /// by the source-side seal path). The private key is wrapped in
    /// a `StaticSecret` inside the `unseal_snapshot` closure, which
    /// is `zeroize`-on-drop — the key is never surfaced as a
    /// readable field on the returned value.
    ///
    /// Used by the SDK's compute runtime to wire identity-envelope
    /// support into the migration dispatcher without handing the
    /// key across the crate boundary. The previous shape exposed
    /// `static_x25519_priv() -> [u8; 32]` as a `pub` method, which
    /// leaked long-term secret material to any SDK caller — any
    /// code with an `Arc<Mesh>` could copy the node's identity key
    /// out and impersonate it indefinitely.
    pub fn migration_identity_context(
        &self,
    ) -> crate::adapter::net::subprotocol::MigrationIdentityContext {
        use crate::adapter::net::state::snapshot::StateSnapshot;
        use crate::adapter::net::subprotocol::MigrationIdentityContext;

        // Construct once; `StaticSecret` zeroizes on drop, so the
        // key is wiped when the last owner of the Arc'd closure is
        // dropped. Rebuilding the StaticSecret on every call would
        // copy the raw bytes through a short-lived stack variable —
        // bounded exposure is fine but once-at-construction is
        // strictly less.
        let priv_secret = x25519_dalek::StaticSecret::from(self.static_keypair.private);
        let unseal_snapshot = Arc::new(
            move |snapshot: &StateSnapshot|
                  -> Result<Option<_>, crate::adapter::net::identity::EnvelopeError> {
                snapshot.open_identity_envelope(&priv_secret)
            },
        );

        let peers = self.peers.clone();
        let peer_static_lookup = Arc::new(move |node_id: u64| {
            peers.get(&node_id).and_then(|e| {
                let pk = e.value().remote_static_pub;
                if pk == [0u8; 32] {
                    None
                } else {
                    Some(pk)
                }
            })
        });

        MigrationIdentityContext {
            unseal_snapshot,
            peer_static_lookup,
        }
    }

    /// The peer's Noise static X25519 public key, captured during
    /// the handshake that established the session. Load-bearing for
    /// daemon migration: the source uses this key as the seal
    /// recipient on the `IdentityEnvelope`, so the only party that
    /// can unseal the daemon's ed25519 seed is the peer whose
    /// static private key completed the Noise handshake.
    ///
    /// Returns `None` if we have no session with `node_id`, or if
    /// the underlying handshake produced a zero-filled static
    /// pubkey (a sentinel for test-only code paths that construct
    /// `SessionKeys` without running a real handshake).
    pub fn peer_static_x25519(&self, node_id: u64) -> Option<[u8; 32]> {
        let entry = self.peers.get(&node_id)?;
        let pk = entry.value().remote_static_pub;
        // Zero-filled → "not available." Real handshakes populate
        // this from `snow`'s post-handshake `get_remote_static`,
        // which returns 32 bytes of non-identity-zero X25519 pubkey.
        if pk == [0u8; 32] {
            None
        } else {
            Some(pk)
        }
    }

    /// Look up a peer's assigned subnet, if one has been recorded.
    /// Only populated from signature-verified
    /// `CapabilityAnnouncement`s — unsigned announcements do not
    /// write here even when a node is running with
    /// `require_signed_capabilities = false`. Exposed for tests +
    /// operator observability; `subnet_visible` consults this map
    /// on the publish / subscribe fan-out path.
    pub fn peer_subnet(&self, node_id: u64) -> Option<SubnetId> {
        self.peer_subnets.get(&node_id).map(|e| *e.value())
    }

    /// Get the local bind address.
    pub fn local_addr(&self) -> SocketAddr {
        self.socket.local_addr()
    }

    /// Get the router (for adding routes, checking stats).
    pub fn router(&self) -> &Arc<NetRouter> {
        &self.router
    }

    /// Get the failure detector.
    pub fn failure_detector(&self) -> &Arc<FailureDetector> {
        &self.failure_detector
    }

    /// Set the migration subprotocol handler.
    ///
    /// Can be called before or after `start()`. When set, inbound
    /// packets with `subprotocol_id == 0x0500` are dispatched to
    /// this handler instead of being queued as events. Idempotent
    /// w.r.t. replacing the handler — a second call swaps in the
    /// new one atomically.
    ///
    /// Use [`Self::clear_migration_handler`] to uninstall (returns
    /// the mesh to the no-handler state where inbound migration
    /// packets hit the `ComputeNotSupported` fallback). Needed by
    /// `DaemonRuntime::shutdown` and by `start`'s lost-race
    /// cleanup path — the mesh must not hold a live handler
    /// pointing at a runtime that is no longer serving daemons.
    pub fn set_migration_handler(&self, handler: Arc<MigrationSubprotocolHandler>) {
        self.migration_handler.store(Some(handler));
    }

    /// Uninstall the migration subprotocol handler. After this
    /// call, inbound migration subprotocol packets hit the
    /// no-handler fallback and synthesise `ComputeNotSupported`
    /// for migration-initiating messages (other message types are
    /// dropped).
    ///
    /// Used by the SDK's `DaemonRuntime::start` to clean up after
    /// losing the install-vs-CAS race against a concurrent
    /// `shutdown`: if `start` installed a handler but its CAS to
    /// `Ready` lost to `shutdown`'s state flip, the mesh would
    /// otherwise be left with a live handler owned by a runtime
    /// that's already been torn down.
    pub fn clear_migration_handler(&self) {
        self.migration_handler.store(None);
    }

    /// Returns `true` iff a migration subprotocol handler is
    /// currently installed on this mesh. Used primarily by tests
    /// that need to observe the ordering of handler installation
    /// against other runtime state transitions — the `ArcSwap` load
    /// itself is a public API surface regardless.
    pub fn has_migration_handler(&self) -> bool {
        self.migration_handler.load().is_some()
    }

    /// Block packets from/to a peer address (simulates network partition).
    pub fn block_peer(&self, addr: SocketAddr) {
        self.partition_filter.insert(addr);
    }

    /// Unblock a peer address (simulates partition healing).
    pub fn unblock_peer(&self, addr: &SocketAddr) {
        self.partition_filter.remove(addr);
    }

    /// Check if a peer is blocked.
    pub fn is_blocked(&self, addr: &SocketAddr) -> bool {
        self.partition_filter.contains(addr)
    }

    /// Get the proximity graph.
    pub fn proximity_graph(&self) -> &Arc<ProximityGraph> {
        &self.proximity_graph
    }

    /// Get the reroute policy (for checking reroute stats in tests).
    pub fn reroute_policy(&self) -> &Arc<ReroutePolicy> {
        &self.reroute_policy
    }

    /// Number of connected peers.
    pub fn peer_count(&self) -> usize {
        self.peers.len()
    }

    /// Connect to a peer. Performs a Noise NKpsk0 handshake as initiator.
    ///
    /// The peer must be listening and ready to accept the handshake.
    /// Returns the peer's node ID on success.
    pub async fn connect(
        &self,
        peer_addr: SocketAddr,
        peer_pubkey: &[u8; 32],
        peer_node_id: u64,
    ) -> Result<u64, AdapterError> {
        let keys = self
            .handshake_initiator(peer_addr, peer_pubkey, peer_node_id)
            .await?;

        let remote_static_pub = keys.remote_static_pub;
        let session = Arc::new(NetSession::new(
            keys,
            peer_addr,
            self.config.packet_pool_size,
            self.config.default_reliable,
        ));

        // Add route so the router can forward packets to this peer
        self.router.add_route(peer_node_id, peer_addr);

        self.peers.insert(
            peer_node_id,
            PeerInfo {
                node_id: peer_node_id,
                addr: peer_addr,
                session,
                remote_static_pub,
            },
        );
        self.addr_to_node.insert(peer_addr, peer_node_id);

        // Register in peer address map (used by reroute policy)
        self.peer_addrs.insert(peer_node_id, peer_addr);

        // Register in proximity graph (1-hop peer). The synthetic
        // pingwave's `origin == peer`, so the back-compat shim
        // attributes the edge correctly (`from_node = origin`).
        let peer_graph_id = node_id_to_graph_id(peer_node_id);
        let pw = EnhancedPingwave::new(peer_graph_id, 0, 1).with_load(0, HealthStatus::Healthy);
        self.proximity_graph.on_pingwave(pw, peer_addr);

        // Register with failure detector
        self.failure_detector.heartbeat(peer_node_id, peer_addr);

        // Push our most recent capability announcement to the new peer,
        // so late joiners pick up our caps without waiting for a
        // re-announce. Races the session's first inbound packet but
        // that's harmless: the receiver's `index()` is version-skip
        // safe and DashMap inserts are idempotent.
        self.push_local_announcement(peer_addr).await;

        Ok(peer_node_id)
    }

    /// Accept a connection from a peer. Performs Noise NKpsk0 as responder.
    ///
    /// Waits for an incoming handshake packet and completes the handshake.
    /// Returns the peer's address and assigns the given node_id.
    ///
    /// # Ordering contract
    ///
    /// `accept()` MUST be called before [`Self::start()`]. Once
    /// `start()` has spawned the dispatch loop, the dispatcher
    /// consumes every inbound UDP datagram from the shared socket;
    /// `try_handshake_responder` polls the same socket directly and
    /// races the dispatcher for incoming msg1 packets. Because no
    /// per-pending-responder registry exists today (initiator-side
    /// handshakes use one — see `pending_direct_initiators` at
    /// `mesh.rs:~1216`; the responder side is a deferred design item),
    /// a `start() → accept()` ordering produces a swallowed msg1 and
    /// a hang. To prevent that hang silently turning into a debugging
    /// nightmare, calling `accept()` after `start()` now returns an
    /// explicit error rather than spinning forever.
    pub async fn accept(&self, peer_node_id: u64) -> Result<(SocketAddr, u64), AdapterError> {
        use std::sync::atomic::Ordering as AtOrd;

        // Race-free entry guard. Increment `accept_in_flight`
        // BEFORE checking `started`. `start()` checks
        // `accept_in_flight` after its CAS and refuses, so the
        // windows are ordered: accept either sees `started=true`
        // and bails OUT (so accept_in_flight goes back to 0 and
        // start sees 0), or `start` sees accept_in_flight > 0 and
        // refuses to run. Reasoning is symmetric to a
        // reader-writer SeqCst handshake: the SeqCst total order
        // on these two atomics makes "either accept observes
        // start and bails, or start observes accept and refuses"
        // mutually exclusive. A bare `started.load(Acquire)` check
        // would be a TOCTOU: `start()` could fire between the
        // check and `handshake_responder`'s recv_from, after
        // which the dispatcher would race the responder for
        // msg1.
        //
        // RAII guard `AcceptGuard` decrements on drop so any
        // early-return (Err from handshake_responder, panic in
        // session construction, future cancellation) doesn't
        // leak the in-flight count.
        struct AcceptGuard<'a>(&'a std::sync::atomic::AtomicUsize);
        impl Drop for AcceptGuard<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, AtOrd::AcqRel);
            }
        }

        self.accept_in_flight.fetch_add(1, AtOrd::AcqRel);
        let _guard = AcceptGuard(&self.accept_in_flight);

        if self.started.load(AtOrd::SeqCst) {
            return Err(AdapterError::Fatal(
                "Mesh::accept called after start() — the dispatch loop is already \
                 consuming inbound packets and would race the responder handshake. \
                 Call accept() for every peer BEFORE invoking start()."
                    .into(),
            ));
        }
        let (keys, peer_addr) = self.handshake_responder(peer_node_id).await?;

        let remote_static_pub = keys.remote_static_pub;
        let session = Arc::new(NetSession::new(
            keys,
            peer_addr,
            self.config.packet_pool_size,
            self.config.default_reliable,
        ));

        self.router.add_route(peer_node_id, peer_addr);

        self.peers.insert(
            peer_node_id,
            PeerInfo {
                node_id: peer_node_id,
                addr: peer_addr,
                session,
                remote_static_pub,
            },
        );
        self.addr_to_node.insert(peer_addr, peer_node_id);

        self.peer_addrs.insert(peer_node_id, peer_addr);

        let peer_graph_id = node_id_to_graph_id(peer_node_id);
        let pw = EnhancedPingwave::new(peer_graph_id, 0, 1).with_load(0, HealthStatus::Healthy);
        self.proximity_graph.on_pingwave(pw, peer_addr);

        self.failure_detector.heartbeat(peer_node_id, peer_addr);

        // See the matching comment in `connect`.
        self.push_local_announcement(peer_addr).await;

        Ok((peer_addr, peer_node_id))
    }

    /// Start the receive loop and heartbeat tasks.
    ///
    /// Must be called after `connect()` / `accept()` to begin processing
    /// inbound packets.
    ///
    /// Refuses (no-op return) if any `accept()` call is currently
    /// in flight. Symmetric to `accept`'s contract: either
    /// `accept` observes `started=true` and bails (in-flight
    /// count goes to 0, then `start` proceeds), or `start`
    /// observes `accept_in_flight > 0` and refuses. The SeqCst
    /// orderings make this mutually exclusive. Without this
    /// counter check, `start` could fire between `accept`'s
    /// `started.load` and its `handshake_responder` poll, after
    /// which the dispatcher would race the responder for the
    /// inbound msg1.
    pub fn start(&self) {
        use std::sync::atomic::Ordering as AtOrd;
        if self.started.swap(true, AtOrd::SeqCst) {
            return; // already started
        }
        // After flipping `started`, observe `accept_in_flight`.
        // If any accept is mid-handshake, roll back and refuse.
        // The accept side either saw our SeqCst store before
        // its load (and bailed cleanly) or saw it after (and
        // we see its incremented counter). Spurious "concurrent
        // start + accept" is rare in production (start is
        // typically called once at boot), but the rollback
        // keeps semantics honest.
        if self.accept_in_flight.load(AtOrd::SeqCst) > 0 {
            // Roll back the flag so a subsequent `start()` after
            // accept finishes can succeed normally.
            self.started.store(false, AtOrd::SeqCst);
            tracing::warn!(
                "MeshNode::start() called while an accept() is in flight — \
                 refusing to start the dispatch loop to avoid racing the \
                 responder handshake. Retry start() after accept() returns."
            );
            return;
        }

        let recv_handle = self.spawn_receive_loop();
        let heartbeat_handle = self.spawn_heartbeat_loop();
        let router_handle = match self.router.start() {
            Some(h) => h,
            None => {
                tracing::warn!(
                    "MeshNode::start called while the router dispatch loop \
                     was already running; ignoring the duplicate start. \
                     This usually indicates start() was invoked twice."
                );
                return;
            }
        };
        let capability_gc_handle = self.spawn_capability_gc_loop();
        let token_sweep_handle = self.spawn_token_sweep_loop();
        // Port-mapping task is opt-in — only spawned when the
        // operator set `try_port_mapping(true)`. Real client is
        // the `SequentialMapper` (NAT-PMP first, UPnP fallback
        // — stage 4b-4). Construction is async because LAN-IP
        // resolution needs a UDP socket bind, so we spawn an
        // outer task that resolves the sequencer then drives
        // the port-mapper task inline. If OS gateway discovery
        // AND LAN-IP resolution both fail, we fall back to the
        // `NullPortMapper` — the task exits quickly without
        // side effects, identical to an unavailable-router
        // environment.
        #[cfg(feature = "port-mapping")]
        let port_mapping_handle = if self.config.try_port_mapping {
            use super::traversal::portmap::{
                sequential_mapper_from_os, MappingSink, NullPortMapper, PortMapperClient,
                PortMapperTask,
            };
            let traversal_stats = self.traversal_stats.clone();
            let reflex_addr = self.reflex_addr.clone();
            let nat_class = self.nat_class.clone();
            let reflex_override_active = self.reflex_override_active.clone();
            let publish_mu = self.traversal_publish_mu.clone();
            let shutdown = self.shutdown.clone();
            let shutdown_notify = self.shutdown_notify.clone();
            let internal_port = self.config.bind_addr.port();
            let renewal = self.traversal_config.port_mapping_renewal;
            Some(tokio::spawn(async move {
                let client: Box<dyn PortMapperClient> = match sequential_mapper_from_os().await {
                    Some(seq) => Box::new(seq),
                    None => {
                        tracing::debug!(
                            "port-mapping: OS gateway + LAN IP resolution failed; \
                                 falling back to NullPortMapper",
                        );
                        Box::new(NullPortMapper::new())
                    }
                };
                let sink = MappingSink::new(
                    traversal_stats,
                    reflex_addr,
                    nat_class,
                    reflex_override_active,
                    publish_mu,
                );
                let task = PortMapperTask::new(
                    client,
                    sink,
                    internal_port,
                    renewal,
                    shutdown,
                    shutdown_notify,
                );
                task.run().await;
            }))
        } else {
            None
        };

        // Store handles — can't block here, but we need them for shutdown
        let tasks = self.tasks.clone();
        tokio::spawn(async move {
            let mut tasks = tasks.lock().await;
            tasks.push(recv_handle);
            tasks.push(heartbeat_handle);
            tasks.push(router_handle);
            tasks.push(capability_gc_handle);
            tasks.push(token_sweep_handle);
            #[cfg(feature = "port-mapping")]
            if let Some(h) = port_mapping_handle {
                tasks.push(h);
            }
        });
    }

    /// Spawn the NAT classification loop. Waits until at least 2
    /// peers are connected, fires the initial sweep, then re-checks
    /// periodically so a mid-session NAT rebind (e.g. gateway
    /// reboot) gets picked up without operator intervention.
    ///
    /// Separate from [`Self::start`] because the loop needs an
    /// `Arc<MeshNode>` to call [`Self::reclassify_nat`] across
    /// `.await` points. Callers that hold a `MeshNode` behind an
    /// `Arc` (SDK, FFI, all production paths) can spawn this
    /// alongside `start` to get continuous classification; callers
    /// that don't can call [`Self::reclassify_nat`] manually via
    /// the `&self` surface.
    ///
    /// The loop is best-effort — it never returns an error surface
    /// to the node, and a failed sweep leaves the previous
    /// classification intact. Exits on `shutdown_notify`.
    #[cfg(feature = "nat-traversal")]
    pub fn spawn_nat_classify_loop(self: &Arc<Self>) -> JoinHandle<()> {
        let node = Arc::clone(self);
        let shutdown = self.shutdown.clone();
        let shutdown_notify = self.shutdown_notify.clone();
        let reclassify_interval = self.traversal_config.classify_deadline.saturating_mul(12);

        tokio::spawn(async move {
            // Poll loop: wait for ≥2 peers before the first sweep.
            // The routed-handshake path seeds `peers` as each
            // connect/accept lands, so the wait is bounded by how
            // fast the operator hands us peers — not something we
            // can tune from here.
            let mut poll = tokio::time::interval(Duration::from_millis(200));
            poll.tick().await; // skip the immediate first tick
            loop {
                if shutdown.load(Ordering::Acquire) {
                    return;
                }
                tokio::select! {
                    _ = shutdown_notify.notified() => return,
                    _ = poll.tick() => {
                        if node.peers.len() >= 2 {
                            break;
                        }
                    }
                }
            }

            // Initial classification sweep — produces the first
            // `nat:*` tag value that any subsequent announce can
            // emit. Callers that want the tag on their very first
            // announce should `await` this future before calling
            // `announce_capabilities`.
            node.reclassify_nat().await;

            // Periodic re-check. `classify_deadline × 12` is long
            // enough to be cheap (probe traffic is cheap but not
            // free) and short enough that a gateway reboot is
            // reflected in outbound announcements within ~1 min of
            // the next re-announce.
            let mut tick = tokio::time::interval(nonzero_interval(reclassify_interval));
            tick.tick().await; // skip the immediate tick
            while !shutdown.load(Ordering::Acquire) {
                tokio::select! {
                    _ = tick.tick() => {
                        node.reclassify_nat().await;
                    }
                    _ = shutdown_notify.notified() => break,
                }
            }
        })
    }

    /// Spawn a port-mapping task driven by `client`. Drives the
    /// UPnP-IGD / NAT-PMP / PCP lifecycle per
    /// `docs/PORT_MAPPING_PLAN.md`:
    ///
    /// 1. Probe the client; on failure, exit without side
    ///    effects.
    /// 2. On probe success, install a mapping for this mesh's
    ///    bind port. On install success, pin the reflex override
    ///    to the mapped external address — same publish order
    ///    as [`Self::set_reflex_override`].
    /// 3. Renew every
    ///    [`super::traversal::TraversalConfig::port_mapping_renewal`];
    ///    3-strike consecutive failures revoke + clear override.
    /// 4. On shutdown, remove the mapping (best-effort), clear
    ///    the override, and exit.
    ///
    /// Callers that want `MeshNode::start` to auto-spawn a
    /// [`super::traversal::portmap::NullPortMapper`] (the
    /// stage-4b-1 default) should set
    /// [`MeshNodeConfig::try_port_mapping`] to `true`; this
    /// method is the explicit-client path used by stages 4b-4+
    /// (real sequencer) and by unit tests that inject mocks.
    ///
    /// Requires the `port-mapping` cargo feature.
    ///
    /// Takes `&self` (not `&Arc<Self>`) because the task body
    /// only needs `Arc`-clones of specific fields
    /// (`traversal_stats`, `reflex_addr`, `nat_class`,
    /// `reflex_override_active`, `shutdown`, `shutdown_notify`),
    /// not the whole `MeshNode`. Callable from `start()` without
    /// forcing the entire `start()` surface to consume an
    /// `Arc<Self>`.
    #[cfg(feature = "port-mapping")]
    pub fn spawn_port_mapping_loop(
        &self,
        client: Box<dyn super::traversal::portmap::PortMapperClient>,
    ) -> JoinHandle<()> {
        use super::traversal::portmap::{MappingSink, PortMapperTask};
        let sink = MappingSink::new(
            self.traversal_stats.clone(),
            self.reflex_addr.clone(),
            self.nat_class.clone(),
            self.reflex_override_active.clone(),
            self.traversal_publish_mu.clone(),
        );
        let internal_port = self.config.bind_addr.port();
        let renewal = self.traversal_config.port_mapping_renewal;
        let task = PortMapperTask::new(
            client,
            sink,
            internal_port,
            renewal,
            self.shutdown.clone(),
            self.shutdown_notify.clone(),
        );
        tokio::spawn(task.run())
    }

    /// Spawn a periodic sweep that evicts expired entries from the
    /// capability index plus stale `(origin, version)` tuples from
    /// the multi-hop dedup cache. Interval from
    /// `config.capability_gc_interval` (default 60 s). Exits on
    /// `shutdown_notify`.
    fn spawn_capability_gc_loop(&self) -> JoinHandle<()> {
        let index = self.capability_index.clone();
        let seen = self.seen_announcements.clone();
        let interval = self.config.capability_gc_interval;
        // Dedup retention = 2× the announcement's own TTL. Longer
        // than one announcement lifetime so the re-announced bumped
        // version isn't confused with the previous one; shorter than
        // `index.gc` retention so the dedup set never outlives the
        // index it guards.
        let dedup_retention =
            std::time::Duration::from_secs(2 * u64::from(CapabilityAnnouncement::DEFAULT_TTL_SECS));
        let shutdown = self.shutdown.clone();
        let shutdown_notify = self.shutdown_notify.clone();

        tokio::spawn(async move {
            // `tokio::time::interval` panics on a zero period — guard
            // against a caller who set `capability_gc_interval` to
            // `Duration::ZERO` in `MeshNodeConfig`. A zero interval
            // isn't a sentinel for "disabled" (that's
            // `Duration::MAX`), it's just pathological input; clamp
            // to 1 s (see `nonzero_interval` for the rationale).
            let mut tick = tokio::time::interval(nonzero_interval(interval));
            // First tick fires immediately; skip it so we don't GC
            // empty state before any announcements have landed.
            tick.tick().await;
            while !shutdown.load(Ordering::Acquire) {
                tokio::select! {
                    _ = tick.tick() => {
                        let _removed = index.gc();
                        seen.retain(|_, instant| instant.elapsed() < dedup_retention);
                    }
                    _ = shutdown_notify.notified() => break,
                }
            }
        })
    }

    /// Spawn a periodic sweep that evicts subscribers whose tokens
    /// have expired. Walks the roster by peer (via
    /// `peer_entity_ids`) and, for every `require_token` channel the
    /// peer is subscribed to, runs the full token-cache check. An
    /// expired or invalid entry causes:
    ///
    /// 1. Revocation in the [`AuthGuard`] (so the next publish
    ///    fan-out sees the denial instantly, before the next
    ///    sweep tick).
    /// 2. Removal from the [`SubscriberRoster`] (so `members()`
    ///    returns the pruned list).
    ///
    /// Interval from `config.token_sweep_interval` (default 30 s).
    /// Skipped when the `channel_configs` registry is `None` — a
    /// node without a registry has no `require_token` channels to
    /// begin with. Similarly, skipped when `token_cache` is `None`.
    fn spawn_token_sweep_loop(&self) -> JoinHandle<()> {
        let roster = self.roster.clone();
        let guard = self.auth_guard.clone();
        let cache = self.token_cache.clone();
        let peer_entity_ids = self.peer_entity_ids.clone();
        let channel_configs = self.channel_configs.clone();
        let interval = self.config.token_sweep_interval;
        let shutdown = self.shutdown.clone();
        let shutdown_notify = self.shutdown_notify.clone();

        tokio::spawn(async move {
            // Same zero-guard as `spawn_capability_gc_loop` —
            // tokio panics if the period is zero.
            let mut tick = tokio::time::interval(nonzero_interval(interval));
            // First tick fires immediately; skip it so we don't
            // sweep empty state before any subscribes have landed.
            tick.tick().await;
            while !shutdown.load(Ordering::Acquire) {
                tokio::select! {
                    _ = tick.tick() => {
                        sweep_expired_subscribers(
                            &roster,
                            &guard,
                            cache.as_ref(),
                            &peer_entity_ids,
                            channel_configs.as_ref(),
                        );
                    }
                    _ = shutdown_notify.notified() => break,
                }
            }
        })
    }

    /// Spawn the main receive loop.
    ///
    /// This is the heart of the mesh node. Every packet from every peer
    /// arrives here. The loop:
    /// 1. Looks up the session by source address
    /// 2. For local packets: decrypts and queues events
    /// 3. For forwarded packets: passes to router (no decryption)
    /// 4. For heartbeats: updates failure detector
    fn spawn_receive_loop(&self) -> JoinHandle<()> {
        let socket = self.socket.socket_arc();
        let shutdown = self.shutdown.clone();
        let shutdown_notify = self.shutdown_notify.clone();

        let ctx = DispatchCtx {
            local_node_id: self.node_id,
            peers: self.peers.clone(),
            addr_to_node: self.addr_to_node.clone(),
            peer_addrs: self.peer_addrs.clone(),
            router: self.router.clone(),
            failure_detector: self.failure_detector.clone(),
            inbound: self.inbound.clone(),
            num_shards: self.config.num_shards,
            migration_handler: self.migration_handler.clone(),
            pending_handshakes: self.pending_handshakes.clone(),
            pending_direct_initiators: self.pending_direct_initiators.clone(),
            static_keypair: self.static_keypair.clone(),
            psk: self.config.psk,
            socket: self.socket.clone(),
            proximity_graph: self.proximity_graph.clone(),
            partition_filter: self.partition_filter.clone(),
            packet_pool_size: self.config.packet_pool_size,
            default_reliable: self.config.default_reliable,
            roster: self.roster.clone(),
            channel_configs: self.channel_configs.clone(),
            pending_membership_acks: self.pending_membership_acks.clone(),
            #[cfg(feature = "nat-traversal")]
            pending_reflex_probes: self.pending_reflex_probes.clone(),
            #[cfg(feature = "nat-traversal")]
            pending_punch_introduces: self.pending_punch_introduces.clone(),
            #[cfg(feature = "nat-traversal")]
            pending_punch_acks: self.pending_punch_acks.clone(),
            #[cfg(feature = "nat-traversal")]
            punch_observers: self.punch_observers.clone(),
            #[cfg(feature = "nat-traversal")]
            traversal_config: self.traversal_config.clone(),
            max_channels_per_peer: self.config.max_channels_per_peer,
            capability_index: self.capability_index.clone(),
            seen_announcements: self.seen_announcements.clone(),
            require_signed_capabilities: self.config.require_signed_capabilities,
            local_subnet: self.local_subnet,
            local_subnet_policy: self.local_subnet_policy.clone(),
            peer_subnets: self.peer_subnets.clone(),
            peer_entity_ids: self.peer_entity_ids.clone(),
            token_cache: self.token_cache.clone(),
            auth_guard: self.auth_guard.clone(),
            auth_failures: self.auth_failures.clone(),
            max_auth_failures_per_window: self.config.max_auth_failures_per_window,
            auth_failure_window: self.config.auth_failure_window,
            auth_throttle_duration: self.config.auth_throttle_duration,
        };

        tokio::spawn(async move {
            let mut receiver = PacketReceiver::new(socket);

            while !shutdown.load(Ordering::Acquire) {
                tokio::select! {
                    result = receiver.recv() => {
                        match result {
                            Ok((data, source)) => {
                                Self::dispatch_packet(data, source, &ctx);
                            }
                            Err(e) => {
                                if !shutdown.load(Ordering::Acquire) {
                                    tracing::warn!(error = %e, "mesh receive error");
                                }
                            }
                        }
                    }
                    _ = shutdown_notify.notified() => {
                        break;
                    }
                }
            }
        })
    }

    /// Dispatch a single received packet.
    ///
    /// This is the routing decision point:
    /// - Handshake packets are ignored (handled during connect/accept)
    /// - Heartbeat packets update the failure detector
    /// - Data packets are decrypted if local, forwarded if not
    fn dispatch_packet(data: Bytes, source: SocketAddr, ctx: &DispatchCtx) {
        // Partition filter: silently drop packets from blocked peers
        if ctx.partition_filter.contains(&source) {
            return;
        }

        // Pre-session keep-alive recognition. Keep-alives are
        // 14-byte packets with `KEEPALIVE_MAGIC` at offset 0. The
        // receive loop fires any observer watching `source` and
        // returns — these packets never continue down the
        // session-decrypt path because no session exists between
        // the peers at punch time. See
        // `traversal::rendezvous`'s module docstring for the
        // wire layout rationale.
        //
        // Ordered BEFORE the pingwave check because both reach
        // into `data[0..2]`; keep-alives are shorter (14 vs 72)
        // so the length guard disambiguates without cost.
        #[cfg(feature = "nat-traversal")]
        if data.len() == super::traversal::rendezvous::KEEPALIVE_LEN {
            if let Some(ka) = super::traversal::rendezvous::decode_keepalive(&data) {
                if let Some((_, tx)) = ctx.punch_observers.remove(&source) {
                    let _ = tx.send(ka);
                }
                return;
            }
        }

        // Check for pingwave. Pingwaves are a fixed 72-byte wire format
        // that does NOT carry the Net header magic. We reject anything
        // that starts with `MAGIC` so a legitimate Net packet that happens
        // to be 72 bytes is never mis-handled (defense in depth — the
        // current Net packet layout has an 80-byte minimum, but relying
        // on that is fragile). The leading-MAGIC check does not
        // authenticate pingwaves against a spoofing attacker; that is a
        // separate protocol concern.
        if data.len() == EnhancedPingwave::SIZE && u16::from_le_bytes([data[0], data[1]]) != MAGIC {
            if let Some(pw) = EnhancedPingwave::from_bytes(&data) {
                let origin_nid = graph_id_to_node_id(&pw.origin_id);

                // DV loop-avoidance rule 1: origin self-check. Drop
                // any pingwave claiming `origin_id == self_id`. This
                // defends against (a) a buggy peer echoing our own
                // origin back at us, and (b) a stale buffered
                // pingwave from a partitioned-then-healed peer.
                if origin_nid == ctx.local_node_id {
                    return;
                }

                // DV loop-avoidance rule 2: MAX_HOPS cap. TTL bounds
                // forwarding; MAX_HOPS bounds install. A pingwave
                // claiming an inflated hop_count can't populate a
                // usable route or graph edge.
                if pw.hop_count >= MAX_HOPS {
                    return;
                }

                // DV loop-avoidance rule 4: only accept pingwaves
                // from registered direct peers. An unknown source
                // addr means either (a) a stale packet from before
                // a handshake was torn down, (b) a peer that never
                // handshaked, or (c) an attacker injecting forged
                // pingwaves. In all three cases we refuse to install
                // route or graph state — otherwise an unauthenticated
                // sender could poison our routing table by claiming
                // to be a next-hop for arbitrary origins.
                let from_node_id = match ctx.addr_to_node.get(&source) {
                    Some(e) => *e.value(),
                    None => return,
                };

                // Install an indirect route `(origin, via=source)`
                // with metric `hop_count + 2`. The `+2` keeps direct
                // routes (metric 1) strictly better than any pingwave
                // route — `add_route_with_metric` preserves the
                // better entry.
                let metric = (pw.hop_count as u16).saturating_add(2);
                ctx.router
                    .routing_table()
                    .add_route_with_metric(origin_nid, source, metric);

                // Hand to the proximity graph to update nodes +
                // edges. `source` is guaranteed registered at this
                // point, so `from_graph_id` faithfully attributes
                // the edge to the forwarding peer's node_id.
                let from_graph_id = node_id_to_graph_id(from_node_id);
                if let Some(fwd_pw) =
                    ctx.proximity_graph
                        .on_pingwave_from(pw, from_graph_id, source)
                {
                    let fwd_bytes = fwd_pw.to_bytes();
                    let socket = ctx.socket.clone();
                    let peers = ctx.peers.clone();
                    let filter = ctx.partition_filter.clone();
                    let router = ctx.router.clone();
                    // DV loop-avoidance rule 3: split horizon on
                    // re-broadcast. If we installed `(origin_nid,
                    // next_hop=X)` — i.e. we'd use X to reach the
                    // origin — don't re-advertise the origin on the
                    // link to X. Prevents X from learning "we can
                    // reach origin in N+1 hops" and installing a
                    // backward loop.
                    tokio::spawn(async move {
                        let next_hop = router.routing_table().lookup(origin_nid);
                        for entry in peers.iter() {
                            let addr = entry.value().addr;
                            if addr == source {
                                continue; // never send back to sender
                            }
                            if Some(addr) == next_hop {
                                continue; // split horizon: that's our path to origin
                            }
                            if filter.contains(&addr) {
                                continue;
                            }
                            let _ = socket.send_to(&fwd_bytes, addr).await;
                        }
                    });
                }
                return;
            }
        }

        let local_node_id = ctx.local_node_id;
        let peers = &ctx.peers;
        let router = &ctx.router;
        let failure_detector = &ctx.failure_detector;
        // Distinguish routed packets from direct packets.
        //
        // Bytes 0-1 of a direct Net packet are [`MAGIC`] (`0x4E45`);
        // bytes 0-1 of a routing header are [`ROUTING_MAGIC`]
        // (`0x5254`). Anything else is malformed and dropped. The
        // previous discriminator ("anything that isn't MAGIC is
        // routed") mis-classified routed packets whenever the
        // recipient's own `node_id` had low-16-bits equal to
        // `MAGIC` — 1-in-65 536 node_ids — silently dropping
        // routed traffic at the AEAD layer.
        let first2 = if data.len() >= 2 {
            u16::from_le_bytes([data[0], data[1]])
        } else {
            0
        };
        let is_routed =
            first2 == ROUTING_MAGIC && data.len() >= ROUTING_HEADER_SIZE + protocol::HEADER_SIZE;
        let is_direct = first2 == MAGIC;
        if !is_routed && !is_direct {
            // Malformed / unrecognized prefix — drop silently.
            return;
        }

        if is_routed {
            // Routed packet: parse routing header, decide forward or local
            if let Some(routing_header) = RoutingHeader::from_bytes(&data[..ROUTING_HEADER_SIZE]) {
                if routing_header.dest_id == local_node_id {
                    // For us — strip routing header, process the inner Net packet.
                    // The inner packet is encrypted with the *sender's* session key
                    // (not the relay's), so we look up the session by session_id
                    // in the inner header, not by source address.
                    let inner = data.slice(ROUTING_HEADER_SIZE..);
                    let parsed = match ParsedPacket::parse(inner, source) {
                        Some(p) => p,
                        None => return,
                    };
                    // Heartbeats are link-local and don't make sense over
                    // the routing layer — drop.
                    if parsed.header.flags.is_heartbeat() {
                        return;
                    }
                    // Routed handshake arrival. Strip routing header and
                    // hand to the responder/msg2 dispatcher.
                    if parsed.header.flags.is_handshake() {
                        Self::handle_routed_handshake(&parsed, &routing_header, source, ctx);
                        return;
                    }
                    // Find the session that matches this packet's session_id
                    let session_id = parsed.header.session_id;
                    let matching_session = peers
                        .iter()
                        .find(|e| e.value().session.session_id() == session_id)
                        .map(|e| e.value().session.clone());
                    if let Some(session) = matching_session {
                        Self::process_local_packet(&parsed, &session, ctx);
                        session.touch();
                    }
                } else {
                    // Not for us — forward without decrypting (header-only
                    // routing). We send via the main socket so the
                    // receiving node sees `source` = our bound addr,
                    // which it can use as a reply path. `router.start()`'s
                    // internal scheduler has a separate ephemeral socket
                    // and would make `source` unusable for replies.
                    if routing_header.is_expired() {
                        return;
                    }
                    let next_hop = match router.routing_table().lookup(routing_header.dest_id) {
                        Some(addr) => addr,
                        None => return,
                    };
                    if ctx.partition_filter.contains(&next_hop) {
                        return;
                    }
                    let mut fwd_header = routing_header;
                    fwd_header.forward();
                    let mut new_data = bytes::BytesMut::with_capacity(data.len());
                    new_data.extend_from_slice(&fwd_header.to_bytes());
                    new_data.extend_from_slice(&data[ROUTING_HEADER_SIZE..]);
                    let forwarded = new_data.freeze();
                    let socket = ctx.socket.clone();
                    tokio::spawn(async move {
                        let _ = socket.send_to(&forwarded, next_hop).await;
                    });
                }
            }
            return;
        }

        // Direct packet (no routing header) — standard path.
        //
        // `session_id` is the authoritative logical-peer key. `addr_to_node`
        // gives a fast path when source addr maps to exactly one session,
        // but we must still validate session_id against the resolved peer
        // and fall back to a session_id scan if it doesn't match. Otherwise
        // two peers that share a wire address (e.g., a direct peer and a
        // relay-peer reachable via the same relay addr) would collide.
        let parsed = match ParsedPacket::parse(data, source) {
            Some(p) => p,
            None => return,
        };

        if parsed.header.flags.is_handshake() {
            // If a direct initiator has registered an oneshot
            // keyed by this source, forward the parsed payload
            // bytes through it. Otherwise (no entry, e.g.
            // responder side or unsolicited handshake) fall
            // through to drop. Without this routing, polling
            // `socket_arc.recv_from` directly from
            // `try_handshake_initiator` would race this dispatch
            // loop — tokio routes a UDP datagram to exactly one
            // waiter — and the response could be swallowed by
            // either consumer.
            if let Some((_, tx)) = ctx.pending_direct_initiators.remove(&source) {
                let _ = tx.send(parsed.payload);
            }
            return;
        }

        let session_id = parsed.header.session_id;
        let matched = ctx
            .addr_to_node
            .get(&source)
            .map(|e| *e.value())
            .and_then(|nid| peers.get(&nid))
            .filter(|p| p.session.session_id() == session_id)
            .map(|p| (p.value().node_id, p.value().session.clone()))
            .or_else(|| {
                peers
                    .iter()
                    .find(|e| e.value().session.session_id() == session_id)
                    .map(|e| (e.value().node_id, e.value().session.clone()))
            });
        let (peer_node_id, session) = match matched {
            Some(x) => x,
            None => return,
        };

        if parsed.header.flags.is_heartbeat() {
            // `verify_and_touch_heartbeat` fuses AEAD verify with
            // `session.touch()` so a future caller can't reorder
            // them or forget to touch on success — the type
            // system enforces verify-then-touch atomically.
            // Fast-pathing the heartbeat without verifying the
            // AEAD tag would let an attacker with the cleartext
            // `session_id` (visible on every prior data packet)
            // and the source UDP address spoof heartbeats from
            // `peer_addr`, indefinitely defeating session-idle
            // timeout and injecting false
            // `failure_detector.heartbeat(...)` notifications.
            // The failure-detector callback is mesh-specific
            // (legacy adapter has no such observer) and stays
            // here, after a successful verify.
            if !session.verify_and_touch_heartbeat(&parsed) {
                return;
            }
            failure_detector.heartbeat(peer_node_id, source);
            return;
        }

        Self::process_local_packet(&parsed, &session, ctx);
        session.touch();
    }

    /// Handle a routed handshake packet that arrived at this node.
    ///
    /// Two cases, discriminated by whether we have a pending initiator
    /// state for `routing_header.src_id`:
    ///
    /// 1. **msg2 for an in-flight initiator.** We started a `connect_via`
    ///    earlier and registered a `PendingHandshake` keyed by the
    ///    responder's node_id. The arriving packet completes that
    ///    initiator state — we run `read_message`, derive keys, and
    ///    signal the caller via the oneshot.
    ///
    /// 2. **msg1 from a new initiator.** We build a responder state with
    ///    the prologue derived from `(routing_header.src_id, self.node_id)`,
    ///    read msg1, write msg2, and send msg2 back via the routing
    ///    table (reversing src/dest in the routing header). On success
    ///    we register the new peer with the routing-path addr (the
    ///    immediate upstream `source`) so that subsequent routed data
    ///    finds a session.
    fn handle_routed_handshake(
        parsed: &ParsedPacket,
        routing_header: &RoutingHeader,
        source: SocketAddr,
        ctx: &DispatchCtx,
    ) {
        // Routing id of the remote party: what we see in the routing
        // header's 32-bit src_id, zero-extended into u64 so it can sit
        // alongside full node_ids in maps without ambiguity.
        let peer_routing_id = routing_header.src_id as u64;

        // Case 1: msg2 for an in-flight initiator. Look up pending state
        // by routing id (that's how it was keyed on insert).
        if let Some((_, pending)) = ctx.pending_handshakes.remove(&peer_routing_id) {
            let PendingHandshake { mut noise, tx } = pending;
            let result = (|| -> Result<SessionKeys, CryptoError> {
                noise.read_message(&parsed.payload)?;
                noise.into_session_keys()
            })();
            let _ = tx.send(result);
            return;
        }

        // Case 2: msg1 from a new initiator.
        //
        // Prologue binds (peer_routing_id, self_routing_id) — same u32
        // projection the initiator used. Full u64 identities don't
        // fit in the routing header (src_id is u32), so we bind what
        // both sides CAN see, and carry the full src node_id inside
        // the Noise payload where it's AEAD-authenticated.
        let self_routing_id = routing_id(ctx.local_node_id);
        let prologue = handshake_prologue(peer_routing_id, self_routing_id);
        let mut noise =
            match NoiseHandshake::responder_with_prologue(&ctx.psk, &ctx.static_keypair, &prologue)
            {
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!(error = %e, "routed handshake: responder build failed");
                    return;
                }
            };
        let msg1_payload = match noise.read_message(&parsed.payload) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "routed handshake: read_message failed (msg1 tampered or wrong PSK)");
                return;
            }
        };

        // Extract the initiator's full u64 node_id from the decrypted
        // payload. Verify its routing id matches the one we got on the
        // wire — a mismatch means the payload was crafted for a
        // different address than what arrived.
        if msg1_payload.len() < 8 {
            tracing::warn!(
                "routed handshake: msg1 payload too short ({}); need 8 bytes of src node_id",
                msg1_payload.len()
            );
            return;
        }
        let peer_node_id = u64::from_le_bytes(msg1_payload[..8].try_into().unwrap());
        if routing_id(peer_node_id) != peer_routing_id {
            tracing::warn!(
                payload = format!("{:#x}", peer_node_id),
                routing = format!("{:#x}", peer_routing_id),
                "routed handshake: src_node_id in payload does not match routing header"
            );
            return;
        }

        let msg2 = match noise.write_message(&[]) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(error = %e, "routed handshake: write_message failed");
                return;
            }
        };
        let keys = match noise.into_session_keys() {
            Ok(k) => k,
            Err(e) => {
                tracing::warn!(error = %e, "routed handshake: key extraction failed");
                return;
            }
        };

        // Build the msg2 packet: Net header (handshake flag) + Noise
        // bytes, wrapped in a routing header with dest = FULL peer
        // node_id (from payload). The initiator's local_node_id check
        // on arrival matches the full u64, so we must put the full
        // value here.
        let mut builder = PacketBuilder::new(&[0u8; 32], 0);
        let inner = builder.build_handshake(&msg2);
        let reply_routing = RoutingHeader::new(
            peer_node_id,
            ctx.local_node_id as u32,
            DEFAULT_HANDSHAKE_TTL,
        );
        let mut routed = bytes::BytesMut::with_capacity(ROUTING_HEADER_SIZE + inner.len());
        routed.extend_from_slice(&reply_routing.to_bytes());
        routed.extend_from_slice(&inner);

        // Pick the next hop for the reply. Prefer the routing table
        // (same path the routed handshake arrived on, symmetrically).
        // Fall back to `source` (the immediate upstream that sent us
        // msg1) — that's a direct peer by construction and guaranteed
        // to have a route back.
        let next_hop = ctx
            .router
            .routing_table()
            .lookup(peer_node_id)
            .unwrap_or(source);

        // Register the new peer. The wire `addr` we record is `source`
        // — the immediate upstream peer that forwarded msg1. That is
        // NOT necessarily the final responder's addr (for multi-hop it
        // isn't), but it's the correct place for future routed data
        // to flow through. Direct data uses this addr; routed data
        // uses the routing table.
        //
        // Registration happens BEFORE the send so that even if the
        // spawned send task is cancelled or panics post-send, the
        // initiator that just derived matching keys finds us.
        //
        // Replay guard: if a live session already exists for
        // `peer_node_id` with the SAME `remote_static_pub`, drop the
        // new handshake. NKpsk0's responder uses a fresh ephemeral
        // on each reply so a captured msg1 replayed by a passive
        // attacker would otherwise overwrite the live session keys
        // — the legitimate initiator still holds the old keys and
        // every subsequent AEAD-protected packet would fail open and
        // be dropped, a trivial DoS against any node that ever
        // handshook on a routed path. Re-handshake from the same
        // identity is gated by session expiry / explicit removal,
        // not by overwriting an active session in place.
        let remote_static_pub = keys.remote_static_pub;
        if let Some(existing) = ctx.peers.get(&peer_node_id) {
            if existing.remote_static_pub == remote_static_pub {
                tracing::warn!(
                    peer_node_id,
                    "routed handshake: dropping msg1 — live session already \
                     established for this peer with matching remote_static_pub \
                     (replay guard)"
                );
                return;
            }
            // Different remote_static_pub for the same node_id is
            // either a peer rotating its static key (legitimate) or
            // an attacker forging a different static — let the
            // initiator's matching-keys check resolve which.
            // Fall through to insert below.
        }
        let session = Arc::new(NetSession::new(
            keys,
            source,
            ctx.packet_pool_size,
            ctx.default_reliable,
        ));
        ctx.peers.insert(
            peer_node_id,
            PeerInfo {
                node_id: peer_node_id,
                addr: source,
                session,
                remote_static_pub,
            },
        );
        ctx.peer_addrs.insert(peer_node_id, source);
        ctx.router.add_route(peer_node_id, source);

        // Spawn the send. If it fails, roll back all three registrations
        // (peer session, peer-addr map, and routing table entry). Leaving
        // the route behind would silently blackhole future routed traffic
        // for `peer_node_id` through an addr we never confirmed was
        // reachable; removing peers without removing the route would also
        // inject a stale entry into rerouting decisions.
        //
        // Route rollback is conditional on the current entry still
        // pointing at `source` — if a concurrent handshake for the same
        // `peer_node_id` already installed a newer (valid) route, we must
        // not overwrite it.
        //
        // A Drop guard owns the rollback. The send marks the
        // guard `completed` only on success; cancellation, panic,
        // or any non-success drops the guard, which runs the
        // rollback. Drop is invoked synchronously when the spawned
        // future is dropped (whether by cancellation or normal
        // completion), so the rollback is no longer dependent on
        // the future actually awaiting through to its error arm.
        // A fire-and-forget `tokio::spawn` with rollback only
        // inside the spawned future on socket-send error would
        // skip the rollback if the runtime was shutting down or
        // the task was cancelled before the send completed,
        // leaving the peer/session/route in an unsendable state.
        let socket = ctx.socket.clone();
        let payload = routed.freeze();
        let guard = PeerRegistrationGuard {
            peer_node_id,
            registered_next_hop: source,
            peers: ctx.peers.clone(),
            peer_addrs: ctx.peer_addrs.clone(),
            router: ctx.router.clone(),
        };
        tokio::spawn(async move {
            match socket.send_to(&payload, next_hop).await {
                Ok(_) => {
                    // `commit` consumes the guard via `mem::forget`,
                    // so the rollback Drop is skipped and the
                    // registrations stay in place.
                    guard.commit();
                }
                Err(e) => {
                    tracing::warn!(
                        peer = format!("{:#x}", peer_node_id),
                        error = %e,
                        "routed handshake: msg2 send failed; unregistering peer"
                    );
                    // `guard` drops at end of scope, running the
                    // rollback.
                }
            }
        });
    }

    /// Process a locally-destined packet: decrypt and queue events.
    ///
    /// This is the same logic as `NetAdapter::process_packet` but extracted
    /// to work with the multi-session dispatch.
    fn process_local_packet(parsed: &ParsedPacket, session: &NetSession, ctx: &DispatchCtx) {
        let inbound = &ctx.inbound;
        let num_shards = ctx.num_shards;
        // Validate payload length
        if !parsed.header.flags.is_handshake()
            && !parsed.header.flags.is_heartbeat()
            && !parsed.is_valid_length()
        {
            return;
        }

        // Decrypt payload
        let aad = parsed.header.aad();
        let counter = u64::from_le_bytes(parsed.header.nonce[4..12].try_into().unwrap_or([0u8; 8]));
        let rx_cipher = session.rx_cipher();
        if !rx_cipher.is_valid_rx_counter(counter) {
            return;
        }
        let decrypted = match rx_cipher.decrypt(counter, &aad, &parsed.payload) {
            Ok(d) => {
                // Commit-time replay check: closes the TOCTOU race where
                // two threads decrypt the same replayed packet concurrently.
                if !rx_cipher.update_rx_counter(counter) {
                    return;
                }
                d
            }
            Err(_) => return,
        };

        // Check subprotocol — migration messages are sent as single event frames
        if parsed.header.subprotocol_id == SUBPROTOCOL_MIGRATION {
            // Resolve sender up-front: both the handler-present and
            // no-handler-default branches need it to route replies
            // back over the inbound session.
            let from_node = ctx
                .peers
                .iter()
                .find(|e| e.value().session.session_id() == session.session_id())
                .map(|e| e.value().node_id)
                .unwrap_or(0);

            // `ArcSwapOption::load` — lock-free on the hot path.
            let handler_guard = ctx.migration_handler.load();
            if let Some(handler) = handler_guard.as_ref() {
                // Extract the payload(s) from the event frame wrapper.
                //
                // Iterate every event in the frame and log a
                // warning for the (anomalous) multi-event case.
                // The protocol design is single-event-per-frame,
                // but the wire format permits multi-event — a
                // hostile (or buggy) peer batching multiple
                // migration messages into one frame must not
                // silently lose every message past the first;
                // operators need to see the protocol violation
                // rather than a silent stall.
                let events =
                    EventFrame::read_events(Bytes::from(decrypted), parsed.header.event_count);
                if events.is_empty() {
                    return;
                }
                if events.len() > 1 {
                    tracing::warn!(
                        n = events.len(),
                        from_node = from_node,
                        "migration subprotocol received multi-event frame \
                         (protocol design is single-event per frame); \
                         processing each message in order"
                    );
                }

                for payload in events {
                    match handler.handle_message(&payload, from_node) {
                        Ok(outbound) => {
                            // BFS queue: self-destined messages loop back
                            // through the dispatcher in-place; any output
                            // the loopback produces joins the same queue
                            // so remote-bound follow-ups reach the socket.
                            //
                            // The 2-node case where orchestrator and
                            // source/target share a node uses this path —
                            // `peers.get(&local_node_id)` is None, so the
                            // loopback short-circuit is the only way a
                            // self-destined wire message gets dispatched.
                            //
                            // The in-place queue preserves all
                            // downstream messages. A
                            // `tokio::spawn`ed fire-and-forget
                            // loopback would discard
                            // `handle_message`'s return value,
                            // and any outbound it produced —
                            // including remote-bound messages
                            // that should have ridden the wire —
                            // would disappear, wedging state
                            // transitions whenever a self-bounce
                            // chained into a further reply.
                            //
                            // Handler work is synchronous and cheap; doing
                            // it on the receive-loop task is fine.
                            let mut pending: std::collections::VecDeque<_> = outbound.into();
                            while let Some(msg) = pending.pop_front() {
                                if msg.dest_node == ctx.local_node_id {
                                    match handler.handle_message(&msg.payload, ctx.local_node_id) {
                                        Ok(more) => pending.extend(more),
                                        Err(e) => {
                                            tracing::warn!(
                                                error = %e,
                                                "migration handler loopback error",
                                            );
                                        }
                                    }
                                    continue;
                                }
                                let dest_session = ctx
                                    .peers
                                    .get(&msg.dest_node)
                                    .map(|e| (e.value().addr, e.value().session.clone()));

                                if let Some((dest_addr, dest_sess)) = dest_session {
                                    // Respect partition filter on outbound path
                                    if ctx.partition_filter.contains(&dest_addr) {
                                        continue;
                                    }
                                    let socket = ctx.socket.clone();
                                    let payload = Bytes::from(msg.payload);
                                    tokio::spawn(async move {
                                        let pool = dest_sess.thread_local_pool();
                                        let mut builder = pool.get();
                                        let seq = {
                                            let stream = dest_sess
                                                .get_or_create_stream(SUBPROTOCOL_MIGRATION as u64);
                                            stream.next_tx_seq()
                                        };
                                        let events = vec![payload];
                                        let packet = builder.build_subprotocol(
                                            SUBPROTOCOL_MIGRATION as u64,
                                            seq,
                                            &events,
                                            PacketFlags::NONE,
                                            SUBPROTOCOL_MIGRATION,
                                        );
                                        let _ = socket.send_to(&packet, dest_addr).await;
                                    });
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "migration handler error");
                        }
                    }
                } // end multi-event payload loop
                return; // handler processed it
            }
            // No handler set — synthesize a `ComputeNotSupported`
            // reply so the source doesn't silently time out. Parses
            // the inbound far enough to extract `daemon_origin` for
            // the reply envelope, then drops. Only responds to the
            // two migration-initiating messages (`TakeSnapshot`,
            // `SnapshotReady`) — other inbound types arrive only
            // mid-migration, and a migration can't be mid-state
            // against a node that has no compute runtime.
            //
            // Iterate every event in the frame so a multi-event
            // migration packet (protocol violation, but possible
            // on the wire) gets one reply per request rather than
            // one for the first and silent drops for the rest.
            let events = EventFrame::read_events(Bytes::from(decrypted), parsed.header.event_count);
            for payload in events {
                if let Some(reply) = synthesize_compute_not_supported_reply(&payload) {
                    let dest_session = ctx
                        .peers
                        .get(&from_node)
                        .map(|e| (e.value().addr, e.value().session.clone()));
                    if let Some((dest_addr, dest_sess)) = dest_session {
                        if !ctx.partition_filter.contains(&dest_addr) {
                            let socket = ctx.socket.clone();
                            tokio::spawn(async move {
                                let pool = dest_sess.thread_local_pool();
                                let mut builder = pool.get();
                                let seq = {
                                    let stream = dest_sess
                                        .get_or_create_stream(SUBPROTOCOL_MIGRATION as u64);
                                    stream.next_tx_seq()
                                };
                                let events = vec![reply];
                                let packet = builder.build_subprotocol(
                                    SUBPROTOCOL_MIGRATION as u64,
                                    seq,
                                    &events,
                                    PacketFlags::NONE,
                                    SUBPROTOCOL_MIGRATION,
                                );
                                let _ = socket.send_to(&packet, dest_addr).await;
                            });
                        }
                    }
                }
            }
            return;
        }

        // Stream-window credit grant: apply to the named stream's
        // `tx_credit_remaining` without touching the inbound event
        // queue. The grant payload is an event frame carrying a
        // 12-byte `StreamWindow` message.
        //
        // Iterate the full event vector and apply each grant. The
        // codec supports multi-event frames, and `StreamWindow` is
        // fixed-size at 16 bytes — there's no codec ambiguity.
        // Using `events.into_iter().next()` would drop every
        // grant past the first when a peer batched multiple
        // stream credits into one event-frame packet, stalling
        // those streams until the sender retransmitted
        // (`apply_authoritative_grant` is monotonic so retransmits
        // eventually catch up — efficiency loss, not data loss).
        if parsed.header.subprotocol_id == SUBPROTOCOL_STREAM_WINDOW {
            let events = EventFrame::read_events(Bytes::from(decrypted), parsed.header.event_count);
            for payload in events {
                match StreamWindow::decode(&payload) {
                    Ok(grant) => {
                        // Quarantine guard: a grant that arrives for a
                        // stream closed within `GRANT_QUARANTINE_WINDOW`
                        // is dropped, even if the stream has already been
                        // reopened with the same id. Without this the
                        // in-flight grant from the *previous* lifetime
                        // would spuriously credit the new `StreamState`
                        // and let the sender exceed its intended window.
                        // Grants for closed / unknown streams are also
                        // dropped silently — the sender will time out on
                        // its own.
                        if session.is_grant_quarantined(grant.stream_id) {
                            tracing::debug!(
                                stream_id = format!("{:#x}", grant.stream_id),
                                "dropping StreamWindow grant for recently-closed stream"
                            );
                        } else if let Some(state) = session.try_stream(grant.stream_id) {
                            state.apply_authoritative_grant(grant.total_consumed);
                        }
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "malformed StreamWindow grant");
                    }
                }
            }
            return;
        }

        // Channel membership: Subscribe / Unsubscribe / Ack.
        //
        // Iterate every event in the frame. `events.into_iter()
        // .next()` would drop every membership op past the first
        // when a peer batched multiple Subscribe/Unsubscribe
        // events into one frame. Each membership op is
        // independent and idempotent on the receiver, so
        // iterating is structurally safe.
        if parsed.header.subprotocol_id == SUBPROTOCOL_CHANNEL_MEMBERSHIP {
            let events = EventFrame::read_events(Bytes::from(decrypted), parsed.header.event_count);
            if events.is_empty() {
                return;
            }
            let from_node = ctx
                .peers
                .iter()
                .find(|e| e.value().session.session_id() == session.session_id())
                .map(|e| e.value().node_id)
                .unwrap_or(0);

            for payload in events {
                Self::handle_membership_message(&payload, from_node, ctx);
            }
            return;
        }

        // Capability announcement: signed, versioned capability metadata.
        // Feeds the local `CapabilityIndex`; never responded to.
        //
        // Iterate every event in the frame. `events.into_iter()
        // .next()` would drop every announcement past the first
        // when a peer batched multiple capability updates into
        // one frame. Each announcement is independently signed
        // and version-skip safe on the index side, so iterating
        // is structurally safe.
        if parsed.header.subprotocol_id == SUBPROTOCOL_CAPABILITY_ANN {
            let events = EventFrame::read_events(Bytes::from(decrypted), parsed.header.event_count);
            if events.is_empty() {
                return;
            }
            let from_node = ctx
                .peers
                .iter()
                .find(|e| e.value().session.session_id() == session.session_id())
                .map(|e| e.value().node_id)
                .unwrap_or(0);

            for payload in events {
                Self::handle_capability_announcement(&payload, from_node, ctx);
            }
            return;
        }

        // Reflex probe: request → observer echoes the UDP-source
        // SocketAddr of the requester. Response → completes the
        // requester's pending oneshot. Both directions ride the
        // same subprotocol; dispatch is length-based
        // (see `traversal::reflex::decode`).
        #[cfg(feature = "nat-traversal")]
        if parsed.header.subprotocol_id == super::traversal::SUBPROTOCOL_REFLEX {
            use super::traversal::reflex;
            let events = EventFrame::read_events(Bytes::from(decrypted), parsed.header.event_count);
            if events.is_empty() {
                return;
            }
            let from_node = ctx
                .peers
                .iter()
                .find(|e| e.value().session.session_id() == session.session_id())
                .map(|e| e.value().node_id)
                .unwrap_or(0);
            if from_node == 0 {
                return;
            }

            // Iterate every event in the frame.
            // `events.into_iter().next()` would drop every reflex
            // message past the first when a peer batched multiple
            // probes into one packet. Reflex Request/Response are
            // each independent so multi-event handling is
            // structurally safe.
            for payload in events {
                let Some(msg) = reflex::decode(&payload) else {
                    continue;
                };
                match msg {
                    reflex::ReflexMsg::Request => {
                        // Echo the observed source address back. Source
                        // is read from `PeerInfo.addr` — the last
                        // address our kernel saw packets from this peer
                        // arrive on, equivalent to a STUN server's
                        // "observed source" because NAT-rewriting is
                        // applied by the time packets reach our socket.
                        let Some((dest_addr, dest_sess)) = ctx
                            .peers
                            .get(&from_node)
                            .map(|e| (e.value().addr, e.value().session.clone()))
                        else {
                            continue;
                        };
                        if ctx.partition_filter.contains(&dest_addr) {
                            continue;
                        }
                        let response = reflex::encode_response(dest_addr);
                        let socket = ctx.socket.clone();
                        tokio::spawn(async move {
                            let pool = dest_sess.thread_local_pool();
                            let mut builder = pool.get();
                            let seq = {
                                let stream = dest_sess.get_or_create_stream(
                                    super::traversal::SUBPROTOCOL_REFLEX as u64,
                                );
                                stream.next_tx_seq()
                            };
                            let events = vec![response];
                            let packet = builder.build_subprotocol(
                                super::traversal::SUBPROTOCOL_REFLEX as u64,
                                seq,
                                &events,
                                PacketFlags::NONE,
                                super::traversal::SUBPROTOCOL_REFLEX,
                            );
                            let _ = socket.send_to(&packet, dest_addr).await;
                        });
                    }
                    reflex::ReflexMsg::Response(observed) => {
                        // Complete the pending probe (if any). A probe
                        // that already timed out has no oneshot entry;
                        // the late response is dropped silently.
                        if let Some((_, (_gen, tx))) = ctx.pending_reflex_probes.remove(&from_node)
                        {
                            let _ = tx.send(observed);
                        }
                    }
                }
            }
            return;
        }

        // Rendezvous coordinator: on `PunchRequest` from peer A,
        // look up target B's reflex in the capability index and
        // send `PunchIntroduce` to both sides with the shared
        // `fire_at` timestamp. Plan §3 — the three-message dance
        // for synchronized hole-punch.
        //
        // `PunchIntroduce` and `PunchAck` inbound wiring lands in
        // stage 3c (endpoint role); stage 3b only handles the
        // coordinator branch so the unit under test is the
        // fan-out itself.
        #[cfg(feature = "nat-traversal")]
        if parsed.header.subprotocol_id == super::traversal::SUBPROTOCOL_RENDEZVOUS {
            use super::traversal::rendezvous;
            let events = EventFrame::read_events(Bytes::from(decrypted), parsed.header.event_count);
            if events.is_empty() {
                return;
            }
            let from_node = ctx
                .peers
                .iter()
                .find(|e| e.value().session.session_id() == session.session_id())
                .map(|e| e.value().node_id)
                .unwrap_or(0);
            if from_node == 0 {
                return;
            }

            // Iterate every event in the frame.
            // `events.into_iter().next()` would drop every
            // rendezvous message past the first when a peer
            // batched PunchRequest / PunchIntroduce / PunchAck
            // into one packet. Each is independent — no
            // cross-event ordering dependency — so multi-event
            // handling is structurally safe.
            for payload in events {
                let Some(msg) = rendezvous::decode(&payload) else {
                    continue;
                };
                match msg {
                    rendezvous::RendezvousMsg::PunchRequest(req) => {
                        Self::handle_punch_request(from_node, req, ctx);
                    }
                    rendezvous::RendezvousMsg::PunchIntroduce(intro) => {
                        // Endpoint side of the rendezvous. Complete
                        // any installed observer waiter (for tests /
                        // explicit awaits), then schedule the
                        // keep-alive train + observer. The
                        // `PunchAck` fires only once the observer
                        // sees inbound traffic from `peer_reflex`
                        // (or — on localhost — at the punch_deadline
                        // fallback). Plan §3 endpoint semantics.
                        if let Some((_, (_gen, tx))) =
                            ctx.pending_punch_introduces.remove(&intro.peer)
                        {
                            let _ = tx.send(intro);
                        }
                        Self::schedule_punch(from_node, intro, ctx);
                    }
                    rendezvous::RendezvousMsg::PunchAck(ack) => {
                        if ack.to_peer == ctx.local_node_id {
                            // Final recipient: complete the correlation
                            // oneshot keyed by `from_peer`. A late ack
                            // for an abandoned `connect_direct` is
                            // dropped silently.
                            if let Some((_, (_gen, tx))) =
                                ctx.pending_punch_acks.remove(&ack.from_peer)
                            {
                                let _ = tx.send(ack);
                            }
                        } else {
                            // Coordinator role: forward verbatim to
                            // `to_peer` via our session with that
                            // peer. The forwarded ack keeps the same
                            // bytes — `from_peer` still points at the
                            // original sender, which is what the
                            // recipient correlates on.
                            Self::forward_punch_ack(ack, ctx);
                        }
                    }
                }
            }
            return;
        }

        // Standard event path: parse event frames and queue.
        //
        // Credit accounting charges the full on-wire size (Net
        // header + AEAD tag + payload) so sender and receiver stay
        // symmetric — the sender debits the same quantity via
        // `wire_bytes_for_payload` on admission.
        let payload_bytes = (decrypted.len() + PACKET_WIRE_OVERHEAD) as u64;
        let events = EventFrame::read_events(Bytes::from(decrypted), parsed.header.event_count);

        let stream_id = parsed.header.stream_id;
        let shard_id = if num_shards > 0 {
            (stream_id % num_shards as u64) as u16
        } else {
            0
        };

        // Credit-window bookkeeping: charge only *accepted* inbound
        // bytes against the stream's RxCreditState. `on_receive`
        // returns `false` for duplicates (already-acked sequences)
        // and for sequences past the Reliable receive window —
        // crediting those would refund send credit for
        // retransmissions / replays, letting a chatty peer inflate
        // `tx_credit_remaining` past what it actually pushed through
        // the protocol. Accounting runs at receive time (not drain
        // time); this closes the v1 gap where a single serial sender
        // ran `Transport(io::Error)` into a full kernel buffer. A
        // separately slow daemon is still backstopped by the
        // existing shard-queue-depth limits.
        let grant_bytes = {
            let stream = session.get_or_create_stream(stream_id);
            let accepted = stream.with_reliability(|r| r.on_receive(parsed.header.sequence));
            if accepted {
                stream.update_rx_seq(parsed.header.sequence);
                stream.on_bytes_consumed(payload_bytes)
            } else {
                None
            }
        };

        if let Some(total_consumed) = grant_bytes {
            // Resolve the sending peer via two O(1) DashMap lookups
            // (`addr_to_node` → `peers`) instead of a linear scan over
            // `ctx.peers`. At high peer counts the scan would make
            // packet receive cost proportional to peer count — the
            // hot path needs to stay constant-time.
            //
            // The addr-based lookup is validated against the arriving
            // `session.session_id()` because multiple peers can share
            // a source address in relay scenarios: `addr_to_node` is
            // keyed by last-seen source and may resolve to a different
            // peer than the one this packet authenticated as. On
            // mismatch (or miss) fall back to a session_id scan so the
            // grant is guaranteed to go back on the session the
            // accepted bytes arrived on.
            let peer_addr = session.peer_addr();
            let peer = ctx
                .addr_to_node
                .get(&peer_addr)
                .and_then(|node_id| {
                    ctx.peers.get(&*node_id).and_then(|p| {
                        (p.value().session.session_id() == session.session_id())
                            .then(|| (p.value().addr, p.value().session.clone()))
                    })
                })
                .or_else(|| {
                    ctx.peers
                        .iter()
                        .find(|e| e.value().session.session_id() == session.session_id())
                        .map(|e| (e.value().addr, e.value().session.clone()))
                });
            if let Some((peer_addr, peer_session)) = peer {
                Self::spawn_stream_window_grant(
                    ctx,
                    peer_session,
                    peer_addr,
                    stream_id,
                    total_consumed,
                );
            }
        }

        let queue = inbound.entry(shard_id).or_default();
        let seq = parsed.header.sequence;
        for (i, event_data) in events.into_iter().enumerate() {
            use std::fmt::Write;
            let mut event_id = String::with_capacity(24);
            let _ = write!(event_id, "{}:{}", seq, i);
            queue.push(StoredEvent::new(event_id, event_data, seq, shard_id));
        }
    }

    /// Emit a `StreamWindow` credit grant back to `peer_addr` on the
    /// existing encrypted session. Fire-and-forget — grants are
    /// **authoritative**, so a lost grant is reconciled by the next
    /// one that successfully arrives (each carries the receiver's
    /// full `total_consumed` picture).
    ///
    /// The grant packet rides on the sentinel [`CONTROL_STREAM_ID`]
    /// (`u64::MAX`) with a sequence drawn from
    /// `NetSession::next_control_tx_seq`. This is a dedicated
    /// session-level counter that cannot collide with user stream
    /// state — a caller who opens a stream numerically equal to
    /// `SUBPROTOCOL_STREAM_WINDOW` (0x0B00) won't see their
    /// sequence space polluted by control traffic.
    fn spawn_stream_window_grant(
        ctx: &DispatchCtx,
        session: Arc<NetSession>,
        peer_addr: SocketAddr,
        stream_id: u64,
        total_consumed: u64,
    ) {
        if ctx.partition_filter.contains(&peer_addr) {
            return;
        }
        let socket = ctx.socket.clone();
        tokio::spawn(async move {
            let payload = StreamWindow {
                stream_id,
                total_consumed,
            }
            .encode();
            let pool = session.thread_local_pool();
            let mut builder = pool.get();
            let seq = session.next_control_tx_seq();
            let events = vec![Bytes::copy_from_slice(&payload)];
            let packet = builder.build_subprotocol(
                CONTROL_STREAM_ID,
                seq,
                &events,
                PacketFlags::NONE,
                SUBPROTOCOL_STREAM_WINDOW,
            );
            if let Err(e) = socket.send_to(&packet, peer_addr).await {
                tracing::debug!(error = %e, "StreamWindow grant send failed");
                return;
            }
            // Grant reached the wire — count it on the emitting
            // stream so the receiver side of stats reflects
            // cumulative grants sent.
            if let Some(state) = session.try_stream(stream_id) {
                state.note_grant_sent();
            }
        });
    }

    /// Spawn heartbeat sender for all peers.
    fn spawn_heartbeat_loop(&self) -> JoinHandle<()> {
        let socket = self.socket.clone();
        let peers = self.peers.clone();
        let addr_to_node = self.addr_to_node.clone();
        let peer_addrs = self.peer_addrs.clone();
        let failure_detector = self.failure_detector.clone();
        let interval = self.config.heartbeat_interval;
        let shutdown = self.shutdown.clone();
        let shutdown_notify = self.shutdown_notify.clone();
        let partition_filter = self.partition_filter.clone();
        let proximity_graph = self.proximity_graph.clone();
        let router = self.router.clone();
        // Sweep routes that haven't been refreshed for 3× the session
        // timeout. Direct routes are refreshed by this loop's own
        // pingwave emission; indirect (pingwave-learned) routes age out
        // here if their origin goes silent.
        let max_route_age = self.config.session_timeout.saturating_mul(3);
        // Dead-peer eviction threshold: a peer that has been Failed
        // for this long with no observed traffic is considered
        // permanently gone and dropped from `peers`. Until then we
        // keep the session entry so a transient-partition recovery
        // (the heartbeat the peer sends on the heal triggers
        // `failure_detector.heartbeat`, which `on_recovery`'s the
        // reroute) can succeed — evicting immediately on the first
        // Failed transition would require a full re-handshake to
        // come back. The 30× multiplier gives partitions plenty of
        // time to heal (at the default 30 s session_timeout that's
        // a 15-minute cleanup delay; at test-tight 300 ms timeouts
        // it's 9 s, still comfortably longer than typical test
        // partition-heal windows).
        let dead_peer_timeout = self.config.session_timeout.saturating_mul(30);
        // Stream lifecycle: drop idle streams past `stream_idle_timeout`
        // and enforce `max_streams` cap via LRU.
        let stream_idle_timeout = self.config.stream_idle_timeout;
        let max_streams = self.config.max_streams;

        tokio::spawn(async move {
            while !shutdown.load(Ordering::Acquire) {
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {
                        // Create a pingwave for this heartbeat cycle
                        let pw = proximity_graph.create_pingwave(HealthStatus::Healthy);
                        let pw_bytes = pw.to_bytes();

                        for entry in peers.iter() {
                            let peer_addr = entry.value().addr;
                            if partition_filter.contains(&peer_addr) {
                                continue;
                            }
                            let session = &entry.value().session;
                            // `Session::build_heartbeat` routes
                            // through `thread_local_pool` (same
                            // pool the data path uses) so
                            // heartbeats and data share a single
                            // `tx_counter`. Constructing a fresh
                            // `PacketBuilder::new(&[0u8; 32],
                            // session.session_id())` per heartbeat
                            // would (a) use the wrong key so the
                            // receiver's AEAD verify would reject
                            // every tag, and (b) reuse counter=0
                            // across heartbeats so the replay
                            // window would reject every heartbeat
                            // after the first.
                            let packet = session.build_heartbeat();
                            let _ = socket.send_to(&packet, peer_addr).await;
                            // Pingwave (raw UDP — not encrypted, topology is public)
                            let _ = socket.send_to(&pw_bytes, peer_addr).await;
                        }

                        // Drop routes whose `updated_at` is past the age
                        // limit. Small scan of the routing table; cheap.
                        router.routing_table().sweep_stale(max_route_age);

                        // Age out proximity graph edges in lockstep
                        // with the routing table. If the peer that
                        // used to relay pingwaves for an origin went
                        // silent, both the (peer→origin) edge and the
                        // routing-table entry that depended on it
                        // disappear on the same tick.
                        proximity_graph.sweep_stale_edges(max_route_age);

                        // Sweep idle streams per-session and enforce the
                        // per-session `max_streams` cap. Each session is
                        // independent; large deployments with many peers
                        // each with many streams pay O(P + total_streams).
                        for entry in peers.iter() {
                            entry.value().session.evict_idle_streams(
                                stream_idle_timeout,
                                max_streams,
                                "idle_timeout",
                            );
                        }

                        // Dead-peer eviction: walk peers in Failed
                        // state whose session has been inactive for
                        // longer than `dead_peer_timeout`. The
                        // failure-detector `on_failure` callback
                        // does not evict `peers` itself — see the
                        // note in `MeshNode::new` — so this sweep is
                        // the single point where a permanently-gone
                        // peer's session / address mapping drops.
                        // Short-term partitions stay in `peers` long
                        // enough for `on_recovery` to fire when the
                        // heartbeats resume.
                        //
                        // Drive `check_all()` first: the failure
                        // detector only transitions `Healthy → Suspected
                        // → Failed` when its state machine runs. Without
                        // this call, `failed_nodes()` would always be
                        // empty outside tests and the sweep would be a
                        // silent no-op even for permanently-dead peers
                        // (cubic code review P1).
                        let _ = failure_detector.check_all();
                        let failed = failure_detector.failed_nodes();
                        for node_id in failed {
                            let still_silent = match peers.get(&node_id) {
                                Some(e) => e.value().session.is_timed_out(dead_peer_timeout),
                                None => false,
                            };
                            if !still_silent {
                                continue;
                            }
                            if let Some((_, old_info)) = peers.remove(&node_id) {
                                let old_addr = old_info.addr;
                                addr_to_node
                                    .remove_if(&old_addr, |_, n| *n == node_id);
                                peer_addrs
                                    .remove_if(&node_id, |_, addr| *addr == old_addr);
                                tracing::info!(
                                    node_id = format!("{:#x}", node_id),
                                    "evicted permanently-dead peer from peer map",
                                );
                            }
                            // Also drop the failure-detector entry so
                            // a later reconnect under the same node_id
                            // starts from a clean slate.
                            failure_detector.remove(node_id);
                        }
                    }
                    _ = shutdown_notify.notified() => {
                        break;
                    }
                }
            }
        })
    }

    /// Send a batch of events to a specific peer by address.
    pub async fn send_to_peer(
        &self,
        peer_addr: SocketAddr,
        batch: Batch,
    ) -> Result<(), AdapterError> {
        // Partition filter: silently drop sends to blocked peers
        if self.partition_filter.contains(&peer_addr) {
            return Ok(());
        }

        let node_id = self
            .addr_to_node
            .get(&peer_addr)
            .map(|e| *e.value())
            .ok_or_else(|| AdapterError::Connection("unknown peer".into()))?;
        let peer = self
            .peers
            .get(&node_id)
            .ok_or_else(|| AdapterError::Connection("unknown peer".into()))?;

        let session = &peer.session;
        let stream_id = batch.shard_id as u64;

        let reliable = {
            let stream = session.get_or_create_stream(stream_id);
            stream.with_reliability(|r| r.needs_ack())
        };

        let pool = session.thread_local_pool();
        let mut builder = pool.get();

        let mut current_batch: Vec<Bytes> = Vec::with_capacity(64);
        let mut current_size = 0usize;

        for event in &batch.events {
            let event_bytes = event.raw.clone();
            let frame_size = EventFrame::LEN_SIZE + event_bytes.len();

            if current_size + frame_size > protocol::MAX_PAYLOAD_SIZE && !current_batch.is_empty() {
                let seq = {
                    let stream = session.get_or_create_stream(stream_id);
                    stream.next_tx_seq()
                };
                let flags = if reliable {
                    PacketFlags::RELIABLE
                } else {
                    PacketFlags::NONE
                };
                let packet = builder.build(stream_id, seq, &current_batch, flags);
                self.socket
                    .send_to(&packet, peer_addr)
                    .await
                    .map_err(|e| AdapterError::Connection(format!("send failed: {}", e)))?;

                current_batch.clear();
                current_size = 0;
            }

            current_batch.push(event_bytes);
            current_size += frame_size;
        }

        if !current_batch.is_empty() {
            let seq = {
                let stream = session.get_or_create_stream(stream_id);
                stream.next_tx_seq()
            };
            let flags = if reliable {
                PacketFlags::RELIABLE
            } else {
                PacketFlags::NONE
            };
            let packet = builder.build(stream_id, seq, &current_batch, flags);
            self.socket
                .send_to(&packet, peer_addr)
                .await
                .map_err(|e| AdapterError::Connection(format!("send failed: {}", e)))?;
        }

        // builder is dropped here — auto-released back to the pool
        drop(builder);
        session.touch();
        Ok(())
    }

    /// Send a batch of events to a destination node via the routing table.
    ///
    /// The events are encrypted with the destination's session key and
    /// a routing header is prepended so intermediate nodes can forward
    /// without decrypting. The packet is sent to the next hop from the
    /// routing table, not directly to the destination.
    ///
    /// Requires:
    /// - A session with `dest_node_id` (for encryption)
    /// - A route to `dest_node_id` in the routing table (for next hop)
    pub async fn send_routed(&self, dest_node_id: u64, batch: Batch) -> Result<(), AdapterError> {
        // Find the session for the destination (needed for encryption)
        let (dest_addr, session) = self
            .peers
            .get(&dest_node_id)
            .map(|e| (e.value().addr, e.value().session.clone()))
            .ok_or_else(|| {
                AdapterError::Connection(format!("no session for node {:#x}", dest_node_id))
            })?;

        // Find the next hop from the routing table
        let next_hop = self
            .router
            .routing_table()
            .lookup(dest_node_id)
            .unwrap_or(dest_addr); // fall back to direct if no route

        let stream_id = batch.shard_id as u64;
        let reliable = {
            let stream = session.get_or_create_stream(stream_id);
            stream.with_reliability(|r| r.needs_ack())
        };

        let pool = session.thread_local_pool();
        let mut builder = pool.get();

        // Build routing header
        let routing_header = RoutingHeader::new(dest_node_id, self.node_id as u32, 8);
        let routing_bytes = routing_header.to_bytes();

        let mut current_batch: Vec<Bytes> = Vec::with_capacity(64);
        let mut current_size = 0usize;

        for event in &batch.events {
            let event_bytes = event.raw.clone();
            let frame_size = EventFrame::LEN_SIZE + event_bytes.len();

            if current_size + frame_size > protocol::MAX_PAYLOAD_SIZE && !current_batch.is_empty() {
                let seq = {
                    let stream = session.get_or_create_stream(stream_id);
                    stream.next_tx_seq()
                };
                let flags = if reliable {
                    PacketFlags::RELIABLE
                } else {
                    PacketFlags::NONE
                };
                // Build encrypted packet, then prepend routing header
                let net_packet = builder.build(stream_id, seq, &current_batch, flags);
                let mut routed =
                    bytes::BytesMut::with_capacity(ROUTING_HEADER_SIZE + net_packet.len());
                routed.extend_from_slice(&routing_bytes);
                routed.extend_from_slice(&net_packet);

                self.socket
                    .send_to(&routed, next_hop)
                    .await
                    .map_err(|e| AdapterError::Connection(format!("send failed: {}", e)))?;

                current_batch.clear();
                current_size = 0;
            }

            current_batch.push(event_bytes);
            current_size += frame_size;
        }

        if !current_batch.is_empty() {
            let seq = {
                let stream = session.get_or_create_stream(stream_id);
                stream.next_tx_seq()
            };
            let flags = if reliable {
                PacketFlags::RELIABLE
            } else {
                PacketFlags::NONE
            };
            let net_packet = builder.build(stream_id, seq, &current_batch, flags);
            let mut routed = bytes::BytesMut::with_capacity(ROUTING_HEADER_SIZE + net_packet.len());
            routed.extend_from_slice(&routing_bytes);
            routed.extend_from_slice(&net_packet);

            self.socket
                .send_to(&routed, next_hop)
                .await
                .map_err(|e| AdapterError::Connection(format!("send failed: {}", e)))?;
        }

        drop(builder);
        session.touch();
        Ok(())
    }

    // ── Channel membership API ─────────────────────────────────────────

    /// Access the per-channel subscriber roster. Used by `ChannelPublisher`
    /// to enumerate subscribers; exposed for diagnostics.
    pub fn roster(&self) -> &Arc<SubscriberRoster> {
        &self.roster
    }

    /// Install a `ChannelConfigRegistry` whose `can_subscribe` /
    /// `can_publish` rules are consulted for incoming Subscribe
    /// messages.
    ///
    /// When unset (the default), all subscribes are accepted. Full
    /// capability/token-based authorization additionally requires a
    /// `TokenCache` — see [`Self::set_token_cache`].
    pub fn set_channel_configs(&mut self, configs: Arc<ChannelConfigRegistry>) {
        self.channel_configs = Some(configs);
    }

    /// Install a shared `TokenCache` used by the channel-auth path.
    /// When set, `authorize_subscribe` and `publish_many` consult
    /// it via `ChannelConfig::can_subscribe` / `can_publish`.
    /// Subscribers that present a token on the wire have their
    /// token installed into this cache (after signature
    /// verification) before the ACL check runs.
    ///
    /// When unset, `require_token` channels always reject —
    /// without a cache there's no way to validate presented tokens
    /// or find pre-cached ones.
    pub fn set_token_cache(&mut self, cache: Arc<TokenCache>) {
        self.token_cache = Some(cache);
    }

    /// Ask `publisher_node_id` to add this node to `channel`'s subscriber set.
    ///
    /// Blocks until the publisher's `Ack` arrives or
    /// `membership_ack_timeout` elapses. Returns `Ok(())` iff the publisher
    /// accepted the subscribe; `AckReason` failures surface as
    /// `AdapterError::Connection`. No token is presented — use
    /// [`Self::subscribe_channel_with_token`] for channels with
    /// `require_token` set.
    pub async fn subscribe_channel(
        &self,
        publisher_node_id: u64,
        channel: ChannelName,
    ) -> Result<(), AdapterError> {
        self.send_membership_request(publisher_node_id, channel, true, None)
            .await
    }

    /// Subscribe with a pre-issued [`PermissionToken`] attached.
    /// The publisher verifies the token and, on success, installs
    /// it in its local `TokenCache` before the
    /// `ChannelConfig::can_subscribe` check.
    pub async fn subscribe_channel_with_token(
        &self,
        publisher_node_id: u64,
        channel: ChannelName,
        token: PermissionToken,
    ) -> Result<(), AdapterError> {
        self.send_membership_request(publisher_node_id, channel, true, Some(token.to_bytes()))
            .await
    }

    /// Ask `publisher_node_id` to remove this node from `channel`'s
    /// subscriber set. Mirror of `subscribe_channel`.
    pub async fn unsubscribe_channel(
        &self,
        publisher_node_id: u64,
        channel: ChannelName,
    ) -> Result<(), AdapterError> {
        self.send_membership_request(publisher_node_id, channel, false, None)
            .await
    }

    async fn send_membership_request(
        &self,
        publisher_node_id: u64,
        channel: ChannelName,
        subscribe: bool,
        token: Option<Vec<u8>>,
    ) -> Result<(), AdapterError> {
        let peer_addr = {
            let peer = self.peers.get(&publisher_node_id).ok_or_else(|| {
                AdapterError::Connection(format!(
                    "no session to publisher {:#x}",
                    publisher_node_id
                ))
            })?;
            peer.addr
        };

        let nonce = {
            use std::sync::atomic::AtomicU64;
            static COUNTER: AtomicU64 = AtomicU64::new(1);
            COUNTER.fetch_add(1, Ordering::Relaxed)
        };
        let msg = if subscribe {
            MembershipMsg::Subscribe {
                channel: channel.clone(),
                nonce,
                token,
            }
        } else {
            MembershipMsg::Unsubscribe {
                channel: channel.clone(),
                nonce,
            }
        };
        let bytes = membership::encode(&msg);

        let (tx, rx) = oneshot::channel::<MembershipAck>();
        self.pending_membership_acks.insert(nonce, tx);

        // Scoped send; if it fails, drop the pending entry so memory
        // doesn't accumulate.
        if let Err(e) = self
            .send_subprotocol(peer_addr, SUBPROTOCOL_CHANNEL_MEMBERSHIP, &bytes)
            .await
        {
            self.pending_membership_acks.remove(&nonce);
            return Err(e);
        }

        let ack = match tokio::time::timeout(self.config.membership_ack_timeout, rx).await {
            Ok(Ok(ack)) => ack,
            Ok(Err(_)) => {
                self.pending_membership_acks.remove(&nonce);
                return Err(AdapterError::Connection(
                    "membership ack channel closed".into(),
                ));
            }
            Err(_) => {
                self.pending_membership_acks.remove(&nonce);
                return Err(AdapterError::Connection(format!(
                    "membership ack timeout ({:?}) for channel {}",
                    self.config.membership_ack_timeout, channel
                )));
            }
        };

        if !ack.accepted {
            return Err(AdapterError::Connection(format!(
                "membership request rejected: {:?}",
                ack.reason
            )));
        }
        Ok(())
    }

    /// Dispatch an inbound Subscribe / Unsubscribe / Ack on the
    /// membership subprotocol.
    fn handle_membership_message(payload: &[u8], from_node: u64, ctx: &DispatchCtx) {
        let msg = match membership::decode(payload) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(error = %e, "membership decode failed");
                return;
            }
        };

        match msg {
            MembershipMsg::Subscribe {
                channel,
                nonce,
                token,
            } => {
                let (accepted, reason) =
                    Self::authorize_subscribe(&channel, from_node, token.as_deref(), ctx);
                if accepted {
                    // Populate the AuthGuard fast path so publish
                    // fan-out can admit this subscriber in <10 ns
                    // without re-walking the ACL. Mirrors the
                    // `roster.add` below — both are keyed on the
                    // channel name so they stay consistent.
                    ctx.auth_guard
                        .allow_channel(subscriber_origin_hash(from_node), &channel);
                    let id = ChannelId::new(channel);
                    ctx.roster.add(id, from_node);
                    Self::clear_auth_failures(from_node, ctx);
                } else if !matches!(
                    reason,
                    Some(AckReason::TooManyChannels) | Some(AckReason::RateLimited)
                ) {
                    // Count auth-rule rejections toward the
                    // failure budget. Resource limits
                    // (TooManyChannels) and throttle short-
                    // circuits (RateLimited) don't — the former
                    // is orthogonal, the latter is the *result*
                    // of past failures and would double-count.
                    Self::record_auth_failure(from_node, ctx);
                }
                Self::send_membership_ack(from_node, nonce, accepted, reason, ctx);
            }
            MembershipMsg::Unsubscribe { channel, nonce } => {
                // Revoke from the fast path first so any in-flight
                // publish stops admitting this subscriber even
                // before the roster update is visible.
                ctx.auth_guard
                    .revoke_channel(subscriber_origin_hash(from_node), &channel);
                let id = ChannelId::new(channel);
                ctx.roster.remove(&id, from_node);
                // Unsubscribe is always accepted — idempotent even if the
                // peer wasn't actually subscribed.
                Self::send_membership_ack(from_node, nonce, true, None, ctx);
            }
            MembershipMsg::Ack {
                nonce,
                accepted,
                reason,
            } => {
                if let Some((_, tx)) = ctx.pending_membership_acks.remove(&nonce) {
                    let _ = tx.send(MembershipAck { accepted, reason });
                } else {
                    tracing::debug!(
                        nonce,
                        "membership ack with no pending request (duplicate or timed out)"
                    );
                }
            }
        }
    }

    /// Dispatch an inbound `CapabilityAnnouncement` into the local
    /// capability index. Drops announcements that:
    /// - fail to decode (malformed bytes),
    /// - carry a `node_id` that doesn't match the session's peer
    ///   (a peer can only announce for itself),
    /// - are missing a signature when
    ///   `require_signed_capabilities` is on,
    /// - carry a signature that fails verification against the
    ///   announcement's own `entity_id` (Stage E upgrade).
    ///
    /// `node_id` and `entity_id` are independent values on the
    /// wire; we pin `node_id → entity_id` on first sight so a
    /// later announcement claiming a different `entity_id` for the
    /// same `node_id` won't silently rebind identity.
    fn handle_capability_announcement(payload: &[u8], from_node: u64, ctx: &DispatchCtx) {
        let Some(ann) = CapabilityAnnouncement::from_bytes(payload) else {
            tracing::trace!(
                from_node = format!("{:#x}", from_node),
                len = payload.len(),
                "capability: decode failed"
            );
            return;
        };

        // Direct peers may only announce their own caps. Forwarded
        // announcements (hop_count > 0) are relayed through a peer
        // that isn't the origin, so we skip the check in that path
        // and rely on signature verification plus the TOFU binding
        // to keep forgers out.
        if ann.hop_count == 0 && ann.node_id != from_node {
            tracing::trace!(
                from_node = format!("{:#x}", from_node),
                ann_node = format!("{:#x}", ann.node_id),
                "capability: node_id mismatch (peer can only announce for itself)"
            );
            return;
        }

        // Origin self-check — if we're the origin, drop. A mesh
        // loop could bounce our own announcement back to us; no
        // reason to re-index or re-broadcast.
        if ann.node_id == ctx.local_node_id {
            return;
        }

        // Dedup on (origin, version). A `(node_id, version)` tuple
        // is processed at most once — protects against diamond
        // topologies where the same announcement arrives twice via
        // different paths. Insert happens AFTER validation so a
        // malformed announcement doesn't poison the cache.
        let dedup_key = (ann.node_id, ann.version);
        if ctx.seen_announcements.contains_key(&dedup_key) {
            return;
        }

        if ctx.require_signed_capabilities && ann.signature.is_none() {
            tracing::trace!(
                from_node = format!("{:#x}", from_node),
                "capability: unsigned announcement rejected"
            );
            return;
        }

        // Verify the signature (when present) against the
        // announcement's self-claimed entity_id. Unsigned
        // announcements skip this branch; receivers that care about
        // authenticity set `require_signed_capabilities = true`.
        let signature_verified = if ann.signature.is_some() {
            if ann.verify().is_err() {
                tracing::trace!(
                    from_node = format!("{:#x}", from_node),
                    "capability: signature verification failed"
                );
                return;
            }
            true
        } else {
            false
        };

        // Bind `node_id` to `entity_id` cryptographically. The
        // signature covers `entity_id` but NOT `node_id` — without
        // this check a signed announcement could claim any
        // `node_id`, poisoning the capability index and route
        // learning for an unrelated peer. `EntityId::node_id()` is
        // a blake2s derivation over the public key, so a forger who
        // doesn't control the key can't produce matching bytes.
        if ann.entity_id.node_id() != ann.node_id {
            tracing::trace!(
                from_node = format!("{:#x}", from_node),
                claimed_node = format!("{:#x}", ann.node_id),
                derived_node = format!("{:#x}", ann.entity_id.node_id()),
                "capability: node_id does not match entity_id derivation"
            );
            return;
        }

        // First-seen identity pin — TOFU. A peer that tries to
        // rebind its `entity_id` in a later announcement is
        // silently rejected. Two preconditions must hold before we
        // pin anything:
        //
        // 1. The announcement is **signature-verified**. An
        //    unsigned announcement's `entity_id` is attacker-
        //    controlled and would poison the binding; unauthenticated
        //    deployments skip the pin entirely, and channel-auth
        //    paths fall through to "missing entity" instead of
        //    trusting forged input.
        // 2. The announcement arrived **directly** from the origin
        //    (`hop_count == 0`). On that path `ann.node_id` was
        //    already checked to equal `from_node` above, so pinning
        //    `from_node → ann.entity_id` binds the session to the
        //    key that actually signed for it. A forwarded
        //    announcement (`hop_count > 0`) travels through an
        //    arbitrary middle peer; pinning `from_node → victim_id`
        //    in that path would let the forwarder pose as the
        //    origin for subsequent channel auth (`authorize_subscribe`
        //    keys on `peer_entity_ids.get(from_node)`). Forwarded
        //    caps still update the capability index + routing, but
        //    the entity binding is deferred to the eventual direct
        //    announcement.
        if signature_verified && ann.hop_count == 0 {
            if let Some(existing) = ctx.peer_entity_ids.get(&from_node) {
                if *existing.value() != ann.entity_id {
                    tracing::trace!(
                        from_node = format!("{:#x}", from_node),
                        "capability: entity_id rebind rejected (TOFU)"
                    );
                    return;
                }
            } else {
                ctx.peer_entity_ids.insert(from_node, ann.entity_id.clone());
            }
        }

        // Derive the peer's subnet *before* moving `ann` into the
        // index — the policy needs `ann.capabilities` and `index()`
        // consumes the announcement by value.
        //
        // Gated on `signature_verified && ann.hop_count == 0` for
        // the same reasons the TOFU pin above has that exact pair
        // of conditions:
        //
        // 1. Unsigned `ann.capabilities` is attacker-controlled, so
        //    a deployment running with
        //    `require_signed_capabilities = false` for discovery
        //    must not let unsigned input feed `peer_subnets` —
        //    that map is read by `subnet_visible` on the
        //    publish / subscribe paths, and a spoofed subnet would
        //    admit a peer to `SubnetLocal` channels it shouldn't
        //    see.
        // 2. On a forwarded announcement (`hop_count > 0`) the
        //    `from_node` in our hands is the relay peer, not the
        //    origin. Writing the origin's derived subnet under the
        //    relay's `node_id` would overwrite the relay's legitimate
        //    subnet binding — a crafted forwarded announcement could
        //    shift any legitimate peer into a different subnet just
        //    by being the last hop on its path. The real binding
        //    comes from the origin's own direct announcement.
        if signature_verified && ann.hop_count == 0 {
            if let Some(policy) = ctx.local_subnet_policy.as_ref() {
                let subnet = policy.assign(&ann.capabilities);
                ctx.peer_subnets.insert(from_node, subnet);
            }
        }

        // Cache BEFORE indexing (the index consumes `ann` by value,
        // but the dedup key is already captured above). Insert at
        // this point so a subsequent duplicate short-circuits at the
        // `contains_key` check without re-parsing + re-verifying.
        ctx.seen_announcements
            .insert(dedup_key, std::time::Instant::now());

        // Topology learning from multi-hop receipt. An announcement
        // arriving with `hop_count > 0` traveled through `from_node`
        // to reach us, so install a route to the origin with metric
        // `hop_count + 2`. The `+2` offset matches the pingwave
        // convention so direct routes (metric 1) always strictly
        // beat any announcement-installed route. Routes from
        // capability announcements compete with pingwave-installed
        // routes via the routing table's "better metric wins" rule.
        // Direct announcements (hop_count == 0) skip this — the
        // session itself is already the authority for that peer.
        if ann.hop_count > 0 {
            if let Some(entry) = ctx.peer_addrs.get(&from_node) {
                let sender_addr = *entry.value();
                let metric = u16::from(ann.hop_count) + 2;
                ctx.router
                    .routing_table()
                    .add_route_with_metric(ann.node_id, sender_addr, metric);
            }
        }

        // Multi-hop forwarding: if we haven't exhausted the hop
        // budget, increment `hop_count` and re-broadcast to every
        // directly-connected peer except the sender and the peer we
        // use to reach the origin (split horizon). Do this BEFORE
        // handing `ann` to the index (which consumes by value) so
        // the forwarder has the current view of `hop_count`.
        if ann.hop_count < MAX_CAPABILITY_HOPS - 1 {
            let mut forwarded = ann.clone();
            // Saturating bump matches every other hop-count
            // increment in the crate (`swarm.rs:122`, `route.rs:254`).
            // The `< MAX_CAPABILITY_HOPS - 1` guard above already
            // bounds this in practice, but a future refactor that
            // raises the cap or relaxes the check would otherwise
            // turn an attacker-controlled byte into a debug-panic /
            // release-wraparound.
            forwarded.hop_count = forwarded.hop_count.saturating_add(1);
            // `to_bytes` on a clone with the bumped counter —
            // signature remains valid because `signed_payload()`
            // zeros `hop_count` on verify.
            let fwd_bytes = forwarded.to_bytes();
            Self::forward_capability_announcement(fwd_bytes, ann.node_id, from_node, ctx);
        }

        ctx.capability_index.index(ann);
    }

    /// Fan an already-serialized capability announcement out to every
    /// directly-connected peer, minus the sender and any split-
    /// horizon-excluded peer. Spawned onto the runtime so the
    /// synchronous dispatch handler isn't blocked on per-peer
    /// encryption and network send. Mirrors the pingwave forwarding
    /// loop at the top of `dispatch_packet` — same split-horizon
    /// rule, same best-effort send semantics.
    fn forward_capability_announcement(
        payload: Vec<u8>,
        origin_node_id: u64,
        sender_node_id: u64,
        ctx: &DispatchCtx,
    ) {
        let peers = ctx.peers.clone();
        let socket = ctx.socket.clone();
        let partition_filter = ctx.partition_filter.clone();
        let router = ctx.router.clone();

        tokio::spawn(async move {
            // Split-horizon: consult the routing table for the
            // origin's best next hop and skip that peer. Matches the
            // pingwave rule so capability forwarding + pingwave
            // forwarding contribute to the same DV loop-avoidance
            // invariant.
            let next_hop_addr = router.routing_table().lookup(origin_node_id);

            for entry in peers.iter() {
                let peer = entry.value();
                if peer.node_id == sender_node_id {
                    continue; // never send back to whoever gave it to us
                }
                if Some(peer.addr) == next_hop_addr {
                    continue; // split horizon: that's our path to the origin
                }
                if partition_filter.contains(&peer.addr) {
                    continue;
                }

                // Build + send a subprotocol packet through this
                // peer's session. Same path as `send_subprotocol`;
                // inlined because the dispatch handler has no `self`.
                let session = &peer.session;
                let stream_id = SUBPROTOCOL_CAPABILITY_ANN as u64;
                let pool = session.thread_local_pool();
                let mut builder = pool.get();
                let seq = {
                    let stream = session.get_or_create_stream(stream_id);
                    stream.next_tx_seq()
                };
                let events = vec![Bytes::copy_from_slice(&payload)];
                let packet = builder.build_subprotocol(
                    stream_id,
                    seq,
                    &events,
                    PacketFlags::NONE,
                    SUBPROTOCOL_CAPABILITY_ANN,
                );
                let _ = socket.send_to(&packet, peer.addr).await;
                drop(builder);
                session.touch();
            }
        });
    }

    /// Coordinator-side handler for a `PunchRequest` from peer A
    /// (who wants to punch to target B).
    ///
    /// Behavior (plan §3 coordinator steps 1–3):
    ///
    /// 1. Resolve B's reflex address. Preferred source is the
    ///    `reflex_addr` field on B's latest signed
    ///    `CapabilityAnnouncement` in the local index. Without a
    ///    cached reflex, coordination can't proceed — we drop the
    ///    request silently. A's side times out on
    ///    `connect_direct` and falls back to routed-handshake.
    /// 2. Pick `fire_at = now() + TraversalConfig::punch_fire_lead`
    ///    (default 500 ms) — short enough to be under any plausible
    ///    NAT keep-alive row timeout, long enough for both
    ///    endpoints to receive `PunchIntroduce` and arm their
    ///    timers.
    /// 3. Fan out `PunchIntroduce` to both A and B with the
    ///    respective counterpart's reflex and the shared
    ///    `fire_at`.
    ///
    /// Best-effort: A unreachable-from-us or B not-in-our-peer-
    /// table short-circuits. Neither is surfaced — the caller's
    /// `connect_direct` timeout is the recovery path.
    #[cfg(feature = "nat-traversal")]
    fn handle_punch_request(
        from_node: u64,
        req: super::traversal::rendezvous::PunchRequest,
        ctx: &DispatchCtx,
    ) {
        use super::traversal::rendezvous::{PunchIntroduce, RendezvousMsg};

        // 1. Resolve B's reflex address. A's self-reported reflex
        //    (carried on the request) is the fallback when the
        //    capability cache doesn't have one yet — matches plan
        //    decision 7's "prefer-cached, fall-back-to-announced."
        //    But we do NOT override A's reflex with the cached one
        //    if A also announces it: the cache may be stale after
        //    a NAT rebind, and A's self-report is the freshest
        //    observation from A's own perspective.
        let Some(b_reflex) = ctx.capability_index.reflex_addr(req.target) else {
            tracing::trace!(
                from_node = format!("{:#x}", from_node),
                target = format!("{:#x}", req.target),
                "rendezvous: no cached reflex for target; dropping PunchRequest",
            );
            return;
        };

        // A's reflex comes from the request body. R trusts A's
        // self-report over the cached value for A-side, per the
        // note above; a mid-session rebind on A's gateway is
        // visible to A before it propagates into R's capability
        // cache via a re-announce.
        let a_reflex = req.self_reflex;

        // 2. Compute the shared fire time.
        let fire_lead = ctx.traversal_config.punch_fire_lead;
        let fire_at_ms = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => d
                .saturating_add(fire_lead)
                .as_millis()
                .min(u64::MAX as u128) as u64,
            Err(_) => {
                // System clock pre-1970 — can't compute a
                // meaningful fire_at. Drop rather than introduce
                // with a garbage time that endpoints would
                // interpret as "already past."
                return;
            }
        };

        // 3. Build introduce payloads. Each side learns the
        //    counterpart's reflex + the shared fire_at.
        let intro_to_a = RendezvousMsg::PunchIntroduce(PunchIntroduce {
            peer: req.target,
            peer_reflex: b_reflex,
            fire_at_ms,
        })
        .encode();
        let intro_to_b = RendezvousMsg::PunchIntroduce(PunchIntroduce {
            peer: from_node,
            peer_reflex: a_reflex,
            fire_at_ms,
        })
        .encode();

        // Look up sessions for both endpoints. If B isn't in our
        // peer table we can't introduce — stage 5 SDK surface
        // will map this to `RendezvousNoRelay` on A's side via
        // the `connect_direct` timeout.
        let Some((a_addr, a_session)) = ctx
            .peers
            .get(&from_node)
            .map(|e| (e.value().addr, e.value().session.clone()))
        else {
            return;
        };
        let Some((b_addr, b_session)) = ctx
            .peers
            .get(&req.target)
            .map(|e| (e.value().addr, e.value().session.clone()))
        else {
            tracing::trace!(
                from_node = format!("{:#x}", from_node),
                target = format!("{:#x}", req.target),
                "rendezvous: target peer not directly connected; dropping",
            );
            return;
        };

        if ctx.partition_filter.contains(&a_addr) || ctx.partition_filter.contains(&b_addr) {
            return;
        }

        let socket_a = ctx.socket.clone();
        let socket_b = ctx.socket.clone();
        tokio::spawn(async move {
            let pool = a_session.thread_local_pool();
            let mut builder = pool.get();
            let seq = {
                let stream =
                    a_session.get_or_create_stream(super::traversal::SUBPROTOCOL_RENDEZVOUS as u64);
                stream.next_tx_seq()
            };
            let events = vec![intro_to_a];
            let packet = builder.build_subprotocol(
                super::traversal::SUBPROTOCOL_RENDEZVOUS as u64,
                seq,
                &events,
                PacketFlags::NONE,
                super::traversal::SUBPROTOCOL_RENDEZVOUS,
            );
            let _ = socket_a.send_to(&packet, a_addr).await;
        });
        tokio::spawn(async move {
            let pool = b_session.thread_local_pool();
            let mut builder = pool.get();
            let seq = {
                let stream =
                    b_session.get_or_create_stream(super::traversal::SUBPROTOCOL_RENDEZVOUS as u64);
                stream.next_tx_seq()
            };
            let events = vec![intro_to_b];
            let packet = builder.build_subprotocol(
                super::traversal::SUBPROTOCOL_RENDEZVOUS as u64,
                seq,
                &events,
                PacketFlags::NONE,
                super::traversal::SUBPROTOCOL_RENDEZVOUS,
            );
            let _ = socket_b.send_to(&packet, b_addr).await;
        });
    }

    /// Endpoint-side: schedule the keep-alive train + observer
    /// after receiving a `PunchIntroduce`. Plan §3 endpoint
    /// behavior, end-to-end:
    ///
    /// 1. At `fire_at`, `fire_at + 100ms`, `fire_at + 250ms`
    ///    send a keep-alive packet to `intro.peer_reflex`. Raw
    ///    UDP, no encryption — the peer has no session with us
    ///    yet. The purpose is to open our side's NAT
    ///    connection-tracking row, so the peer's keep-alive
    ///    (fired in the same window on their side) can arrive.
    /// 2. Install an observer on `punch_observers[peer_reflex]`.
    ///    The receive loop's pre-session keep-alive recognition
    ///    fires it on first matching inbound.
    /// 3. Wait up to `punch_deadline` for the observer. On
    ///    success, emit a `PunchAck` via the coordinator; on
    ///    timeout, drop silently — the counterpart's
    ///    `await_punch_ack` times out too, and `connect_direct`
    ///    records the fallback.
    ///
    /// Best-effort throughout: a failed send at any step is
    /// logged-and-skipped. Rendezvous is an optimization (plan
    /// framing); routed-handshake is always the safety net.
    #[cfg(feature = "nat-traversal")]
    fn schedule_punch(
        coordinator_node_id: u64,
        intro: super::traversal::rendezvous::PunchIntroduce,
        ctx: &DispatchCtx,
    ) {
        use super::traversal::rendezvous::{encode_keepalive, Keepalive, PunchAck, RendezvousMsg};

        let Some((coord_addr, coord_session)) = ctx
            .peers
            .get(&coordinator_node_id)
            .map(|e| (e.value().addr, e.value().session.clone()))
        else {
            return;
        };
        if ctx.partition_filter.contains(&coord_addr) {
            return;
        }
        if ctx.partition_filter.contains(&intro.peer_reflex) {
            return;
        }

        // Install the observer. A prior pending entry at the same
        // addr (unusual — would mean two simultaneous punches to
        // the same peer_reflex) is replaced; the earlier scheduler
        // task sees a `SendError` on its oneshot.
        let (obs_tx, obs_rx) = oneshot::channel();
        ctx.punch_observers.insert(intro.peer_reflex, obs_tx);

        // Compute keep-alive send delays. `fire_at_ms` is a Unix
        // epoch millisecond value synthesized by R; we subtract
        // "now" to get the lead. If the computed lead is
        // negative (clock skew, slow path), treat as "fire
        // immediately" rather than as an error.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let base_lead_ms = intro.fire_at_ms.saturating_sub(now_ms);
        let base_lead = std::time::Duration::from_millis(base_lead_ms);
        let offsets = [
            base_lead,
            base_lead.saturating_add(std::time::Duration::from_millis(100)),
            base_lead.saturating_add(std::time::Duration::from_millis(250)),
        ];

        let local_node_id = ctx.local_node_id;
        let peer_reflex = intro.peer_reflex;
        let peer = intro.peer;
        let socket_send = ctx.socket.clone();
        let socket_ack = ctx.socket.clone();
        let deadline = ctx.traversal_config.punch_deadline;
        let punch_observers = ctx.punch_observers.clone();

        // Keep-alive sender task: fires three packets at
        // absolute offsets from the spawn instant. Each
        // `offsets[i]` is the intended delay from `start`, not
        // from the previous iteration — `sleep_until(start + offset)`
        // keeps the schedule anchored so a slow `send_to` on
        // packet N doesn't push packet N+1 past its deadline.
        //
        // History: an earlier revision did `sleep(offset)` in
        // the loop, which cumulatively summed to 500 / 1100 /
        // 1850 ms instead of 500 / 600 / 750 ms at the default
        // fire_lead — the later packets missed the peer's punch
        // window entirely. cubic flagged this as P1.
        let keepalive_payload = encode_keepalive(&Keepalive {
            sender_node_id: local_node_id,
            punch_id: 0, // reserved; no generator wiring yet
        });
        tokio::spawn(async move {
            let start = tokio::time::Instant::now();
            for offset in offsets {
                tokio::time::sleep_until(start + offset).await;
                let _ = socket_send
                    .send_to(&keepalive_payload[..], peer_reflex)
                    .await;
            }
        });

        // Observer task: waits for the receive loop to fire the
        // oneshot. On success emits the PunchAck; on timeout /
        // cancellation defers to `await_punch_observer_outcome`
        // for cleanup so the map-eviction race against a
        // replacement observer is handled in one place.
        tokio::spawn(async move {
            let outcome =
                await_punch_observer_outcome(obs_rx, deadline, &punch_observers, peer_reflex).await;
            if !outcome {
                return;
            }
            // Observer fired — build + send the ack via the
            // coordinator session, same shape as the former
            // `send_punch_ack_via` helper.
            let ack_body = RendezvousMsg::PunchAck(PunchAck {
                from_peer: local_node_id,
                to_peer: peer,
                punch_id: 0,
            })
            .encode();
            let pool = coord_session.thread_local_pool();
            let mut builder = pool.get();
            let seq = {
                let stream = coord_session
                    .get_or_create_stream(super::traversal::SUBPROTOCOL_RENDEZVOUS as u64);
                stream.next_tx_seq()
            };
            let events = vec![ack_body];
            let packet = builder.build_subprotocol(
                super::traversal::SUBPROTOCOL_RENDEZVOUS as u64,
                seq,
                &events,
                PacketFlags::NONE,
                super::traversal::SUBPROTOCOL_RENDEZVOUS,
            );
            let _ = socket_ack.send_to(&packet, coord_addr).await;
        });
    }

    /// Coordinator-side: forward a `PunchAck` whose `to_peer`
    /// isn't us to the session with `to_peer`. Best-effort —
    /// if we don't have a session with `to_peer`, drop.
    ///
    /// Wire bytes are preserved intact: `from_peer` still names
    /// the original sender so the recipient can correlate
    /// against its `pending_punch_acks` map.
    #[cfg(feature = "nat-traversal")]
    fn forward_punch_ack(ack: super::traversal::rendezvous::PunchAck, ctx: &DispatchCtx) {
        use super::traversal::rendezvous::RendezvousMsg;

        let Some((dest_addr, dest_session)) = ctx
            .peers
            .get(&ack.to_peer)
            .map(|e| (e.value().addr, e.value().session.clone()))
        else {
            tracing::trace!(
                to_peer = format!("{:#x}", ack.to_peer),
                "rendezvous: no session with PunchAck.to_peer; dropping",
            );
            return;
        };
        if ctx.partition_filter.contains(&dest_addr) {
            return;
        }

        let body = RendezvousMsg::PunchAck(ack).encode();
        let socket = ctx.socket.clone();
        tokio::spawn(async move {
            let pool = dest_session.thread_local_pool();
            let mut builder = pool.get();
            let seq = {
                let stream = dest_session
                    .get_or_create_stream(super::traversal::SUBPROTOCOL_RENDEZVOUS as u64);
                stream.next_tx_seq()
            };
            let events = vec![body];
            let packet = builder.build_subprotocol(
                super::traversal::SUBPROTOCOL_RENDEZVOUS as u64,
                seq,
                &events,
                PacketFlags::NONE,
                super::traversal::SUBPROTOCOL_RENDEZVOUS,
            );
            let _ = socket.send_to(&packet, dest_addr).await;
        });
    }

    /// Decide whether a Subscribe from `from_node` on `channel` is allowed.
    ///
    /// Rules, in order:
    /// 1. Per-peer channel cap — rejects with `TooManyChannels`.
    /// 2. If a `channel_configs` registry is set and the channel isn't in
    ///    it, reject with `UnknownChannel`.
    /// 3. Channel [`Visibility`] must permit the subscriber's subnet
    ///    — reject cross-subnet subscribes with `Unauthorized`.
    /// 4. Channel auth — `publish_caps` / `subscribe_caps` /
    ///    `require_token` on `ChannelConfig` are honored via
    ///    `ChannelConfig::can_subscribe`. A presented token is
    ///    installed into the local `TokenCache` (after signature
    ///    verification) before the check runs.
    fn authorize_subscribe(
        channel: &ChannelName,
        from_node: u64,
        token_bytes: Option<&[u8]>,
        ctx: &DispatchCtx,
    ) -> (bool, Option<AckReason>) {
        // Rate-limit check runs first — a throttled peer short-
        // circuits without consuming any ed25519 work. The
        // failure counter increments only on actual auth-rule
        // rejections below (not on `TooManyChannels`, which is a
        // resource-limit failure, not an auth failure).
        if Self::is_auth_throttled(from_node, ctx) {
            return (false, Some(AckReason::RateLimited));
        }
        if ctx.roster.channels_for_peer_count(from_node) >= ctx.max_channels_per_peer {
            return (false, Some(AckReason::TooManyChannels));
        }
        let Some(ref configs) = ctx.channel_configs else {
            // No registry → no ACL (test / permissive deployments).
            return (true, None);
        };
        let Some(cfg_ref) = configs.get_by_name(channel.as_str()) else {
            return (false, Some(AckReason::UnknownChannel));
        };
        // Clone the cfg so we can drop the DashMap guard before
        // any further work — the cfg fields are all cheap to clone
        // and doing so releases the registry's read lock early.
        let cfg = cfg_ref.clone();
        drop(cfg_ref);

        let peer_subnet = ctx
            .peer_subnets
            .get(&from_node)
            .map(|e| *e.value())
            .unwrap_or(SubnetId::GLOBAL);
        if !Self::subnet_visible(ctx.local_subnet, peer_subnet, cfg.visibility) {
            return (false, Some(AckReason::Unauthorized));
        }

        // Parse + verify the presented token into a LOCAL scratch
        // value. We do NOT insert into the shared cache until the
        // full auth check passes — otherwise an attacker can spam
        // self-signed subscribes (which fail at cap/visibility
        // checks) yet leave their tokens permanently in the shared
        // cache, exhausting memory keyed under attacker-controlled
        // `(subject, channel_hash)` slots.
        let presented_token = token_bytes
            .and_then(|bytes| PermissionToken::from_bytes(bytes).ok())
            .filter(|tok| tok.verify().is_ok());

        // Whether any cap / token gate is in play. A fully open
        // channel (no filters, no require_token) short-circuits
        // without needing a peer entity_id at all.
        let has_auth_gates =
            cfg.publish_caps.is_some() || cfg.subscribe_caps.is_some() || cfg.require_token;
        if !has_auth_gates {
            return (true, None);
        }

        // Peer caps default to empty — subscribe-before-announce
        // races return `None` from the index; we treat that as an
        // empty capability set, which makes `subscribe_caps` filters
        // fail closed.
        let peer_caps = ctx.capability_index.get(from_node).unwrap_or_default();

        // Peer entity — load-bearing for `require_token`. Without
        // it we can't validate the subject. Missing entity +
        // require_token = reject.
        let Some(peer_entity) = ctx
            .peer_entity_ids
            .get(&from_node)
            .map(|e| e.value().clone())
        else {
            if cfg.require_token {
                return (false, Some(AckReason::Unauthorized));
            }
            // Cap-filter-only mode without a known entity — build a
            // dummy id so `can_subscribe` can still run the cap
            // match. Token check is skipped via `require_token=false`.
            let dummy = EntityId::from_bytes([0u8; 32]);
            let empty_cache = Arc::new(TokenCache::new());
            return if cfg.can_subscribe(&peer_caps, &dummy, &empty_cache) {
                (true, None)
            } else {
                (false, Some(AckReason::Unauthorized))
            };
        };

        // Run the ACL check against a scratch cache containing only
        // the presented token first. A positive verdict means the
        // fresh token alone is sufficient. If that fails, fall back
        // to the shared cache so a peer relying on a previously-
        // stored delegation can still re-subscribe without having
        // to re-present. The shared cache is read-only for this
        // decision.
        let scratch_cache = Arc::new(TokenCache::new());
        if let Some(ref tok) = presented_token {
            scratch_cache.insert_unchecked(tok.clone());
        }
        let passed_with_scratch = cfg.can_subscribe(&peer_caps, &peer_entity, &scratch_cache);
        let passed = passed_with_scratch
            || ctx
                .token_cache
                .as_ref()
                .is_some_and(|shared| cfg.can_subscribe(&peer_caps, &peer_entity, shared));
        if !passed {
            return (false, Some(AckReason::Unauthorized));
        }

        // Auth passed — now and only now promote the presented token
        // to the shared cache so future subscribes on the same
        // (subject, channel_hash) can skip re-presenting.
        if let (Some(tok), Some(shared)) = (presented_token, ctx.token_cache.as_ref()) {
            let _ = shared.insert(tok);
        }
        (true, None)
    }

    /// Check whether `from_node` is currently auth-throttled.
    /// Reads + clears the `throttled_until` instant atomically so
    /// an expired throttle state doesn't leak into future windows.
    fn is_auth_throttled(from_node: u64, ctx: &DispatchCtx) -> bool {
        if ctx.max_auth_failures_per_window == u16::MAX {
            return false; // threshold disabled
        }
        let Some(mut entry) = ctx.auth_failures.get_mut(&from_node) else {
            return false;
        };
        match entry.throttled_until {
            Some(until) if std::time::Instant::now() < until => true,
            Some(_) => {
                // Throttle elapsed — reset so the peer gets a
                // clean slate next time around.
                entry.throttled_until = None;
                entry.failures = 0;
                entry.window_start = None;
                false
            }
            None => false,
        }
    }

    /// Record an authorization-rule rejection against `from_node`.
    /// Increments the rolling-window counter; once it crosses
    /// `max_auth_failures_per_window`, marks the peer as throttled
    /// for `auth_throttle_duration`.
    fn record_auth_failure(from_node: u64, ctx: &DispatchCtx) {
        if ctx.max_auth_failures_per_window == u16::MAX {
            return;
        }
        let now = std::time::Instant::now();
        let mut entry = ctx.auth_failures.entry(from_node).or_default();
        // Window reset: if the current window has elapsed, start
        // fresh. Keeps failure counts from leaking across honest
        // retry storms separated by long idle periods.
        let reset_window = match entry.window_start {
            Some(start) => now.duration_since(start) >= ctx.auth_failure_window,
            None => true,
        };
        if reset_window {
            entry.window_start = Some(now);
            entry.failures = 0;
        }
        entry.failures = entry.failures.saturating_add(1);
        if entry.failures >= ctx.max_auth_failures_per_window {
            entry.throttled_until = Some(now + ctx.auth_throttle_duration);
        }
    }

    /// Wipe `from_node`'s failure counter. Called after a successful
    /// subscribe so honest peers that occasionally fail (stale
    /// token, renewal race) don't accumulate toward the throttle.
    fn clear_auth_failures(from_node: u64, ctx: &DispatchCtx) {
        ctx.auth_failures.remove(&from_node);
    }

    /// `true` if a packet with `visibility` originating in `source`
    /// should be delivered to a peer in `dest`.
    ///
    /// Mirrors the `SubnetGateway::should_forward` visibility matrix
    /// but doesn't need the gateway's state (`peer_subnets`,
    /// `export_table`). Regular participants use this for
    /// publish-fan-out filtering + subscribe-gate checks;
    /// border-gateway nodes with richer routing state should use the
    /// full `SubnetGateway` instead.
    ///
    /// `Exported` is conservative — returns `false` unless a
    /// per-channel export table is consulted elsewhere. Wiring that
    /// is a documented follow-up.
    fn subnet_visible(source: SubnetId, dest: SubnetId, visibility: Visibility) -> bool {
        match visibility {
            Visibility::Global => true,
            Visibility::SubnetLocal => source.is_same_subnet(dest),
            Visibility::ParentVisible => {
                // "Visible to the parent subnet but not siblings" —
                // strictly upward. A child's broadcast reaches its
                // own subnet (covered by `is_ancestor_of` since a
                // subnet is its own ancestor) and any ancestor; a
                // parent broadcasting down to descendants would leak
                // region-scoped traffic and is rejected.
                dest.is_ancestor_of(source)
            }
            Visibility::Exported => false,
        }
    }

    /// Send an `Ack` on the membership subprotocol back to `to_node`.
    /// Non-fatal if `to_node` is not in the peer map or the send fails;
    /// the requester will simply hit its ack timeout.
    fn send_membership_ack(
        to_node: u64,
        nonce: u64,
        accepted: bool,
        reason: Option<AckReason>,
        ctx: &DispatchCtx,
    ) {
        let Some(peer_entry) = ctx.peers.get(&to_node) else {
            return;
        };
        let dest_addr = peer_entry.value().addr;
        if ctx.partition_filter.contains(&dest_addr) {
            return;
        }
        let dest_sess = peer_entry.value().session.clone();
        let socket = ctx.socket.clone();
        let ack = MembershipMsg::Ack {
            nonce,
            accepted,
            reason,
        };
        let bytes = Bytes::from(membership::encode(&ack));
        drop(peer_entry);

        tokio::spawn(async move {
            let pool = dest_sess.thread_local_pool();
            let mut builder = pool.get();
            let stream_id = SUBPROTOCOL_CHANNEL_MEMBERSHIP as u64;
            let seq = {
                let stream = dest_sess.get_or_create_stream(stream_id);
                stream.next_tx_seq()
            };
            let events = vec![bytes];
            let packet = builder.build_subprotocol(
                stream_id,
                seq,
                &events,
                PacketFlags::NONE,
                SUBPROTOCOL_CHANNEL_MEMBERSHIP,
            );
            let _ = socket.send_to(&packet, dest_addr).await;
        });
    }

    // ── Channel fan-out (ChannelPublisher) ─────────────────────────────

    /// Build a [`ChannelPublisher`] recipe. Does NOT talk to the wire —
    /// combine with [`publish`](Self::publish) or
    /// [`publish_many`](Self::publish_many) to actually fan out.
    pub fn channel_publisher(
        &self,
        channel: ChannelName,
        config: PublishConfig,
    ) -> ChannelPublisher {
        ChannelPublisher::new(channel, config)
    }

    /// Fan `payload` out to every subscriber of the publisher's channel.
    ///
    /// One per-peer unicast per subscriber — no multicast primitive, no
    /// group crypto. Per-peer concurrency is bounded by
    /// `PublishConfig::max_inflight`. The failure policy controls whether
    /// per-peer errors short-circuit the fan-out (see [`OnFailure`]).
    pub async fn publish(
        &self,
        publisher: &ChannelPublisher,
        payload: Bytes,
    ) -> Result<PublishReport, AdapterError> {
        self.publish_many(publisher, &[payload]).await
    }

    /// Fan multiple payloads out to every subscriber of the publisher's
    /// channel. Semantics are the same as [`publish`](Self::publish); the
    /// whole `events` slice is delivered as one batch per subscriber.
    pub async fn publish_many(
        &self,
        publisher: &ChannelPublisher,
        events: &[Bytes],
    ) -> Result<PublishReport, AdapterError> {
        // Publisher-side auth: if the channel is registered with
        // `publish_caps` / `require_token`, the local node must
        // satisfy them *before* fan-out begins. Keeps a node from
        // silently publishing to a channel whose own ACL it doesn't
        // match. Channels absent from the registry are treated as
        // open (permissive default).
        let cfg_snapshot = self.channel_configs.as_ref().and_then(|cr| {
            cr.get_by_name(publisher.channel().name().as_str())
                .map(|c| c.clone())
        });
        if let Some(cfg) = cfg_snapshot.as_ref() {
            if cfg.publish_caps.is_some() || cfg.require_token {
                let self_caps = self
                    .local_announcement
                    .load()
                    .as_deref()
                    .map(|ann| ann.capabilities.clone())
                    .unwrap_or_default();
                let self_entity = self.identity.entity_id().clone();
                let cache = self
                    .token_cache
                    .clone()
                    .unwrap_or_else(|| Arc::new(TokenCache::new()));
                if !cfg.can_publish(&self_caps, &self_entity, &cache) {
                    return Err(AdapterError::Connection(
                        "channel: publish denied by channel ACL".into(),
                    ));
                }
            }
        }

        // Snapshot subscribers at call time; late subscribers won't see
        // this publish, early-unsubscribes may still receive it — both
        // are documented non-goals.
        let mut subscribers = self.roster.members(publisher.channel());

        // Subnet visibility filter. Look up the channel's
        // configured visibility; if the channel has no registry
        // entry, fall back to `config.default_visibility` (which
        // itself defaults to `Visibility::Global` for back-compat
        // with simple registry-less deployments). Fleet operators
        // who want fail-closed behavior set
        // `with_default_visibility(Visibility::SubnetLocal)` so
        // a forgotten registry entry confines messages to the
        // local subnet rather than leaking them mesh-wide.
        // Filtered subscribers don't show up in `attempted` or
        // `errors` — they're policy decisions, not failures.
        let visibility = cfg_snapshot
            .as_ref()
            .map(|c| c.visibility)
            .unwrap_or(self.config.default_visibility);
        subscribers.retain(|peer_id| {
            let peer_subnet = self
                .peer_subnets
                .get(peer_id)
                .map(|e| *e.value())
                .unwrap_or(SubnetId::GLOBAL);
            Self::subnet_visible(self.local_subnet, peer_subnet, visibility)
        });

        // AuthGuard fast path. Populated by `authorize_subscribe`;
        // revoked on unsubscribe and by the expiry sweep. Consulted
        // on every publish so revocations take effect on the next
        // fan-out without waiting for a roster refresh.
        //
        // Three-way verdict:
        //
        // - `Allowed`: bloom hit + verified-cache entry says yes.
        //   The verified cache is keyed on the 16-bit `channel_hash`
        //   that rides the wire header, which collides routinely at
        //   mesh scale — one subscriber's grant on channel A can
        //   falsely admit them on channel B when the hashes alias.
        //   Cross-check the canonical name against `exact` before
        //   trusting the verdict; a mismatch means the allow came
        //   from a different channel that happened to collide.
        // - `Denied`: bloom miss — no auth entry exists for this
        //   (origin, channel). Skip the subscriber.
        // - `Unknown`: bloom hit but verified cache missed. Fall
        //   back to the exact-channel ACL. On hit, promote back
        //   into the verified cache so subsequent publishes take
        //   the fast path.
        //
        // Open channels (no auth configured) are admitted on every
        // subscribe via `allow_channel`, so the fast path trivially
        // passes for them — no conditional branch needed.
        //
        // Additionally, when the channel is `require_token`, we do a
        // **lazy expiry check** on each admitted subscriber. The
        // periodic token sweep (`spawn_token_sweep_loop`) is the
        // primary eviction path, but a caller may deliberately
        // configure `token_sweep_interval = Duration::MAX` to opt out
        // — and without a second line of defence an expired token
        // would keep authorizing packets forever. Probing the token
        // cache per admitted subscriber costs one DashMap lookup +
        // a timestamp compare per publish, which is only paid on
        // token-gated channels (`require_token = false` skips the
        // branch entirely). An expired subscriber is revoked inline
        // so the next publish takes the `Denied` path.
        let channel_name = publisher.channel().name().clone();
        let channel_hash = channel_name.hash();
        let auth_guard = self.auth_guard.clone();
        let require_token = cfg_snapshot
            .as_ref()
            .map(|c| c.require_token)
            .unwrap_or(false);
        subscribers.retain(|peer_id| {
            let origin = subscriber_origin_hash(*peer_id);
            let admitted = match auth_guard.check_fast(origin, channel_hash) {
                AuthVerdict::Allowed => auth_guard.is_authorized_full(origin, &channel_name),
                AuthVerdict::Denied => false,
                AuthVerdict::NeedsFullCheck => {
                    if auth_guard.is_authorized_full(origin, &channel_name) {
                        auth_guard.allow_channel(origin, &channel_name);
                        true
                    } else {
                        false
                    }
                }
            };
            if !admitted {
                return false;
            }
            if !require_token {
                return true;
            }
            // Token-gated branch: ensure the subscriber still has a
            // valid token. If the cache answer is "no valid token
            // authorizes SUBSCRIBE on this channel," revoke inline.
            let (Some(cache), Some(entity)) = (
                self.token_cache.as_ref(),
                self.peer_entity_ids.get(peer_id).map(|e| e.value().clone()),
            ) else {
                // Missing entity binding or no cache installed —
                // treat as unauthorized. The subscribe path would
                // have rejected this peer in the first place;
                // reaching here means a config drift that we must
                // not paper over by admitting the publish.
                auth_guard.revoke_channel(origin, &channel_name);
                return false;
            };
            if cache
                .check(&entity, TokenScope::SUBSCRIBE, channel_hash)
                .is_err()
            {
                auth_guard.revoke_channel(origin, &channel_name);
                return false;
            }
            true
        });

        let mut report = PublishReport {
            attempted: subscribers.len(),
            delivered: 0,
            errors: Vec::new(),
        };
        if subscribers.is_empty() {
            return Ok(report);
        }

        let reliable = publisher.config().reliability.is_reliable();
        let stream_id = Self::publish_stream_id(publisher.channel());
        let max_inflight = publisher.config().max_inflight;
        let on_failure = publisher.config().on_failure;

        use tokio::sync::Semaphore;
        let sem = Arc::new(Semaphore::new(max_inflight.max(1)));

        match on_failure {
            OnFailure::FailFast => {
                // Sequential; stop on first error. Concurrency isn't
                // meaningful here because we'd be discarding in-flight
                // results anyway.
                for peer_id in &subscribers {
                    match self
                        .publish_to_peer(*peer_id, stream_id, reliable, events)
                        .await
                    {
                        Ok(()) => report.delivered += 1,
                        Err(e) => {
                            report.errors.push((*peer_id, e));
                            return Ok(report);
                        }
                    }
                }
                Ok(report)
            }
            OnFailure::BestEffort | OnFailure::Collect => {
                let mut handles = Vec::with_capacity(subscribers.len());
                for peer_id in subscribers {
                    let permit = Arc::clone(&sem);
                    let events_owned: Vec<Bytes> = events.to_vec();
                    let fut = async move {
                        let _permit = permit.acquire_owned().await.ok();
                        (
                            peer_id,
                            self.publish_to_peer(peer_id, stream_id, reliable, &events_owned)
                                .await,
                        )
                    };
                    handles.push(fut);
                }
                let results = futures::future::join_all(handles).await;
                for (peer_id, res) in results {
                    match res {
                        Ok(()) => report.delivered += 1,
                        Err(e) => report.errors.push((peer_id, e)),
                    }
                }
                // BestEffort returns Ok as long as at least one subscriber
                // got the payload — empty roster was handled above, so
                // here there was at least one attempt.
                if matches!(on_failure, OnFailure::BestEffort)
                    && report.delivered == 0
                    && !report.errors.is_empty()
                {
                    let first = report
                        .errors
                        .first()
                        .map(|(id, e)| {
                            format!(
                                "all {} peers failed (first: {:#x}: {})",
                                report.attempted, id, e
                            )
                        })
                        .unwrap_or_else(|| "all peers failed".into());
                    return Err(AdapterError::Connection(first));
                }
                Ok(report)
            }
        }
    }

    /// Encode the channel hash into a `u64` stream id so that per-channel
    /// ordering holds within a session. Hash collisions between channels
    /// are possible but harmless here — streams are opaque u64 to the
    /// transport and have no ACL meaning.
    fn publish_stream_id(channel: &ChannelId) -> u64 {
        // Place channel hash in the low 16 bits; the upper bits stay zero
        // so that channel-keyed publisher streams don't alias the common
        // subprotocol range (0x0400..0x0A00).
        0x0001_0000_0000_0000 | (channel.hash() as u64)
    }

    /// Send one per-peer leg of a publish. Reuses the same packet-build
    /// path as `send_on_stream`, with an explicit stream opened per
    /// `(peer, channel)` pair.
    async fn publish_to_peer(
        &self,
        peer_node_id: u64,
        stream_id: u64,
        reliable: bool,
        events: &[Bytes],
    ) -> Result<(), AdapterError> {
        let (dest_addr, session) = match self.peers.get(&peer_node_id) {
            Some(p) => (p.value().addr, p.value().session.clone()),
            None => {
                return Err(AdapterError::Connection(format!(
                    "publish: no session for subscriber {:#x}",
                    peer_node_id
                )));
            }
        };

        if self.partition_filter.contains(&dest_addr) {
            return Err(AdapterError::Connection(format!(
                "publish: peer {:#x} is partitioned",
                peer_node_id
            )));
        }

        // Ensure a stream is open with the right reliability mode.
        // `open_stream_with` seeds the stream with
        // `DEFAULT_STREAM_WINDOW_BYTES` so publish traffic rides
        // the same v2 byte-credit window as `send_on_stream`.
        session.open_stream_with(stream_id, reliable, 1);

        // Charge credit on the wire-byte size of the packet we're
        // about to build. The `TxSlotGuard` refunds on Drop unless
        // we `commit()` after a successful socket send, so a failed
        // send doesn't strand credit.
        let payload_bytes: usize = events.iter().map(|e| EventFrame::LEN_SIZE + e.len()).sum();
        let needed = wire_bytes_for_payload(payload_bytes);
        let (guard, seq) = match session.try_acquire_tx_credit_guard(stream_id, needed) {
            TxAdmit::Acquired { guard, seq } => (guard, seq),
            TxAdmit::WindowFull => {
                return Err(AdapterError::Connection(format!(
                    "publish: stream {:#x} backpressured",
                    stream_id
                )));
            }
            TxAdmit::StreamClosed => {
                return Err(AdapterError::Connection(format!(
                    "publish: stream {:#x} closed",
                    stream_id
                )));
            }
        };

        let pool = session.thread_local_pool();
        let mut builder = pool.get();
        let packet = builder.build_subprotocol(
            stream_id,
            seq,
            events,
            PacketFlags::NONE,
            0, /* subprotocol_id 0 = event-plane */
        );

        let next_hop = self
            .router
            .routing_table()
            .lookup(peer_node_id)
            .unwrap_or(dest_addr);

        self.socket
            .send_to(&packet, next_hop)
            .await
            .map_err(|e| AdapterError::Connection(format!("publish send failed: {}", e)))?;
        guard.commit(); // wire-accepted — bytes now belong to the receiver

        drop(builder);
        session.touch();
        Ok(())
    }

    /// Send a raw subprotocol message to a peer.
    ///
    /// The payload is sent as a single event frame with the specified
    /// `subprotocol_id` set in the Net header (included in AEAD AAD).
    pub async fn send_subprotocol(
        &self,
        peer_addr: SocketAddr,
        subprotocol_id: u16,
        payload: &[u8],
    ) -> Result<(), AdapterError> {
        if self.partition_filter.contains(&peer_addr) {
            return Ok(());
        }

        let node_id = self
            .addr_to_node
            .get(&peer_addr)
            .map(|e| *e.value())
            .ok_or_else(|| AdapterError::Connection("unknown peer".into()))?;
        let peer = self
            .peers
            .get(&node_id)
            .ok_or_else(|| AdapterError::Connection("unknown peer".into()))?;

        let session = &peer.session;
        let stream_id = subprotocol_id as u64;

        let pool = session.thread_local_pool();
        let mut builder = pool.get();

        let seq = {
            let stream = session.get_or_create_stream(stream_id);
            stream.next_tx_seq()
        };

        let events = vec![Bytes::copy_from_slice(payload)];
        let packet =
            builder.build_subprotocol(stream_id, seq, &events, PacketFlags::NONE, subprotocol_id);

        self.socket
            .send_to(&packet, peer_addr)
            .await
            .map_err(|e| AdapterError::Connection(format!("send failed: {}", e)))?;

        drop(builder);
        session.touch();
        Ok(())
    }

    // ── Capability announcements ──────────────────────────────────────
    //
    // `SUBPROTOCOL_CAPABILITY_ANN` payloads; the on-wire form is
    // `CapabilityAnnouncement::to_bytes`. Direct-peer push only in
    // v1; multi-hop gossip is a follow-up.

    /// Announce this node's capabilities to every directly-connected
    /// peer. Also self-indexes so single-node `find_nodes_by_filter`
    /// queries return us too.
    ///
    /// TTL defaults to 5 minutes. Unsigned (signatures tie in with
    /// Stage E channel auth). For explicit control over TTL or
    /// signing, see [`Self::announce_capabilities_with`].
    pub async fn announce_capabilities(&self, caps: CapabilitySet) -> Result<(), AdapterError> {
        // Default to signed — the node always has a keypair (either
        // caller-supplied or ephemeral at construction time), so
        // signing is free and closes the trust-on-first-use gap.
        self.announce_capabilities_with(caps, Duration::from_secs(300), true)
            .await
    }

    /// Extended announce with explicit TTL and signing opt-in.
    ///
    /// `sign = true` signs the announcement with the node's
    /// [`EntityKeypair`] so receivers can validate end-to-end.
    /// `sign = false` broadcasts unsigned — useful in trusted
    /// environments where the wire signature adds no value.
    /// Receivers with `require_signed_capabilities = true` drop
    /// unsigned announcements regardless.
    pub async fn announce_capabilities_with(
        &self,
        caps: CapabilitySet,
        ttl: Duration,
        sign: bool,
    ) -> Result<(), AdapterError> {
        let version = self.capability_version.fetch_add(1, Ordering::Relaxed) + 1;

        // Piggyback the current NAT classification as a `nat:*`
        // capability tag so peers can filter-match on NAT type,
        // and the reflex address on the dedicated announcement
        // field so peers have a direct-connect candidate without a
        // separate discovery round-trip. Both are feature-gated on
        // `nat-traversal` — callers compiled without the feature
        // emit announcements identical to the pre-traversal format.
        //
        // The two reads happen under `traversal_publish_mu` so
        // the announcement always carries a consistent (class,
        // reflex) pair. Without the mutex, a concurrent
        // set/clear/commit could interleave between our reads
        // and let us publish a torn state — e.g., the new
        // override's reflex paired with the pre-override NAT
        // class. The lock is held only for the two atomic reads,
        // not across the signing or network send.
        #[cfg(feature = "nat-traversal")]
        let (caps, reflex_snapshot) = {
            use super::traversal::classify::NatClass;
            let _g = self.traversal_publish_mu.lock();
            let class =
                NatClass::from_u8(self.nat_class.load(std::sync::atomic::Ordering::Acquire));
            let reflex = self.reflex_addr.load_full().map(|arc| *arc);
            // Strip any prior `nat:*` tags before adding the fresh
            // one so a reclassification doesn't leave a stale tag
            // behind when the class transitions.
            let mut next = caps;
            next.tags.retain(|t| !t.starts_with("nat:"));
            let next = next.add_tag(class.tag().to_string());
            (next, reflex)
        };

        let mut ann = CapabilityAnnouncement::new(
            self.node_id,
            self.identity.entity_id().clone(),
            version,
            caps,
        )
        .with_ttl(ttl.as_secs().min(u32::MAX as u64) as u32);
        #[cfg(feature = "nat-traversal")]
        {
            ann = ann.with_reflex_addr(reflex_snapshot);
        }
        if sign {
            ann.sign(&self.identity);
        }

        // Self-index so local queries see our own caps. Always runs
        // regardless of rate limit — the self-index reflects the
        // latest intended announcement.
        self.capability_index.index(ann.clone());

        // Publish as the latest local announcement so future
        // session-opens push this version to new peers. Also always
        // runs so late joiners get the latest caps even when we've
        // rate-limited away the broadcast.
        self.local_announcement.store(Some(Arc::new(ann.clone())));

        // Origin-side rate limit: within-window calls update the
        // self-index + `local_announcement` but skip the network
        // broadcast. Callers that want to force an immediate
        // re-broadcast should lower `min_announce_interval` on
        // `MeshNodeConfig`.
        let now = std::time::Instant::now();
        let min_interval = self.config.min_announce_interval;
        let should_broadcast = {
            let mut last = self.last_announce_at.lock();
            let elapsed = last.map(|t| now.saturating_duration_since(t));
            let can_send = elapsed.is_none_or(|e| e >= min_interval);
            if can_send {
                *last = Some(now);
            }
            can_send
        };
        if !should_broadcast {
            return Ok(());
        }

        // Fan out to currently-connected peers. Best-effort — a
        // per-peer send failure is logged and skipped rather than
        // short-circuiting the broadcast.
        let bytes = ann.to_bytes();
        let peer_addrs: Vec<SocketAddr> = self.peers.iter().map(|e| e.value().addr).collect();
        for addr in peer_addrs {
            if let Err(e) = self
                .send_subprotocol(addr, SUBPROTOCOL_CAPABILITY_ANN, &bytes)
                .await
            {
                tracing::trace!(peer = %addr, error = %e, "capability: announce send failed");
            }
        }
        Ok(())
    }

    /// Query the capability index. Returns node ids (including our
    /// own `node_id`) whose latest announcement matches `filter`.
    pub fn find_nodes_by_filter(&self, filter: &CapabilityFilter) -> Vec<u64> {
        self.capability_index.query(filter)
    }

    /// Scoped variant of [`Self::find_nodes_by_filter`]. Filters
    /// candidates through `scope` (derived from each peer's
    /// `scope:*` reserved tags) on top of the capability filter.
    /// `SubnetLocal` peers and the [`ScopeFilter::SameSubnet`]
    /// filter resolve same-subnet membership against
    /// `peer_subnets`.
    ///
    /// **Warm-up rule.** When a peer's subnet is unknown:
    /// - **With** a `local_subnet_policy`, the candidate is
    ///   admitted (a fresh peer's announcement may not have
    ///   landed yet — the policy will resolve it on receipt).
    /// - **Without** a `local_subnet_policy`, `peer_subnets`
    ///   stays permanently empty (the dispatch handler only
    ///   writes it when a policy is installed), so "unknown"
    ///   means "will never resolve" — admitting unknowns there
    ///   leaks every peer through `SameSubnet`. The candidate
    ///   is excluded.
    pub fn find_nodes_by_filter_scoped(
        &self,
        filter: &CapabilityFilter,
        scope: &ScopeFilter<'_>,
    ) -> Vec<u64> {
        let my_subnet = self.local_subnet;
        let peer_subnets = self.peer_subnets.clone();
        let local_node_id = self.node_id;
        // See doc-comment: without a policy, an unresolvable
        // "unknown" cannot be admitted as same-subnet.
        let policy_installed = self.local_subnet_policy.is_some();
        self.capability_index
            .find_nodes_scoped(filter, scope, |nid| {
                if nid == local_node_id {
                    // Querying our own node: same subnet by definition.
                    return true;
                }
                match peer_subnets.get(&nid).map(|e| *e.value()) {
                    Some(s) => s == my_subnet,
                    None => policy_installed,
                }
            })
    }

    /// Read a peer's most recently advertised public reflex
    /// `SocketAddr` from the capability index. `None` before the
    /// peer has sent a stage-2 announcement, or when the peer was
    /// compiled without `nat-traversal`.
    ///
    /// Stage 3 (rendezvous) reads this field to resolve the punch
    /// target's public address. Exposed for observability and for
    /// tests that want to verify capability-announcement propagation
    /// of the reflex field.
    ///
    /// Requires the `nat-traversal` cargo feature.
    #[cfg(feature = "nat-traversal")]
    pub fn peer_reflex_addr(&self, peer_node_id: u64) -> Option<std::net::SocketAddr> {
        self.capability_index.reflex_addr(peer_node_id)
    }

    /// Read a peer's most recently advertised NAT classification
    /// from the capability index. Parses the `nat:*` tag on the
    /// peer's announcement. Returns `NatClass::Unknown` when the
    /// peer has not indexed (we've never received an announcement),
    /// or the announcement carried no `nat:*` tag (peer was
    /// compiled without `nat-traversal`, or hasn't classified yet).
    ///
    /// Consumed by the pair-type matrix (plan §3) — `connect_direct`
    /// reads this to decide whether to attempt a punch or
    /// short-circuit to the routed path.
    ///
    /// Requires the `nat-traversal` cargo feature.
    #[cfg(feature = "nat-traversal")]
    pub fn peer_nat_class(&self, peer_node_id: u64) -> super::traversal::classify::NatClass {
        use super::traversal::classify::NatClass;
        let Some(caps) = self.capability_index.get(peer_node_id) else {
            return NatClass::Unknown;
        };
        for tag in caps.tags.iter() {
            if let Some(class) = NatClass::from_tag(tag) {
                return class;
            }
        }
        NatClass::Unknown
    }

    /// Cumulative traversal counters — punch attempts, successes,
    /// and relay fallbacks. Returns a consistent point-in-time
    /// snapshot.
    ///
    /// See [`super::traversal::TraversalStatsSnapshot`] for the
    /// field semantics. Counters are monotonic and never reset;
    /// callers that want deltas should subtract snapshots.
    ///
    /// Requires the `nat-traversal` cargo feature.
    #[cfg(feature = "nat-traversal")]
    pub fn traversal_stats(&self) -> super::traversal::TraversalStatsSnapshot {
        self.traversal_stats.snapshot()
    }

    /// Establish a direct session to `peer_node_id`, using the
    /// pair-type matrix (plan §3) to decide between a direct
    /// handshake, a rendezvous-coordinated single-shot punch, or
    /// a routed-only fallback.
    ///
    /// # Flow
    ///
    /// 1. Read local + remote NAT classifications (self
    ///    `nat_class()` + `peer_nat_class`). Unknown sides are
    ///    handled per the matrix — never treated as "don't attempt"
    ///    (plan decision 8).
    /// 2. Resolve the peer's reflex address from the local
    ///    capability index. Fails with
    ///    [`super::traversal::TraversalError::PeerNotReachable`] if no reflex is
    ///    cached (peer hasn't announced yet).
    /// 3. Apply the matrix:
    ///    - `Direct` → connect via the routing table's
    ///      first-hop; `coordinator` is not consulted and its
    ///      reachability is irrelevant. `relay_fallbacks`
    ///      increments (we didn't attempt a punch).
    ///    - `SkipPunch` → connect via `coordinator` as the
    ///      relay; symmetric pairs have no better option.
    ///      Fails with `PeerNotReachable` if `coordinator`
    ///      isn't a live peer.
    ///    - `SinglePunch` → ask `coordinator` to mediate via
    ///      [`Self::request_punch`]. On successful introduction,
    ///      increment `punches_attempted` + `punches_succeeded`
    ///      and connect to `peer_reflex`. On failure, increment
    ///      `punches_attempted` + `relay_fallbacks` and fall
    ///      back to connecting via the coordinator — the plan's
    ///      framing treats punch-failed as "optimization missed,"
    ///      not a connectivity failure.
    ///
    /// # Scope note
    ///
    /// Stage 3c wires the orchestration + stats end-to-end but
    /// always establishes the session via the routed handshake
    /// through `coordinator` — the framing "traffic rides the
    /// relay until a direct punch upgrades it" matches the plan's
    /// "optimization, not correctness" contract. Stage 3d lands
    /// the keep-alive train + `PunchAck` round-trip, at which
    /// point a successful `SinglePunch` outcome upgrades to a
    /// direct session; failed punches (or matrix-skipped pairs)
    /// continue to resolve on the routed path as they do today.
    ///
    /// Stats are set on the stage-3c semantics already:
    /// `punches_attempted` increments when the matrix picks
    /// `SinglePunch` and the coordinator mediates; stage 3d
    /// refines `punches_succeeded` / `relay_fallbacks` against
    /// the real keep-alive outcome.
    ///
    /// Requires the `nat-traversal` cargo feature.
    #[cfg(feature = "nat-traversal")]
    pub async fn connect_direct(
        &self,
        peer_node_id: u64,
        peer_pubkey: &[u8; 32],
        coordinator: u64,
    ) -> Result<u64, super::traversal::TraversalError> {
        use super::traversal::classify::{pair_action, PairAction};
        use super::traversal::TraversalError;

        // NOTE: `peer_reflex` and `coordinator` are deliberately
        // NOT resolved here. Two separate cubic P1 reviews
        // flagged that eager lookups + `PeerNotReachable` fast-
        // fails at the top broke branches that didn't actually
        // need those inputs — `Direct` doesn't need the
        // coordinator, and `SkipPunch` / `SinglePunch` don't need
        // the peer's reflex (SinglePunch gets it from the
        // coordinator's `PunchIntroduce`; SkipPunch rides the
        // coordinator relay and doesn't probe the peer directly).
        // Both lookups now happen lazily inside the arms that
        // actually consume them.

        let local_class = self.nat_class();
        let remote_class = self.peer_nat_class(peer_node_id);
        let action = pair_action(local_class, remote_class);

        // Resolve `coordinator` into a wire address. Only call
        // from the SkipPunch / SinglePunch arms — `Direct`
        // routes via the routing table (below).
        let coordinator_addr = || {
            self.peer_addrs
                .get(&coordinator)
                .map(|e| *e.value())
                .ok_or(TraversalError::PeerNotReachable)
        };

        // Helper: `true` iff we already have a session with
        // `peer_node_id` whose transport points at `want_addr`.
        // Used by the branches below to decide whether to
        // short-circuit (session is already on the path we want)
        // vs. re-handshake to upgrade (session exists but on the
        // wrong path — typically a relayed session that a fresh
        // direct/punched attempt should replace).
        // Unconditionally short-circuiting on any existing session
        // would leave callers stuck on the relay forever,
        // defeating the optimization.
        let session_matches = |want_addr: std::net::SocketAddr| {
            self.peers
                .get(&peer_node_id)
                .map(|e| e.value().addr == want_addr)
                .unwrap_or(false)
        };

        // Helper: open a session directly to `target_addr` by
        // wrapping msg1 in a routing header and sending straight
        // to the peer. Works for `Direct` (target_addr = peer_reflex)
        // and for post-punch upgrade (same). Uses the dispatch-
        // loop pending_handshakes path via `connect_via`, which
        // avoids recv-loop contention on a post-`start()` node.
        let connect_on_direct_path = |target_addr: std::net::SocketAddr| async move {
            if session_matches(target_addr) {
                return Ok(peer_node_id);
            }
            self.connect_via(target_addr, peer_pubkey, peer_node_id)
                .await
                .map_err(|e| TraversalError::Transport(e.to_string()))
        };

        // Helper: open a relayed session via `coord_addr`. Short-
        // circuits only when an existing session is already on
        // exactly that coordinator's path. An unrelated session
        // (stale, dead, on a different hop) is NOT treated as
        // success — a `contains_key`-based short-circuit here
        // would mask a failed direct attempt behind whatever
        // stale session happened to still be in the peers map,
        // so `connect_direct` would report "success" without
        // actually establishing the intended path. The handshake
        // runs unless we can confirm the existing session is
        // already the one this call was asked to resolve.
        let connect_via_coordinator = |coord_addr: std::net::SocketAddr| async move {
            if session_matches(coord_addr) {
                return Ok(peer_node_id);
            }
            self.connect_via(coord_addr, peer_pubkey, peer_node_id)
                .await
                .map_err(|e| TraversalError::Transport(e.to_string()))
        };

        match action {
            PairAction::Direct => {
                // `Direct` pairs (Open/Open, Open/Cone,
                // Open/Unknown, Unknown/Unknown, etc.) don't
                // need the coordinator — the peer is publicly
                // reachable at its advertised reflex. Send the
                // routed-handshake packet straight to
                // `peer_reflex`; the peer's dispatch loop sees
                // `dest == self` and completes locally. Only
                // when that direct attempt fails do we try the
                // routing table's first-hop as a fallback
                // (pingwave-installed routes, etc.). Always
                // going via the routing table would add an
                // unnecessary relay hop when a direct path is
                // available.
                //
                // Stats note: `record_relay_fallback` fires only
                // when we actually fall back to the routed path
                // — not on entry. A successful direct connect is
                // not a fallback; attributing it as one breaks
                // `TraversalStats.relay_fallbacks`'s documented
                // meaning ("ended up on the routed-
                // handshake path") and makes the counter useless
                // for assessing NAT-traversal effectiveness.
                // `Direct` is the one branch that genuinely
                // needs the peer's reflex — it's the wire target
                // for the direct-handshake attempt. Resolve it
                // lazily here (cubic P1): putting this lookup
                // at the top of the function used to reject
                // `SkipPunch` pairs (which don't need a reflex
                // at all) with `PeerNotReachable`.
                let peer_reflex = self
                    .peer_reflex_addr(peer_node_id)
                    .ok_or(TraversalError::PeerNotReachable)?;

                match connect_on_direct_path(peer_reflex).await {
                    Ok(id) => Ok(id),
                    Err(_) => {
                        // Direct handshake on `peer_reflex` failed.
                        // Run the routing-table fallback
                        // *unconditionally* — cubic P2 flagged
                        // that short-circuiting on "any session
                        // exists" would mask the failed direct
                        // attempt behind a stale / unrelated
                        // session, preventing the upgrade this
                        // API is meant to attempt. If
                        // `connect_routed` itself finds the
                        // existing session is already on a valid
                        // first-hop it'll succeed quickly; if no
                        // route is cached it returns an honest
                        // error the caller can observe.
                        //
                        // Stats ordering (cubic P2):
                        // `record_relay_fallback` only fires
                        // *after* `connect_routed` actually
                        // succeeds. Bumping it before the
                        // fallback runs would overcount — if
                        // the routing-table path also fails,
                        // the call returns Err and the counter
                        // would still have moved, breaking
                        // `relay_fallbacks`'s documented meaning
                        // ("resolutions that stayed on the
                        // routed path").
                        let id = self
                            .connect_routed(peer_pubkey, peer_node_id)
                            .await
                            .map_err(|e| TraversalError::Transport(e.to_string()))?;
                        self.traversal_stats.record_relay_fallback();
                        Ok(id)
                    }
                }
            }
            PairAction::SkipPunch => {
                // Symmetric × Symmetric (and Symmetric ×
                // Unknown). Punch can't land; the coordinator
                // is the only way to relay. Fail fast if the
                // caller's coordinator isn't reachable — there
                // is no viable fallback in this branch.
                //
                // Stats note (cubic P2): `record_relay_fallback`
                // runs only *after* `connect_via_coordinator`
                // actually succeeds. A failed coordinator
                // handshake returns Err without bumping, so
                // `relay_fallbacks` continues to mean "ended
                // up on the routed-handshake path" — not "the
                // matrix picked the routed path but the
                // handshake also failed."
                let coord = coordinator_addr()?;
                let id = connect_via_coordinator(coord).await?;
                self.traversal_stats.record_relay_fallback();
                Ok(id)
            }
            PairAction::SinglePunch => {
                // Punch requires the coordinator for rendezvous
                // mediation. Resolve it here (not eagerly at the
                // top) so Direct-path callers with no active
                // coordinator peer can still succeed.
                let coord = coordinator_addr()?;
                let self_reflex = self.reflex_addr().unwrap_or_else(|| self.local_addr());

                // Install the PunchAck waiter BEFORE firing the
                // request so a fast round-trip can't beat us to
                // the correlation map. Generation-stamped so
                // cleanup paths below only evict our own entry,
                // not a racing concurrent call.
                let (ack_tx, ack_rx) = oneshot::channel();
                let ack_gen = self
                    .next_waiter_gen
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.pending_punch_acks
                    .insert(peer_node_id, (ack_gen, ack_tx));

                let punch_outcome = self
                    .request_punch(coordinator, peer_node_id, self_reflex)
                    .await;

                // Stats ordering (cubic P2): `record_punch_attempt`
                // fires only when `request_punch` returns Ok —
                // i.e., the coordinator mediated the introduction,
                // which proves the send succeeded and the intro
                // arrived. Bumping before knowing the outcome
                // would overcount cases where no wire activity
                // actually happened (coordinator unreachable,
                // socket send failed). Similarly, the relay-
                // fallback counters below only fire after the
                // fallback handshake itself lands.
                let intro = match punch_outcome {
                    Ok(intro) => {
                        self.traversal_stats.record_punch_attempt();
                        intro
                    }
                    Err(_) => {
                        // Coordinator mediation failed — no
                        // point waiting for an ack that will
                        // never come. Evict the waiter only if
                        // it's still ours (a concurrent call may
                        // have replaced it; cubic P1). No
                        // `record_punch_attempt` — the wire
                        // punch didn't happen. The relay-
                        // fallback counter is bumped only after
                        // `connect_via_coordinator` actually
                        // succeeds.
                        self.pending_punch_acks
                            .remove_if(&peer_node_id, |_, (g, _)| *g == ack_gen);
                        let id = connect_via_coordinator(coord).await?;
                        self.traversal_stats.record_relay_fallback();
                        return Ok(id);
                    }
                };

                // Await the counterpart's PunchAck, forwarded by
                // the coordinator. On success we try a direct
                // handshake to the peer's advertised reflex —
                // that's the whole point of the punch, and
                // without it this code path is a fancy way of
                // doing a relayed connect while lying in stats
                // that a punch happened. On ack-timeout or
                // direct-handshake failure we fall back to relay
                // so the caller still gets a usable session.
                let deadline = self.traversal_config.punch_deadline;
                match tokio::time::timeout(deadline, ack_rx).await {
                    Ok(Ok(_ack)) => {
                        match connect_on_direct_path(intro.peer_reflex).await {
                            Ok(id) => {
                                self.traversal_stats.record_punch_success();
                                Ok(id)
                            }
                            Err(_) => {
                                // Punch opened but direct
                                // handshake failed (NAT rebound
                                // between ack and handshake, or
                                // the peer's socket buffer
                                // filled). Relay fallback — bump
                                // `relay_fallbacks` only after
                                // the coordinator handshake
                                // actually lands.
                                let id = connect_via_coordinator(coord).await?;
                                self.traversal_stats.record_relay_fallback();
                                Ok(id)
                            }
                        }
                    }
                    Ok(Err(_)) => {
                        // Our sender was replaced by a concurrent
                        // call — the map entry is now theirs, do
                        // NOT remove. Fall through to relay on
                        // our side.
                        let id = connect_via_coordinator(coord).await?;
                        self.traversal_stats.record_relay_fallback();
                        Ok(id)
                    }
                    Err(_) => {
                        self.pending_punch_acks
                            .remove_if(&peer_node_id, |_, (g, _)| *g == ack_gen);
                        let id = connect_via_coordinator(coord).await?;
                        self.traversal_stats.record_relay_fallback();
                        Ok(id)
                    }
                }
            }
        }
    }

    /// Rank peers for a scored requirement. Returns the best-
    /// scoring node's id, or `None` if no peer matches.
    pub fn find_best_node(&self, req: &CapabilityRequirement) -> Option<u64> {
        self.capability_index.find_best(req)
    }

    /// Scoped variant of [`Self::find_best_node`]. See
    /// [`Self::find_nodes_by_filter_scoped`] for the scope
    /// resolution semantics; selection picks the highest-scoring
    /// candidate within the scoped set.
    pub fn find_best_node_scoped(
        &self,
        req: &CapabilityRequirement,
        scope: &ScopeFilter<'_>,
    ) -> Option<u64> {
        let my_subnet = self.local_subnet;
        let peer_subnets = self.peer_subnets.clone();
        let local_node_id = self.node_id;
        // Same warm-up rule as `find_nodes_by_filter_scoped` —
        // see that doc-comment for the rationale.
        let policy_installed = self.local_subnet_policy.is_some();
        self.capability_index
            .find_best_node_scoped(req, scope, |nid| {
                if nid == local_node_id {
                    return true;
                }
                match peer_subnets.get(&nid).map(|e| *e.value()) {
                    Some(s) => s == my_subnet,
                    None => policy_installed,
                }
            })
    }

    /// Shared reference to the capability index. Use this for
    /// queries the two helpers above don't cover (listing all known
    /// peers, reading stats, etc.).
    pub fn capability_index(&self) -> &Arc<CapabilityIndex> {
        &self.capability_index
    }

    /// Push the currently-stored local announcement (if any) to
    /// `peer_addr`. Called from the end of `connect` / `accept` so
    /// late joiners don't have to wait for a re-announce. No-op
    /// when we haven't yet announced anything.
    async fn push_local_announcement(&self, peer_addr: SocketAddr) {
        let Some(ann) = self.local_announcement.load_full() else {
            return;
        };
        let bytes = ann.to_bytes();
        if let Err(e) = self
            .send_subprotocol(peer_addr, SUBPROTOCOL_CAPABILITY_ANN, &bytes)
            .await
        {
            tracing::trace!(
                peer = %peer_addr,
                error = %e,
                "capability: session-open push failed"
            );
        }
    }

    // ── Stream API ─────────────────────────────────────────────────────

    /// Open (or look up) a logical stream to a connected peer.
    ///
    /// A stream is one ordered, independently reliability-configured
    /// channel inside the encrypted session to `peer_node_id`. Multiple
    /// streams share one session, one cipher, and one UDP socket, but
    /// have independent sequence numbers and reliability state. See
    /// [`Stream`] for the full contract.
    ///
    /// **Idempotent:** repeated calls for the same `(peer_node_id,
    /// stream_id)` return handles backed by the same underlying state;
    /// a config argument that differs from the first call's is ignored
    /// with a warning log. Close + re-open to change a stream's config.
    pub fn open_stream(
        &self,
        peer_node_id: u64,
        stream_id: u64,
        config: StreamConfig,
    ) -> Result<Stream, AdapterError> {
        let peer = self.peers.get(&peer_node_id).ok_or_else(|| {
            AdapterError::Connection(format!(
                "open_stream: no session for peer {:#x}",
                peer_node_id
            ))
        })?;
        let reliable = config.reliability.is_reliable();
        // Capture the freshly-allocated (or existing, on idempotent
        // re-open) epoch so the returned `Stream` handle can later
        // reject stale sends after a close+reopen.
        let epoch = peer.session.open_stream_full(
            stream_id,
            reliable,
            config.fairness_weight,
            config.window_bytes,
        );
        // Propagate the weight to the router's fair scheduler so
        // forwarded traffic on this stream (e.g., multi-hop relays
        // where we're an intermediate) respects the weight too. v1
        // caveat: local outbound sends via `send_on_stream` bypass the
        // scheduler; the weight only becomes observable on the wire
        // when a packet with this stream_id transits *this* node as
        // a forwarder. Documented in STREAM_MULTIPLEXING_PLAN.md.
        self.router
            .scheduler()
            .set_stream_weight(stream_id, config.fairness_weight);
        // Opportunistic eviction: if this open just pushed us over the
        // cap, trim via the same path as close_stream (idle==0 means
        // only the cap-exceeded pass runs).
        if peer.session.stream_count() > self.config.max_streams {
            peer.session.evict_idle_streams(
                Duration::from_nanos(u64::MAX),
                self.config.max_streams,
                "cap_exceeded",
            );
        }
        Ok(Stream {
            peer_node_id,
            stream_id,
            epoch,
            config,
        })
    }

    /// Close a stream: drop its `StreamState` from the session, ending
    /// delivery of any buffered inbound events for the stream and
    /// dropping outbound packets that haven't hit the wire yet.
    /// Idempotent. `CloseBehavior::DrainThenClose` is honored only to
    /// the extent the router's scheduler has already flushed; there is
    /// no wire "drain-then-close" signal in v1.
    pub fn close_stream(&self, peer_node_id: u64, stream_id: u64) {
        if let Some(peer) = self.peers.get(&peer_node_id) {
            peer.session.close_stream(stream_id);
        }
    }

    /// Send a batch of events on an explicit stream.
    ///
    /// Uses the stream's reliability mode from its original `open_stream`
    /// config. Returns `Backpressure` when the stream's in-flight count
    /// (`tx_inflight`) would exceed its configured `tx_window`; the event
    /// was not enqueued — the caller decides what to do (drop, retry,
    /// or buffer at the app layer). `tx_window == 0` disables the check
    /// and preserves pre-backpressure behavior. `Transport` is returned
    /// for underlying socket send failures.
    ///
    /// Returns `NotConnected` when the stream was never opened or has
    /// been closed since (`close_stream`, idle eviction, cap-exceeded
    /// LRU). A previously-closed `Stream` handle is inert by design —
    /// reusing it does NOT silently re-create the stream with default
    /// config; the caller must explicitly re-open.
    pub async fn send_on_stream(
        &self,
        stream: &Stream,
        events: &[Bytes],
    ) -> Result<(), StreamError> {
        let peer = self
            .peers
            .get(&stream.peer_node_id)
            .ok_or(StreamError::NotConnected)?;
        let peer_addr = peer.addr;
        let session = peer.session.clone();
        drop(peer);

        if self.partition_filter.contains(&peer_addr) {
            return Ok(()); // matches send_to_peer's silent drop
        }

        let stream_id = stream.stream_id;
        let reliable = stream.config.reliability.is_reliable();

        // Refuse to send on a stream that isn't currently open, OR
        // whose live state has a different epoch than the handle. The
        // second case covers the subtle "close + reopen with the same
        // id" bug: the handle's epoch was captured at its original
        // open, but a reopen allocates a fresh `StreamState` with a
        // new epoch. A naive existence-only check would silently
        // reroute the send onto the new stream — wrong config, wrong
        // stats, wrong tx_window accounting.
        match session.try_stream(stream_id) {
            None => return Err(StreamError::NotConnected),
            Some(state) if state.epoch() != stream.epoch => {
                return Err(StreamError::NotConnected);
            }
            Some(_) => {}
        }

        let pool = session.thread_local_pool();
        let mut builder = pool.get();

        let mut current_batch: Vec<Bytes> = Vec::with_capacity(64);
        let mut current_size = 0usize;

        // Each socket send acquires byte credit from the stream's
        // `tx_credit_remaining`. On success we `commit()` the guard —
        // the bytes now belong to the receiver, which will refund via
        // `StreamWindow` grants once it drains them. On any failure
        // (socket error, cancellation, `close_stream` race) the guard
        // drops without commit and refunds the bytes — the bytes
        // never hit the wire, so pretending they did would strand
        // credit. `NotConnected` is surfaced when the stream
        // disappears mid-call.
        let flags = if reliable {
            PacketFlags::RELIABLE
        } else {
            PacketFlags::NONE
        };

        for event in events {
            let frame_size = EventFrame::LEN_SIZE + event.len();
            if current_size + frame_size > protocol::MAX_PAYLOAD_SIZE && !current_batch.is_empty() {
                // Charge the **wire size** (Net header + AEAD tag +
                // payload) rather than just the event-frame payload
                // so the byte window matches the bandwidth the sender
                // actually pumps onto the link. Both ends add the
                // same fixed per-packet overhead, so sender and
                // receiver accounting stay symmetric.
                //
                // `TxAdmit::Acquired` returns credit + sequence under
                // the same DashMap lookup — a close+reopen race can't
                // slip a stale sequence from the old lifetime onto
                // the new state.
                let needed = wire_bytes_for_payload(current_size);
                let (guard, seq) = match session.try_acquire_tx_credit_matching_epoch(
                    stream_id,
                    stream.epoch,
                    needed,
                ) {
                    TxAdmit::Acquired { guard, seq } => (guard, seq),
                    TxAdmit::WindowFull => return Err(StreamError::Backpressure),
                    TxAdmit::StreamClosed => return Err(StreamError::NotConnected),
                };
                let packet = builder.build(stream_id, seq, &current_batch, flags);
                let send_res = self.socket.send_to(&packet, peer_addr).await;
                send_res.map_err(|e| StreamError::Transport(format!("send failed: {}", e)))?;
                guard.commit(); // socket accepted the packet — bytes are the receiver's now
                current_batch.clear();
                current_size = 0;
            }
            current_batch.push(event.clone());
            current_size += frame_size;
        }

        if !current_batch.is_empty() {
            let needed = wire_bytes_for_payload(current_size);
            let (guard, seq) =
                match session.try_acquire_tx_credit_matching_epoch(stream_id, stream.epoch, needed)
                {
                    TxAdmit::Acquired { guard, seq } => (guard, seq),
                    TxAdmit::WindowFull => return Err(StreamError::Backpressure),
                    TxAdmit::StreamClosed => return Err(StreamError::NotConnected),
                };
            let packet = builder.build(stream_id, seq, &current_batch, flags);
            let send_res = self.socket.send_to(&packet, peer_addr).await;
            send_res.map_err(|e| StreamError::Transport(format!("send failed: {}", e)))?;
            guard.commit();
        }

        drop(builder);
        session.touch();
        Ok(())
    }

    /// Send `events` on `stream`, retrying on `Backpressure` with
    /// exponential backoff (5 ms → 200 ms, doubling) up to `max_retries`
    /// times. Transport failures are returned immediately — they're a
    /// real error, not a pressure signal, and retrying would just mask
    /// them. Returns the final `Backpressure` error if the stream stays
    /// saturated across every attempt.
    pub async fn send_with_retry(
        &self,
        stream: &Stream,
        events: &[Bytes],
        max_retries: usize,
    ) -> Result<(), StreamError> {
        let mut delay = Duration::from_millis(5);
        let cap = Duration::from_millis(200);
        let mut last_backpressure: Option<StreamError> = None;
        for _ in 0..max_retries.saturating_add(1) {
            match self.send_on_stream(stream, events).await {
                Ok(()) => return Ok(()),
                Err(StreamError::Backpressure) => {
                    last_backpressure = Some(StreamError::Backpressure);
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(cap);
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_backpressure.unwrap_or(StreamError::Backpressure))
    }

    /// Convenience wrapper around [`send_with_retry`](Self::send_with_retry)
    /// with a generous retry count. Blocks the calling task until the
    /// send succeeds or a transport error occurs. Use when you'd rather
    /// wait than drop; prefer `send_with_retry` if you need a concrete
    /// upper bound on retry attempts.
    pub async fn send_blocking(
        &self,
        stream: &Stream,
        events: &[Bytes],
    ) -> Result<(), StreamError> {
        // 4096 retries × 200 ms cap = ~13 minutes in the worst case,
        // effectively "block until the network lets up or something is
        // actually wrong." Callers that need a tighter bound should use
        // `send_with_retry` directly.
        self.send_with_retry(stream, events, 4096).await
    }

    /// Snapshot of per-stream stats for a single stream.
    ///
    /// Returns `None` if either the peer or the stream doesn't exist.
    pub fn stream_stats(&self, peer_node_id: u64, stream_id: u64) -> Option<StreamStats> {
        let peer = self.peers.get(&peer_node_id)?;
        let state = peer.session.get_stream(stream_id)?;
        Some(StreamStats {
            tx_seq: state.current_tx_seq(),
            rx_seq: state.current_rx_seq(),
            inbound_pending: state.inbound_len() as u64,
            last_activity_ns: state.last_activity_ns(),
            active: state.is_active(),
            backpressure_events: state.backpressure_events(),
            tx_credit_remaining: state.tx_credit_remaining(),
            tx_window: state.tx_window(),
            credit_grants_received: state.credit_grants_received(),
            credit_grants_sent: state.credit_grants_sent(),
        })
    }

    /// Snapshot of per-stream stats for every stream in the session to
    /// `peer_node_id`. Empty vec if the peer doesn't exist.
    pub fn all_stream_stats(&self, peer_node_id: u64) -> Vec<(u64, StreamStats)> {
        let peer = match self.peers.get(&peer_node_id) {
            Some(p) => p,
            None => return Vec::new(),
        };
        let session = peer.session.clone();
        drop(peer);
        session
            .stream_ids()
            .into_iter()
            .filter_map(|sid| {
                let state = session.get_stream(sid)?;
                Some((
                    sid,
                    StreamStats {
                        tx_seq: state.current_tx_seq(),
                        rx_seq: state.current_rx_seq(),
                        inbound_pending: state.inbound_len() as u64,
                        last_activity_ns: state.last_activity_ns(),
                        active: state.is_active(),
                        backpressure_events: state.backpressure_events(),
                        tx_credit_remaining: state.tx_credit_remaining(),
                        tx_window: state.tx_window(),
                        credit_grants_received: state.credit_grants_received(),
                        credit_grants_sent: state.credit_grants_sent(),
                    },
                ))
            })
            .collect()
    }

    /// Connect to a peer whose first hop on the wire is `relay_addr`.
    ///
    /// The handshake is an ordinary Net packet with the `HANDSHAKE` flag
    /// plus a routing header addressed to `dest_node_id`. The routing
    /// layer forwards it hop-by-hop (like any other packet); the
    /// responder's msg2 comes back the same way. There's no separate
    /// subprotocol, no per-hop re-encryption — Noise NKpsk0 provides
    /// end-to-end confidentiality and authenticity, and the prologue
    /// binds `(src_node_id, dest_node_id)` so a relay that rewrites
    /// either identity in the routing header fails the responder's MAC
    /// check on msg1.
    ///
    /// `start()` must have been called before `connect_via` — the
    /// receive loop has to be running to deliver msg2 back to us.
    pub async fn connect_via(
        &self,
        relay_addr: SocketAddr,
        dest_pubkey: &[u8; 32],
        dest_node_id: u64,
    ) -> Result<u64, AdapterError> {
        // Build msg1. Prologue uses *routing-identity* (32-bit) versions
        // of (self, dest) — that's what a malicious relay could see and
        // rewrite in the routing header, so binding those bits into the
        // Noise transcript catches tampering. The FULL u64 self.node_id
        // is carried inside the msg1 payload (Noise-AEAD-authenticated),
        // so the responder learns it after decryption and can address
        // msg2 back to the correct u64 identity.
        let pending_key = routing_id(dest_node_id);
        let prologue = handshake_prologue(routing_id(self.node_id), pending_key);
        let mut noise =
            NoiseHandshake::initiator_with_prologue(&self.config.psk, dest_pubkey, &prologue)
                .map_err(|e| AdapterError::Fatal(format!("handshake init failed: {}", e)))?;
        let msg1 = noise
            .write_message(&self.node_id.to_le_bytes())
            .map_err(|e| AdapterError::Connection(format!("write_message failed: {}", e)))?;

        // Register pending-initiator state so the dispatch loop can
        // complete the handshake when msg2 arrives. Keyed by the
        // 32-bit routing identity because msg2's routing header carries
        // the truncated src_id — that's the only key the dispatch loop
        // has when it tries to find the matching initiator.
        let (tx, rx) = oneshot::channel();
        match self.pending_handshakes.entry(pending_key) {
            dashmap::mapref::entry::Entry::Occupied(_) => {
                return Err(AdapterError::Connection(format!(
                    "connect_via: handshake already in flight for peer {:#x}",
                    dest_node_id
                )));
            }
            dashmap::mapref::entry::Entry::Vacant(v) => {
                v.insert(PendingHandshake { noise, tx });
            }
        }

        // Wrap msg1 in a Net handshake packet + routing header and
        // send to the first hop. No session encryption — the handshake
        // payload is the raw Noise bytes, authenticated/confidential
        // by Noise itself.
        let inner = {
            let mut builder = PacketBuilder::new(&[0u8; 32], 0);
            builder.build_handshake(&msg1)
        };
        let routing = RoutingHeader::new(dest_node_id, self.node_id as u32, DEFAULT_HANDSHAKE_TTL);
        let mut routed = bytes::BytesMut::with_capacity(ROUTING_HEADER_SIZE + inner.len());
        routed.extend_from_slice(&routing.to_bytes());
        routed.extend_from_slice(&inner);
        if let Err(e) = self.socket.send_to(&routed, relay_addr).await {
            self.pending_handshakes.remove(&pending_key);
            return Err(AdapterError::Connection(format!("send failed: {}", e)));
        }

        // Wait for the dispatch loop to complete msg2.
        let keys = match tokio::time::timeout(self.config.handshake_timeout, rx).await {
            Ok(Ok(Ok(k))) => k,
            Ok(Ok(Err(e))) => {
                self.pending_handshakes.remove(&pending_key);
                return Err(AdapterError::Fatal(format!("handshake failed: {}", e)));
            }
            Ok(Err(_)) => {
                self.pending_handshakes.remove(&pending_key);
                return Err(AdapterError::Connection("handshake channel dropped".into()));
            }
            Err(_) => {
                self.pending_handshakes.remove(&pending_key);
                return Err(AdapterError::Connection("handshake timeout".into()));
            }
        };

        // Register the new peer with `relay_addr` as the wire address.
        // Packets to `dest_node_id` go to the relay first; the routing
        // table does the rest. `addr_to_node` is deliberately NOT
        // updated — `relay_addr` still maps to the relay's own node_id
        // for direct-packet dispatch.
        let remote_static_pub = keys.remote_static_pub;
        let session = Arc::new(NetSession::new(
            keys,
            relay_addr,
            self.config.packet_pool_size,
            self.config.default_reliable,
        ));
        self.router.add_route(dest_node_id, relay_addr);
        self.peers.insert(
            dest_node_id,
            PeerInfo {
                node_id: dest_node_id,
                addr: relay_addr,
                session,
                remote_static_pub,
            },
        );
        self.peer_addrs.insert(dest_node_id, relay_addr);

        Ok(dest_node_id)
    }

    /// Connect to a peer by node id, using the routing table to pick the
    /// first hop. Fails with `Connection("no route to ...")` if the
    /// routing table doesn't have a route to the destination yet — in
    /// which case the caller can retry once pingwaves have propagated.
    pub async fn connect_routed(
        &self,
        dest_pubkey: &[u8; 32],
        dest_node_id: u64,
    ) -> Result<u64, AdapterError> {
        let first_hop = self
            .router
            .routing_table()
            .lookup(dest_node_id)
            .ok_or_else(|| {
                AdapterError::Connection(format!(
                    "connect_routed: no route to peer {:#x}",
                    dest_node_id
                ))
            })?;
        self.connect_via(first_hop, dest_pubkey, dest_node_id).await
    }

    // ── Handshake helpers ───────────────────────────────────────────────

    async fn handshake_initiator(
        &self,
        peer_addr: SocketAddr,
        peer_pubkey: &[u8; 32],
        peer_node_id: u64,
    ) -> Result<SessionKeys, AdapterError> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self
                .try_handshake_initiator(peer_addr, peer_pubkey, peer_node_id)
                .await
            {
                Ok(keys) => return Ok(keys),
                Err(e) if attempt < self.config.handshake_retries => {
                    tracing::warn!(attempt, error = %e, "mesh handshake failed, retrying");
                    tokio::time::sleep(Duration::from_millis(100 * attempt as u64)).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn try_handshake_initiator(
        &self,
        peer_addr: SocketAddr,
        peer_pubkey: &[u8; 32],
        peer_node_id: u64,
    ) -> Result<SessionKeys, AdapterError> {
        let timeout = self.config.handshake_timeout;

        // Prologue uses the 32-bit `routing_id` projection of the node
        // ids — the same projection routed handshakes use, so the two
        // paths share one prologue convention. Direct handshakes don't
        // traverse the routing plane, but the unified convention
        // simplifies reasoning and future code reuse.
        let prologue = handshake_prologue(routing_id(self.node_id), routing_id(peer_node_id));
        let mut handshake =
            NoiseHandshake::initiator_with_prologue(&self.config.psk, peer_pubkey, &prologue)
                .map_err(|e| AdapterError::Fatal(format!("handshake init failed: {}", e)))?;

        let msg1 = handshake
            .write_message(&[])
            .map_err(|e| AdapterError::Connection(format!("write_message failed: {}", e)))?;

        let mut builder = PacketBuilder::new(&[0u8; 32], 0);
        let packet = builder.build_handshake(&msg1);

        // Polling `socket_arc.recv_from` directly would race
        // `spawn_receive_loop`'s consumer post-`start()` (tokio
        // dispatches a UDP datagram to exactly one waiter), so:
        //   - Pre-`start()`: use `recv_from`; the dispatcher isn't
        //     running, so there's no race. This preserves the
        //     existing init-time ordering where `connect()` is
        //     called before `start()`.
        //   - Post-`start()`: register an oneshot in
        //     `pending_direct_initiators`, then send msg1, then
        //     await the oneshot. The dispatcher's direct-handshake
        //     branch forwards the parsed payload bytes through.
        // Concurrent direct connects on the same node also work
        // — each registers under its own peer_addr.
        let payload_bytes = if self.started.load(Ordering::Acquire) {
            let (tx, rx) = oneshot::channel::<Bytes>();
            // Register BEFORE sending msg1 so we can't miss a
            // fast responder that replies before we'd otherwise
            // be ready to receive. `insert` replaces any prior
            // entry for the same `peer_addr` — last writer wins.
            self.pending_direct_initiators.insert(peer_addr, tx);

            if let Err(e) = self.socket.send_to(&packet, peer_addr).await {
                self.pending_direct_initiators.remove(&peer_addr);
                return Err(AdapterError::Connection(format!("send failed: {}", e)));
            }

            match tokio::time::timeout(timeout, rx).await {
                Ok(Ok(payload)) => payload,
                Ok(Err(_)) => {
                    // Sender dropped — the dispatcher removed our
                    // entry without forwarding. Should not happen
                    // unless start() shut down; treat as timeout.
                    self.pending_direct_initiators.remove(&peer_addr);
                    return Err(AdapterError::Connection("handshake channel dropped".into()));
                }
                Err(_) => {
                    // Timeout — the responder never replied or its
                    // reply arrived for a different source. Clean up.
                    self.pending_direct_initiators.remove(&peer_addr);
                    return Err(AdapterError::Connection("handshake timeout".into()));
                }
            }
        } else {
            // Pre-start fallback: dispatcher is not running, so
            // there's nothing to forward through the registry.
            // Poll the socket directly — no race exists yet.
            let socket_arc = self.socket.socket_arc();
            self.socket
                .send_to(&packet, peer_addr)
                .await
                .map_err(|e| AdapterError::Connection(format!("send failed: {}", e)))?;

            let parsed = tokio::time::timeout(timeout, async {
                loop {
                    let mut recv_buf = bytes::BytesMut::with_capacity(protocol::MAX_PACKET_SIZE);
                    recv_buf.resize(protocol::MAX_PACKET_SIZE, 0);

                    let (n, source) = socket_arc
                        .recv_from(&mut recv_buf)
                        .await
                        .map_err(|e| AdapterError::Connection(format!("recv failed: {}", e)))?;

                    if source != peer_addr {
                        continue;
                    }

                    recv_buf.truncate(n);
                    let data = recv_buf.freeze();

                    if let Some(p) = ParsedPacket::parse(data, source) {
                        if p.header.flags.is_handshake() {
                            return Ok::<_, AdapterError>(p);
                        }
                    }
                }
            })
            .await
            .map_err(|_| AdapterError::Connection("handshake timeout".into()))??;
            parsed.payload
        };

        handshake
            .read_message(&payload_bytes)
            .map_err(|e| AdapterError::Connection(format!("read_message failed: {}", e)))?;

        handshake
            .into_session_keys()
            .map_err(|e| AdapterError::Fatal(format!("key extraction failed: {}", e)))
    }

    async fn handshake_responder(
        &self,
        peer_node_id: u64,
    ) -> Result<(SessionKeys, SocketAddr), AdapterError> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self.try_handshake_responder(peer_node_id).await {
                Ok(result) => return Ok(result),
                Err(e) if attempt < self.config.handshake_retries => {
                    tracing::warn!(attempt, error = %e, "mesh accept failed, retrying");
                    tokio::time::sleep(Duration::from_millis(100 * attempt as u64)).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn try_handshake_responder(
        &self,
        peer_node_id: u64,
    ) -> Result<(SessionKeys, SocketAddr), AdapterError> {
        let timeout = self.config.handshake_timeout;
        let socket_arc = self.socket.socket_arc();

        // Wait for initiator's handshake
        let (parsed, source) = tokio::time::timeout(timeout, async {
            loop {
                let mut recv_buf = bytes::BytesMut::with_capacity(protocol::MAX_PACKET_SIZE);
                recv_buf.resize(protocol::MAX_PACKET_SIZE, 0);

                let (n, source) = socket_arc
                    .recv_from(&mut recv_buf)
                    .await
                    .map_err(|e| AdapterError::Connection(format!("recv failed: {}", e)))?;

                recv_buf.truncate(n);
                let data = recv_buf.freeze();

                if let Some(p) = ParsedPacket::parse(data, source) {
                    if p.header.flags.is_handshake() {
                        return Ok::<_, AdapterError>((p, source));
                    }
                }
            }
        })
        .await
        .map_err(|_| AdapterError::Connection("handshake timeout".into()))??;

        // Direct responder: mirror the initiator's `routing_id`-based
        // prologue so direct and routed share one convention.
        let prologue = handshake_prologue(routing_id(peer_node_id), routing_id(self.node_id));
        let mut handshake = NoiseHandshake::responder_with_prologue(
            &self.config.psk,
            &self.static_keypair,
            &prologue,
        )
        .map_err(|e| AdapterError::Fatal(format!("handshake init failed: {}", e)))?;

        handshake
            .read_message(&parsed.payload)
            .map_err(|e| AdapterError::Connection(format!("read_message failed: {}", e)))?;

        let msg2 = handshake
            .write_message(&[])
            .map_err(|e| AdapterError::Connection(format!("write_message failed: {}", e)))?;

        let mut builder = PacketBuilder::new(&[0u8; 32], 0);
        let packet = builder.build_handshake(&msg2);

        self.socket
            .send_to(&packet, source)
            .await
            .map_err(|e| AdapterError::Connection(format!("send failed: {}", e)))?;

        let keys = handshake
            .into_session_keys()
            .map_err(|e| AdapterError::Fatal(format!("key extraction failed: {}", e)))?;

        Ok((keys, source))
    }

    // ── NAT traversal ──────────────────────────────────────────────────
    //
    // `SUBPROTOCOL_REFLEX` client. The handler half lives in the
    // packet-dispatch loop (`process_local_packet`, in the
    // `SUBPROTOCOL_REFLEX` branch) and echoes the observed UDP
    // source back as a `ReflexResponse`. This method is the
    // requester side: send an empty-body request, await the
    // pending-oneshot, return the observed `SocketAddr`.
    //
    // Remember: reflex discovery is an optimization, not a
    // connectivity guarantee. A `ReflexTimeout` or `PeerNotReachable`
    // doesn't mean the peers can't talk; it means this specific
    // address-discovery path didn't resolve.

    /// Send one reflex probe to `peer_node_id` and return the
    /// public `SocketAddr` the peer observed on the probe's UDP
    /// envelope.
    ///
    /// Waits up to [`super::traversal::TraversalConfig::reflex_timeout`] (default
    /// 3 s) for the response. Fails with [`super::traversal::TraversalError::ReflexTimeout`]
    /// on timeout, [`super::traversal::TraversalError::PeerNotReachable`] if the peer
    /// has no active session, or [`super::traversal::TraversalError::Transport`] on
    /// a socket-level send failure.
    ///
    /// Requires the `nat-traversal` cargo feature.
    #[cfg(feature = "nat-traversal")]
    pub async fn probe_reflex(
        &self,
        peer_node_id: u64,
    ) -> Result<std::net::SocketAddr, super::traversal::TraversalError> {
        use super::traversal::{reflex, TraversalError};

        let peer_addr = self
            .peer_addrs
            .get(&peer_node_id)
            .map(|e| *e.value())
            .ok_or(TraversalError::PeerNotReachable)?;

        // Install the pending-oneshot BEFORE sending so an
        // improbably-fast response can still complete it. A
        // previously-in-flight probe to the same peer is
        // overwritten — its waiter will hit ReflexTimeout, which
        // matches the "one probe per peer in flight" contract.
        //
        // Stamp each waiter with a unique generation so our
        // timeout cleanup only evicts *our* entry, not a racing
        // replacement. See `next_waiter_gen` doc for the race.
        let (tx, rx) = oneshot::channel();
        let gen = self
            .next_waiter_gen
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.pending_reflex_probes.insert(peer_node_id, (gen, tx));

        // Empty-body event frame on the reflex subprotocol.
        let body = reflex::encode_request();
        if let Err(e) = self
            .send_subprotocol(peer_addr, super::traversal::SUBPROTOCOL_REFLEX, &body)
            .await
        {
            self.pending_reflex_probes
                .remove_if(&peer_node_id, |_, (g, _)| *g == gen);
            return Err(TraversalError::Transport(e.to_string()));
        }

        let timeout = self.traversal_config.reflex_timeout;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(addr)) => Ok(addr),
            Ok(Err(_recv_err)) => {
                // oneshot cancelled — treat as timeout. Only
                // happens if a concurrent probe to the same peer
                // replaced our sender; the replacement owns the
                // map entry now and we must NOT touch it.
                Err(TraversalError::ReflexTimeout)
            }
            Err(_elapsed) => {
                self.pending_reflex_probes
                    .remove_if(&peer_node_id, |_, (g, _)| *g == gen);
                Err(TraversalError::ReflexTimeout)
            }
        }
    }

    /// The current NAT classification for this node. `Unknown`
    /// until the classification sweep has run; updated atomically
    /// by the sweep and by [`Self::reclassify_nat`]. Read-only for
    /// external callers — the sweep is the only writer.
    ///
    /// Requires the `nat-traversal` cargo feature.
    #[cfg(feature = "nat-traversal")]
    pub fn nat_class(&self) -> super::traversal::classify::NatClass {
        super::traversal::classify::NatClass::from_u8(
            self.nat_class.load(std::sync::atomic::Ordering::Acquire),
        )
    }

    /// This node's public-facing `SocketAddr` as observed by a
    /// remote peer during the classification sweep. `None` before
    /// the first sweep has produced an observation. Exposed
    /// primarily for tests + observability; the announce-
    /// capabilities path piggybacks this value onto every signed
    /// `CapabilityAnnouncement`.
    ///
    /// Requires the `nat-traversal` cargo feature.
    #[cfg(feature = "nat-traversal")]
    pub fn reflex_addr(&self) -> Option<std::net::SocketAddr> {
        self.reflex_addr.load_full().map(|arc| *arc)
    }

    /// Install a runtime reflex override. Forces `nat_class =
    /// Open` and `reflex_addr = Some(external)` immediately, and
    /// short-circuits any further classifier sweeps until
    /// [`Self::clear_reflex_override`] is called.
    ///
    /// # Publishing to peers
    ///
    /// This method updates only *local* state. To propagate the
    /// change to peers, call [`Self::announce_capabilities`]
    /// afterward. The setter resets the announce rate-limit
    /// floor so the next announce is guaranteed to broadcast
    /// rather than coalesce against the previous send — cubic
    /// P2 pinned this, after flagging that callers who set an
    /// override within `min_announce_interval` of a prior
    /// announce would find peers still seeing the old reflex.
    ///
    /// **Optimization, not correctness.** A node with no override
    /// still reaches every peer through the routed-handshake
    /// path; the override just pins the publicly-advertised
    /// address when it's already known (port-forwarded server, a
    /// successful stage-4 port-mapping install, etc).
    ///
    /// Safe to call concurrently with `announce_capabilities` —
    /// the triple-write runs under `traversal_publish_mu`
    /// (alongside the announce's multi-field read), so a
    /// concurrent announce either sees the pre-override state
    /// or the fully-installed override, never a torn mix.
    ///
    /// Requires the `nat-traversal` cargo feature.
    #[cfg(feature = "nat-traversal")]
    pub fn set_reflex_override(&self, external: SocketAddr) {
        use std::sync::atomic::Ordering;
        // Hold the publication mutex across the triple-write.
        // Without it, a concurrent `announce_capabilities_with`
        // could interleave its two reads between our three
        // writes and publish a torn state (e.g. the new reflex
        // paired with the pre-override NAT class). The mutex is
        // briefly contended only with other override writers or
        // the announce multi-field read; single-field readers
        // stay lock-free.
        let _g = self.traversal_publish_mu.lock();
        self.reflex_addr.store(Some(Arc::new(external)));
        self.nat_class.store(
            super::traversal::classify::NatClass::Open.as_u8(),
            Ordering::Release,
        );
        self.reflex_override_active.store(true, Ordering::Release);

        // Reset the rate-limit floor so the next
        // `announce_capabilities` call is guaranteed to
        // broadcast — cubic P2 flagged that the override
        // setter's doc implies immediate peer visibility, but
        // the rate limit could coalesce an announce that lands
        // inside `min_announce_interval`. Callers that want
        // peers to see the new reflex "right away" still need
        // to call announce themselves; this just makes that
        // call's broadcast step unconditional instead of
        // coalesced.
        *self.last_announce_at.lock() = None;
    }

    /// Drop a previously-installed runtime reflex override. The
    /// classifier sweep resumes on its normal cadence; the next
    /// sweep repopulates `reflex_addr` and `nat_class` from real
    /// probe observations. `reflex_addr` is cleared to `None`
    /// immediately so a between-sweep read doesn't return a stale
    /// override value as "still current."
    ///
    /// # Publishing to peers
    ///
    /// Mirrors [`Self::set_reflex_override`]: only local state
    /// changes here. Call [`Self::announce_capabilities`] after
    /// this to tell peers. The rate-limit floor is reset so that
    /// call broadcasts unconditionally.
    ///
    /// No-op when no override is active — safe to call
    /// unconditionally during shutdown / port-mapping revoke
    /// paths.
    ///
    /// Requires the `nat-traversal` cargo feature.
    #[cfg(feature = "nat-traversal")]
    pub fn clear_reflex_override(&self) {
        use std::sync::atomic::Ordering;
        // Hold the publication mutex across the flag flip + the
        // field resets so a concurrent announce can't observe
        // the cleared flag alongside the not-yet-reset reflex /
        // NAT class (or vice versa). Same invariant as in
        // `set_reflex_override`.
        let _g = self.traversal_publish_mu.lock();
        if !self.reflex_override_active.swap(false, Ordering::AcqRel) {
            return;
        }
        // Reset reflex_addr + nat_class to their pre-classification
        // defaults so a caller reading immediately gets an honest
        // "no current observation" answer rather than the stale
        // override.
        self.reflex_addr.store(None);
        self.nat_class.store(
            super::traversal::classify::NatClass::Unknown.as_u8(),
            Ordering::Release,
        );
        // Same rate-limit reset as `set_reflex_override` — the
        // next `announce_capabilities` call broadcasts
        // unconditionally instead of coalescing against the
        // previous send. See that method's comment for details
        // (cubic P2).
        *self.last_announce_at.lock() = None;
    }

    /// Testing / debugging hook: force this node's advertised
    /// `NatClass` without running the probe sweep. On loopback
    /// every node classifies as `Open`, which means the pair-type
    /// matrix always picks `Direct` — useful for Open×Open cases
    /// but leaves the `SinglePunch` path unexercised. Regression
    /// tests that need to drive `connect_direct` through the
    /// punch branch set this to `Cone` (or `Symmetric`) before
    /// calling `announce_capabilities`, then the peer reads it
    /// back via `peer_nat_class` and the matrix resolves to
    /// `SinglePunch`.
    ///
    /// Not for production use: the classifier is the intended
    /// writer, and forcing a class short-circuits real NAT
    /// observation. Exposed as `pub` (not `pub(crate)`) only so
    /// `tests/connect_direct.rs` can reach it from a separate
    /// crate.
    ///
    /// Requires the `nat-traversal` cargo feature.
    #[cfg(feature = "nat-traversal")]
    #[doc(hidden)]
    pub fn force_nat_class_for_test(&self, class: super::traversal::classify::NatClass) {
        self.nat_class.store(class.as_u8(), Ordering::Release);
    }

    /// Test / debug accessor for the most-recent local
    /// capability announcement. Returns `None` until the first
    /// `announce_capabilities*` call. The stored value is the
    /// coherent snapshot published by
    /// [`Self::announce_capabilities_with`] under
    /// `traversal_publish_mu` — regression tests for the
    /// (class, reflex_addr) race read this instead of the
    /// separate atomic accessors, which are lock-free and can
    /// return torn pairs under concurrent mutation.
    #[doc(hidden)]
    pub fn local_announcement_for_test(&self) -> Option<Arc<CapabilityAnnouncement>> {
        self.local_announcement.load_full()
    }

    /// Send a `PunchRequest` to a coordinator peer `relay`, asking
    /// it to mediate a hole-punch to `target`. Returns the
    /// `PunchIntroduce` produced by the coordinator (the one
    /// arriving on this node's side of the introduction — carrying
    /// `target`'s reflex and the shared `fire_at`).
    ///
    /// This is a stage-3b primitive: it exercises the coordinator
    /// fan-out end-to-end but does not itself schedule the
    /// keep-alive train or finalize the punched session. The
    /// full `connect_direct` flow lands in stage 3c.
    ///
    /// Fails with:
    /// - [`super::traversal::TraversalError::PeerNotReachable`] if `relay` has no
    ///   active session.
    /// - [`super::traversal::TraversalError::Transport`] on a socket-level send
    ///   failure.
    /// - [`super::traversal::TraversalError::PunchFailed`] if the coordinator
    ///   doesn't introduce within [`super::traversal::TraversalConfig::punch_deadline`]
    ///   (likely: R has no cached reflex for `target`).
    ///
    /// Requires the `nat-traversal` cargo feature.
    #[cfg(feature = "nat-traversal")]
    pub async fn request_punch(
        &self,
        relay: u64,
        target: u64,
        self_reflex: std::net::SocketAddr,
    ) -> Result<super::traversal::rendezvous::PunchIntroduce, super::traversal::TraversalError>
    {
        use super::traversal::rendezvous::{PunchRequest, RendezvousMsg};
        use super::traversal::TraversalError;

        let relay_addr = self
            .peer_addrs
            .get(&relay)
            .map(|e| *e.value())
            .ok_or(TraversalError::PeerNotReachable)?;

        // Install the waiter BEFORE sending. An improbably fast
        // coordinator response (R is local, A is local) could
        // otherwise arrive before the oneshot is registered.
        // Keyed by `target` because the introduce we'll receive
        // has `peer = target` in its body.
        //
        // Generation-stamped so our cleanup only evicts our own
        // entry — a concurrent `request_punch` to the same
        // target installs a new entry (and drops our sender),
        // and that replacement must survive our timeout/send-
        // failure remove.
        let (tx, rx) = oneshot::channel();
        let gen = self
            .next_waiter_gen
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.pending_punch_introduces.insert(target, (gen, tx));

        let body = RendezvousMsg::PunchRequest(PunchRequest {
            target,
            self_reflex,
        })
        .encode();
        if let Err(e) = self
            .send_subprotocol(relay_addr, super::traversal::SUBPROTOCOL_RENDEZVOUS, &body)
            .await
        {
            self.pending_punch_introduces
                .remove_if(&target, |_, (g, _)| *g == gen);
            return Err(TraversalError::Transport(e.to_string()));
        }

        let deadline = self.traversal_config.punch_deadline;
        match tokio::time::timeout(deadline, rx).await {
            Ok(Ok(intro)) => Ok(intro),
            Ok(Err(_recv_err)) => {
                // oneshot cancelled — another request_punch to the
                // same target replaced our sender. Don't touch the
                // map: the replacement owns the entry now.
                Err(TraversalError::PunchFailed)
            }
            Err(_elapsed) => {
                self.pending_punch_introduces
                    .remove_if(&target, |_, (g, _)| *g == gen);
                Err(TraversalError::PunchFailed)
            }
        }
    }

    /// Install a waiter for an incoming `PunchIntroduce` from
    /// `counterpart`. The returned future resolves when the
    /// dispatcher decodes a matching introduce, or with
    /// [`super::traversal::TraversalError::PunchFailed`] after
    /// [`super::traversal::TraversalConfig::punch_deadline`].
    ///
    /// Stage-3b responder-side primitive: the peer being punched
    /// *into* uses this to observe the introduce without
    /// initiating the flow itself. Stage 3c wires the keep-alive
    /// train onto the returned introduce.
    ///
    /// Requires the `nat-traversal` cargo feature.
    #[cfg(feature = "nat-traversal")]
    pub async fn await_punch_introduce(
        &self,
        counterpart: u64,
    ) -> Result<super::traversal::rendezvous::PunchIntroduce, super::traversal::TraversalError>
    {
        use super::traversal::TraversalError;
        let (tx, rx) = oneshot::channel();
        let gen = self
            .next_waiter_gen
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.pending_punch_introduces.insert(counterpart, (gen, tx));
        let deadline = self.traversal_config.punch_deadline;
        match tokio::time::timeout(deadline, rx).await {
            Ok(Ok(intro)) => Ok(intro),
            Ok(Err(_)) => Err(TraversalError::PunchFailed),
            Err(_) => {
                self.pending_punch_introduces
                    .remove_if(&counterpart, |_, (g, _)| *g == gen);
                Err(TraversalError::PunchFailed)
            }
        }
    }

    /// Install a waiter for an incoming `PunchAck` whose
    /// `from_peer` matches `counterpart`. Stage-3d correlation
    /// surface — the `SinglePunch` path in `connect_direct`
    /// registers the waiter before firing `request_punch` and
    /// awaits it afterward. Times out with
    /// [`super::traversal::TraversalError::PunchFailed`] after
    /// [`super::traversal::TraversalConfig::punch_deadline`].
    ///
    /// Note: the caller inserts the oneshot sender into
    /// `pending_punch_acks` before issuing the request so the ack
    /// can't arrive and be dropped before the await call is
    /// entered.
    ///
    /// Requires the `nat-traversal` cargo feature.
    #[cfg(feature = "nat-traversal")]
    pub async fn await_punch_ack(
        &self,
        counterpart: u64,
    ) -> Result<super::traversal::rendezvous::PunchAck, super::traversal::TraversalError> {
        use super::traversal::TraversalError;
        let (tx, rx) = oneshot::channel();
        let gen = self
            .next_waiter_gen
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.pending_punch_acks.insert(counterpart, (gen, tx));
        let deadline = self.traversal_config.punch_deadline;
        match tokio::time::timeout(deadline, rx).await {
            Ok(Ok(ack)) => Ok(ack),
            Ok(Err(_)) => Err(TraversalError::PunchFailed),
            Err(_) => {
                self.pending_punch_acks
                    .remove_if(&counterpart, |_, (g, _)| *g == gen);
                Err(TraversalError::PunchFailed)
            }
        }
    }

    /// Fire the classification sweep. Picks up to two currently-
    /// connected peers, runs [`Self::probe_reflex`] against each in
    /// parallel, feeds the observations to
    /// [`super::traversal::classify::ClassifyFsm`], and updates
    /// `nat_class` + `reflex_addr` with the result.
    ///
    /// Runs at most one sweep at a time — a second call while a
    /// sweep is in flight is a no-op. Exits early if fewer than 2
    /// peers are currently connected; callers should check
    /// [`Self::nat_class`] after the returned future completes to
    /// see whether classification produced a definite verdict or
    /// stayed at `Unknown`.
    ///
    /// Bounded by [`super::traversal::TraversalConfig::classify_deadline`] — even if
    /// probes hang, the sweep returns within that window with
    /// whatever observations arrived.
    ///
    /// Requires the `nat-traversal` cargo feature.
    #[cfg(feature = "nat-traversal")]
    pub async fn reclassify_nat(&self) {
        use super::traversal::classify::ClassifyFsm;

        // Reflex-override short-circuit: an operator-set (or
        // port-mapping installed) external address already tells
        // us everything the classifier would: NAT type is Open,
        // reflex is the override. Running the multi-peer probe
        // sweep would only replace the overridden values with
        // (possibly worse) observations — skip it.
        if self
            .reflex_override_active
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return;
        }

        // Snapshot up to two peers. Two is enough to distinguish
        // Cone vs. Symmetric (plan §2); sampling more adds probe
        // traffic for no classification gain.
        let peers: Vec<u64> = self.peers.iter().map(|e| *e.key()).take(2).collect();
        if peers.len() < 2 {
            return;
        }

        // Fire probes in parallel so the sweep finishes in one
        // reflex_timeout window rather than N*timeout. Each probe
        // already respects its own timeout via `probe_reflex`.
        let bind = self.local_addr();
        let futures = peers.iter().copied().map(|peer| async move {
            let res = self.probe_reflex(peer).await;
            (peer, res)
        });
        let deadline = self.traversal_config.classify_deadline;
        let results = match tokio::time::timeout(deadline, futures::future::join_all(futures)).await
        {
            Ok(results) => results,
            Err(_elapsed) => {
                // Classification-deadline blown. Leave current
                // classification untouched — a later sweep can
                // retry; treating deadline-expired as "Unknown"
                // would flap state on a temporarily slow link.
                tracing::debug!("nat-traversal: classify_deadline elapsed, keeping prior state");
                return;
            }
        };

        let mut fsm = ClassifyFsm::new();
        let mut latest_reflex: Option<std::net::SocketAddr> = None;
        for (peer, res) in results {
            if let Ok(addr) = res {
                fsm.observe(peer, addr);
                latest_reflex = Some(addr);
            }
        }

        let class = fsm.classify(bind);
        self.commit_reclassify_observations(class, latest_reflex);
    }

    /// Commit the result of a classification sweep.
    ///
    /// Split out from [`Self::reclassify_nat`] so the
    /// override-race guard is unit-testable without standing up
    /// a full probe mesh. The guard fixes a cubic-flagged P1
    /// bug: the entry-time check in `reclassify_nat` races with
    /// any `set_reflex_override` call that lands *during* the
    /// probe sweep — the flag flips false→true while we're
    /// awaiting probe futures, and a blind commit would silently
    /// stomp the fresh override with whatever the classifier
    /// observed. A port-mapping install is a strong signal (we
    /// have a known-public `external` address) that outranks
    /// peer-probed reflex; clobbering it would demote the node
    /// from Open back to whatever NAT class the probes inferred
    /// and could re-advertise the wrong reflex to peers.
    #[cfg(feature = "nat-traversal")]
    fn commit_reclassify_observations(
        &self,
        class: super::traversal::classify::NatClass,
        latest_reflex: Option<std::net::SocketAddr>,
    ) {
        use std::sync::atomic::Ordering;
        // Hold the publication mutex across the re-check + the
        // (potentially) paired writes. This both makes the
        // override mid-sweep guard atomic with the commit
        // (no TOCTOU on the flag vs stores), and serializes
        // against concurrent `announce_capabilities_with` reads
        // so the classifier can't publish nat_class without
        // reflex_addr (or vice versa) being visible together.
        let _g = self.traversal_publish_mu.lock();
        if self.reflex_override_active.load(Ordering::Acquire) {
            tracing::debug!("nat-traversal: reflex override installed mid-sweep, skipping commit");
            return;
        }
        self.nat_class.store(class.as_u8(), Ordering::Release);
        if let Some(addr) = latest_reflex {
            self.reflex_addr.store(Some(Arc::new(addr)));
        }
        tracing::debug!(
            nat_class = ?class,
            reflex = ?latest_reflex,
            "nat-traversal: reclassified",
        );
    }
}

// ── Adapter trait impl ──────────────────────────────────────────────────

#[async_trait]
impl Adapter for MeshNode {
    async fn init(&mut self) -> Result<(), AdapterError> {
        // MeshNode is initialized via new() + connect(). This is a no-op.
        Ok(())
    }

    async fn on_batch(&self, batch: Batch) -> Result<(), AdapterError> {
        // Send to the first connected peer. For a real mesh, this should
        // use the routing table to pick the right peer based on the
        // event's destination. For now, round-robin or first-match.
        let peer_addr = self
            .peers
            .iter()
            .next()
            .map(|e| e.value().addr)
            .ok_or_else(|| AdapterError::Connection("no peers connected".into()))?;

        self.send_to_peer(peer_addr, batch).await
    }

    async fn flush(&self) -> Result<(), AdapterError> {
        Ok(())
    }

    async fn shutdown(&self) -> Result<(), AdapterError> {
        self.shutdown.store(true, Ordering::Release);
        self.shutdown_notify.notify_waiters();
        self.router.stop();

        // Deactivate all sessions
        for entry in self.peers.iter() {
            entry.value().session.deactivate();
        }

        // Wait for background tasks
        let tasks = std::mem::take(&mut *self.tasks.lock().await);
        for handle in tasks {
            let _ = handle.await;
        }

        Ok(())
    }

    async fn poll_shard(
        &self,
        shard_id: u16,
        from_id: Option<&str>,
        limit: usize,
    ) -> Result<ShardPollResult, AdapterError> {
        let queue = match self.inbound.get(&shard_id) {
            Some(q) => q,
            None => return Ok(ShardPollResult::empty()),
        };

        let mut events = Vec::with_capacity(limit.min(1000));
        let mut last_id = None;
        // from_id is ignored — the SegQueue is consume-once, so every pop
        // removes the event permanently. Cursor-based skipping would destroy
        // events that have already been consumed. Callers should consume
        // from the head without a cursor.
        let _ = from_id;

        for _ in 0..limit {
            match queue.pop() {
                Some(event) => {
                    last_id = Some(event.id.clone());
                    events.push(event);
                }
                None => break,
            }
        }

        let has_more = !queue.is_empty();

        Ok(ShardPollResult {
            events,
            next_id: last_id,
            has_more,
        })
    }

    fn name(&self) -> &'static str {
        "mesh"
    }

    async fn is_healthy(&self) -> bool {
        self.started.load(Ordering::Acquire) && !self.peers.is_empty()
    }
}

impl Drop for MeshNode {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        self.shutdown_notify.notify_waiters();
        self.router.stop();
    }
}

#[cfg(all(test, feature = "nat-traversal"))]
mod punch_observer_tests {
    //! Tests for [`await_punch_observer_outcome`].
    //!
    //! Regression coverage for a cubic-flagged bug (P2): earlier
    //! revisions collapsed the `Ok(Err(_))` (sender dropped) and
    //! `Err(_)` (deadline) arms into a single `remove`-in-both
    //! branch, which evicted replacement observers. Each test
    //! below pins one of the three outcomes independently.
    use super::*;
    use crate::adapter::net::traversal::rendezvous::Keepalive;
    use tokio::sync::oneshot;

    fn sample_ka() -> Keepalive {
        Keepalive {
            sender_node_id: 0x1234,
            punch_id: 0,
        }
    }

    fn sample_peer() -> SocketAddr {
        "198.51.100.5:9001".parse().unwrap()
    }

    /// Keep-alive fires before the deadline — outcome is `true`
    /// (caller emits ack). The map entry was consumed by the
    /// receive loop when it fired the oneshot, so the helper
    /// doesn't need to remove anything.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fires_true_when_keepalive_arrives() {
        let observers: DashMap<SocketAddr, oneshot::Sender<Keepalive>> = DashMap::new();
        let peer = sample_peer();
        let (tx, rx) = oneshot::channel();
        observers.insert(peer, tx);

        // Simulate the receive loop firing the oneshot: remove
        // from the map + send the keepalive.
        let (_, fired_tx) = observers.remove(&peer).unwrap();
        fired_tx.send(sample_ka()).expect("send");

        let result =
            await_punch_observer_outcome(rx, Duration::from_secs(1), &observers, peer).await;
        assert!(result, "keepalive arrival should return true");
    }

    /// Deadline expires while our sender is still the live value
    /// in the map — helper must evict the stale entry so a late
    /// keep-alive doesn't find it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn timeout_evicts_own_stale_entry() {
        let observers: DashMap<SocketAddr, oneshot::Sender<Keepalive>> = DashMap::new();
        let peer = sample_peer();
        let (tx, rx) = oneshot::channel();
        observers.insert(peer, tx);

        // Very short deadline; nobody fires.
        let result =
            await_punch_observer_outcome(rx, Duration::from_millis(50), &observers, peer).await;
        assert!(!result, "timeout should return false");
        assert!(
            !observers.contains_key(&peer),
            "timeout should evict our own stale entry",
        );
    }

    /// The cubic-flagged regression: a newer observer replaces
    /// ours (our sender is dropped). Our task's `Ok(Err(_))` arm
    /// must **not** remove the peer_reflex key — it would evict
    /// the replacement observer that's the current live value
    /// in the map.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sender_dropped_leaves_replacement_observer_intact() {
        let observers: DashMap<SocketAddr, oneshot::Sender<Keepalive>> = DashMap::new();
        let peer = sample_peer();

        // Install observer A.
        let (tx_a, rx_a) = oneshot::channel::<Keepalive>();
        observers.insert(peer, tx_a);

        // Install observer B via insert — this drops A's sender
        // (the old value returned from the insert is dropped
        // immediately). Now the map contains B's sender.
        let (tx_b, _rx_b) = oneshot::channel::<Keepalive>();
        observers.insert(peer, tx_b);
        assert!(observers.contains_key(&peer), "B's sender in map");

        // A's task runs the cleanup helper. A's rx_a sees
        // RecvError (tx_a dropped), outcome is `Ok(Err(_))`.
        let result =
            await_punch_observer_outcome(rx_a, Duration::from_secs(5), &observers, peer).await;
        assert!(!result, "sender-dropped path returns false");
        assert!(
            observers.contains_key(&peer),
            "B's sender must still be in the map — A's cleanup must not evict",
        );
    }

    /// Idempotent-by-peer check: after a timeout-eviction,
    /// removing again is a no-op. Prevents a hypothetical
    /// double-eviction regression where the helper called
    /// `remove` on every cleanup path.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn timeout_then_sender_drop_does_not_double_evict() {
        let observers: DashMap<SocketAddr, oneshot::Sender<Keepalive>> = DashMap::new();
        let peer = sample_peer();

        // First task: install A, let it time out, evict.
        let (tx_a, rx_a) = oneshot::channel::<Keepalive>();
        observers.insert(peer, tx_a);
        let r1 =
            await_punch_observer_outcome(rx_a, Duration::from_millis(20), &observers, peer).await;
        assert!(!r1);
        assert!(!observers.contains_key(&peer));

        // Second task: install B, drop B's sender via a fresh
        // insert from C — simulates the "replacement" scenario
        // but now for a different observer lineage.
        let (tx_b, rx_b) = oneshot::channel::<Keepalive>();
        observers.insert(peer, tx_b);
        let (tx_c, _rx_c) = oneshot::channel::<Keepalive>();
        observers.insert(peer, tx_c);

        // B's task cleanup. Must NOT remove peer (C is live).
        let r2 = await_punch_observer_outcome(rx_b, Duration::from_secs(5), &observers, peer).await;
        assert!(!r2);
        assert!(
            observers.contains_key(&peer),
            "C's sender must remain after B's sender-dropped cleanup",
        );
    }
}

#[cfg(all(test, feature = "nat-traversal"))]
mod reclassify_override_race_tests {
    //! Regression coverage for a cubic-flagged P1 bug:
    //! [`MeshNode::reclassify_nat`] checked
    //! `reflex_override_active` only at entry, then ran the
    //! multi-peer probe sweep, then committed the result. A
    //! `set_reflex_override` call landing *after* the entry
    //! check but *before* the commit was silently stomped:
    //! the commit unconditionally overwrote `nat_class` and
    //! `reflex_addr` with the probe-derived values, undoing
    //! the freshly-installed override.
    //!
    //! Fix: [`MeshNode::reclassify_nat`] now calls
    //! [`MeshNode::commit_reclassify_observations`], which
    //! re-loads the flag before any store and bails out if an
    //! override landed mid-sweep. Tests below pin that guard
    //! without needing to stand up a real probe mesh.
    use super::*;
    use crate::adapter::net::traversal::classify::NatClass;
    use std::net::SocketAddr;

    async fn build_node_for_test() -> Arc<MeshNode> {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let cfg = MeshNodeConfig::new(addr, [0x17u8; 32]);
        Arc::new(
            MeshNode::new(EntityKeypair::generate(), cfg)
                .await
                .expect("MeshNode::new"),
        )
    }

    /// Pre-fix behavior: with override inactive, the commit
    /// path overwrites both `nat_class` and `reflex_addr`.
    /// This pins the positive case so the guard doesn't
    /// silently turn into a no-op.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn commit_applies_when_override_inactive() {
        let node = build_node_for_test().await;
        let probed: SocketAddr = "198.51.100.9:4242".parse().unwrap();

        // Override flag is false by default.
        node.commit_reclassify_observations(NatClass::Cone, Some(probed));

        assert_eq!(node.nat_class(), NatClass::Cone);
        assert_eq!(node.reflex_addr(), Some(probed));
    }

    /// The race guard: flag flipped true mid-sweep →
    /// commit must be a no-op. Without the fix the `nat_class`
    /// store and the `reflex_addr` store would land regardless,
    /// demoting the override-provided Open/external-ip back to
    /// the classifier's observation.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn commit_skips_when_override_installed_mid_sweep() {
        let node = build_node_for_test().await;
        let override_addr: SocketAddr = "203.0.113.77:9999".parse().unwrap();
        let probed: SocketAddr = "198.51.100.9:4242".parse().unwrap();

        // Simulate the race: `reclassify_nat` passed its entry
        // check with the flag false, ran probes, and is about
        // to commit. In between, port-mapping install fires
        // `set_reflex_override` — the flag flips true and the
        // reflex/class are written.
        node.set_reflex_override(override_addr);
        assert_eq!(node.nat_class(), NatClass::Open);
        assert_eq!(node.reflex_addr(), Some(override_addr));

        // Now the classifier's (stale) commit tries to land.
        // With the guard in place, it must be skipped.
        node.commit_reclassify_observations(NatClass::Symmetric, Some(probed));

        assert_eq!(
            node.nat_class(),
            NatClass::Open,
            "mid-sweep commit stomped the override's NAT class — \
             the node would be demoted from Open back to Symmetric",
        );
        assert_eq!(
            node.reflex_addr(),
            Some(override_addr),
            "mid-sweep commit stomped the override reflex — the \
             node would advertise the classifier's observation \
             instead of the known-public mapping",
        );
    }

    /// Same guard, but the `latest_reflex = None` path — when
    /// every probe failed, the buggy commit *still* wrote
    /// `nat_class` (only the `reflex_addr` store was gated by
    /// `Some`). The fix's guard covers both stores.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn commit_skips_nat_class_store_even_when_reflex_absent() {
        let node = build_node_for_test().await;
        let override_addr: SocketAddr = "203.0.113.77:9999".parse().unwrap();

        node.set_reflex_override(override_addr);

        // Classifier gave up (all probes failed); it would
        // still store `Unknown` without the guard.
        node.commit_reclassify_observations(NatClass::Unknown, None);

        assert_eq!(
            node.nat_class(),
            NatClass::Open,
            "`nat_class` was overwritten with Unknown even though \
             `latest_reflex` was None — demonstrates the second \
             half of the bug that the bare `if let Some` would've \
             missed",
        );
        assert_eq!(node.reflex_addr(), Some(override_addr));
    }

    /// After `clear_reflex_override` is called, the classifier
    /// regains write access. Without this, the fix would
    /// permanently freeze classification once any override had
    /// ever been installed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn commit_resumes_after_override_cleared() {
        let node = build_node_for_test().await;
        let override_addr: SocketAddr = "203.0.113.77:9999".parse().unwrap();
        let probed: SocketAddr = "198.51.100.9:4242".parse().unwrap();

        node.set_reflex_override(override_addr);
        node.clear_reflex_override();

        node.commit_reclassify_observations(NatClass::Cone, Some(probed));

        assert_eq!(node.nat_class(), NatClass::Cone);
        assert_eq!(node.reflex_addr(), Some(probed));
    }
}

#[cfg(test)]
mod heartbeat_aead_tests {
    //! Regression for BUG_AUDIT_2026_04_30_CORE.md #85: the mesh
    //! dispatch loop's heartbeat fast-path used to skip AEAD
    //! verification, letting an off-path attacker who observed the
    //! cleartext `session_id` and source UDP address spoof
    //! heartbeats indefinitely. The fix routes through
    //! [`NetSession::verify_and_touch_heartbeat`] before calling
    //! `failure_detector.heartbeat`. The verify+touch are now fused
    //! into the session method so a future caller can't reorder
    //! them or forget to touch on success. These tests pin both
    //! the AEAD-verify outcome AND the touch-on-success/no-touch-
    //! on-failure invariant at the same coverage bar the legacy
    //! single-peer adapter has at
    //! `mod.rs::heartbeat_is_aead_authenticated`.
    use super::*;
    use crate::adapter::net::crypto::{NoiseHandshake, StaticKeypair};
    use crate::adapter::net::pool::PacketBuilder;
    use crate::adapter::net::protocol::{NetHeader, PacketFlags};

    /// Extract the TX counter (bytes 16..24, little-endian u64) from
    /// a serialized packet's header. Both data packets and
    /// heartbeats patch the counter into the same wire-format
    /// position; see `pool.rs::PacketBuilder::build` lines
    /// `header_bytes[16..24].copy_from_slice(&counter.to_le_bytes())`
    /// and the matching line in `build_heartbeat`.
    fn counter_of(packet: &[u8]) -> u64 {
        u64::from_le_bytes(
            packet[16..24]
                .try_into()
                .expect("packet header is at least 24 bytes"),
        )
    }

    fn make_session_keys() -> (
        crate::adapter::net::crypto::SessionKeys,
        crate::adapter::net::crypto::SessionKeys,
    ) {
        let psk = [0x42u8; 32];
        let responder_kp = StaticKeypair::generate();
        let mut initiator = NoiseHandshake::initiator(&psk, &responder_kp.public).unwrap();
        let mut responder = NoiseHandshake::responder(&psk, &responder_kp).unwrap();
        let msg1 = initiator.write_message(&[]).unwrap();
        responder.read_message(&msg1).unwrap();
        let msg2 = responder.write_message(&[]).unwrap();
        initiator.read_message(&msg2).unwrap();
        (
            initiator.into_session_keys().unwrap(),
            responder.into_session_keys().unwrap(),
        )
    }

    #[test]
    fn aead_authenticated_heartbeat_passes_verification_and_touches_session() {
        let (init_keys, resp_keys) = make_session_keys();
        let resp_session = NetSession::new(resp_keys, "127.0.0.1:5000".parse().unwrap(), 4, false);
        let mut builder = PacketBuilder::new(&init_keys.tx_key, init_keys.session_id);
        let bytes = builder.build_heartbeat();

        let parsed = ParsedPacket::parse(bytes, "127.0.0.1:5000".parse().unwrap())
            .expect("legitimate heartbeat must parse");
        assert!(parsed.header.flags.is_heartbeat());

        let last_before = resp_session.last_activity_ns();
        // Sleep so `current_timestamp()` ticks observably forward
        // before `verify_and_touch_heartbeat` reads it.
        std::thread::sleep(std::time::Duration::from_millis(2));

        assert!(
            resp_session.verify_and_touch_heartbeat(&parsed),
            "AEAD-authenticated heartbeat must verify against the matched session"
        );
        assert!(
            resp_session.last_activity_ns() > last_before,
            "successful verify must touch the session — verify+touch are fused"
        );
    }

    #[test]
    fn unauthenticated_heartbeat_fails_verification_and_does_not_touch() {
        let (_init_keys, resp_keys) = make_session_keys();
        let resp_session = NetSession::new(resp_keys, "127.0.0.1:5000".parse().unwrap(), 4, false);

        // Attacker forges a heartbeat header with the right
        // session_id but garbage 16-byte tail. Pre-fix this passed
        // through; post-fix it must fail.
        let mut forged = bytes::BytesMut::new();
        let mut header_bytes = NetHeader::heartbeat(resp_session.session_id()).to_bytes();
        // Stamp a plausible nonce so the receiver gets to decrypt
        // (otherwise it bails earlier on the counter check).
        header_bytes[12..16].copy_from_slice(&[0u8; 4]);
        header_bytes[16..24].copy_from_slice(&1u64.to_le_bytes());
        forged.extend_from_slice(&header_bytes);
        forged.extend_from_slice(&[0xAAu8; 16]); // garbage tag
        let parsed = ParsedPacket::parse(forged.freeze(), "127.0.0.1:5000".parse().unwrap())
            .expect("forged heartbeat must still parse — verification is downstream");
        assert!(parsed.header.flags.is_heartbeat());

        let last_before = resp_session.last_activity_ns();
        std::thread::sleep(std::time::Duration::from_millis(2));

        assert!(
            !resp_session.verify_and_touch_heartbeat(&parsed),
            "heartbeat with garbage AEAD tag must NOT verify — pre-fix the \
             mesh dispatcher would have called session.touch() / \
             failure_detector.heartbeat() unconditionally"
        );
        assert_eq!(
            resp_session.last_activity_ns(),
            last_before,
            "failed verify must NOT touch the session — verify+touch are fused"
        );
    }

    /// Regression for BUG_AUDIT_2026_04_30_CORE.md #87: dropping
    /// a `PeerRegistrationGuard` whose `completed` flag is still
    /// `false` (cancellation, panic, or non-success path) must
    /// run the same rollback the legacy inline error arm did —
    /// remove the peer entry, peer-addr mapping, and routing
    /// table entry, but only if those entries still match the
    /// values this handshake wrote (a concurrent retry may have
    /// replaced them).
    #[tokio::test]
    async fn peer_registration_guard_rolls_back_on_drop_when_not_completed() {
        let peer_id = 0xDEAD_BEEFu64;
        let next_hop: SocketAddr = "10.0.0.1:9000".parse().unwrap();

        let peers: Arc<DashMap<u64, PeerInfo>> = Arc::new(DashMap::new());
        let peer_addrs: Arc<DashMap<u64, SocketAddr>> = Arc::new(DashMap::new());
        let router = Arc::new(
            NetRouter::new(crate::adapter::net::router::RouterConfig::new(
                0xCAFE_BABE,
                "127.0.0.1:0".parse().unwrap(),
            ))
            .await
            .unwrap(),
        );

        // Simulate the post-handshake registration: insert peer,
        // peer-addr, and a route. We can't construct a fully-
        // populated `PeerInfo` without the matched session keys,
        // but the rollback only inspects `pi.addr`, so we build
        // a minimal session and check post-Drop that the entry
        // is gone.
        let (init_keys, _resp_keys) = make_session_keys();
        let session = Arc::new(NetSession::new(init_keys, next_hop, 4, false));
        peers.insert(
            peer_id,
            PeerInfo {
                node_id: peer_id,
                addr: next_hop,
                session,
                remote_static_pub: [0u8; 32],
            },
        );
        peer_addrs.insert(peer_id, next_hop);
        router.add_route(peer_id, next_hop);

        // Drop guard without committing.
        {
            let _guard = PeerRegistrationGuard {
                peer_node_id: peer_id,
                registered_next_hop: next_hop,
                peers: peers.clone(),
                peer_addrs: peer_addrs.clone(),
                router: router.clone(),
            };
        } // Drop runs here.

        assert!(
            !peers.contains_key(&peer_id),
            "peers entry must be removed by Drop rollback"
        );
        assert!(
            !peer_addrs.contains_key(&peer_id),
            "peer_addrs entry must be removed by Drop rollback"
        );
        assert!(
            router.routing_table().lookup(peer_id).is_none(),
            "route must be removed by Drop rollback"
        );
    }

    #[tokio::test]
    async fn peer_registration_guard_is_no_op_on_drop_when_completed() {
        let peer_id = 0xCAFE_F00Du64;
        let next_hop: SocketAddr = "10.0.0.2:9000".parse().unwrap();

        let peers: Arc<DashMap<u64, PeerInfo>> = Arc::new(DashMap::new());
        let peer_addrs: Arc<DashMap<u64, SocketAddr>> = Arc::new(DashMap::new());
        let router = Arc::new(
            NetRouter::new(crate::adapter::net::router::RouterConfig::new(
                0xCAFE_BABE,
                "127.0.0.1:0".parse().unwrap(),
            ))
            .await
            .unwrap(),
        );

        let (init_keys, _resp_keys) = make_session_keys();
        let session = Arc::new(NetSession::new(init_keys, next_hop, 4, false));
        peers.insert(
            peer_id,
            PeerInfo {
                node_id: peer_id,
                addr: next_hop,
                session,
                remote_static_pub: [0u8; 32],
            },
        );
        peer_addrs.insert(peer_id, next_hop);
        router.add_route(peer_id, next_hop);

        {
            let guard = PeerRegistrationGuard {
                peer_node_id: peer_id,
                registered_next_hop: next_hop,
                peers: peers.clone(),
                peer_addrs: peer_addrs.clone(),
                router: router.clone(),
            };
            // Successful-send path consumes the guard without
            // running Drop's rollback.
            guard.commit();
        }

        assert!(peers.contains_key(&peer_id));
        assert!(peer_addrs.contains_key(&peer_id));
        assert!(router.routing_table().lookup(peer_id).is_some());
    }

    /// Regression: if a concurrent retry has overwritten the
    /// peer-addr / route to a different next-hop, the guard's
    /// rollback must NOT clobber the newer (valid) registration.
    /// Mirrors the `remove_if`/`remove_route_if_next_hop_is`
    /// guarantees from the original inline rollback.
    #[tokio::test]
    async fn peer_registration_guard_preserves_concurrent_overwrite() {
        let peer_id = 0xFACE_F00Du64;
        let stale: SocketAddr = "10.0.0.3:9000".parse().unwrap();
        let fresh: SocketAddr = "10.0.0.4:9000".parse().unwrap();

        let peers: Arc<DashMap<u64, PeerInfo>> = Arc::new(DashMap::new());
        let peer_addrs: Arc<DashMap<u64, SocketAddr>> = Arc::new(DashMap::new());
        let router = Arc::new(
            NetRouter::new(crate::adapter::net::router::RouterConfig::new(
                0xCAFE_BABE,
                "127.0.0.1:0".parse().unwrap(),
            ))
            .await
            .unwrap(),
        );

        let (init_keys, _resp_keys) = make_session_keys();
        let session = Arc::new(NetSession::new(init_keys, fresh, 4, false));
        // Concurrent retry has overwritten with `fresh` — the
        // stale guard about to drop should NOT remove this.
        peers.insert(
            peer_id,
            PeerInfo {
                node_id: peer_id,
                addr: fresh,
                session,
                remote_static_pub: [0u8; 32],
            },
        );
        peer_addrs.insert(peer_id, fresh);
        router.add_route(peer_id, fresh);

        {
            let _guard = PeerRegistrationGuard {
                peer_node_id: peer_id,
                registered_next_hop: stale, // NOT what's currently in the maps
                peers: peers.clone(),
                peer_addrs: peer_addrs.clone(),
                router: router.clone(),
            };
        }

        assert!(
            peers.contains_key(&peer_id),
            "peers must keep the fresh (concurrent-retry) entry"
        );
        assert_eq!(*peer_addrs.get(&peer_id).unwrap(), fresh);
        assert_eq!(router.routing_table().lookup(peer_id), Some(fresh));
    }

    /// Regression for BUG_AUDIT_2026_04_30_CORE.md #97: the
    /// production heartbeat path must (a) build with the
    /// session's actual TX key — not `&[0u8; 32]` — and (b) use a
    /// **shared** `tx_counter` across builders so successive
    /// heartbeats don't all encrypt under counter=0 and trigger
    /// the receiver's replay window. Both bugs were latent before
    /// #85 wired up AEAD verification on the mesh receiver; this
    /// test pins the post-fix invariant by acquiring two builders
    /// from the SAME session pool (mirroring what the heartbeat
    /// timer at `mesh.rs:3220` does on each tick) and verifying
    /// that BOTH heartbeats verify against the peer session in
    /// order. Pre-fix behavior with the all-zero key would fail
    /// the first verify; pre-fix behavior with per-builder
    /// counters would fail the second.
    #[test]
    fn pooled_heartbeat_builds_succeed_in_sequence_and_verify() {
        let (init_keys, resp_keys) = make_session_keys();
        let init_session = NetSession::new(
            init_keys.clone(),
            "127.0.0.1:5001".parse().unwrap(),
            4,
            false,
        );
        let resp_session = NetSession::new(resp_keys, "127.0.0.1:5000".parse().unwrap(), 4, false);

        // Mirror the production sender — go through
        // `Session::build_heartbeat`, not a fresh
        // `PacketBuilder::new`.
        let h1_bytes = init_session.build_heartbeat();
        let h2_bytes = init_session.build_heartbeat();

        let p1 = ParsedPacket::parse(h1_bytes, "127.0.0.1:5001".parse().unwrap())
            .expect("first heartbeat must parse");
        let p2 = ParsedPacket::parse(h2_bytes, "127.0.0.1:5001".parse().unwrap())
            .expect("second heartbeat must parse");

        assert!(
            resp_session.verify_and_touch_heartbeat(&p1),
            "first pooled heartbeat must verify — pre-fix the \
             all-zero key would have produced an AEAD tag the \
             receiver couldn't decrypt"
        );
        assert!(
            resp_session.verify_and_touch_heartbeat(&p2),
            "second pooled heartbeat must also verify — pre-fix, \
             a per-builder fresh counter would reuse counter=0 \
             and the receiver's replay window would reject this \
             as a duplicate"
        );
    }

    #[test]
    fn replay_of_authenticated_heartbeat_fails_verification_on_second_try() {
        let (init_keys, resp_keys) = make_session_keys();
        let resp_session = NetSession::new(resp_keys, "127.0.0.1:5000".parse().unwrap(), 4, false);
        let mut builder = PacketBuilder::new(&init_keys.tx_key, init_keys.session_id);
        let bytes = builder.build_heartbeat();
        let parsed = ParsedPacket::parse(bytes, "127.0.0.1:5000".parse().unwrap()).unwrap();

        assert!(resp_session.verify_and_touch_heartbeat(&parsed));
        // Replay: counter is now committed, so the second attempt
        // must fail at the replay-window check.
        assert!(
            !resp_session.verify_and_touch_heartbeat(&parsed),
            "replay of an already-accepted heartbeat must fail"
        );
    }

    /// Invariant: heartbeats and data-path packets share a
    /// single TX counter via `thread_local_pool`. Pre-#106-fix,
    /// `NetSession` exposed two pools (`packet_pool` and
    /// `thread_local_pool`) with the same key but independent
    /// counters; a caller that mixed `session.packet_pool().get()`
    /// for some packets and `session.thread_local_pool().get()` for
    /// others would produce ChaCha20-Poly1305 nonce reuse against
    /// the same key, leaking plaintext via XOR.
    ///
    /// The fix removed `packet_pool` and routes all TX through
    /// `thread_local_pool`. Today `Session::build_heartbeat` calls
    /// `self.thread_local_pool.get().build_heartbeat()` and the
    /// data path builds via `self.thread_local_pool.get().build(...)`.
    /// This test pins the resulting wire-level invariant: the
    /// counters on heartbeat and data packets interleave strictly
    /// monotonically (no two packets ever share a counter, no
    /// matter the order they're built in). A future contributor who
    /// re-introduced a separate pool/counter for heartbeats would
    /// see this test fail because both sequences would restart at
    /// 0.
    #[test]
    fn heartbeat_and_data_share_tx_counter_strictly_monotonic() {
        let (init_keys, _resp_keys) = make_session_keys();
        let init_session = NetSession::new(
            init_keys.clone(),
            "127.0.0.1:5001".parse().unwrap(),
            4,
            false,
        );

        // Build sequence: heartbeat, data, heartbeat, data,
        // heartbeat. All five must have strictly-increasing
        // counters.
        let h1 = init_session.build_heartbeat();
        let d1 = {
            let mut pooled = init_session.thread_local_pool().get();
            pooled.build(
                0xCAFE_F00D,
                0,
                &[bytes::Bytes::from_static(b"event-a")],
                PacketFlags::NONE,
            )
        };
        let h2 = init_session.build_heartbeat();
        let d2 = {
            let mut pooled = init_session.thread_local_pool().get();
            pooled.build(
                0xCAFE_F00D,
                1,
                &[bytes::Bytes::from_static(b"event-b")],
                PacketFlags::NONE,
            )
        };
        let h3 = init_session.build_heartbeat();

        let counters = [
            counter_of(&h1),
            counter_of(&d1),
            counter_of(&h2),
            counter_of(&d2),
            counter_of(&h3),
        ];

        // Strict monotonicity: each counter > the previous one.
        for window in counters.windows(2) {
            assert!(
                window[0] < window[1],
                "tx counters must be strictly increasing across heartbeat/data \
                 interleave; got {:?} (regression: heartbeats and data \
                 are drawing from independent counters)",
                counters
            );
        }
    }

    /// CR-8: source-level tripwire pinning that no dispatch
    /// branch uses `events.into_iter().next()` to drop multi-event
    /// frames. The original fix only patched
    /// `SUBPROTOCOL_STREAM_WINDOW`; CR-8 extended the same fix to
    /// the migration / channel-membership / capability-ann / reflex
    /// / rendezvous branches. This test scans the file source for
    /// any new occurrence outside fix-doc comments and fails loudly
    /// if a future maintainer reintroduces the pattern.
    ///
    /// We assemble the forbidden token at runtime so the test's
    /// own source doesn't trigger itself.
    #[test]
    fn cr8_dispatch_must_not_use_single_event_pattern() {
        // Build the forbidden token from fragments so this test's
        // source doesn't contain the literal substring.
        let needle = format!("events.into_iter().{}()", "next");

        let src = include_str!("mesh.rs");
        for (lineno, line) in src.lines().enumerate() {
            let trimmed = line.trim_start();
            // Skip line comments and doc comments — these are
            // ALLOWED to mention the pre-fix shape (the fix-doc
            // narratives are load-bearing context for future
            // maintainers).
            if trimmed.starts_with("//") {
                continue;
            }
            // Skip lines inside doc-string-style comments inside
            // string literals: we don't try to be too clever here,
            // since a code line containing `events.into_iter().next()`
            // outside a comment is the regression we want to catch.
            assert!(
                !trimmed.contains(&needle),
                "CR-8 regression: single-event dispatch pattern reintroduced \
                 at mesh.rs:{} — multi-event frames will silently drop \
                 every payload past the first.\n  line: {}",
                lineno + 1,
                line
            );
        }
    }

    /// Source-level pin: every `hop_count += 1` / `hop_count = X + 1`
    /// pattern in this file MUST go through `saturating_add`.
    /// `hop_count: u8` saturates at 255; an attacker-controlled
    /// inbound packet whose `hop_count` is already u8::MAX would
    /// debug-panic (`overflow`) or release-wraparound to 0 on a
    /// bare `+= 1`. Today the upstream `< MAX_CAPABILITY_HOPS - 1`
    /// guard bounds the value at 14 before the bump, so the
    /// saturating call is dormant — but a future change that
    /// raises the cap or relaxes the guard would otherwise turn
    /// an attacker byte into UB / wrap. This pin ensures the
    /// hardening stays in place even if the upstream gate moves.
    #[test]
    fn hop_count_increments_must_be_saturating() {
        // Build the forbidden token at runtime so this test's source
        // doesn't trigger itself.
        let bare_bump = format!("hop_count {} 1", "+=");

        let src = include_str!("mesh.rs");
        for (lineno, line) in src.lines().enumerate() {
            let trimmed = line.trim_start();
            // Comments are allowed to mention the pre-fix shape.
            if trimmed.starts_with("//") {
                continue;
            }
            assert!(
                !trimmed.contains(&bare_bump),
                "hop_count regression: bare `+= 1` reintroduced at \
                 mesh.rs:{} — use `saturating_add(1)` so an attacker-\
                 controlled `hop_count == u8::MAX` cannot wrap.\n  line: {}",
                lineno + 1,
                line,
            );
        }
    }
}
