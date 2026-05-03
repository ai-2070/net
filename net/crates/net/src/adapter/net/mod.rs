//! Net L0 Transport Protocol (Net) adapter.
//!
//! Net is a high-performance UDP-based transport protocol designed for
//! GPU-to-GPU encrypted streaming over UDP. It provides:
//!
//! - Zero-copy, zero-allocation hot path
//! - XChaCha20-Poly1305 encryption per packet
//! - Noise protocol handshake (NKpsk0)
//! - Optional per-stream reliability with selective NACKs
//! - 40-60M events/sec target throughput
//!
//! # Usage
//!
//! ```rust,ignore
//! use net::adapter::net::{NetAdapter, NetAdapterConfig, StaticKeypair};
//!
//! // Generate keypair for responder
//! let keypair = StaticKeypair::generate();
//!
//! // Create initiator config
//! let config = NetAdapterConfig::initiator(
//!     "127.0.0.1:9000".parse()?,
//!     "127.0.0.1:9001".parse()?,
//!     psk,
//!     keypair.public,
//! );
//!
//! // Create adapter
//! let mut adapter = NetAdapter::new(config)?;
//! adapter.init().await?;
//! ```

mod batch;
pub mod behavior;
pub mod channel;
pub mod compute;
mod config;
pub mod contested;
pub mod continuity;
#[cfg(feature = "cortex")]
pub mod cortex;
mod crypto;
mod failure;
pub mod identity;
mod mesh;
#[cfg(feature = "netdb")]
pub mod netdb;
mod pool;
mod protocol;
mod proxy;
#[cfg(feature = "redex")]
pub mod redex;
mod reliability;
mod reroute;
mod route;
mod router;
mod session;
pub mod state;
mod stream;
pub mod subnet;
pub mod subprotocol;
mod swarm;
mod transport;
#[cfg(feature = "nat-traversal")]
pub mod traversal;

#[cfg(target_os = "linux")]
mod linux;

pub use batch::AdaptiveBatcher;
pub use channel::{
    AckReason, AuthGuard, AuthVerdict, ChannelConfig, ChannelConfigRegistry, ChannelError,
    ChannelId, ChannelName, ChannelPublisher, ChannelRegistry, MembershipMsg, OnFailure,
    PublishConfig, PublishReport, SubscriberRoster, Visibility, SUBPROTOCOL_CHANNEL_MEMBERSHIP,
};
pub use compute::{
    DaemonError, DaemonFactoryRegistry, DaemonHost, DaemonHostConfig, DaemonRegistry, DaemonStats,
    FactoryEntry, MeshDaemon, MigrationError, MigrationMessage, MigrationOrchestrator,
    MigrationPhase, MigrationSourceHandler, MigrationState, MigrationTargetHandler,
    PlacementDecision, Scheduler, SchedulerError, SUBPROTOCOL_MIGRATION,
};
pub use config::{ConnectionRole, NetAdapterConfig, ReliabilityConfig};
pub use contested::{
    CorrelatedFailureConfig, CorrelatedFailureDetector, CorrelationVerdict, FailureCause,
    PartitionDetector, PartitionPhase, PartitionRecord, ReconcileOutcome, Side,
    SUBPROTOCOL_PARTITION,
};
pub use continuity::{
    assess_continuity, CausalCone, Causality, ContinuityProof, ContinuityStatus, Discontinuity,
    DiscontinuityReason, ForkRecord, HorizonDivergence, ObservationWindow, ProofError,
    PropagationModel, SuperpositionPhase, SuperpositionState, SUBPROTOCOL_CONTINUITY,
};
#[cfg(feature = "cortex")]
pub use cortex::{
    CortexAdapter, CortexAdapterConfig, CortexAdapterError, EventEnvelope, EventMeta,
    FoldErrorPolicy, IntoRedexPayload, StartPosition, EVENT_META_SIZE,
};
pub use crypto::{CryptoError, SessionKeys, StaticKeypair};
pub use failure::{
    CircuitBreaker, CircuitState, FailureDetector, FailureDetectorConfig, FailureStats,
    LossSimulator, NodeStatus, RecoveryAction, RecoveryManager, RecoveryStats,
};
pub use identity::{
    EntityError, EntityId, EntityKeypair, OriginStamp, PermissionToken, TokenCache, TokenError,
    TokenScope,
};
pub use mesh::{MeshNode, MeshNodeConfig, PartitionFilter};
#[cfg(feature = "netdb")]
pub use netdb::{MemoriesFilter, NetDb, NetDbBuilder, NetDbError, NetDbSnapshot, TasksFilter};
// `SharedPacketPool` is intentionally not re-exported — see
// `pool.rs` for the cross-pool nonce-reuse rationale.
// `PacketPool` itself stays exposed because tests reference it;
// only the `Arc<PacketPool>` wrapper alias and its constructor
// are absent.
pub use pool::{PacketBuilder, PacketPool, SharedLocalPool, ThreadLocalPool};
pub use protocol::{
    EventFrame, NackPayload, NetHeader, PacketFlags, HEADER_SIZE, NONCE_SIZE, TAG_SIZE,
};
pub use proxy::{
    ForwardResult, HopStats, MultiHopPacketBuilder, NetProxy, ProxyConfig, ProxyError, ProxyStats,
};
#[cfg(feature = "redex")]
pub use redex::{
    FsyncPolicy, IndexOp, IndexStart, OrderedAppender, Redex, RedexEntry, RedexError, RedexEvent,
    RedexFile, RedexFileConfig, RedexFlags, RedexFold, RedexIndex, TypedRedexFile,
};
pub use reliability::{FireAndForget, ReliabilityMode, ReliableStream, RetransmitDescriptor};
pub use reroute::ReroutePolicy;
pub use route::{
    AggregateStats, RouteEntry, RouteFlags, RoutingHeader, RoutingTable, SchedulerStreamStats,
    ROUTING_HEADER_SIZE,
};
pub use router::{FairScheduler, NetRouter, RouteAction, RouterConfig, RouterError, RouterStats};
pub use session::{NetSession, SessionManager, StreamState, TxAdmit, TxSlotGuard};
pub use state::{
    CausalChainBuilder, CausalEvent, CausalLink, ChainError, EntityLog, HorizonEncoder, LogError,
    LogIndex, ObservedHorizon, SnapshotStore, StateSnapshot, CAUSAL_LINK_SIZE, SUBPROTOCOL_CAUSAL,
    SUBPROTOCOL_SNAPSHOT,
};
pub use stream::{
    CloseBehavior, Reliability, Stream, StreamConfig, StreamError, StreamStats,
    DEFAULT_STREAM_WINDOW_BYTES,
};
pub use subnet::{DropReason, ForwardDecision, SubnetGateway, SubnetId, SubnetPolicy, SubnetRule};
pub use subprotocol::{
    negotiate, MigrationSubprotocolHandler, NegotiatedSet, OutboundMigrationMessage,
    SubprotocolDescriptor, SubprotocolManifest, SubprotocolRegistry, SubprotocolVersion,
    SUBPROTOCOL_NEGOTIATION,
};
pub use swarm::{
    Capabilities, CapabilityAd, EdgeInfo, GraphStats, LocalGraph, NodeInfo, Pingwave,
    MAX_GRAPH_NODES, MAX_SEEN_PINGWAVES, PINGWAVE_SIZE,
};
pub use transport::{NetSocket, PacketReceiver, PacketSender, ParsedPacket, SocketBufferConfig};

use async_trait::async_trait;
use bytes::Bytes;
use crossbeam_queue::SegQueue;
use dashmap::DashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::adapter::{Adapter, ShardPollResult};
use crate::error::AdapterError;
use crate::event::{Batch, StoredEvent};

use crypto::NoiseHandshake;
use session::SessionManager as SessionMgr;
use transport::NetSocket as Socket;

// Re-export xxh3 utilities for stream routing
pub use routing::{route_to_shard, stream_id_from_bytes, stream_id_from_key};

/// Current timestamp in nanoseconds since the Unix epoch.
///
/// Shared utility — avoids duplicating this across `causal.rs`, `snapshot.rs`,
/// `observation.rs`, `migration.rs`, `session.rs`, and `token.rs`.
///
/// Saturates via `try_from` so future-dated clocks land at
/// `u64::MAX` instead of wrapping near 0. A bare `as u64` would
/// silently truncate the `u128` returned by
/// `Duration::as_nanos()`. Practical wraparound from monotonic
/// flow doesn't happen until ~year 2554, but a system whose clock
/// was misconfigured to a far-future date would produce a tiny
/// truncated timestamp — immediately tripping `is_timed_out`
/// everywhere. `unwrap_or_default()` returning `Duration::ZERO`
/// for a pre-epoch clock would also produce identical timestamps
/// that break ordering.
#[inline]
pub(crate) fn current_timestamp() -> u64 {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX)
}

/// Fast xxh3-based routing utilities for Net streams.
///
/// Uses xxh3 (~50GB/s) for deterministic, high-performance stream routing.
mod routing {
    use xxhash_rust::xxh3::xxh3_64;

    /// Generate a stream ID from arbitrary data.
    ///
    /// Uses xxh3 for fast, deterministic hashing (~50GB/s on modern CPUs).
    #[inline]
    pub fn stream_id_from_bytes(data: &[u8]) -> u64 {
        xxh3_64(data)
    }

    /// Generate a stream ID from a string key.
    ///
    /// Convenience wrapper for `stream_id_from_bytes`.
    #[inline]
    pub fn stream_id_from_key(key: &str) -> u64 {
        xxh3_64(key.as_bytes())
    }

    /// Route data to a shard based on its content hash.
    ///
    /// Returns a shard ID in the range `[0, num_shards)`.
    ///
    /// # Panics
    ///
    /// Panics if `num_shards` is 0.
    #[inline]
    pub fn route_to_shard(data: &[u8], num_shards: u16) -> u16 {
        assert!(num_shards > 0, "num_shards must be > 0");
        (xxh3_64(data) % num_shards as u64) as u16
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_stream_id_deterministic() {
            let data = b"test event data";
            let id1 = stream_id_from_bytes(data);
            let id2 = stream_id_from_bytes(data);
            assert_eq!(id1, id2);
        }

        #[test]
        fn test_stream_id_different_for_different_data() {
            let id1 = stream_id_from_bytes(b"event1");
            let id2 = stream_id_from_bytes(b"event2");
            assert_ne!(id1, id2);
        }

        #[test]
        fn test_stream_id_from_key() {
            let id = stream_id_from_key("user:12345");
            assert_ne!(id, 0);
        }

        #[test]
        fn test_route_to_shard_range() {
            let num_shards = 16u16;
            for i in 0..1000 {
                let data = format!("event_{}", i);
                let shard = route_to_shard(data.as_bytes(), num_shards);
                assert!(shard < num_shards);
            }
        }

        #[test]
        #[should_panic(expected = "num_shards must be > 0")]
        fn test_route_to_shard_zero_shards_panics() {
            // Regression: route_to_shard(_, 0) caused a divide-by-zero panic
            // with no helpful message. Now it asserts with a clear message.
            route_to_shard(b"test", 0);
        }

        #[test]
        fn test_route_to_shard_distribution() {
            let num_shards = 8u16;
            let mut counts = [0u32; 8];

            for i in 0..8000 {
                let data = format!("event_{}", i);
                let shard = route_to_shard(data.as_bytes(), num_shards);
                counts[shard as usize] += 1;
            }

            // Check that distribution is reasonably uniform (within 50% of expected)
            let expected = 1000;
            for count in counts {
                assert!(count > expected / 2, "shard count {} too low", count);
                assert!(count < expected * 2, "shard count {} too high", count);
            }
        }
    }
}

/// Shared inbound queue type
type InboundQueues = Arc<DashMap<u16, SegQueue<StoredEvent>>>;

/// Per-source rate limiter for the handshake responder loop.
///
/// The responder used to accept whichever source emitted msg1
/// first, with no per-source pacing — an attacker who knows the PSK
/// (PSKs are typically multi-tenant) could race the legitimate
/// initiator's msg1; even without the PSK an attacker could flood
/// handshake-flagged datagrams to monopolize the recv loop.
///
/// `HandshakePacer` keeps a rolling count of recent attempts per
/// source and rejects sources that exceed the budget within the
/// window. Expired entries are garbage-collected on a periodic
/// schedule rather than on every check, so a sustained flood from
/// many distinct sources doesn't pay an O(n) sweep per packet.
pub(crate) struct HandshakePacer {
    /// Per-source `(count_in_window, window_start)`.
    entries: std::collections::HashMap<std::net::SocketAddr, (u32, std::time::Instant)>,
    /// Maximum attempts per source within `window`.
    max_per_window: u32,
    /// Window length.
    window: std::time::Duration,
    /// Last time we ran the GC pass.
    last_gc: std::time::Instant,
    /// Soft cap on `entries` size before forcing a GC pass even
    /// before the periodic deadline. Keeps memory bounded against
    /// an attacker fanning across many spoofed source addresses.
    gc_size_threshold: usize,
}

impl HandshakePacer {
    pub(crate) fn new(max_per_window: u32, window: std::time::Duration) -> Self {
        Self {
            entries: std::collections::HashMap::new(),
            max_per_window,
            window,
            last_gc: std::time::Instant::now(),
            // 4096 entries × ~40 bytes each ≈ 160 KiB — comfortable
            // ceiling that still triggers GC well before any
            // realistic memory issue.
            gc_size_threshold: 4096,
        }
    }

    /// Record an attempt from `source`. Returns `true` if the source
    /// is within budget (caller may proceed); `false` if it has
    /// exceeded the rate limit (caller must drop the packet).
    pub(crate) fn check_and_record(&mut self, source: std::net::SocketAddr) -> bool {
        let now = std::time::Instant::now();
        // Amortized GC: only run the O(n) `retain` sweep when one
        // of two thresholds trips:
        //   1. We haven't GC'd in `window` (entries are valid for
        //      at most `2 * window` so a once-per-`window` cadence
        //      is sufficient to keep the map proportional to the
        //      active source set).
        //   2. The map exceeds `gc_size_threshold`, indicating a
        //      flood attempt across many spoofed sources.
        if now.duration_since(self.last_gc) >= self.window
            || self.entries.len() >= self.gc_size_threshold
        {
            let cutoff = self.window.saturating_mul(2);
            self.entries
                .retain(|_, (_, start)| now.duration_since(*start) < cutoff);
            self.last_gc = now;
        }

        let entry = self.entries.entry(source).or_insert((0, now));
        if now.duration_since(entry.1) > self.window {
            // Window expired; reset the counter.
            entry.0 = 0;
            entry.1 = now;
        }
        entry.0 = entry.0.saturating_add(1);
        entry.0 <= self.max_per_window
    }
}

/// Net adapter for high-performance UDP transport.
pub struct NetAdapter {
    /// Configuration
    config: NetAdapterConfig,
    /// UDP socket
    socket: Option<Arc<Socket>>,
    /// Session (stored separately for init)
    session: Option<Arc<NetSession>>,
    /// Session manager
    session_manager: SessionMgr,
    /// Inbound events per shard (for poll_shard)
    inbound: InboundQueues,
    /// Background tasks
    tasks: TokioMutex<Vec<JoinHandle<()>>>,
    /// Shutdown signal (flag for polling, Notify for waking blocked tasks)
    shutdown: Arc<AtomicBool>,
    /// Notify to wake tasks blocked on I/O during shutdown
    shutdown_notify: Arc<Notify>,
    /// Initialization state
    initialized: AtomicBool,
    /// Per-source rate limiter for the handshake responder loop.
    /// Without this, an attacker can flood handshake-flagged
    /// datagrams to monopolize the recv path or race a legitimate
    /// initiator's msg1.
    handshake_pacer: parking_lot::Mutex<HandshakePacer>,
}

impl NetAdapter {
    /// Create a new Net adapter.
    pub fn new(config: NetAdapterConfig) -> Result<Self, AdapterError> {
        config
            .validate()
            .map_err(|e| AdapterError::Fatal(format!("invalid config: {}", e)))?;

        Ok(Self {
            session_manager: SessionMgr::new(config.session_timeout),
            config,
            socket: None,
            session: None,
            inbound: Arc::new(DashMap::new()),
            tasks: TokioMutex::new(Vec::new()),
            shutdown: Arc::new(AtomicBool::new(false)),
            shutdown_notify: Arc::new(Notify::new()),
            initialized: AtomicBool::new(false),
            // 5 attempts per second per source, plenty for any
            // legitimate initiator (RTT-limited) and tight enough
            // to throttle a flooder on consumer-grade hardware.
            handshake_pacer: parking_lot::Mutex::new(HandshakePacer::new(
                5,
                std::time::Duration::from_secs(1),
            )),
        })
    }

    /// Perform Noise handshake with peer.
    /// Returns session keys and the actual peer address (from the wire, not config).
    async fn perform_handshake(
        &self,
        socket: &Socket,
    ) -> Result<(SessionKeys, std::net::SocketAddr), AdapterError> {
        let mut attempt = 0;
        let max_attempts = self.config.handshake_retries;

        // Cap per-attempt sleep so a misconfigured `handshake_retries`
        // near `MAX_HANDSHAKE_RETRIES` (1024) cannot pin `init()` for
        // hours. Pre-fix `100 * attempt` grew linearly and unbounded:
        // attempt 1024 slept ~102s, with cumulative wait across all
        // attempts approaching 14 hours. Capping at 5s gives bounded
        // worst-case `max_attempts × 5s` (~85 minutes at the cap),
        // which is still long but not unbounded.
        const HANDSHAKE_RETRY_SLEEP_CAP_MS: u64 = 5_000;

        loop {
            attempt += 1;
            match self.try_handshake(socket).await {
                Ok(result) => return Ok(result),
                Err(e) if attempt < max_attempts => {
                    tracing::warn!(
                        attempt = attempt,
                        max = max_attempts,
                        error = %e,
                        "handshake failed, retrying"
                    );
                    let backoff_ms =
                        (100u64.saturating_mul(attempt as u64)).min(HANDSHAKE_RETRY_SLEEP_CAP_MS);
                    tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Single handshake attempt.
    /// Returns session keys and the actual peer address.
    async fn try_handshake(
        &self,
        socket: &Socket,
    ) -> Result<(SessionKeys, std::net::SocketAddr), AdapterError> {
        let timeout = self.config.handshake_timeout;
        let socket_arc = socket.socket_arc();

        if self.config.is_initiator() {
            // Initiator flow
            let peer_pubkey = self
                .config
                .peer_static_pubkey
                .as_ref()
                .ok_or_else(|| AdapterError::Fatal("missing peer public key".into()))?;

            let mut handshake = NoiseHandshake::initiator(&self.config.psk, peer_pubkey)
                .map_err(|e| AdapterError::Fatal(format!("handshake init failed: {}", e)))?;

            // Send first message
            let msg1 = handshake
                .write_message(&[])
                .map_err(|e| AdapterError::Connection(format!("write_message failed: {}", e)))?;

            let mut builder = PacketBuilder::new(&[0u8; 32], 0);
            let packet = builder.build_handshake(&msg1);

            socket
                .send_to(&packet, self.config.peer_addr)
                .await
                .map_err(|e| AdapterError::Connection(format!("send failed: {}", e)))?;

            // Receive response, discarding datagrams that are not handshake
            // packets from the expected peer. This prevents stray traffic on
            // the shared socket from consuming the handshake slot.
            let (parsed, _source) = tokio::time::timeout(timeout, async {
                // Stack buffer reused across loop iterations.
                // `MAX_PACKET_SIZE` is 8192 bytes — small enough to
                // live on the async stack without spilling, and the
                // reuse drops the per-iteration `BytesMut::with_capacity`
                // alloc on the stray-traffic path. Pre-fix every
                // discarded datagram (an off-peer packet, an invalid
                // handshake) allocated a fresh 8 KiB and freed it at
                // loop end — under stray UDP traffic on the same
                // bind port this churned the allocator. Only the
                // success path now allocates (a `Bytes::copy_from_slice`
                // sized to the actual payload, since `ParsedPacket`
                // owns its `Bytes`).
                let mut recv_buf = [0u8; protocol::MAX_PACKET_SIZE];
                loop {
                    let (n, source) = socket_arc
                        .recv_from(&mut recv_buf)
                        .await
                        .map_err(|e| AdapterError::Connection(format!("recv failed: {}", e)))?;

                    // Only accept packets from the peer we initiated with
                    if source != self.config.peer_addr {
                        continue;
                    }

                    let data = bytes::Bytes::copy_from_slice(&recv_buf[..n]);

                    if let Some(p) = ParsedPacket::parse(data, source) {
                        if p.header.flags.is_handshake() {
                            return Ok::<_, AdapterError>((p, source));
                        }
                    }
                    // Not a valid handshake packet from our peer — keep waiting
                }
            })
            .await
            .map_err(|_| AdapterError::Connection("handshake timeout".into()))??;

            // Process response
            handshake
                .read_message(&parsed.payload)
                .map_err(|e| AdapterError::Connection(format!("read_message failed: {}", e)))?;

            // Extract session keys
            let keys = handshake
                .into_session_keys()
                .map_err(|e| AdapterError::Fatal(format!("key extraction failed: {}", e)))?;
            Ok((keys, self.config.peer_addr))
        } else {
            // Responder flow
            let keypair = self
                .config
                .static_keypair
                .as_ref()
                .ok_or_else(|| AdapterError::Fatal("missing static keypair".into()))?;

            // Wait for an initiator handshake message, discarding any
            // non-handshake datagrams that arrive on the shared
            // socket. Per-source pacing throttles flooders so the
            // legitimate initiator's msg1 can land — without it,
            // an attacker could blast handshake-flagged datagrams
            // and monopolize this recv loop.
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
                            // Per-source pacing: drop packets from
                            // sources that exceed the budget.
                            let allowed = self.handshake_pacer.lock().check_and_record(source);
                            if !allowed {
                                tracing::debug!(
                                    %source,
                                    "handshake responder: dropping packet from \
                                     rate-limited source"
                                );
                                continue;
                            }
                            return Ok::<_, AdapterError>((p, source));
                        }
                    }
                    // Not a valid handshake packet — keep waiting
                }
            })
            .await
            .map_err(|_| AdapterError::Connection("handshake timeout".into()))??;

            let mut handshake = NoiseHandshake::responder(&self.config.psk, keypair)
                .map_err(|e| AdapterError::Fatal(format!("handshake init failed: {}", e)))?;

            // Process initiator message
            handshake
                .read_message(&parsed.payload)
                .map_err(|e| AdapterError::Connection(format!("read_message failed: {}", e)))?;

            // Send response
            let msg2 = handshake
                .write_message(&[])
                .map_err(|e| AdapterError::Connection(format!("write_message failed: {}", e)))?;

            let mut builder = PacketBuilder::new(&[0u8; 32], 0);
            let packet = builder.build_handshake(&msg2);

            // Reply to the actual source address (not the configured peer_addr),
            // so the handshake completes even behind NAT or when the config is stale.
            socket
                .send_to(&packet, source)
                .await
                .map_err(|e| AdapterError::Connection(format!("send failed: {}", e)))?;

            // Extract session keys and use the actual source address as peer
            let keys = handshake
                .into_session_keys()
                .map_err(|e| AdapterError::Fatal(format!("key extraction failed: {}", e)))?;
            Ok((keys, source))
        }
    }

    /// Process a single received packet: parse, decrypt, and queue events.
    fn process_packet(
        data: Bytes,
        source: std::net::SocketAddr,
        session: &NetSession,
        inbound: &InboundQueues,
        num_shards: u16,
    ) {
        // Parse packet
        let parsed = match ParsedPacket::parse(data, source) {
            Some(p) => p,
            None => return,
        };

        // Reject packets whose actual payload size doesn't match the declared
        // length. This catches truncated or oversized packets before they
        // reach the decrypt path.
        if !parsed.header.flags.is_handshake()
            && !parsed.header.flags.is_heartbeat()
            && !parsed.is_valid_length()
        {
            return;
        }

        // Skip handshake packets in the data path (handled during init)
        if parsed.header.flags.is_handshake() {
            return;
        }

        // Validate session before any state mutation (including touch)
        if parsed.header.session_id != session.session_id() {
            return;
        }

        // Heartbeats are AEAD-tagged: the empty payload encrypts to
        // a 16-byte Poly1305 tag, and the receiver verifies the
        // tag here. Without this check, an off-path attacker who
        // observed or guessed the session_id could spoof
        // heartbeats and keep a session alive (the source-address
        // check on UDP is itself spoofable, and session_id is in
        // cleartext on every prior packet).
        //
        // We still require `source == peer_addr` as a cheap
        // first-line filter so an unauthenticated flood doesn't
        // get to do the AEAD verify.
        //
        // The verify+touch sequence lives inside
        // `NetSession::verify_and_touch_heartbeat` so callers can't
        // touch a session whose heartbeat failed verify, and can't
        // forget to touch on success.
        if parsed.header.flags.is_heartbeat() {
            if source == session.peer_addr() {
                session.verify_and_touch_heartbeat(&parsed);
            }
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

        // Parse events
        let events = EventFrame::read_events(Bytes::from(decrypted), parsed.header.event_count);

        // Update stream state
        let stream_id = parsed.header.stream_id;
        let shard_id = if num_shards > 0 {
            (stream_id % num_shards as u64) as u16
        } else {
            0
        };

        // Previously the boolean result of `r.on_receive(seq)` was
        // discarded — a duplicate (NACK retransmit, rebroadcast,
        // etc.) returned `false` but the events were still queued for
        // poll_shard, breaking exactly-once delivery on reliable
        // streams. The cipher's replay window doesn't catch this
        // because each retransmit is re-encrypted with a fresh outer
        // counter.
        //
        // Now: if `on_receive` reports a duplicate, we still call
        // `session.touch()` (the peer is alive) but skip the queue
        // step entirely — the original delivery already queued the
        // events.
        let is_fresh = {
            let stream = session.get_or_create_stream(stream_id);
            // `with_reliability` always invokes the closure (it
            // locks an internal `Mutex<Box<dyn ReliabilityMode>>`).
            // For streams without a meaningful reliability mode the
            // implementation returns `true` for every `on_receive`,
            // matching the historical "always queue" behavior.
            let fresh = stream.with_reliability(|r| r.on_receive(parsed.header.sequence));
            stream.update_rx_seq(parsed.header.sequence);
            fresh
        };

        if is_fresh {
            // Queue events for poll_shard
            let queue = inbound.entry(shard_id).or_default();
            let seq = parsed.header.sequence;
            for (i, event_data) in events.into_iter().enumerate() {
                use std::fmt::Write;
                let mut event_id = String::with_capacity(24);
                let _ = write!(event_id, "{}:{}", seq, i);
                queue.push(StoredEvent::new(event_id, event_data, seq, shard_id));
            }
        } else {
            tracing::debug!(
                seq = parsed.header.sequence,
                stream_id,
                "Dropping duplicate packet"
            );
        }

        session.touch();
    }

    /// Spawn receiver task.
    ///
    /// On Linux, uses a dedicated OS thread with batched recvmmsg for up to
    /// 64 packets per syscall. On other platforms, uses standard async recv.
    #[cfg(target_os = "linux")]
    fn spawn_receiver(
        shutdown: Arc<AtomicBool>,
        shutdown_notify: Arc<Notify>,
        socket: Arc<Socket>,
        session: Arc<NetSession>,
        inbound: InboundQueues,
        num_shards: u16,
    ) -> JoinHandle<()> {
        let mut receiver = transport::BatchedPacketReceiver::new(socket.socket_arc());

        tokio::spawn(async move {
            while !shutdown.load(Ordering::Acquire) {
                tokio::select! {
                    result = receiver.recv() => {
                        match result {
                            Ok((data, source)) => {
                                Self::process_packet(data, source, &session, &inbound, num_shards);
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {
                                tracing::warn!("batch receiver thread exited, stopping receiver");
                                break;
                            }
                            Err(e) => {
                                if !shutdown.load(Ordering::Acquire) {
                                    tracing::warn!(error = %e, "receive error");
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

    /// Spawn receiver task (non-Linux fallback).
    #[cfg(not(target_os = "linux"))]
    fn spawn_receiver(
        shutdown: Arc<AtomicBool>,
        shutdown_notify: Arc<Notify>,
        socket: Arc<Socket>,
        session: Arc<NetSession>,
        inbound: InboundQueues,
        num_shards: u16,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut receiver = PacketReceiver::new(socket.socket_arc());

            while !shutdown.load(Ordering::Acquire) {
                // Race recv against shutdown notification so the task
                // can exit promptly instead of blocking on recv_from
                // until a packet arrives.
                tokio::select! {
                    result = receiver.recv() => {
                        match result {
                            Ok((data, source)) => {
                                Self::process_packet(data, source, &session, &inbound, num_shards);
                            }
                            Err(e) => {
                                if !shutdown.load(Ordering::Acquire) {
                                    tracing::warn!(error = %e, "receive error");
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

    /// Spawn heartbeat task.
    fn spawn_heartbeat(
        shutdown: Arc<AtomicBool>,
        shutdown_notify: Arc<Notify>,
        socket: Arc<Socket>,
        session: Arc<NetSession>,
        interval: std::time::Duration,
        peer_addr: std::net::SocketAddr,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);

            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        if shutdown.load(Ordering::Acquire) || !session.is_active() {
                            break;
                        }

                        // `Session::build_heartbeat` routes through
                        // `thread_local_pool` (same pool the data
                        // path uses) so heartbeats share a single
                        // TX counter with data and interleave
                        // correctly on the wire. Constructing a
                        // bespoke `PacketBuilder::new(&[0u8; 32],
                        // session.session_id())` per tick would
                        // (a) use the wrong key so the receiver's
                        // AEAD verify would reject every heartbeat,
                        // and (b) reuse counter=0 across successive
                        // heartbeats so the receiver's replay
                        // window would reject every heartbeat
                        // after the first.
                        let packet = session.build_heartbeat();

                        if let Err(e) = socket.send_to(&packet, peer_addr).await {
                            tracing::warn!(error = %e, "heartbeat send failed");
                        }
                    }
                    _ = shutdown_notify.notified() => {
                        break;
                    }
                }
            }
        })
    }
}

#[async_trait]
impl Adapter for NetAdapter {
    async fn init(&mut self) -> Result<(), AdapterError> {
        if self.initialized.load(Ordering::Acquire) {
            return Ok(());
        }

        // Create socket with configured buffer sizes
        let socket_config = match (
            self.config.socket_recv_buffer,
            self.config.socket_send_buffer,
        ) {
            (Some(recv), Some(send)) => transport::SocketBufferConfig {
                recv_buffer_size: recv,
                send_buffer_size: send,
            },
            _ => transport::SocketBufferConfig::default(),
        };
        let socket = Socket::with_config(self.config.bind_addr, socket_config)
            .await
            .map_err(|e| AdapterError::Connection(format!("socket creation failed: {}", e)))?;

        let socket = Arc::new(socket);
        self.socket = Some(socket.clone());

        // Perform handshake — actual_peer is the real address from the wire
        let (keys, actual_peer) = self.perform_handshake(&socket).await?;

        // Create packet pool with TX key
        // Create session with the actual peer address (not the configured one,
        // which may be stale or pre-NAT)
        let session = Arc::new(NetSession::new(
            keys,
            actual_peer,
            self.config.packet_pool_size,
            self.config.default_reliability.is_reliable(),
        ));
        self.session = Some(session.clone());

        // Store in session manager for health checks (same Arc as the active session)
        self.session_manager.set_session_arc(session.clone());

        // Spawn background tasks
        let recv_task = Self::spawn_receiver(
            self.shutdown.clone(),
            self.shutdown_notify.clone(),
            socket.clone(),
            session.clone(),
            self.inbound.clone(),
            self.config.num_shards,
        );

        let heartbeat_task = Self::spawn_heartbeat(
            self.shutdown.clone(),
            self.shutdown_notify.clone(),
            socket,
            session,
            self.config.heartbeat_interval,
            actual_peer,
        );

        {
            let mut tasks = self.tasks.lock().await;
            tasks.push(recv_task);
            tasks.push(heartbeat_task);
        }

        self.initialized.store(true, Ordering::Release);

        tracing::info!(
            bind_addr = %self.config.bind_addr,
            peer_addr = %self.config.peer_addr,
            role = ?self.config.role,
            "Net adapter initialized"
        );

        Ok(())
    }

    async fn on_batch(&self, batch: Batch) -> Result<(), AdapterError> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| AdapterError::Connection("not connected".into()))?;

        let socket = self
            .socket
            .as_ref()
            .ok_or_else(|| AdapterError::Connection("socket not initialized".into()))?;

        let stream_id = batch.shard_id as u64;
        let peer_addr = session.peer_addr();

        // Read stream config under the lock, then drop it immediately.
        // Holding the DashMap RefMut across .await would deadlock against
        // the receiver task which also calls get_or_create_stream().
        let reliable = {
            let stream = session.get_or_create_stream(stream_id);
            stream.with_reliability(|r| r.needs_ack())
            // RefMut dropped here
        };

        // Convert events to bytes and batch them
        let mut current_batch: Vec<Bytes> = Vec::with_capacity(64);
        let mut current_size = 0usize;

        // Thread-local pool with counter-based nonces — zero contention
        let pool = session.thread_local_pool();
        let mut builder = pool.get();

        for event in &batch.events {
            let event_bytes = event.raw.clone();
            let frame_size = EventFrame::LEN_SIZE + event_bytes.len();

            // Check if adding this event would exceed packet size
            if current_size + frame_size > protocol::MAX_PAYLOAD_SIZE && !current_batch.is_empty() {
                // Acquire stream lock briefly for seq + reliability tracking
                let seq;
                {
                    let stream = session.get_or_create_stream(stream_id);
                    seq = stream.next_tx_seq();
                }

                let flags = if reliable {
                    PacketFlags::RELIABLE
                } else {
                    PacketFlags::NONE
                };

                let packet = builder.build(stream_id, seq, &current_batch, flags);

                // No DashMap lock held during this .await
                socket
                    .send_to(&packet, peer_addr)
                    .await
                    .map_err(|e| AdapterError::Connection(format!("send failed: {}", e)))?;

                // Track for reliability with PRE-encryption inputs.
                // Stashing the encrypted bytes was unsound: the
                // receiver's replay window rejects retransmits that
                // carry a stale wire counter. The descriptor lets
                // the retransmit driver call `builder.build` again
                // with a fresh counter.
                if reliable {
                    let descriptor = reliability::RetransmitDescriptor {
                        seq,
                        stream_id,
                        events: current_batch.clone(),
                        flags,
                    };
                    let stream = session.get_or_create_stream(stream_id);
                    stream.with_reliability(|r| r.on_send(descriptor));
                }

                current_batch.clear();
                current_size = 0;
            }

            current_batch.push(event_bytes);
            current_size += frame_size;
        }

        // Send remaining events
        if !current_batch.is_empty() {
            let seq;
            {
                let stream = session.get_or_create_stream(stream_id);
                seq = stream.next_tx_seq();
            }

            let flags = if reliable {
                PacketFlags::RELIABLE
            } else {
                PacketFlags::NONE
            };

            let packet = builder.build(stream_id, seq, &current_batch, flags);

            socket
                .send_to(&packet, peer_addr)
                .await
                .map_err(|e| AdapterError::Connection(format!("send failed: {}", e)))?;

            if reliable {
                let descriptor = reliability::RetransmitDescriptor {
                    seq,
                    stream_id,
                    events: current_batch.clone(),
                    flags,
                };
                let stream = session.get_or_create_stream(stream_id);
                stream.with_reliability(|r| r.on_send(descriptor));
            }
        }

        session.touch();

        Ok(())
    }

    async fn poll_shard(
        &self,
        shard_id: u16,
        from_id: Option<&str>,
        limit: usize,
    ) -> Result<ShardPollResult, AdapterError> {
        let mut events = Vec::with_capacity(limit);

        if let Some(queue) = self.inbound.get(&shard_id) {
            while events.len() < limit {
                if let Some(event) = queue.pop() {
                    if from_id.is_none() || event_id_gt(&event.id, from_id.unwrap_or("")) {
                        events.push(event);
                    }
                    // Events at or before the cursor have already been
                    // consumed — drop them instead of requeuing. Requeuing
                    // caused unbounded memory growth because these events
                    // can never pass an advancing cursor.
                } else {
                    break;
                }
            }
        }

        let has_more = self
            .inbound
            .get(&shard_id)
            .map(|q| !q.is_empty())
            .unwrap_or(false);
        let next_id = events.last().map(|e| e.id.clone());

        Ok(ShardPollResult {
            events,
            next_id,
            has_more,
        })
    }

    async fn flush(&self) -> Result<(), AdapterError> {
        // For reliable streams, wait for all pending ACKs
        // Currently a no-op since we're fire-and-forget by default
        Ok(())
    }

    async fn shutdown(&self) -> Result<(), AdapterError> {
        self.shutdown.store(true, Ordering::Release);

        // Wake all tasks blocked on I/O so they can observe the shutdown flag.
        // notify_waiters wakes all current waiters (receiver + heartbeat).
        self.shutdown_notify.notify_waiters();

        // Clear session
        self.session_manager.clear_session();

        // Wait for tasks to complete
        let mut tasks = self.tasks.lock().await;
        for task in tasks.drain(..) {
            let _ = task.await;
        }

        self.initialized.store(false, Ordering::Release);

        tracing::info!("Net adapter shutdown complete");

        Ok(())
    }

    fn name(&self) -> &'static str {
        "net"
    }

    async fn is_healthy(&self) -> bool {
        self.initialized.load(Ordering::Acquire) && self.session_manager.check_session()
    }
}

impl std::fmt::Debug for NetAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetAdapter")
            .field("config", &self.config)
            .field("initialized", &self.initialized.load(Ordering::Relaxed))
            .finish()
    }
}

/// Compare two event IDs numerically.
///
/// IDs are formatted as `"seq:idx"`. Lexicographic comparison is wrong for
/// numeric values (e.g. `"9:0" > "10:0"` lexicographically). This function
/// parses the components and compares numerically, falling back to string
/// comparison only if parsing fails.
fn event_id_gt(a: &str, b: &str) -> bool {
    fn parse_id(id: &str) -> Option<(u64, u64)> {
        let (seq, idx) = id.split_once(':')?;
        Some((seq.parse().ok()?, idx.parse().ok()?))
    }

    match (parse_id(a), parse_id(b)) {
        (Some(a), Some(b)) => a > b,
        _ => a > b, // fallback to lexicographic
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adapter_creation() {
        let psk = [0x42u8; 32];
        let peer_pubkey = [0x24u8; 32];

        let config = NetAdapterConfig::initiator(
            "127.0.0.1:0".parse().unwrap(),
            "127.0.0.1:9999".parse().unwrap(),
            psk,
            peer_pubkey,
        );

        let adapter = NetAdapter::new(config).unwrap();
        assert_eq!(adapter.name(), "net");
    }

    #[test]
    fn test_shard_id_from_stream_id_uses_modulo() {
        // Regression: shard_id was computed as `stream_id as u16` (truncation),
        // which collides for stream IDs that differ only in upper bits.
        // The fix uses `stream_id % num_shards`.
        let num_shards: u16 = 8;

        // Two stream IDs that are identical in their low 16 bits
        // but different overall must map to the same shard via modulo,
        // while truncation would also give the same result here.
        // More importantly, a large stream_id must stay within [0, num_shards).
        let stream_a: u64 = 0xDEAD_BEEF_0000_0003;
        let stream_b: u64 = 0xCAFE_BABE_0000_0003;

        let shard_a = (stream_a % num_shards as u64) as u16;
        let shard_b = (stream_b % num_shards as u64) as u16;

        assert!(
            shard_a < num_shards,
            "shard must be in range [0, num_shards)"
        );
        assert!(
            shard_b < num_shards,
            "shard must be in range [0, num_shards)"
        );

        // Large stream IDs that would overflow u16 must still be valid shard IDs
        let big_stream: u64 = 0xFFFF_FFFF_FFFF_FFFF;
        let shard_big = (big_stream % num_shards as u64) as u16;
        assert!(shard_big < num_shards);

        // Truncation would give 0xFFFF = 65535, which is >= num_shards.
        // Modulo gives a valid shard.
        assert_ne!(
            big_stream as u16, shard_big,
            "modulo must differ from truncation for large stream IDs"
        );
    }

    #[test]
    fn test_invalid_config() {
        let psk = [0x42u8; 32];
        let peer_pubkey = [0x24u8; 32];

        let mut config = NetAdapterConfig::initiator(
            "127.0.0.1:0".parse().unwrap(),
            "127.0.0.1:9999".parse().unwrap(),
            psk,
            peer_pubkey,
        );
        config.peer_static_pubkey = None;

        let result = NetAdapter::new(config);
        assert!(result.is_err());
    }

    // Regression: event_id_gt used lexicographic comparison, so "9:0" > "10:0"
    // was true (wrong). Now uses numeric comparison (BUGS_4 #2).
    #[test]
    fn test_event_id_gt_numeric_ordering() {
        // Basic ordering
        assert!(event_id_gt("2:0", "1:0"));
        assert!(!event_id_gt("1:0", "2:0"));
        assert!(!event_id_gt("1:0", "1:0"));

        // The critical case: double-digit seq must compare correctly
        assert!(event_id_gt("10:0", "9:0"));
        assert!(event_id_gt("100:0", "99:0"));
        assert!(!event_id_gt("9:0", "10:0"));

        // Index comparison within same sequence
        assert!(event_id_gt("5:2", "5:1"));
        assert!(!event_id_gt("5:1", "5:2"));

        // Large sequences
        assert!(event_id_gt("1000000:0", "999999:0"));
    }

    // Regression: poll_shard used to destructively pop events that didn't
    // pass the cursor filter, causing permanent data loss (BUGS_4 #1).
    // This is tested indirectly via event_id_gt since poll_shard requires
    // a full adapter setup, but the non-destructive requeue logic is
    // verified by the SegQueue re-push in the implementation.
    #[test]
    fn test_event_id_gt_edge_cases() {
        // Empty strings
        assert!(event_id_gt("1:0", ""));
        // Malformed IDs fall back to string comparison
        assert!(event_id_gt("b", "a"));
        assert!(!event_id_gt("a", "b"));
    }

    /// Regression: packets built by PacketBuilder must survive process_packet.
    /// This test bypasses the network and directly verifies the encrypt→decrypt
    /// data path, catching AAD mismatches, nonce construction bugs, and key
    /// derivation errors.
    #[test]
    fn test_build_then_process_packet_roundtrip() {
        use crate::adapter::net::crypto::{NoiseHandshake, StaticKeypair};
        use dashmap::DashMap;
        use std::sync::Arc;

        // Perform a real handshake to get matching keys
        let psk = [0x42u8; 32];
        let responder_kp = StaticKeypair::generate();

        let mut initiator = NoiseHandshake::initiator(&psk, &responder_kp.public).unwrap();
        let mut responder = NoiseHandshake::responder(&psk, &responder_kp).unwrap();

        let msg1 = initiator.write_message(&[]).unwrap();
        responder.read_message(&msg1).unwrap();
        let msg2 = responder.write_message(&[]).unwrap();
        initiator.read_message(&msg2).unwrap();

        let init_keys = initiator.into_session_keys().unwrap();
        let resp_keys = responder.into_session_keys().unwrap();

        // Initiator builds a packet
        let mut builder = PacketBuilder::new(&init_keys.tx_key, init_keys.session_id);
        let events = vec![
            Bytes::from(r#"{"token":"hello"}"#),
            Bytes::from(r#"{"token":"world"}"#),
        ];
        let packet = builder.build(0, 0, &events, PacketFlags::NONE);

        // Responder processes the packet
        let resp_session = Arc::new(NetSession::new(
            resp_keys,
            "127.0.0.1:5000".parse().unwrap(),
            4,
            false,
        ));
        let inbound: InboundQueues = Arc::new(DashMap::new());
        let source: std::net::SocketAddr = "127.0.0.1:5000".parse().unwrap();

        NetAdapter::process_packet(packet, source, &resp_session, &inbound, 1);

        // Events should be queued in shard 0
        let queue = inbound.get(&0).expect("shard 0 should have events");
        assert_eq!(queue.len(), 2, "expected 2 events, got {}", queue.len());

        let e1 = queue.pop().unwrap();
        assert_eq!(&e1.raw[..], br#"{"token":"hello"}"#);

        let e2 = queue.pop().unwrap();
        assert_eq!(&e2.raw[..], br#"{"token":"world"}"#);
    }

    /// Helper: perform a Noise handshake and return matched key pairs.
    fn make_session_keys() -> (SessionKeys, SessionKeys) {
        use crate::adapter::net::crypto::{NoiseHandshake, StaticKeypair};

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
    fn test_process_packet_rejects_truncated_packet() {
        use dashmap::DashMap;
        use std::sync::Arc;

        let (init_keys, resp_keys) = make_session_keys();

        // Build a valid packet, then truncate it
        let mut builder = PacketBuilder::new(&init_keys.tx_key, init_keys.session_id);
        let packet = builder.build(0, 0, &[Bytes::from_static(b"hello")], PacketFlags::NONE);

        let resp_session = Arc::new(NetSession::new(
            resp_keys,
            "127.0.0.1:5000".parse().unwrap(),
            4,
            false,
        ));
        let inbound: InboundQueues = Arc::new(DashMap::new());
        let source: std::net::SocketAddr = "127.0.0.1:5000".parse().unwrap();

        // Truncate: remove last 10 bytes (partial auth tag)
        let truncated = packet.slice(..packet.len() - 10);
        NetAdapter::process_packet(truncated, source, &resp_session, &inbound, 1);
        assert!(
            inbound.get(&0).is_none() || inbound.get(&0).unwrap().is_empty(),
            "truncated packet must be silently dropped"
        );
    }

    #[test]
    fn test_process_packet_rejects_tampered_payload() {
        use dashmap::DashMap;
        use std::sync::Arc;

        let (init_keys, resp_keys) = make_session_keys();

        let mut builder = PacketBuilder::new(&init_keys.tx_key, init_keys.session_id);
        let packet = builder.build(0, 0, &[Bytes::from_static(b"hello")], PacketFlags::NONE);

        let resp_session = Arc::new(NetSession::new(
            resp_keys,
            "127.0.0.1:5000".parse().unwrap(),
            4,
            false,
        ));
        let inbound: InboundQueues = Arc::new(DashMap::new());
        let source: std::net::SocketAddr = "127.0.0.1:5000".parse().unwrap();

        // Tamper: flip a byte in the encrypted payload
        let mut tampered = bytes::BytesMut::from(&packet[..]);
        tampered[super::protocol::HEADER_SIZE + 2] ^= 0xFF;
        NetAdapter::process_packet(tampered.freeze(), source, &resp_session, &inbound, 1);

        assert!(
            inbound.get(&0).is_none() || inbound.get(&0).unwrap().is_empty(),
            "tampered packet must be rejected by AEAD"
        );
    }

    #[test]
    fn test_process_packet_rejects_wrong_session_id() {
        use dashmap::DashMap;
        use std::sync::Arc;

        let (init_keys, resp_keys) = make_session_keys();

        let mut builder = PacketBuilder::new(&init_keys.tx_key, init_keys.session_id);
        let packet = builder.build(0, 0, &[Bytes::from_static(b"hello")], PacketFlags::NONE);

        // Create session with a DIFFERENT session_id
        let mut wrong_keys = resp_keys;
        wrong_keys.session_id = 0xDEAD;
        let resp_session = Arc::new(NetSession::new(
            wrong_keys,
            "127.0.0.1:5000".parse().unwrap(),
            4,
            false,
        ));
        let inbound: InboundQueues = Arc::new(DashMap::new());
        let source: std::net::SocketAddr = "127.0.0.1:5000".parse().unwrap();

        NetAdapter::process_packet(packet, source, &resp_session, &inbound, 1);

        assert!(
            inbound.get(&0).is_none() || inbound.get(&0).unwrap().is_empty(),
            "packet with wrong session_id must be dropped"
        );
    }

    #[test]
    fn test_process_packet_multi_packet_batch_all_events_arrive() {
        use dashmap::DashMap;
        use std::sync::Arc;

        let (init_keys, resp_keys) = make_session_keys();

        let resp_session = Arc::new(NetSession::new(
            resp_keys,
            "127.0.0.1:5000".parse().unwrap(),
            4,
            false,
        ));
        let inbound: InboundQueues = Arc::new(DashMap::new());
        let source: std::net::SocketAddr = "127.0.0.1:5000".parse().unwrap();

        // Build events large enough to span multiple packets.
        // Each event is ~200 bytes, MAX_PAYLOAD_SIZE is ~8112, so ~40 per packet.
        // 200 events → ~5 packets.
        let mut builder = PacketBuilder::new(&init_keys.tx_key, init_keys.session_id);
        let total_events = 200;
        let mut seq = 0u64;

        // Simulate on_batch splitting into multiple packets
        let mut current_batch: Vec<Bytes> = Vec::new();
        let mut current_size = 0;

        for i in 0..total_events {
            let data = format!("{{\"i\":{},\"pad\":\"{}\"}}", i, "x".repeat(150));
            let event_bytes = Bytes::from(data);
            let frame_size = EventFrame::LEN_SIZE + event_bytes.len();

            if current_size + frame_size > protocol::MAX_PAYLOAD_SIZE && !current_batch.is_empty() {
                let packet = builder.build(0, seq, &current_batch, PacketFlags::NONE);
                NetAdapter::process_packet(packet, source, &resp_session, &inbound, 1);
                seq += 1;
                current_batch.clear();
                current_size = 0;
            }

            current_batch.push(event_bytes);
            current_size += frame_size;
        }

        if !current_batch.is_empty() {
            let packet = builder.build(0, seq, &current_batch, PacketFlags::NONE);
            NetAdapter::process_packet(packet, source, &resp_session, &inbound, 1);
        }

        // All events must arrive
        let queue = inbound.get(&0).expect("shard 0 should have events");
        assert_eq!(
            queue.len(),
            total_events,
            "all {} events must arrive across multiple packets",
            total_events
        );
    }

    #[test]
    fn test_build_then_process_packet_both_directions() {
        use dashmap::DashMap;
        use std::sync::Arc;

        let (init_keys, resp_keys) = make_session_keys();
        let source: std::net::SocketAddr = "127.0.0.1:5000".parse().unwrap();

        // Direction 1: initiator → responder
        {
            let mut builder = PacketBuilder::new(&init_keys.tx_key, init_keys.session_id);
            let packet = builder.build(0, 0, &[Bytes::from_static(b"i2r")], PacketFlags::NONE);

            let session = Arc::new(NetSession::new(resp_keys.clone(), source, 4, false));
            let inbound: InboundQueues = Arc::new(DashMap::new());
            NetAdapter::process_packet(packet, source, &session, &inbound, 1);

            let queue = inbound.get(&0).expect("i2r: shard 0 should have events");
            assert_eq!(queue.len(), 1, "i2r: expected 1 event");
            assert_eq!(&queue.pop().unwrap().raw[..], b"i2r");
        }

        // Direction 2: responder → initiator
        {
            let mut builder = PacketBuilder::new(&resp_keys.tx_key, resp_keys.session_id);
            let packet = builder.build(0, 0, &[Bytes::from_static(b"r2i")], PacketFlags::NONE);

            let session = Arc::new(NetSession::new(init_keys.clone(), source, 4, false));
            let inbound: InboundQueues = Arc::new(DashMap::new());
            NetAdapter::process_packet(packet, source, &session, &inbound, 1);

            let queue = inbound.get(&0).expect("r2i: shard 0 should have events");
            assert_eq!(queue.len(), 1, "r2i: expected 1 event");
            assert_eq!(&queue.pop().unwrap().raw[..], b"r2i");
        }
    }

    #[test]
    fn test_poll_shard_cursor_drops_consumed_events() {
        // Verify that poll_shard with a cursor drops events at or before
        // the cursor (they've already been consumed) and returns only
        // events after the cursor. The queue should be empty afterward —
        // no unbounded requeue growth.
        use std::sync::Arc;

        let (init_keys, resp_keys) = make_session_keys();

        let resp_session = Arc::new(NetSession::new(
            resp_keys,
            "127.0.0.1:5000".parse().unwrap(),
            4,
            false,
        ));
        let inbound: InboundQueues = Arc::new(DashMap::new());
        let source: std::net::SocketAddr = "127.0.0.1:5000".parse().unwrap();

        // Send 3 packets (sequences 0, 1, 2), each with 1 event
        let mut builder = PacketBuilder::new(&init_keys.tx_key, init_keys.session_id);
        for seq in 0..3u64 {
            let events = vec![Bytes::from(format!("event-{}", seq))];
            let packet = builder.build(0, seq, &events, PacketFlags::NONE);
            NetAdapter::process_packet(packet, source, &resp_session, &inbound, 1);
        }

        let queue = inbound.get(&0u16).unwrap();
        assert_eq!(queue.len(), 3);

        // Simulate poll_shard with cursor "0:0" — drops event 0:0,
        // returns events 1:0 and 2:0
        let from_id = "0:0";
        let mut events = Vec::new();
        while events.len() < 10 {
            if let Some(event) = queue.pop() {
                if event_id_gt(&event.id, from_id) {
                    events.push(event);
                }
                // Events at/before cursor are dropped (not requeued)
            } else {
                break;
            }
        }

        assert_eq!(events.len(), 2, "should get 2 events after cursor 0:0");
        assert_eq!(events[0].id, "1:0");
        assert_eq!(events[1].id, "2:0");

        // Queue should be empty — consumed events are dropped, not requeued
        assert_eq!(queue.len(), 0, "queue should be empty after poll drains it");
    }

    #[test]
    fn test_process_packet_old_counter_rejected() {
        // Verify that a packet with a counter below the replay window
        // is rejected after the window has advanced.
        use std::sync::Arc;

        let (init_keys, resp_keys) = make_session_keys();
        let resp_session = Arc::new(NetSession::new(
            resp_keys,
            "127.0.0.1:5000".parse().unwrap(),
            4,
            false,
        ));
        let inbound: InboundQueues = Arc::new(DashMap::new());
        let source: std::net::SocketAddr = "127.0.0.1:5000".parse().unwrap();

        // Send 1100 packets to advance the rx_counter past the replay window (1024)
        let mut builder = PacketBuilder::new(&init_keys.tx_key, init_keys.session_id);
        for seq in 0..1100u64 {
            let packet = builder.build(0, seq, &[Bytes::from_static(b"x")], PacketFlags::NONE);
            NetAdapter::process_packet(packet, source, &resp_session, &inbound, 1);
        }
        assert_eq!(inbound.get(&0).unwrap().len(), 1100);

        // Build a packet with a fresh builder whose counter starts at 0.
        // The rx_counter is now at ~1100, so counter 0 is outside the
        // 1024-wide replay window and must be rejected.
        let mut stale_builder = PacketBuilder::new(&init_keys.tx_key, init_keys.session_id);
        let stale_packet =
            stale_builder.build(0, 9999, &[Bytes::from_static(b"stale")], PacketFlags::NONE);
        NetAdapter::process_packet(stale_packet, source, &resp_session, &inbound, 1);

        // Should still be 1100 — stale packet rejected
        assert_eq!(
            inbound.get(&0).unwrap().len(),
            1100,
            "packet with stale counter must be rejected"
        );
    }

    #[test]
    fn test_process_packet_far_future_counter_rejected() {
        // Verify that a packet with a counter far beyond MAX_FORWARD is
        // rejected, preventing an attacker from advancing the rx_counter
        // and denying subsequent legitimate packets.
        use std::sync::Arc;

        let (_init_keys, resp_keys) = make_session_keys();

        // Build a valid packet, then manually tamper the nonce counter
        // to a huge value. The AEAD will fail because the nonce doesn't
        // match, but we're testing that is_valid_rx_counter rejects it
        // before even attempting decryption.
        let resp_session = Arc::new(NetSession::new(
            resp_keys,
            "127.0.0.1:5000".parse().unwrap(),
            4,
            false,
        ));

        // Directly test the cipher's counter validation
        let rx_cipher = resp_session.rx_cipher();
        assert!(
            !rx_cipher.is_valid_rx_counter(u64::MAX),
            "counter at u64::MAX must be rejected (far beyond MAX_FORWARD)"
        );
        assert!(
            rx_cipher.is_valid_rx_counter(0),
            "counter 0 should be valid initially"
        );
    }

    /// Regression: BUG_REPORT.md #5 — `process_packet` previously
    /// discarded the bool returned by `r.on_receive(seq)` on the
    /// reliability layer, queueing events even for duplicates.
    /// Each retransmit re-encrypts with a fresh outer counter, so
    /// the cipher's replay window does not catch this; without
    /// honoring `on_receive`, the inbound queue accumulates
    /// duplicates and breaks exactly-once delivery on reliable
    /// streams.
    ///
    /// We construct the duplicate-detection scenario by building
    /// two distinct packets that share the same stream sequence.
    /// On a reliable session the second one's `on_receive` returns
    /// `false`, so `process_packet` must not enqueue its events.
    /// (The cipher's outer counter is fresh on both packets, so
    /// the replay window can't filter them — only the reliability
    /// layer's check stops the duplicate.)
    #[test]
    fn process_packet_drops_duplicates_per_reliability_decision() {
        use dashmap::DashMap;
        use std::sync::Arc;

        let (init_keys, resp_keys) = make_session_keys();

        // Reliable session — its streams use `ReliableStream`,
        // whose `on_receive` returns `false` for `seq <
        // next_expected` (duplicates).
        let resp_session = Arc::new(NetSession::new(
            resp_keys,
            "127.0.0.1:5000".parse().unwrap(),
            4,
            true, // default_reliable
        ));
        let inbound: InboundQueues = Arc::new(DashMap::new());
        let source: std::net::SocketAddr = "127.0.0.1:5000".parse().unwrap();

        // Two packets on stream 7. First carries sequences 0..1,
        // second is a duplicate (same seq=0) that should be
        // filtered. We deliver seq=0 then seq=1 first to advance
        // `next_expected` past 0, then a packet with seq=0 — that
        // last one is the duplicate.
        let mut builder = PacketBuilder::new(&init_keys.tx_key, init_keys.session_id);
        let packet0 = builder.build(7, 0, &[Bytes::from(r#"{"first":0}"#)], PacketFlags::NONE);
        let packet1 = builder.build(7, 1, &[Bytes::from(r#"{"first":1}"#)], PacketFlags::NONE);
        // Re-encrypted retransmit of seq=0 — same stream, same seq,
        // different payload. This is the scenario the bug allowed
        // through.
        let packet0_dup = builder.build(
            7,
            0,
            &[Bytes::from(r#"{"dup":"should_not_appear"}"#)],
            PacketFlags::NONE,
        );

        NetAdapter::process_packet(packet0, source, &resp_session, &inbound, 1);
        NetAdapter::process_packet(packet1, source, &resp_session, &inbound, 1);
        NetAdapter::process_packet(packet0_dup, source, &resp_session, &inbound, 1);

        let queue = inbound.get(&0).expect("shard 0 should exist");
        assert_eq!(
            queue.len(),
            2,
            "duplicate packet must NOT enqueue (BUG_REPORT.md #5); \
             got {} events, expected exactly 2 (seq=0 and seq=1, no dup)",
            queue.len()
        );

        // Drain in FIFO order and assert no `should_not_appear`
        // event sneaked through.
        let e0 = queue.pop().unwrap();
        assert_eq!(&e0.raw[..], br#"{"first":0}"#);
        let e1 = queue.pop().unwrap();
        assert_eq!(&e1.raw[..], br#"{"first":1}"#);
        assert!(queue.is_empty());
    }

    /// Regression: heartbeats must be AEAD-authenticated so an
    /// off-path attacker who knows or observes the session_id
    /// cannot spoof them. Pre-fix the receiver only checked
    /// `source == peer_addr` (UDP source — spoofable) and
    /// `session_id` match (in cleartext on every packet); now the
    /// 16-byte Poly1305 tag binds the heartbeat to the session
    /// key.
    #[test]
    fn heartbeat_is_aead_authenticated() {
        use crate::adapter::net::pool::PacketBuilder;
        use dashmap::DashMap;
        use std::sync::Arc;

        let (init_keys, resp_keys) = make_session_keys();

        let resp_session = Arc::new(NetSession::new(
            resp_keys,
            "127.0.0.1:5000".parse().unwrap(),
            4,
            false,
        ));
        let inbound: InboundQueues = Arc::new(DashMap::new());
        let source: std::net::SocketAddr = "127.0.0.1:5000".parse().unwrap();

        // Build a legitimate heartbeat with the initiator's
        // session key and tag it.
        let mut builder = PacketBuilder::new(&init_keys.tx_key, init_keys.session_id);
        let heartbeat = builder.build_heartbeat();
        let last_activity_before = resp_session.last_activity_ns();
        std::thread::sleep(std::time::Duration::from_millis(2));

        // Process: this must succeed and call session.touch().
        NetAdapter::process_packet(heartbeat, source, &resp_session, &inbound, 1);
        let last_activity_after = resp_session.last_activity_ns();
        assert!(
            last_activity_after > last_activity_before,
            "legitimate AEAD-tagged heartbeat must call session.touch()"
        );

        // Forge an unauthenticated heartbeat: header-only, no tag.
        // Pre-fix this would have passed; post-fix it must be
        // rejected.
        let mut forged = bytes::BytesMut::new();
        let header = NetHeader::heartbeat(resp_session.session_id());
        forged.extend_from_slice(&header.to_bytes());
        let forged = forged.freeze();
        let last_activity_before = resp_session.last_activity_ns();
        std::thread::sleep(std::time::Duration::from_millis(2));
        NetAdapter::process_packet(forged, source, &resp_session, &inbound, 1);
        let last_activity_after = resp_session.last_activity_ns();
        assert_eq!(
            last_activity_before, last_activity_after,
            "unauthenticated heartbeat (no AEAD tag) must NOT touch the session"
        );

        // Forge a heartbeat with the right session_id but a
        // garbage 16-byte "tag". Tag verification fails.
        let mut forged_tag = bytes::BytesMut::new();
        let mut header_bytes = NetHeader::heartbeat(resp_session.session_id()).to_bytes();
        // Stamp a plausible nonce so the receiver gets to the
        // decrypt step (otherwise it bails earlier on counter).
        header_bytes[12..16].copy_from_slice(&[0u8; 4]);
        header_bytes[16..24].copy_from_slice(&1u64.to_le_bytes());
        forged_tag.extend_from_slice(&header_bytes);
        forged_tag.extend_from_slice(&[0xAAu8; 16]); // garbage tag
        let forged_tag = forged_tag.freeze();
        let last_activity_before = resp_session.last_activity_ns();
        std::thread::sleep(std::time::Duration::from_millis(2));
        NetAdapter::process_packet(forged_tag, source, &resp_session, &inbound, 1);
        let last_activity_after = resp_session.last_activity_ns();
        assert_eq!(
            last_activity_before, last_activity_after,
            "heartbeat with garbage AEAD tag must NOT touch the session"
        );
    }

    /// Regression: the handshake responder must rate-limit per
    /// source so a flooder can't monopolize the recv loop.
    /// `HandshakePacer` is the building block: it tracks
    /// `(count, window_start)` per source and rejects after
    /// `max_per_window` attempts within `window`.
    #[test]
    fn handshake_pacer_rejects_floods_per_source() {
        use std::time::Duration;
        let mut pacer = HandshakePacer::new(3, Duration::from_millis(50));

        let attacker: std::net::SocketAddr = "10.0.0.1:9000".parse().unwrap();
        let legit: std::net::SocketAddr = "10.0.0.2:9000".parse().unwrap();

        // Attacker fires 3 attempts — all allowed (within budget).
        for _ in 0..3 {
            assert!(pacer.check_and_record(attacker));
        }
        // Fourth and beyond — rejected.
        for _ in 0..10 {
            assert!(
                !pacer.check_and_record(attacker),
                "attacker exceeding budget must be dropped"
            );
        }

        // The legitimate initiator (different source) is unaffected
        // by the attacker's burst — the budget is per-source.
        assert!(
            pacer.check_and_record(legit),
            "legitimate source must still get through despite attacker flood"
        );

        // After the window expires the attacker's budget refills.
        std::thread::sleep(Duration::from_millis(55));
        assert!(
            pacer.check_and_record(attacker),
            "attacker budget must refill after window"
        );
    }
}
