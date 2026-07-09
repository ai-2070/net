//! Node.js bindings for Net event bus.
//!
//! Provides high-performance event ingestion and consumption for Node.js/TypeScript.

// Identity / capabilities / subnets ride the `net` feature as a
// single security unit — they share `adapter::net`'s subprotocol
// dispatch and operate together at runtime, so gating each module
// separately would only enable combinations that aren't meaningful.
#[cfg(feature = "dataforts")]
mod blob;
#[cfg(feature = "net")]
mod capabilities;
mod capability_aggregation;
#[cfg(any(
    feature = "meshdb",
    feature = "cortex",
    feature = "compute",
    feature = "groups",
    feature = "aggregator",
    feature = "publish",
))]
mod common;
#[cfg(feature = "compute")]
mod compute;
// Local consent surface — CapabilityId, ConsentPolicy, and the
// lock-protocol PinStore, graduated to net-mesh-sdk by the MCP
// bridge SDK plan's P0 and bound here in P2. Pure local-state
// primitives (no mesh dependency), so the feature pulls only the
// net-sdk dep itself.
#[cfg(feature = "consent")]
mod consent;
#[cfg(feature = "cortex")]
mod cortex;
#[cfg(feature = "net")]
mod gang;
#[cfg(feature = "groups")]
mod groups;
#[cfg(feature = "dataforts")]
mod transport;
// nRPC binding (B1: raw-bytes serve_rpc / call / call_streaming).
// Reuses the cortex feature gate because nRPC is part of the
// cortex / netdb feature unit.
#[cfg(feature = "net")]
mod identity;
#[cfg(feature = "cortex")]
mod mesh_rpc;
// MeshDB query layer (Node SDK slice 1: factory AST + in-memory
// ChainReader + async runner + Phase F cache options). Gated
// behind the binding's `meshdb` Cargo feature.
#[cfg(feature = "meshdb")]
mod meshdb;
// MeshOS daemon-author SDK (Phase 3 slice 1). Builds on `compute`
// for the `MeshDaemon` trait + `Identity` wrapper.
#[cfg(feature = "meshos")]
mod meshos;
// MCP bridge pure helpers — classify + lower_tool only (the bridge's
// forwarding/keychain internals are never bound).
#[cfg(feature = "mcp")]
mod mcp_helpers;
// The consent-gated capability gateway (search/describe/invoke over an
// embedded NetMesh node) + the caller payment flow. Behind `payments`.
#[cfg(feature = "payments")]
mod capability_gateway;
// JS signer callbacks bridged into the payments SchemeSigner seam (real-network
// settlement). Behind `payments`.
#[cfg(feature = "payments")]
mod payment_signer;
// The outbound HTTP-402 client (pay an external x402 HTTP API). Behind the
// opt-in `payments-http` (pulls reqwest via net-payments/http-facilitator).
#[cfg(feature = "payments-http")]
mod payment_http;
// Publish a node's OWN local tools as mesh capabilities (the inverse of `net
// wrap`), backed by a JS async tool handler. The free publish path + the
// prerequisite for a Node payment provider. Behind `publish`.
#[cfg(feature = "publish")]
mod publish;
// The provider-side payment surface: author `net.pricing.terms@1`
// (`buildPricingTerms`) + the `PaymentProvider` class (price + charge over one
// shared PaymentEngine). Behind `payments`; the provider class additionally
// needs `publish` (the tool-publish building blocks).
#[cfg(feature = "payments")]
mod payment_provider;
// Deck SDK — operator-side bindings (Phase 5 slice 1). Builds on
// `meshos` for the supervisor runtime accessors.
#[cfg(feature = "aggregator")]
mod aggregator;
#[cfg(feature = "deck")]
mod deck;
#[cfg(feature = "net")]
mod placement;
#[cfg(feature = "redis")]
mod redis_dedup;
#[cfg(feature = "net")]
mod subnets;
#[cfg(feature = "tool")]
mod tool;

use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::sync::Arc;

use arc_swap::ArcSwapOption;

use net::{
    config::{AdapterConfig, BackpressureMode, EventBusConfig},
    consumer::Ordering,
    event::RawEvent,
    ConsumeRequest, EventBus, Filter,
};

/// Pre-computed hash for events that will be reused.
/// Store on JS side and pass back to avoid recomputing xxhash.
///
/// Hash is exposed as BigInt to preserve full 64-bit xxh3 precision.
#[napi(object)]
pub struct HashedEvent {
    pub data: Buffer,
    pub hash: BigInt,
}

#[cfg(feature = "redis")]
use net::config::RedisAdapterConfig;

#[cfg(feature = "jetstream")]
use net::config::JetStreamAdapterConfig;

#[cfg(feature = "net")]
use net::adapter::net::{NetAdapterConfig, ReliabilityConfig, StaticKeypair};

/// Redis adapter configuration.
#[napi(object)]
pub struct RedisOptions {
    /// Redis connection URL (e.g., "redis://localhost:6379")
    pub url: String,
    /// Stream key prefix (default: "net")
    pub prefix: Option<String>,
    /// Maximum commands per pipeline (default: 1000)
    pub pipeline_size: Option<u32>,
    /// Connection pool size (default: num_shards)
    pub pool_size: Option<u32>,
    /// Connection timeout in milliseconds (default: 5000)
    pub connect_timeout_ms: Option<u32>,
    /// Command timeout in milliseconds (default: 1000)
    pub command_timeout_ms: Option<u32>,
    /// Maximum stream length, unlimited if not set
    pub max_stream_len: Option<u32>,
}

/// NATS JetStream adapter configuration.
#[napi(object)]
pub struct JetStreamOptions {
    /// NATS server URL (e.g., "nats://localhost:4222")
    pub url: String,
    /// Stream name prefix (default: "net")
    pub prefix: Option<String>,
    /// Connection timeout in milliseconds (default: 5000)
    pub connect_timeout_ms: Option<u32>,
    /// Request timeout in milliseconds (default: 5000)
    pub request_timeout_ms: Option<u32>,
    /// Maximum messages per stream, unlimited if not set
    pub max_messages: Option<i64>,
    /// Maximum bytes per stream, unlimited if not set
    pub max_bytes: Option<i64>,
    /// Maximum age for messages in milliseconds, unlimited if not set
    pub max_age_ms: Option<u32>,
    /// Number of stream replicas (default: 1)
    pub replicas: Option<u32>,
}

/// Net keypair for encrypted UDP transport.
#[napi(object)]
pub struct NetKeypair {
    /// Hex-encoded 32-byte public key
    pub public_key: String,
    /// Hex-encoded 32-byte secret key
    pub secret_key: String,
}

/// Net adapter configuration for encrypted UDP transport.
#[napi(object)]
pub struct NetOptions {
    /// Local bind address (e.g., "127.0.0.1:9000")
    pub bind_addr: String,
    /// Remote peer address (e.g., "127.0.0.1:9001")
    pub peer_addr: String,
    /// Hex-encoded 32-byte pre-shared key
    pub psk: String,
    /// Connection role: "initiator" or "responder"
    pub role: String,
    /// Hex-encoded peer's static public key (required for initiator)
    pub peer_public_key: Option<String>,
    /// Hex-encoded secret key (required for responder)
    pub secret_key: Option<String>,
    /// Hex-encoded public key (required for responder)
    pub public_key: Option<String>,
    /// Reliability mode: "none" (default), "light", or "full"
    pub reliability: Option<String>,
    /// Heartbeat interval in milliseconds (default: 5000)
    pub heartbeat_interval_ms: Option<u32>,
    /// Session timeout in milliseconds (default: 30000)
    pub session_timeout_ms: Option<u32>,
    /// Enable batched I/O for Linux (default: false)
    pub batched_io: Option<bool>,
    /// Packet pool size (default: 64)
    pub packet_pool_size: Option<u32>,
}

/// Configuration options for creating an EventBus.
#[napi(object)]
pub struct EventBusOptions {
    /// Number of shards (defaults to CPU core count)
    pub num_shards: Option<u32>,
    /// Ring buffer capacity per shard (must be power of 2)
    pub ring_buffer_capacity: Option<u32>,
    /// Backpressure mode: "drop_newest", "drop_oldest", "fail_producer"
    pub backpressure_mode: Option<String>,
    /// Redis adapter configuration (if not set, uses in-memory noop adapter)
    pub redis: Option<RedisOptions>,
    /// NATS JetStream adapter configuration (alternative to Redis)
    pub jetstream: Option<JetStreamOptions>,
    /// Net adapter configuration for encrypted UDP transport
    pub net: Option<NetOptions>,
}

/// Options for polling events.
#[napi(object)]
pub struct PollOptions {
    /// Maximum number of events to return
    pub limit: u32,
    /// Optional cursor to resume from
    pub cursor: Option<String>,
    /// Optional JSON filter expression
    pub filter: Option<String>,
    /// Event ordering: "none" (default, fastest) or "insertion_ts" (cross-shard ordering)
    pub ordering: Option<String>,
}

/// Cumulative NAT-traversal counters surfaced via
/// `NetMesh.traversalStats()`. All counters are monotonic u64s
/// and never reset — subtract snapshots for deltas. Exposed as
/// BigInt because JavaScript numbers can't round-trip full u64.
///
/// Framing reminder (plan §5): NAT traversal is an optimization,
/// not a connectivity requirement. `relay_fallbacks` isn't a
/// failure counter — it counts `connect_direct` resolutions that
/// stayed on the routed-handshake path, which is always
/// available regardless of NAT shape.
#[cfg(feature = "nat-traversal")]
#[napi(object)]
pub struct TraversalStats {
    /// Number of hole-punch attempts the pair-type matrix
    /// elected to initiate. Bumps once per attempt, whether the
    /// punch eventually succeeds or falls back.
    pub punches_attempted: BigInt,
    /// Subset of attempts that produced a direct session.
    /// Always `<= punchesAttempted`; the difference is the
    /// punch-failure rate.
    pub punches_succeeded: BigInt,
    /// Number of `connect_direct` resolutions that stayed on
    /// the routed-handshake path — matrix-skipped pairs plus
    /// punch-failed attempts. Every `connect_direct` that
    /// doesn't establish a directly-punched session increments
    /// this counter.
    pub relay_fallbacks: BigInt,
}

/// Convert the core `NatClass` enum to the stable string form
/// used on the binding boundary. Stable vocabulary per plan §5:
/// `"open" | "cone" | "symmetric" | "unknown"`. Keep this in
/// sync with the Python / Go bindings — callers do
/// `mesh.natType() === "open"` branching against these strings.
#[cfg(feature = "nat-traversal")]
fn nat_class_to_string(class: net::adapter::net::traversal::classify::NatClass) -> String {
    use net::adapter::net::traversal::classify::NatClass;
    match class {
        NatClass::Open => "open",
        NatClass::Cone => "cone",
        NatClass::Symmetric => "symmetric",
        NatClass::Unknown => "unknown",
    }
    .to_string()
}

/// Format a core `TraversalError` into a NAPI `Error` whose
/// message follows the `traversal: <kind>[: <detail>]`
/// convention — mirrors the `migration: <kind>[: <detail>]`
/// pattern used by the compute module. The kind string is the
/// stable discriminator callers parse in `catch (e) { ... }`.
#[cfg(feature = "nat-traversal")]
fn traversal_err(e: net::adapter::net::traversal::TraversalError) -> Error {
    use net::adapter::net::traversal::TraversalError;
    let body = match &e {
        TraversalError::Transport(msg) => format!("transport: {msg}"),
        TraversalError::RendezvousRejected(msg) => format!("rendezvous-rejected: {msg}"),
        _ => e.kind().to_string(),
    };
    Error::from_reason(format!("traversal: {body}"))
}

/// A stored event returned from polling.
#[napi(object)]
pub struct StoredEvent {
    /// Backend-specific event ID
    pub id: String,
    /// Raw payload as UTF-8. When the payload is not valid UTF-8
    /// (binary payloads), this is the empty string and the original
    /// bytes are in `raw_bytes` instead.
    pub raw: String,
    /// Raw payload bytes. Always populated — consumers that need binary
    /// fidelity should prefer this over `raw`. For UTF-8 payloads the
    /// two fields carry the same content in different representations.
    pub raw_bytes: Buffer,
    /// Insertion timestamp (nanoseconds)
    pub insertion_ts: i64,
    /// Shard ID
    pub shard_id: u32,
}

/// Poll response containing events and cursor.
#[napi(object)]
pub struct PollResponse {
    /// List of events
    pub events: Vec<StoredEvent>,
    /// Cursor for pagination (pass to next poll)
    pub next_id: Option<String>,
    /// Whether there are more events available
    pub has_more: bool,
}

/// Ingestion result.
#[napi(object)]
pub struct IngestResult {
    /// Shard the event was assigned to
    pub shard_id: u32,
    /// Insertion timestamp
    pub timestamp: i64,
}

/// Ingestion statistics.
///
/// Counters are surfaced as `BigInt` rather than `number` so that
/// long-running nodes report exact totals past 2^53. The previous
/// surface clamped `u64` values into `i64` and silently capped at
/// `i64::MAX`; production busses ingesting millions of events per
/// second hit that cap inside a few months and the metric started
/// lying.
#[napi(object)]
pub struct Stats {
    /// Total events ingested
    pub events_ingested: BigInt,
    /// Events dropped due to backpressure
    pub events_dropped: BigInt,
}

/// High-performance event bus for Node.js.
///
/// Example usage:
/// ```typescript
/// import { Net } from '@net-mesh/core';
///
/// const bus = await Net.create({ numShards: 4 });
///
/// // Fast sync ingestion (no async overhead)
/// bus.ingestRawSync('{"token": "hello", "index": 0}');
///
/// // Or batch for maximum throughput
/// bus.ingestRawBatchSync(['{"a":1}', '{"a":2}']);
///
/// // Poll events (async)
/// const response = await bus.poll({ limit: 100 });
///
/// await bus.shutdown();
/// ```
#[napi]
pub struct Net {
    /// Lock-free bus handle using ArcSwap for maximum performance.
    /// ArcSwapOption allows atomic load/store without mutex overhead.
    bus: Arc<ArcSwapOption<EventBus>>,
}

#[napi]
impl Net {
    /// Create a new Net event bus.
    #[napi(factory)]
    pub async fn create(options: Option<EventBusOptions>) -> Result<Net> {
        let config = build_config(options)?;

        let bus = EventBus::new(config)
            .await
            .map_err(|e| Error::from_reason(format!("Failed to create EventBus: {}", e)))?;

        Ok(Net {
            bus: Arc::new(ArcSwapOption::from_pointee(bus)),
        })
    }

    // =========================================================================
    // ULTRA FAST PATH - Minimal overhead methods
    // =========================================================================

    /// Pre-compute hash for an event buffer.
    ///
    /// Use this for events that will be ingested multiple times (e.g., templates).
    /// The returned HashedEvent can be passed to pushHashed() to skip hash computation.
    #[napi]
    pub fn prehash(&self, data: Buffer) -> HashedEvent {
        let hash = xxhash_rust::xxh3::xxh3_64(data.as_ref());
        HashedEvent {
            data,
            hash: BigInt::from(hash),
        }
    }

    /// Ingest with a pre-computed hash (fastest single-event path).
    ///
    /// Pass the buffer and its pre-computed xxhash (as BigInt) to skip hash
    /// computation. BigInt preserves full 64-bit precision.
    #[napi]
    pub fn push_with_hash(&self, data: Buffer, hash: BigInt) -> bool {
        let guard = self.bus.load();
        let bus = match guard.as_ref() {
            Some(b) => b,
            None => return false,
        };

        let (sign, value, lossless) = hash.get_u64();
        // Reject negative or out-of-range BigInts to avoid silent wrong-shard routing
        if sign || !lossless {
            return false;
        }
        let raw =
            RawEvent::from_bytes_with_hash(bytes::Bytes::copy_from_slice(data.as_ref()), value);
        bus.ingest_raw(raw).is_ok()
    }

    /// Ingest raw bytes (fastest single-event path).
    ///
    /// Accepts a Buffer directly from JS - no string conversion overhead.
    /// Returns true on success, false on failure.
    #[napi]
    pub fn push(&self, data: Buffer) -> bool {
        let guard = self.bus.load();
        let bus = match guard.as_ref() {
            Some(b) => b,
            None => return false,
        };

        let raw = RawEvent::from_bytes(bytes::Bytes::copy_from_slice(data.as_ref()));
        bus.ingest_raw(raw).is_ok()
    }

    /// Batch push raw buffers (fastest batch path).
    ///
    /// Each buffer is one event. Returns count of successfully ingested.
    #[napi]
    pub fn push_batch(&self, events: Vec<Buffer>) -> u32 {
        let guard = self.bus.load();
        let bus = match guard.as_ref() {
            Some(b) => b,
            None => return 0,
        };

        let raw_events: Vec<RawEvent> = events
            .iter()
            .map(|b| RawEvent::from_bytes(bytes::Bytes::copy_from_slice(b.as_ref())))
            .collect();
        bus.ingest_raw_batch(raw_events) as u32
    }

    // =========================================================================
    // SYNC FAST PATH - Use these for maximum throughput
    // =========================================================================

    /// Ingest a raw JSON string synchronously (fastest path).
    ///
    /// This is the recommended method for high-throughput ingestion.
    /// No async overhead - directly calls into Rust core.
    #[napi]
    pub fn ingest_raw_sync(&self, json: String) -> Result<IngestResult> {
        let guard = self.bus.load();
        let bus = guard
            .as_ref()
            .ok_or_else(|| Error::from_reason("EventBus has been shut down"))?;

        let raw = RawEvent::from_str(&json);
        let (shard_id, ts) = bus
            .ingest_raw(raw)
            .map_err(|e| Error::from_reason(format!("Ingestion failed: {}", e)))?;

        Ok(IngestResult {
            shard_id: shard_id as u32,
            timestamp: ts as i64,
        })
    }

    /// Ingest multiple raw JSON strings in a batch synchronously.
    ///
    /// Most efficient method for bulk ingestion - single FFI boundary crossing.
    #[napi]
    pub fn ingest_raw_batch_sync(&self, events: Vec<String>) -> Result<u32> {
        let guard = self.bus.load();
        let bus = guard
            .as_ref()
            .ok_or_else(|| Error::from_reason("EventBus has been shut down"))?;

        let raw_events: Vec<RawEvent> = events.iter().map(|s| RawEvent::from_str(s)).collect();
        let count = bus.ingest_raw_batch(raw_events);

        Ok(count as u32)
    }

    /// Fire-and-forget ingestion - returns immediately, no result.
    ///
    /// Fastest possible path when you don't need confirmation.
    #[napi]
    pub fn ingest_fire(&self, json: String) -> bool {
        let guard = self.bus.load();
        let bus = match guard.as_ref() {
            Some(b) => b,
            None => return false,
        };

        let raw = RawEvent::from_str(&json);
        bus.ingest_raw(raw).is_ok()
    }

    /// Fire-and-forget batch ingestion - returns count only.
    #[napi]
    pub fn ingest_batch_fire(&self, events: Vec<String>) -> u32 {
        let guard = self.bus.load();
        let bus = match guard.as_ref() {
            Some(b) => b,
            None => return 0,
        };

        let raw_events: Vec<RawEvent> = events.iter().map(|s| RawEvent::from_str(s)).collect();
        bus.ingest_raw_batch(raw_events) as u32
    }

    // =========================================================================
    // ASYNC METHODS - For compatibility, use sync methods for perf
    // =========================================================================

    /// Ingest a raw JSON string (async version).
    ///
    /// For maximum performance, use `ingestRawSync` instead.
    #[napi]
    pub async fn ingest_raw(&self, json: String) -> Result<IngestResult> {
        self.ingest_raw_sync(json)
    }

    /// Ingest a JavaScript object (convenience method).
    ///
    /// The object is serialized to JSON before ingestion.
    /// For maximum performance, use `ingestRawSync` with pre-serialized JSON.
    #[napi]
    pub async fn ingest(&self, event: String) -> Result<IngestResult> {
        // Accept JSON string, parse to validate, then use raw path
        let _: serde_json::Value = serde_json::from_str(&event)
            .map_err(|e| Error::from_reason(format!("Invalid JSON: {}", e)))?;
        self.ingest_raw_sync(event)
    }

    /// Ingest multiple raw JSON strings in a batch (async version).
    ///
    /// For maximum performance, use `ingestRawBatchSync` instead.
    #[napi]
    pub async fn ingest_raw_batch(&self, events: Vec<String>) -> Result<u32> {
        self.ingest_raw_batch_sync(events)
    }

    /// Poll events from the bus.
    #[napi]
    pub async fn poll(&self, options: PollOptions) -> Result<PollResponse> {
        // Load the Arc - this is lock-free with ArcSwap
        let bus_arc = self
            .bus
            .load_full()
            .ok_or_else(|| Error::from_reason("EventBus has been shut down"))?;

        let mut request = ConsumeRequest::new(options.limit as usize);

        if let Some(cursor) = options.cursor {
            request = request.from(cursor);
        }

        if let Some(filter_json) = options.filter {
            let filter: Filter = serde_json::from_str(&filter_json)
                .map_err(|e| Error::from_reason(format!("Invalid filter: {}", e)))?;
            request = request.filter(filter);
        }

        if let Some(ordering) = options.ordering {
            let ord = match ordering.as_str() {
                "none" => Ordering::None,
                "insertion_ts" => Ordering::InsertionTs,
                _ => {
                    return Err(Error::from_reason(format!(
                        "Invalid ordering: {}. Use 'none' or 'insertion_ts'",
                        ordering
                    )));
                }
            };
            request = request.ordering(ord);
        }

        let response = bus_arc
            .poll(request)
            .await
            .map_err(|e| Error::from_reason(format!("Poll failed: {}", e)))?;

        let events: Vec<StoredEvent> = response
            .events
            .into_iter()
            .map(|e| {
                let raw = e.raw_str().unwrap_or("").to_string();
                let raw_bytes = Buffer::from(e.raw.to_vec());
                StoredEvent {
                    id: e.id,
                    raw,
                    raw_bytes,
                    insertion_ts: e.insertion_ts as i64,
                    shard_id: e.shard_id as u32,
                }
            })
            .collect();

        Ok(PollResponse {
            events,
            next_id: response.next_id,
            has_more: response.has_more,
        })
    }

    /// Get the number of active shards.
    #[napi]
    pub fn num_shards(&self) -> Result<u32> {
        let guard = self.bus.load();
        let bus = guard
            .as_ref()
            .ok_or_else(|| Error::from_reason("EventBus has been shut down"))?;

        Ok(bus.num_shards() as u32)
    }

    /// Get ingestion statistics.
    #[napi]
    pub fn stats(&self) -> Result<Stats> {
        let guard = self.bus.load();
        let bus = guard
            .as_ref()
            .ok_or_else(|| Error::from_reason("EventBus has been shut down"))?;

        let stats = bus.stats();
        Ok(Stats {
            events_ingested: BigInt::from(
                stats
                    .events_ingested
                    .load(std::sync::atomic::Ordering::Relaxed),
            ),
            events_dropped: BigInt::from(
                stats
                    .events_dropped
                    .load(std::sync::atomic::Ordering::Relaxed),
            ),
        })
    }

    /// Flush all pending batches to the backend.
    ///
    /// Call this after ingesting events to ensure they are persisted
    /// before polling.
    #[napi]
    pub async fn flush(&self) -> Result<()> {
        let bus_arc = self
            .bus
            .load_full()
            .ok_or_else(|| Error::from_reason("EventBus has been shut down"))?;

        bus_arc
            .flush()
            .await
            .map_err(|e| Error::from_reason(format!("Flush failed: {}", e)))?;
        Ok(())
    }

    /// Gracefully shutdown the event bus.
    ///
    /// Returns an error if there are outstanding references to the bus
    /// (e.g., from concurrent async operations).
    #[napi]
    pub async fn shutdown(&self) -> Result<()> {
        // Swap out the bus atomically - no lock needed
        let bus_arc = self.bus.swap(None);

        if let Some(bus) = bus_arc {
            // Try to unwrap the Arc - if we're the only holder, we can shutdown
            match Arc::try_unwrap(bus) {
                Ok(bus) => {
                    bus.shutdown()
                        .await
                        .map_err(|e| Error::from_reason(format!("Shutdown failed: {}", e)))?;
                }
                Err(arc) => {
                    // Put the bus back so it isn't permanently lost.
                    // The caller can retry after outstanding operations complete.
                    self.bus.store(Some(arc));
                    return Err(Error::from_reason(
                        "Cannot shutdown: outstanding references to EventBus exist. \
                         Ensure all async operations have completed before calling shutdown()."
                            .to_string(),
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Generate a new Net keypair for encrypted UDP transport.
///
/// Returns a keypair with hex-encoded public and secret keys.
/// Use this to generate keys for a responder, then share the public key
/// with the initiator.
#[cfg(feature = "net")]
#[napi]
pub fn generate_net_keypair() -> NetKeypair {
    let keypair = StaticKeypair::generate();
    NetKeypair {
        public_key: hex::encode(keypair.public_key()),
        secret_key: hex::encode(keypair.secret_key()),
    }
}

fn build_config(options: Option<EventBusOptions>) -> Result<EventBusConfig> {
    let mut builder = EventBusConfig::builder();

    if let Some(opts) = options {
        if let Some(num_shards) = opts.num_shards {
            let shards: u16 = num_shards.try_into().map_err(|_| {
                Error::from_reason(format!(
                    "num_shards must be <= {}, got {}",
                    u16::MAX,
                    num_shards
                ))
            })?;
            builder = builder.num_shards(shards);
        }
        if let Some(capacity) = opts.ring_buffer_capacity {
            builder = builder.ring_buffer_capacity(capacity as usize);
        }
        if let Some(mode) = opts.backpressure_mode {
            let bp_mode = match mode.as_str() {
                "drop_newest" => BackpressureMode::DropNewest,
                "drop_oldest" => BackpressureMode::DropOldest,
                "fail_producer" => BackpressureMode::FailProducer,
                _ => {
                    return Err(Error::from_reason(format!(
                        "Invalid backpressure mode: {}",
                        mode
                    )));
                }
            };
            builder = builder.backpressure_mode(bp_mode);
        }

        // Configure adapter
        if let Some(redis) = opts.redis {
            #[cfg(feature = "redis")]
            {
                use std::time::Duration;
                let mut redis_config = RedisAdapterConfig::new(&redis.url);
                if let Some(prefix) = redis.prefix {
                    redis_config = redis_config.with_prefix(&prefix);
                }
                if let Some(pipeline_size) = redis.pipeline_size {
                    redis_config = redis_config.with_pipeline_size(pipeline_size as usize);
                }
                if let Some(pool_size) = redis.pool_size {
                    redis_config = redis_config.with_pool_size(pool_size as usize);
                }
                if let Some(connect_timeout_ms) = redis.connect_timeout_ms {
                    redis_config = redis_config
                        .with_connect_timeout(Duration::from_millis(connect_timeout_ms as u64));
                }
                if let Some(command_timeout_ms) = redis.command_timeout_ms {
                    redis_config = redis_config
                        .with_command_timeout(Duration::from_millis(command_timeout_ms as u64));
                }
                if let Some(max_stream_len) = redis.max_stream_len {
                    redis_config = redis_config.with_max_stream_len(max_stream_len as usize);
                }
                builder = builder.adapter(AdapterConfig::Redis(redis_config));
            }
            #[cfg(not(feature = "redis"))]
            {
                let _ = redis;
                return Err(Error::from_reason(
                    "Redis support not enabled. Rebuild with --features redis".to_string(),
                ));
            }
        } else if let Some(jetstream) = opts.jetstream {
            #[cfg(feature = "jetstream")]
            {
                use std::time::Duration;
                let mut js_config = JetStreamAdapterConfig::new(&jetstream.url);
                if let Some(prefix) = jetstream.prefix {
                    js_config = js_config.with_prefix(&prefix);
                }
                if let Some(connect_timeout_ms) = jetstream.connect_timeout_ms {
                    js_config = js_config
                        .with_connect_timeout(Duration::from_millis(connect_timeout_ms as u64));
                }
                if let Some(request_timeout_ms) = jetstream.request_timeout_ms {
                    js_config = js_config
                        .with_request_timeout(Duration::from_millis(request_timeout_ms as u64));
                }
                if let Some(max_messages) = jetstream.max_messages {
                    js_config = js_config.with_max_messages(max_messages);
                }
                if let Some(max_bytes) = jetstream.max_bytes {
                    js_config = js_config.with_max_bytes(max_bytes);
                }
                if let Some(max_age_ms) = jetstream.max_age_ms {
                    js_config = js_config.with_max_age(Duration::from_millis(max_age_ms as u64));
                }
                if let Some(replicas) = jetstream.replicas {
                    js_config = js_config.with_replicas(replicas as usize);
                }
                builder = builder.adapter(AdapterConfig::JetStream(js_config));
            }
            #[cfg(not(feature = "jetstream"))]
            {
                let _ = jetstream;
                return Err(Error::from_reason(
                    "JetStream support not enabled. Rebuild with --features jetstream".to_string(),
                ));
            }
        } else if let Some(net) = opts.net {
            #[cfg(feature = "net")]
            {
                use std::time::Duration;

                let bind_addr: std::net::SocketAddr = net
                    .bind_addr
                    .parse()
                    .map_err(|e| Error::from_reason(format!("Invalid bind_addr: {}", e)))?;

                let peer_addr: std::net::SocketAddr = net
                    .peer_addr
                    .parse()
                    .map_err(|e| Error::from_reason(format!("Invalid peer_addr: {}", e)))?;

                let psk: [u8; 32] = hex::decode(&net.psk)
                    .map_err(|e| Error::from_reason(format!("Invalid psk hex: {}", e)))?
                    .try_into()
                    .map_err(|_| Error::from_reason("psk must be exactly 32 bytes".to_string()))?;

                let mut net_config = match net.role.as_str() {
                    "initiator" => {
                        let peer_pubkey_hex = net.peer_public_key.ok_or_else(|| {
                            Error::from_reason(
                                "peer_public_key is required for initiator".to_string(),
                            )
                        })?;
                        let peer_pubkey: [u8; 32] = hex::decode(&peer_pubkey_hex)
                            .map_err(|e| {
                                Error::from_reason(format!("Invalid peer_public_key hex: {}", e))
                            })?
                            .try_into()
                            .map_err(|_| {
                                Error::from_reason(
                                    "peer_public_key must be exactly 32 bytes".to_string(),
                                )
                            })?;
                        NetAdapterConfig::initiator(bind_addr, peer_addr, psk, peer_pubkey)
                    }
                    "responder" => {
                        let secret_key_hex = net.secret_key.ok_or_else(|| {
                            Error::from_reason("secret_key is required for responder".to_string())
                        })?;
                        let public_key_hex = net.public_key.ok_or_else(|| {
                            Error::from_reason("public_key is required for responder".to_string())
                        })?;
                        let secret_key: [u8; 32] = hex::decode(&secret_key_hex)
                            .map_err(|e| {
                                Error::from_reason(format!("Invalid secret_key hex: {}", e))
                            })?
                            .try_into()
                            .map_err(|_| {
                                Error::from_reason(
                                    "secret_key must be exactly 32 bytes".to_string(),
                                )
                            })?;
                        let public_key: [u8; 32] = hex::decode(&public_key_hex)
                            .map_err(|e| {
                                Error::from_reason(format!("Invalid public_key hex: {}", e))
                            })?
                            .try_into()
                            .map_err(|_| {
                                Error::from_reason(
                                    "public_key must be exactly 32 bytes".to_string(),
                                )
                            })?;
                        let keypair = StaticKeypair::from_keys(secret_key, public_key);
                        NetAdapterConfig::responder(bind_addr, peer_addr, psk, keypair)
                    }
                    _ => {
                        return Err(Error::from_reason(format!(
                            "Invalid role: {}. Use 'initiator' or 'responder'",
                            net.role
                        )));
                    }
                };

                // Apply optional settings
                if let Some(reliability) = net.reliability {
                    net_config = net_config.with_reliability(match reliability.as_str() {
                        "light" => ReliabilityConfig::Light,
                        "full" => ReliabilityConfig::Full,
                        _ => ReliabilityConfig::None,
                    });
                }
                if let Some(interval_ms) = net.heartbeat_interval_ms {
                    net_config = net_config
                        .with_heartbeat_interval(Duration::from_millis(interval_ms as u64));
                }
                if let Some(timeout_ms) = net.session_timeout_ms {
                    net_config =
                        net_config.with_session_timeout(Duration::from_millis(timeout_ms as u64));
                }
                if let Some(batched) = net.batched_io {
                    net_config = net_config.with_batched_io(batched);
                }
                if let Some(pool_size) = net.packet_pool_size {
                    net_config = net_config.with_pool_size(pool_size as usize);
                }

                builder = builder.adapter(AdapterConfig::Net(Box::new(net_config)));
            }
            #[cfg(not(feature = "net"))]
            {
                let _ = net;
                return Err(Error::from_reason(
                    "Net support not enabled. Rebuild with --features net".to_string(),
                ));
            }
        }
    }

    builder
        .build()
        .map_err(|e| Error::from_reason(format!("Invalid configuration: {}", e)))
}

// ============================================================================
// MeshNode bindings
// ============================================================================

#[cfg(feature = "net")]
mod mesh_bindings {
    use super::*;
    use net::adapter::net::{
        EntityKeypair, MeshNode, MeshNodeConfig, Reliability, Stream as CoreStream, StreamConfig,
        StreamError, DEFAULT_STREAM_WINDOW_BYTES,
    };
    use net::adapter::Adapter;
    use std::time::Duration;

    // ─── Stream API type bridges ─────────────────────────────────────

    /// Reliability mode for a stream. Wire value is a plain tag string:
    /// `"fire_and_forget"` (default) or `"reliable"`. Anything else
    /// errors at stream-open time.
    #[napi(object)]
    pub struct StreamOptions {
        /// Caller-chosen `u64` stream id. Stream IDs are opaque; no
        /// range has transport-level meaning. Crosses the boundary
        /// as `BigInt` so full u64 precision is preserved.
        pub stream_id: BigInt,
        /// `"fire_and_forget"` | `"reliable"`. Default: `"fire_and_forget"`.
        pub reliability: Option<String>,
        /// Initial send-credit window in bytes. Defaults to
        /// `DEFAULT_STREAM_WINDOW_BYTES` (64 KB) when unset — v2
        /// backpressure is ON out of the box. Pass `0` to restore the
        /// v1 unbounded-queue behavior for a specific stream.
        pub window_bytes: Option<u32>,
        /// Fair-scheduler weight (1 = equal share). Default: 1.
        pub fairness_weight: Option<u8>,
    }

    /// Handle to an open stream. Opaque to JS callers; pass back to
    /// `sendOnStream` / `sendWithRetry` / `sendBlocking` / `closeStream`.
    #[napi]
    pub struct NetStream {
        peer_node_id: u64,
        stream_id: u64,
        core: CoreStream,
    }

    #[napi]
    impl NetStream {
        /// The peer this stream terminates at.
        #[napi(getter)]
        pub fn peer_node_id(&self) -> BigInt {
            BigInt::from(self.peer_node_id)
        }
        /// The caller-chosen stream id.
        #[napi(getter)]
        pub fn stream_id(&self) -> BigInt {
            BigInt::from(self.stream_id)
        }
    }

    /// Snapshot of per-stream stats.
    ///
    /// u64 fields are exposed as `BigInt` so values outside the JS
    /// safe-integer range (notably `last_activity_ns`, which is
    /// Unix-epoch nanoseconds and always well above `2^53`) don't
    /// lose precision or trip the TS SDK's safe-integer guard. The
    /// u32 fields are safe as regular numbers.
    #[napi(object)]
    pub struct NetStreamStats {
        pub tx_seq: BigInt,
        pub rx_seq: BigInt,
        pub inbound_pending: BigInt,
        pub last_activity_ns: BigInt,
        pub active: bool,
        pub backpressure_events: BigInt,
        pub tx_credit_remaining: u32,
        pub tx_window: u32,
        pub credit_grants_received: BigInt,
        pub credit_grants_sent: BigInt,
    }

    /// Prefix convention for JS SDK error-class routing. The TS wrapper
    /// matches on the message prefix to re-throw a `BackpressureError`
    /// or `NotConnectedError` subclass. The rest of the message is
    /// human-readable detail. Keep these strings stable — they are part
    /// of the SDK contract.
    pub(crate) const ERR_BACKPRESSURE_PREFIX: &str = "stream would block";
    pub(crate) const ERR_NOT_CONNECTED_PREFIX: &str = "stream not connected";

    pub(crate) fn stream_error_to_napi(e: StreamError) -> Error {
        // Map each variant to a stable, prefix-sniffable message. The
        // TS `sendOnStream` wrapper (`sdk-ts`) inspects this prefix to
        // re-throw `BackpressureError` or `NotConnectedError`.
        match e {
            StreamError::Backpressure => {
                Error::from_reason(format!("{}: stream queue full", ERR_BACKPRESSURE_PREFIX))
            }
            StreamError::NotConnected => {
                Error::from_reason(format!("{}: peer session gone", ERR_NOT_CONNECTED_PREFIX))
            }
            StreamError::Transport(msg) => {
                Error::from_reason(format!("stream transport error: {}", msg))
            }
        }
    }

    pub(crate) fn stream_config_from_opts(opts: &StreamOptions) -> Result<StreamConfig> {
        let reliability = match opts.reliability.as_deref() {
            None | Some("fire_and_forget") => Reliability::FireAndForget,
            Some("reliable") => Reliability::Reliable,
            Some(other) => {
                return Err(Error::from_reason(format!(
                    "unknown reliability mode {:?}; expected \"fire_and_forget\" or \"reliable\"",
                    other
                )));
            }
        };
        Ok(StreamConfig::new()
            .with_reliability(reliability)
            .with_window_bytes(opts.window_bytes.unwrap_or(DEFAULT_STREAM_WINDOW_BYTES))
            .with_fairness_weight(opts.fairness_weight.unwrap_or(1)))
    }

    /// Configuration for creating a MeshNode.
    #[napi(object)]
    pub struct MeshOptions {
        /// Local bind address (e.g., "127.0.0.1:9000")
        pub bind_addr: String,
        /// Hex-encoded 32-byte pre-shared key
        pub psk: String,
        /// Heartbeat interval in milliseconds (default: 5000)
        pub heartbeat_interval_ms: Option<u32>,
        /// Session timeout in milliseconds (default: 30000)
        pub session_timeout_ms: Option<u32>,
        /// Number of inbound shards (default: 4)
        pub num_shards: Option<u32>,
        /// Capability-index GC sweep interval in milliseconds.
        /// Default: 60_000. Shorter values make TTL-driven eviction
        /// more responsive at the cost of extra CPU.
        pub capability_gc_interval_ms: Option<u32>,
        /// Drop inbound `CapabilityAnnouncement` packets without a
        /// signature. Default: false. Signature *validity* is not
        /// yet enforced; this is presence-only policy today.
        pub require_signed_capabilities: Option<bool>,
        /// Pin this node to a specific subnet. Defaults to
        /// `SubnetId::GLOBAL` (no restriction). Visibility checks on
        /// publish + subscribe compare against this value.
        pub subnet: Option<crate::subnets::SubnetIdJs>,
        /// Policy applied to inbound `CapabilityAnnouncement`s to
        /// derive each peer's subnet. `None` disables per-peer
        /// subnet tracking.
        pub subnet_policy: Option<crate::subnets::SubnetPolicyJs>,
        /// 32-byte ed25519 seed. When set, the mesh derives its
        /// keypair (and therefore its `entity_id` + stable
        /// `node_id`) from these bytes instead of generating
        /// ephemeral ones. Treat as secret material.
        pub identity_seed: Option<Buffer>,
        /// Pin this mesh's publicly-advertised reflex to the
        /// supplied external `"ip:port"`. Classification is
        /// skipped; the node starts in `nat:open` and advertises
        /// this address on capability announcements.
        ///
        /// Use for port-forwarded servers (operator knows the
        /// external address) and stage-4 UPnP / NAT-PMP
        /// integration. **Optimization, not correctness** —
        /// nodes without an override still reach every peer via
        /// the routed-handshake path.
        ///
        /// Silently ignored when the Rust cdylib was built
        /// without `--features nat-traversal`.
        pub reflex_override: Option<String>,
        /// Opt into opportunistic UPnP-IGD / NAT-PMP / PCP port
        /// mapping at `start()` time. When `true`, the mesh
        /// spawns a port-mapping task that probes NAT-PMP + UPnP,
        /// installs a mapping against the operator's router on
        /// success, pins the reflex to the mapped external, and
        /// renews every 30 min.
        ///
        /// **Optimization, not correctness.** Safe to set
        /// `true` on any network — the task degrades cleanly
        /// when no router responds.
        ///
        /// Silently ignored when the Rust cdylib was built
        /// without `--features port-mapping`.
        pub try_port_mapping: Option<bool>,
        /// Opt out of channel authorization: when `true`, no
        /// `ChannelConfigRegistry` is installed on the node, so
        /// membership/subscribe requests aren't gated against
        /// per-channel config (any channel is reachable). Default
        /// `false` — the registry is installed and unconfigured
        /// channels are rejected with `UnknownChannel`.
        ///
        /// Required for dynamic-channel surfaces whose channels
        /// aren't pre-registered — notably `publishTools` (the
        /// served tools + describe service ride dynamically-named
        /// channels). Mirrors the Python binding's
        /// `permissive_channels`. Leave `false` for
        /// production nodes that pre-register their channels.
        pub permissive_channels: Option<bool>,
    }

    /// JS-facing channel config, mirroring the core `ChannelConfig`
    /// field-for-field. v1 does not expose `publishCaps` /
    /// Channel registration config. `publishCaps` / `subscribeCaps`
    /// are capability filters enforced at subscribe time + before
    /// the publish fan-out; `requireToken` gates on a valid
    /// `PermissionToken`. See `docs/CHANNEL_AUTH_PLAN.md`.
    #[napi(object)]
    pub struct ChannelConfigJs {
        /// Canonical channel name. Crosses the boundary as a string
        /// (not the u16 hash) to avoid ACL bypass via collision.
        pub name: String,
        /// `"subnet-local" | "parent-visible" | "exported" | "global"`.
        /// Default `"global"`.
        pub visibility: Option<String>,
        /// Default reliability for streams on this channel.
        pub reliable: Option<bool>,
        /// When true, subscribers must present a valid
        /// `PermissionToken` whose subject matches their entity id.
        pub require_token: Option<bool>,
        /// Priority (0 = lowest).
        pub priority: Option<u8>,
        /// Rate cap in packets per second.
        pub max_rate_pps: Option<u32>,
        /// Capability filter the publisher itself must satisfy
        /// before fan-out. Rejected with a `channel:` error on
        /// mismatch.
        pub publish_caps: Option<crate::capabilities::CapabilityFilterJs>,
        /// Capability filter each subscriber must satisfy. Rejected
        /// as `Unauthorized` on mismatch.
        pub subscribe_caps: Option<crate::capabilities::CapabilityFilterJs>,
        /// Root(s) of trust for token authorization: 32-byte entity ids
        /// whose signature may root a presented token chain. Setting
        /// this turns on `requireToken` and anchors the channel — a
        /// chain is only honored if its root link was issued by one of
        /// these entities. `requireToken: true` with no `tokenRoots`
        /// fails every authorization closed (no authority to anchor
        /// to), so prefer this to `requireToken` alone.
        pub token_roots: Option<Vec<Buffer>>,
    }

    impl ChannelConfigJs {
        fn into_core(self) -> Result<net::adapter::net::ChannelConfig> {
            let name = net::adapter::net::ChannelName::new(&self.name)
                .map_err(|e| Error::from_reason(format!("channel: invalid name: {}", e)))?;
            let mut cfg =
                net::adapter::net::ChannelConfig::new(net::adapter::net::ChannelId::new(name));
            if let Some(v) = self.visibility {
                cfg = cfg.with_visibility(parse_visibility(&v)?);
            }
            if let Some(r) = self.reliable {
                cfg = cfg.with_reliable(r);
            }
            if let Some(req) = self.require_token {
                cfg = cfg.with_require_token(req);
            }
            if let Some(roots) = self.token_roots {
                let mut parsed = Vec::with_capacity(roots.len());
                for buf in roots {
                    let arr: [u8; 32] = buf.as_ref().try_into().map_err(|_| {
                        Error::from_reason(format!(
                            "channel: token_roots entry must be 32 bytes, got {}",
                            buf.len()
                        ))
                    })?;
                    parsed.push(net::adapter::net::identity::EntityId::from_bytes(arr));
                }
                cfg = cfg.with_token_roots(parsed);
            }
            if let Some(p) = self.priority {
                cfg = cfg.with_priority(p);
            }
            if let Some(pps) = self.max_rate_pps {
                cfg = cfg.with_rate_limit(pps);
            }
            if let Some(filter) = self.publish_caps {
                cfg = cfg.with_publish_caps(crate::capabilities::capability_filter_from_js(filter));
            }
            if let Some(filter) = self.subscribe_caps {
                cfg =
                    cfg.with_subscribe_caps(crate::capabilities::capability_filter_from_js(filter));
            }
            Ok(cfg)
        }
    }

    fn parse_visibility(s: &str) -> Result<net::adapter::net::Visibility> {
        use net::adapter::net::Visibility;
        match s {
            "subnet-local" => Ok(Visibility::SubnetLocal),
            "parent-visible" => Ok(Visibility::ParentVisible),
            "exported" => Ok(Visibility::Exported),
            "global" => Ok(Visibility::Global),
            _ => Err(Error::from_reason(format!(
                "channel: invalid visibility {:?} (expected subnet-local | parent-visible | exported | global)",
                s
            ))),
        }
    }

    /// Publish-fanout config, mirror of the core `PublishConfig`.
    #[napi(object, object_from_js = true)]
    #[derive(Default)]
    pub struct PublishConfigJs {
        /// `"reliable" | "fire_and_forget"`. Default `"fire_and_forget"`.
        pub reliability: Option<String>,
        /// `"best_effort" | "fail_fast" | "collect"`. Default
        /// `"best_effort"`.
        pub on_failure: Option<String>,
        /// Max concurrent per-peer sends. Default 32.
        pub max_inflight: Option<u32>,
    }

    impl PublishConfigJs {
        fn into_core(self) -> Result<net::adapter::net::PublishConfig> {
            use net::adapter::net::{OnFailure, PublishConfig, Reliability};
            let mut cfg = PublishConfig {
                reliability: Reliability::FireAndForget,
                on_failure: OnFailure::BestEffort,
                max_inflight: 32,
            };
            if let Some(r) = self.reliability {
                cfg.reliability = match r.as_str() {
                    "reliable" => Reliability::Reliable,
                    "fire_and_forget" => Reliability::FireAndForget,
                    other => {
                        return Err(Error::from_reason(format!(
                            "channel: invalid reliability {:?}",
                            other
                        )));
                    }
                };
            }
            if let Some(f) = self.on_failure {
                cfg.on_failure = match f.as_str() {
                    "best_effort" => OnFailure::BestEffort,
                    "fail_fast" => OnFailure::FailFast,
                    "collect" => OnFailure::Collect,
                    other => {
                        return Err(Error::from_reason(format!(
                            "channel: invalid on_failure {:?}",
                            other
                        )));
                    }
                };
            }
            if let Some(n) = self.max_inflight {
                cfg.max_inflight = n as usize;
            }
            Ok(cfg)
        }
    }

    /// Per-peer report returned by `publish`.
    #[napi(object)]
    pub struct PublishReportJs {
        /// Total subscribers the publisher attempted to reach.
        pub attempted: u32,
        /// Subscribers that received the payload.
        pub delivered: u32,
        /// Per-peer errors. Each entry is `{ nodeId, message }`.
        pub errors: Vec<PublishFailureJs>,
    }

    #[napi(object)]
    pub struct PublishFailureJs {
        pub node_id: BigInt,
        pub message: String,
    }

    impl PublishReportJs {
        fn from_core(report: net::adapter::net::PublishReport) -> Self {
            PublishReportJs {
                attempted: report.attempted as u32,
                delivered: report.delivered as u32,
                errors: report
                    .errors
                    .into_iter()
                    .map(|(id, e)| PublishFailureJs {
                        node_id: BigInt::from(id),
                        message: format!("{}", e),
                    })
                    .collect(),
            }
        }
    }

    use crate::common::bigint_u64 as bigint_u64_lossless;

    /// Translate an `AdapterError` from a channel operation into a
    /// napi `Error`, tagging `"channel:"` prefixed messages so the
    /// SDK-ts layer can classify them into `ChannelError` /
    /// `ChannelAuthError`.
    fn map_channel_adapter_error(err: net::error::AdapterError) -> Error {
        use net::error::AdapterError;
        if let AdapterError::Connection(ref msg) = err {
            let prefix = "membership request rejected: ";
            if let Some(tail) = msg.strip_prefix(prefix) {
                // Classify rejection reasons for typed SDK errors.
                let reason = tail.trim();
                if reason == "Some(Unauthorized)" {
                    return Error::from_reason("channel: unauthorized".to_string());
                }
                if reason == "Some(UnknownChannel)" {
                    return Error::from_reason("channel: unknown channel".to_string());
                }
                if reason == "Some(RateLimited)" {
                    return Error::from_reason("channel: rate limited".to_string());
                }
                if reason == "Some(TooManyChannels)" {
                    return Error::from_reason("channel: too many channels".to_string());
                }
                return Error::from_reason(format!("channel: rejected ({})", reason));
            }
        }
        Error::from_reason(format!("channel: {}", err))
    }

    /// A multi-peer mesh node for Node.js.
    ///
    /// Manages encrypted connections to multiple peers over a single
    /// UDP socket with automatic failure detection and rerouting.
    ///
    /// ```typescript
    /// import { NetMesh } from '@net-mesh/core';
    ///
    /// const node = await NetMesh.create({
    ///   bindAddr: '127.0.0.1:9000',
    ///   psk: '0'.repeat(64), // 32-byte hex
    /// });
    ///
    /// console.log('public key:', node.publicKey());
    ///
    /// await node.connect('127.0.0.1:9001', peerPubkey, 0x2222);
    /// node.start();
    ///
    /// node.pushTo('127.0.0.1:9001', Buffer.from('{"token":"hi"}'));
    ///
    /// const events = await node.poll(100);
    ///
    /// await node.shutdown();
    /// ```
    #[napi]
    pub struct NetMesh {
        node: Arc<ArcSwapOption<MeshNode>>,
        /// Channel config registry shared with the underlying MeshNode.
        /// `register_channel` inserts here; the node's membership ACL
        /// path reads from this same registry.
        channel_configs: Arc<net::adapter::net::ChannelConfigRegistry>,
    }

    #[napi]
    impl NetMesh {
        /// Create a new mesh node.
        #[napi(factory)]
        pub async fn create(options: MeshOptions) -> Result<NetMesh> {
            let bind_addr: std::net::SocketAddr = options
                .bind_addr
                .parse()
                .map_err(|e| Error::from_reason(format!("invalid bind address: {}", e)))?;

            let psk_bytes = hex::decode(&options.psk)
                .map_err(|e| Error::from_reason(format!("invalid PSK hex: {}", e)))?;
            if psk_bytes.len() != 32 {
                return Err(Error::from_reason("PSK must be 32 bytes (64 hex chars)"));
            }
            let mut psk = [0u8; 32];
            psk.copy_from_slice(&psk_bytes);

            let mut config = MeshNodeConfig::new(bind_addr, psk);
            if let Some(ms) = options.heartbeat_interval_ms {
                config = config.with_heartbeat_interval(Duration::from_millis(ms as u64));
            }
            if let Some(ms) = options.session_timeout_ms {
                config = config.with_session_timeout(Duration::from_millis(ms as u64));
            }
            if let Some(n) = options.num_shards {
                let n = u16::try_from(n).map_err(|_| {
                    Error::from_reason(format!("num_shards must be in [0, 65535]; got {}", n))
                })?;
                config = config.with_num_shards(n);
            }
            if let Some(ms) = options.capability_gc_interval_ms {
                config = config.with_capability_gc_interval(Duration::from_millis(ms as u64));
            }
            if let Some(b) = options.require_signed_capabilities {
                config = config.with_require_signed_capabilities(b);
            }
            if let Some(id_js) = options.subnet {
                let id = crate::subnets::subnet_id_from_js(id_js)?;
                config = config.with_subnet(id);
            }
            if let Some(policy_js) = options.subnet_policy {
                let policy = std::sync::Arc::new(crate::subnets::subnet_policy_from_js(policy_js)?);
                config = config.with_subnet_policy(policy);
            }
            #[cfg(feature = "nat-traversal")]
            if let Some(external_str) = options.reflex_override.as_deref() {
                let external: std::net::SocketAddr = external_str
                    .parse()
                    .map_err(|e| Error::from_reason(format!("invalid reflex_override: {e}")))?;
                config = config.with_reflex_override(external);
            }
            #[cfg(feature = "port-mapping")]
            if options.try_port_mapping == Some(true) {
                config = config.with_try_port_mapping(true);
            }

            let identity = match options.identity_seed {
                Some(seed) => {
                    let bytes: &[u8] = seed.as_ref();
                    if bytes.len() != 32 {
                        return Err(Error::from_reason(format!(
                            "identity_seed must be 32 bytes, got {}",
                            bytes.len()
                        )));
                    }
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(bytes);
                    EntityKeypair::from_bytes(arr)
                }
                None => EntityKeypair::generate(),
            };

            let mut node = MeshNode::new(identity, config)
                .await
                .map_err(|e| Error::from_reason(format!("MeshNode creation failed: {}", e)))?;

            // Install a shared ChannelConfigRegistry so `register_channel`
            // can insert without needing `&mut NetMesh`. `permissiveChannels`
            // opts out: no registry on the node → no membership ACL (matching
            // the Rust integration-test default), which dynamic-channel
            // surfaces like `publishTools` need. The `NetMesh` keeps the
            // registry either way — `register_channel` / `channelConfigsArc`
            // read it; in permissive mode it's just detached from the node.
            let channel_configs = Arc::new(net::adapter::net::ChannelConfigRegistry::new());
            if !options.permissive_channels.unwrap_or(false) {
                node.set_channel_configs(channel_configs.clone());
            }
            // Always install a TokenCache — channel auth needs
            // somewhere to stash tokens presented on subscribe.
            // Callers that want to pre-seed can use `installToken`
            // on a caller-side `Identity` constructed from the same
            // `identity_seed`.
            node.set_token_cache(Arc::new(net::adapter::net::identity::TokenCache::new()));

            Ok(NetMesh {
                node: Arc::new(ArcSwapOption::from_pointee(node)),
                channel_configs,
            })
        }

        /// Get this node's Noise public key (hex-encoded).
        #[napi]
        pub fn public_key(&self) -> Result<String> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            Ok(hex::encode(node.public_key()))
        }

        /// This node's actual bound socket address (`"ip:port"`). With
        /// `bindAddr: "127.0.0.1:0"` the OS assigns the port; read it here to
        /// hand to a peer's `connect(...)`. Mirrors the Python `local_addr`.
        #[napi]
        pub fn local_addr(&self) -> Result<String> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            Ok(node.local_addr().to_string())
        }

        /// Get this node's ID. Returned as `BigInt` so full u64
        /// precision is preserved — keypair-derived node_ids
        /// routinely exceed `Number.MAX_SAFE_INTEGER`.
        #[napi]
        pub fn node_id(&self) -> Result<BigInt> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            Ok(BigInt::from(node.node_id()))
        }

        /// Get this node's ed25519 entity id (32 bytes — the same
        /// value as `new Identity(seed).entityId` when the mesh was
        /// constructed with `identitySeed = seed`).
        #[napi]
        pub fn entity_id(&self) -> Result<Buffer> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            Ok(Buffer::from(node.entity_id().as_bytes().to_vec()))
        }

        /// Connect to a peer (initiator side).
        #[napi]
        pub async fn connect(
            &self,
            peer_addr: String,
            peer_public_key: String,
            peer_node_id: BigInt,
        ) -> Result<()> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            let addr: std::net::SocketAddr = peer_addr
                .parse()
                .map_err(|e| Error::from_reason(format!("invalid peer address: {}", e)))?;

            let pubkey_bytes = hex::decode(&peer_public_key)
                .map_err(|e| Error::from_reason(format!("invalid public key hex: {}", e)))?;
            if pubkey_bytes.len() != 32 {
                return Err(Error::from_reason("public key must be 32 bytes"));
            }
            let mut pubkey = [0u8; 32];
            pubkey.copy_from_slice(&pubkey_bytes);

            let peer_node_id = crate::common::bigint_u64(peer_node_id)?;
            node.connect(addr, &pubkey, peer_node_id)
                .await
                .map_err(|e| Error::from_reason(format!("connect failed: {}", e)))?;
            Ok(())
        }

        /// Accept an incoming connection (responder side).
        #[napi]
        pub async fn accept(&self, peer_node_id: BigInt) -> Result<String> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            let peer_node_id = crate::common::bigint_u64(peer_node_id)?;
            let (addr, _) = node
                .accept(peer_node_id)
                .await
                .map_err(|e| Error::from_reason(format!("accept failed: {}", e)))?;
            Ok(addr.to_string())
        }

        /// Start the receive loop, heartbeats, and router.
        ///
        /// Declared `async` so napi-rs invokes it with an active
        /// tokio runtime — `MeshNode::start()` spawns background
        /// tasks via `tokio::spawn` and panics outside a reactor.
        #[napi]
        pub async fn start(&self) -> Result<()> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            node.start();
            Ok(())
        }

        /// Send raw bytes to a direct peer.
        #[napi]
        pub async fn push_to(&self, peer_addr: String, data: Buffer) -> Result<bool> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            let addr: std::net::SocketAddr = peer_addr
                .parse()
                .map_err(|e| Error::from_reason(format!("invalid address: {}", e)))?;

            let batch = net::event::Batch {
                shard_id: 0,
                events: vec![net::event::InternalEvent::new(
                    bytes::Bytes::copy_from_slice(data.as_ref()),
                    0,
                    0,
                )],
                sequence_start: 0,
                process_nonce: net::event::batch_process_nonce(),
            };

            node.send_to_peer(addr, &batch)
                .await
                .map_err(|e| Error::from_reason(format!("send failed: {}", e)))?;
            Ok(true)
        }

        /// Poll for received events.
        #[napi]
        pub async fn poll(&self, limit: u32) -> Result<Vec<StoredEvent>> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            let result = node
                .poll_shard(0, None, limit as usize)
                .await
                .map_err(|e| Error::from_reason(format!("poll failed: {}", e)))?;

            Ok(result
                .events
                .into_iter()
                .map(|e| {
                    // Preserve binary payloads in `raw_bytes`. `raw` is
                    // kept for back-compat with UTF-8 consumers but is
                    // deliberately empty (not a silent UTF-8-lossy
                    // substitution) when the payload isn't valid UTF-8 —
                    // callers that need fidelity must use `raw_bytes`.
                    let raw = e.raw_str().unwrap_or("").to_string();
                    let raw_bytes = Buffer::from(e.raw.to_vec());
                    StoredEvent {
                        id: e.id,
                        raw,
                        raw_bytes,
                        insertion_ts: e.insertion_ts as i64,
                        shard_id: e.shard_id as u32,
                    }
                })
                .collect())
        }

        /// Add a route to a destination node.
        #[napi]
        pub fn add_route(&self, dest_node_id: BigInt, next_hop_addr: String) -> Result<()> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            let addr: std::net::SocketAddr = next_hop_addr
                .parse()
                .map_err(|e| Error::from_reason(format!("invalid address: {}", e)))?;
            let dest_node_id = crate::common::bigint_u64(dest_node_id)?;
            node.router().add_route(dest_node_id, addr);
            Ok(())
        }

        /// Number of connected peers.
        #[napi]
        pub fn peer_count(&self) -> Result<u32> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            Ok(node.peer_count() as u32)
        }

        /// Number of nodes discovered via pingwave.
        #[napi]
        pub fn discovered_nodes(&self) -> Result<u32> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            Ok(node.proximity_graph().node_count() as u32)
        }

        // ─── Stream API ────────────────────────────────────────────

        /// Open (or look up) a stream to a connected peer. Repeated
        /// calls for the same `(peer, streamId)` return handles to the
        /// same underlying state (first-open wins; differing configs
        /// are logged and ignored).
        #[napi]
        pub fn open_stream(&self, peer_node_id: BigInt, opts: StreamOptions) -> Result<NetStream> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            let peer_u64 = crate::common::bigint_u64(peer_node_id)?;
            let stream_u64 = crate::common::bigint_u64(opts.stream_id.clone())?;
            let config = stream_config_from_opts(&opts)?;
            let core = node
                .open_stream(peer_u64, stream_u64, config)
                .map_err(|e| Error::from_reason(format!("open_stream failed: {}", e)))?;
            Ok(NetStream {
                peer_node_id: peer_u64,
                stream_id: stream_u64,
                core,
            })
        }

        /// Close a stream. Idempotent.
        #[napi]
        pub fn close_stream(&self, peer_node_id: BigInt, stream_id: BigInt) -> Result<()> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            let peer_u64 = crate::common::bigint_u64(peer_node_id)?;
            let stream_u64 = crate::common::bigint_u64(stream_id)?;
            node.close_stream(peer_u64, stream_u64);
            Ok(())
        }

        /// Send a batch of events on an explicit stream.
        ///
        /// **Error contract for SDK wrappers:** message prefixes are
        /// stable. `"stream would block"` = `BackpressureError`;
        /// `"stream not connected"` = `NotConnectedError`; anything
        /// else is a real transport failure. See `sdk-ts` for the
        /// class-based re-throw layer.
        #[napi]
        pub async fn send_on_stream(&self, stream: &NetStream, events: Vec<Buffer>) -> Result<()> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            let payloads: Vec<bytes::Bytes> = events
                .into_iter()
                .map(|b| bytes::Bytes::copy_from_slice(b.as_ref()))
                .collect();
            node.send_on_stream(&stream.core, &payloads)
                .await
                .map_err(stream_error_to_napi)
        }

        /// Send events, retrying on `Backpressure` with 5 ms → 200 ms
        /// exponential backoff up to `maxRetries` times. Transport
        /// errors are returned immediately (not retried).
        #[napi]
        pub async fn send_with_retry(
            &self,
            stream: &NetStream,
            events: Vec<Buffer>,
            max_retries: u32,
        ) -> Result<()> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            let payloads: Vec<bytes::Bytes> = events
                .into_iter()
                .map(|b| bytes::Bytes::copy_from_slice(b.as_ref()))
                .collect();
            node.send_with_retry(&stream.core, &payloads, max_retries as usize)
                .await
                .map_err(stream_error_to_napi)
        }

        /// Block the calling JS task until the send succeeds or a
        /// transport error occurs. Retries `Backpressure` with 5 ms →
        /// 200 ms exponential backoff up to 4096 times (~13 min worst
        /// case) — effectively "block until the network lets up" for
        /// practical workloads, but with a hard upper bound so runaway
        /// pressure can't hang a caller forever. Use `sendWithRetry`
        /// directly if you need a tighter bound.
        #[napi]
        pub async fn send_blocking(&self, stream: &NetStream, events: Vec<Buffer>) -> Result<()> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            let payloads: Vec<bytes::Bytes> = events
                .into_iter()
                .map(|b| bytes::Bytes::copy_from_slice(b.as_ref()))
                .collect();
            node.send_blocking(&stream.core, &payloads)
                .await
                .map_err(stream_error_to_napi)
        }

        /// Snapshot of per-stream stats. Returns `null` if the peer or
        /// stream isn't registered.
        #[napi]
        pub fn stream_stats(
            &self,
            peer_node_id: BigInt,
            stream_id: BigInt,
        ) -> Result<Option<NetStreamStats>> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            let peer_u64 = crate::common::bigint_u64(peer_node_id)?;
            let stream_u64 = crate::common::bigint_u64(stream_id)?;
            Ok(node
                .stream_stats(peer_u64, stream_u64)
                .map(|s| NetStreamStats {
                    tx_seq: BigInt::from(s.tx_seq),
                    rx_seq: BigInt::from(s.rx_seq),
                    inbound_pending: BigInt::from(s.inbound_pending),
                    last_activity_ns: BigInt::from(s.last_activity_ns),
                    active: s.active,
                    backpressure_events: BigInt::from(s.backpressure_events),
                    tx_credit_remaining: s.tx_credit_remaining,
                    tx_window: s.tx_window,
                    credit_grants_received: BigInt::from(s.credit_grants_received),
                    credit_grants_sent: BigInt::from(s.credit_grants_sent),
                }))
        }

        // =====================================================
        // Channels (distributed pub/sub)
        // =====================================================

        /// Register a channel on this (publisher) node. Subscribers
        /// who ask to join are validated against this config before
        /// being added to the roster.
        ///
        /// `config` is a JSON object mirroring the core `ChannelConfig`:
        ///
        /// ```json
        /// {
        ///   "name": "sensors/temp",
        ///   "visibility": "global",   // "subnet-local" | "parent-visible" | "exported" | "global"
        ///   "reliable": true,
        ///   "requireToken": false,
        ///   "priority": 0,
        ///   "maxRatePps": 1000
        /// }
        /// ```
        ///
        /// (The v1 binding does not expose `publishCaps` /
        /// `subscribeCaps` — those require a capability surface that
        /// lands with the security plan.)
        #[napi]
        pub fn register_channel(&self, config: ChannelConfigJs) -> Result<()> {
            let cfg = config.into_core()?;
            self.load_node()?;
            self.channel_configs.insert(cfg);
            Ok(())
        }

        /// Ask `publisher_node_id` to add this node to `channel`'s
        /// subscriber set. Blocks until the publisher's `Ack` arrives
        /// or the membership-ack timeout elapses.
        ///
        /// Optional `token` is the serialized `PermissionToken` bytes
        /// (161 bytes) — attach it when the publisher set
        /// `requireToken = true` on the channel, or when the caller's
        /// caps don't satisfy `subscribeCaps` on their own.
        #[napi]
        pub async fn subscribe_channel(
            &self,
            publisher_node_id: BigInt,
            channel: String,
            token: Option<Buffer>,
        ) -> Result<()> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            let pub_id = bigint_u64_lossless(publisher_node_id)?;
            let name = net::adapter::net::ChannelName::new(&channel)
                .map_err(|e| Error::from_reason(format!("channel: invalid name: {}", e)))?;
            match token {
                Some(bytes) => {
                    let parsed =
                        net::adapter::net::identity::PermissionToken::from_bytes(bytes.as_ref())
                            .map_err(crate::identity::token_err_for)?;
                    node.subscribe_channel_with_token(pub_id, name, parsed)
                        .await
                        .map_err(map_channel_adapter_error)
                }
                None => node
                    .subscribe_channel(pub_id, name)
                    .await
                    .map_err(map_channel_adapter_error),
            }
        }

        /// Mirror of [`Self::subscribe_channel`]. Idempotent on the
        /// publisher side.
        #[napi]
        pub async fn unsubscribe_channel(
            &self,
            publisher_node_id: BigInt,
            channel: String,
        ) -> Result<()> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            let pub_id = bigint_u64_lossless(publisher_node_id)?;
            let name = net::adapter::net::ChannelName::new(&channel)
                .map_err(|e| Error::from_reason(format!("channel: invalid name: {}", e)))?;
            node.unsubscribe_channel(pub_id, name)
                .await
                .map_err(map_channel_adapter_error)
        }

        /// Publish one payload to every subscriber of `channel`.
        /// Returns a `PublishReport` describing per-peer outcomes.
        ///
        /// `config` maps to the core `PublishConfig`:
        ///
        /// ```json
        /// {
        ///   "reliability": "reliable",          // or "fire_and_forget"
        ///   "onFailure":   "best_effort",       // or "fail_fast" | "collect"
        ///   "maxInflight": 32
        /// }
        /// ```
        #[napi]
        pub async fn publish(
            &self,
            channel: String,
            payload: Buffer,
            config: Option<PublishConfigJs>,
        ) -> Result<PublishReportJs> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            let name = net::adapter::net::ChannelName::new(&channel)
                .map_err(|e| Error::from_reason(format!("channel: invalid name: {}", e)))?;
            let pub_cfg = config.unwrap_or_default().into_core()?;
            let publisher = net::adapter::net::ChannelPublisher::new(name, pub_cfg);
            let payload_bytes = bytes::Bytes::copy_from_slice(payload.as_ref());
            let report = node
                .publish(&publisher, payload_bytes)
                .await
                .map_err(map_channel_adapter_error)?;
            Ok(PublishReportJs::from_core(report))
        }

        /// Announce this node's capabilities to every directly-
        /// connected peer. Also self-indexes, so `findNodes` on the
        /// same node matches on the announcement.
        ///
        /// Multi-hop propagation is deferred — peers more than one
        /// hop away will not see the announcement.
        #[napi]
        pub async fn announce_capabilities(
            &self,
            caps: crate::capabilities::CapabilitySetJs,
        ) -> Result<()> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            let core = crate::capabilities::capability_set_from_js(caps);
            node.announce_capabilities(core)
                .await
                .map_err(|e| Error::from_reason(format!("capability: {}", e)))
        }

        /// Query the local capability index. Returns node ids
        /// (including our own if we self-match) whose latest
        /// announcement matches `filter`.
        #[napi]
        pub fn find_nodes(
            &self,
            filter: crate::capabilities::CapabilityFilterJs,
        ) -> Result<Vec<BigInt>> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            let core = crate::capabilities::capability_filter_from_js(filter);
            Ok(node
                .find_nodes_by_filter(&core)
                .into_iter()
                .map(BigInt::from)
                .collect())
        }

        /// Scoped variant of [`Self::find_nodes`]. Filters candidates
        /// through a `ScopeFilterJs` (derived from each node's
        /// `scope:*` reserved tags). Untagged nodes stay visible
        /// under most filters by design; nodes tagged
        /// `scope:subnet-local` only show up under `sameSubnet`.
        #[napi]
        pub fn find_nodes_scoped(
            &self,
            filter: crate::capabilities::CapabilityFilterJs,
            scope: crate::capabilities::ScopeFilterJs,
        ) -> Result<Vec<BigInt>> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            let core = crate::capabilities::capability_filter_from_js(filter);
            let owned = crate::capabilities::scope_filter_from_js(scope);
            let ids = crate::capabilities::with_scope_filter(&owned, |f| {
                node.find_nodes_by_filter_scoped(&core, f)
            });
            Ok(ids.into_iter().map(BigInt::from).collect())
        }

        /// Bucketed aggregation over the local capability fold —
        /// `Fold::aggregate(matcher, groupBy, agg)`. Arguments are
        /// JSON-encoded tagged unions; the TS SDK ships ergonomic
        /// constructors. Returns
        /// `[{ bucket: string, value: bigint }]` sorted lex by bucket.
        ///
        /// `matcherJson = null` walks every entry. Phase 6c-A of
        /// `MULTIFOLD_PHASE_6C_CAPACITY_AGGREGATION.md`.
        #[napi]
        pub fn capability_aggregate(
            &self,
            matcher_json: Option<String>,
            group_by_json: String,
            aggregation_json: String,
        ) -> Result<Vec<crate::capability_aggregation::AggregateRowJs>> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            crate::capability_aggregation::aggregate(
                node.capability_fold(),
                matcher_json,
                group_by_json,
                aggregation_json,
            )
        }

        /// Capacity-ranked materialized view over the local
        /// capability fold — `Fold::capacity_ranking(query,
        /// rttLookup)`. `queryJson` is a JSON-encoded
        /// `CapacityQuery`; `rttEntries` is the materialized RTT map
        /// (`null`/empty disables the RTT filter regardless of
        /// `query.maxRttMs`). Faulty entries are always excluded;
        /// rows return sorted by `available` desc, ties broken by
        /// bucket asc, truncated to `query.limit`.
        ///
        /// Phase 6c-B of `MULTIFOLD_PHASE_6C_CAPACITY_AGGREGATION.md`.
        /// The plan flags a `ThreadsafeFunction` closure variant as
        /// the natural shape for TS; that ships as a follow-up.
        #[napi]
        pub fn capability_capacity_ranking(
            &self,
            query_json: String,
            rtt_entries: Option<Vec<crate::capability_aggregation::RttEntryJs>>,
        ) -> Result<Vec<crate::capability_aggregation::CapacityRowJs>> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            crate::capability_aggregation::capacity_ranking(
                node.capability_fold(),
                query_json,
                rtt_entries,
            )
        }

        /// Shutdown the mesh node.
        #[napi]
        pub async fn shutdown(&self) -> Result<()> {
            let node_arc = self
                .node
                .swap(None)
                .ok_or_else(|| Error::from_reason("already shut down"))?;

            match Arc::try_unwrap(node_arc) {
                Ok(node) => {
                    node.shutdown()
                        .await
                        .map_err(|e| Error::from_reason(format!("shutdown failed: {}", e)))?;
                }
                Err(arc) => {
                    // Put it back if there are outstanding references
                    self.node.store(Some(arc));
                    return Err(Error::from_reason(
                        "cannot shutdown: outstanding references exist",
                    ));
                }
            }
            Ok(())
        }

        fn load_node(&self) -> Result<arc_swap::Guard<Option<Arc<MeshNode>>>> {
            let guard = self.node.load();
            if guard.is_none() {
                return Err(Error::from_reason("MeshNode has been shut down"));
            }
            Ok(guard)
        }

        /// Clone an `Arc<MeshNode>` out of the `ArcSwapOption`
        /// slot. Used by sibling modules (`compute`, `mesh_rpc`)
        /// that build their own SDK-level wrappers against the
        /// same live node — no second UDP socket, no second
        /// handshake table. Returns an error if the node has
        /// been shut down.
        ///
        /// Read the slot once and inspect the snapshot — a
        /// concurrent shutdown can swap to None between the
        /// `load` inside `load_node` and a subsequent re-check,
        /// so the previous "load_node + as_ref + expect" pattern
        /// could panic on a real shutdown race. Surface as a
        /// typed error instead.
        #[cfg(any(
            feature = "compute",
            feature = "cortex",
            feature = "aggregator",
            feature = "payments",
            feature = "publish"
        ))]
        pub(crate) fn node_arc_clone(&self) -> Result<Arc<MeshNode>> {
            let guard = self.node.load();
            match guard.as_ref() {
                Some(arc) => Ok(arc.clone()),
                None => Err(Error::from_reason("MeshNode has been shut down")),
            }
        }

        /// Share the `ChannelConfigRegistry` for sibling-module
        /// access. Same rationale as [`Self::node_arc_clone`].
        /// Currently consumed by `compute` only; nRPC's serve_rpc
        /// auto-registers via the SDK glue without needing
        /// per-binding access. Kept gated on either feature so the
        /// accessor is available if `mesh_rpc` ever needs it.
        #[cfg(any(feature = "compute", feature = "cortex", feature = "payments"))]
        #[cfg_attr(
            all(
                any(feature = "cortex", feature = "payments"),
                not(feature = "compute")
            ),
            allow(dead_code)
        )]
        pub(crate) fn channel_configs_arc(&self) -> Arc<net::adapter::net::ChannelConfigRegistry> {
            self.channel_configs.clone()
        }
    }

    // =====================================================================
    // SDK Phase 7 slice 2 — custom `PlacementFilter` callback registry.
    //
    // Bindings expose `registerPlacementFilter(id, fn)` and
    // `unregisterPlacementFilter(id)` so JS code can plug a
    // `(candidate) => boolean` predicate into the substrate's
    // `select_*` machinery. The wrapper bridges TSFN ↔ trait
    // (lives in `placement.rs`); registration goes through the
    // process-wide singleton `global_placement_filter_registry`.
    //
    // Sits in its own `#[napi] impl NetMesh` block — same
    // expansion-time concern as the NAT traversal block below.
    // =====================================================================

    #[cfg(feature = "net")]
    #[napi]
    impl NetMesh {
        /// Register a JS placement-filter predicate under `id`.
        ///
        /// JS contract: `fn(candidate: PlacementCandidate) =>
        /// boolean`. Returning `true` keeps the candidate
        /// (placement-score 1.0); returning `false` (or throwing)
        /// vetoes it. The predicate runs per candidate per
        /// placement decision — keep it tight and avoid I/O.
        ///
        /// Returns `false` if `id` is already registered (the SDK's
        /// `placementFilterFromFn` generates unique IDs by counter,
        /// so collisions are an SDK-side concern). Use
        /// `unregisterPlacementFilter` first if you intend to swap
        /// the predicate behind a stable id.
        #[napi]
        pub fn register_placement_filter(
            &self,
            id: String,
            predicate: napi::bindgen_prelude::Function<
                'static,
                crate::placement::PlacementCandidateJs,
                bool,
            >,
        ) -> Result<bool> {
            use net::adapter::net::behavior::placement::PlacementFilter;
            use net::adapter::net::behavior::placement_registry::global_placement_filter_registry;

            let guard = self.load_node()?;
            let node = guard
                .as_ref()
                .ok_or_else(|| Error::from_reason("MeshNode has been shut down"))?;
            let capability_fold = node.capability_fold().clone();

            // Build the TSFN inside the Node main thread; the
            // resulting handle is `Send + Sync + Clone` and can
            // cross threads as part of the wrapper.
            let tsfn: crate::placement::PlacementFilterTsfn =
                predicate.build_threadsafe_function().build()?;
            let wrapper =
                crate::placement::TsfnPlacementFilter::new(id.clone(), tsfn, capability_fold);
            let arc: std::sync::Arc<dyn PlacementFilter> = std::sync::Arc::new(wrapper);

            // SDK Phase 7 polish: `"node"` binding label drives the
            // `dataforts_placement_callback_invocations_total{binding="node"}`
            // counter on the substrate registry.
            Ok(global_placement_filter_registry().register(id, arc, "node"))
        }

        /// Drop the placement-filter registration under `id`.
        ///
        /// Returns `true` if `id` was registered. Existing
        /// `Arc<dyn PlacementFilter>` clones held by in-flight
        /// scheduler calls keep the predicate alive until those
        /// calls finish — see the registry docs.
        #[napi]
        pub fn unregister_placement_filter(&self, id: String) -> bool {
            use net::adapter::net::behavior::placement_registry::global_placement_filter_registry;
            global_placement_filter_registry().unregister(&id)
        }

        /// Whether `id` is currently registered. Mainly for tests.
        #[napi]
        pub fn has_placement_filter(&self, id: String) -> bool {
            use net::adapter::net::behavior::placement_registry::global_placement_filter_registry;
            global_placement_filter_registry().contains(&id)
        }
    }

    // =====================================================================
    // Local tool publishing — a node announces its OWN tools as mesh
    // capabilities backed by a JS async handler (the inverse of `net wrap`).
    // Its own `#[napi] impl` block gated on `publish`, for the same
    // expansion-time reason the AI-tool block below calls out.
    // =====================================================================

    #[cfg(feature = "publish")]
    #[napi]
    impl NetMesh {
        /// Publish this node's OWN local `tools` as mesh capabilities, backed by
        /// a JS **async** `handler`. Each tool is
        /// `{ name, description?, inputSchema }` (`inputSchema` a JSON-object
        /// string); the `handler` is
        /// `(args: { toolName, argumentsJson }) => Promise<{ text, isError? }>`,
        /// called when a consumer invokes a tool — its resolved `text` is the
        /// tool's output (`isError: true` flags a tool-level failure). A
        /// consumer discovers + invokes these through the ordinary
        /// `CapabilityGateway`; no consume-side change.
        ///
        /// `options.ownerOrigin` scopes admission: an `originHash` (BigInt)
        /// admits only that caller; omit it to admit **only this node itself**
        /// (fail-closed default — the tools are backed by an arbitrary local
        /// callback). Set `options.allowAnyCaller = true` to explicitly admit
        /// every mesh peer (overrides `ownerOrigin`; gate invocations yourself).
        ///
        /// Resolves to a [`LocalPublicationHandle`](crate::publish::LocalPublicationHandle)
        /// that must be held to keep the tools published. This node must be
        /// `start()`ed. (Requires the `publish` feature.)
        #[napi]
        pub fn publish_tools<'env>(
            &self,
            env: &'env Env,
            tools: Vec<crate::publish::PublishToolJs>,
            handler: Function<
                '_,
                crate::publish::ToolInvokeArgs,
                Promise<crate::publish::ToolCallResultJs>,
            >,
            options: Option<crate::publish::PublishOptions>,
        ) -> Result<PromiseRaw<'env, crate::publish::LocalPublicationHandle>> {
            let node = self.node_arc_clone()?;
            // Free path: no pricing, no payment gate.
            crate::publish::spawn_publish_tools(
                env,
                node,
                tools,
                handler,
                options,
                std::collections::BTreeMap::new(),
                None,
            )
        }
    }

    // =====================================================================
    // AI tool calling surface — separate `#[napi] impl` block for the
    // same expansion-time reason the NAT traversal block below calls
    // out: napi-derive collects method names regardless of inner
    // `#[cfg]` attributes, so a `#[cfg(feature = "tool")]` on the
    // method alone leaves a dangling `list_tools_c_callback` reference
    // when the feature is off. Gating the impl block itself sidesteps
    // that.
    // =====================================================================

    #[cfg(feature = "tool")]
    #[napi]
    impl NetMesh {
        /// Walk the local capability fold for every AI tool
        /// published in the mesh and return one
        /// [`ToolDescriptorJs`] per `(toolId, version)` slot, with
        /// `nodeCount` filled in by the aggregating walk.
        ///
        /// One in-memory pass; no network. Schemas live as
        /// JSON-encoded strings on `descriptor.inputSchema` /
        /// `descriptor.outputSchema` — call `JSON.parse(...)` if
        /// you need the parsed shape for a provider's
        /// tool-definition lowering.
        ///
        /// Mirror of the Rust SDK's `Mesh::list_tools(None)`. v1
        /// always walks unfiltered; matcher-pushdown lands in a
        /// follow-up that adds `TagMatcherJs` to the napi surface.
        #[napi]
        pub fn list_tools(&self) -> Result<Vec<crate::tool::ToolDescriptorJs>> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            Ok(node
                .list_tools(None)
                .into_iter()
                .map(crate::tool::descriptor_to_js)
                .collect())
        }

        /// Event-driven watch over the local capability fold's tool
        /// view. Returns a [`ToolWatchIter`](crate::tool::ToolWatchIter)
        /// whose `next()` yields one JSON-encoded `ToolListChange` per
        /// addition / removal / publisher-count change — delivered the
        /// moment the fold mutates, not on a timer. The `watchTools`
        /// TS wrapper `JSON.parse`s each change into the discriminated
        /// union.
        ///
        /// `intervalMs` is a debounce ceiling, NOT a poll cadence:
        /// `null`/`0` is pure event-driven (idle fold = zero periodic
        /// work); a positive value additionally guarantees a re-diff at
        /// least every `intervalMs` as a safety net.
        ///
        /// `async` so the substrate diff task is spawned inside the
        /// napi tokio runtime (the body has no `await` of its own — the
        /// substrate `watch_tools` is sync but `tokio::spawn`s).
        #[napi]
        pub async fn watch_tools(
            &self,
            interval_ms: Option<u32>,
        ) -> Result<crate::tool::ToolWatchIter> {
            let node = self.node_arc_clone()?;
            let interval = match interval_ms {
                Some(ms) if ms > 0 => Some(std::time::Duration::from_millis(ms as u64)),
                _ => None,
            };
            let watch = node.watch_tools(None, interval);
            Ok(crate::tool::new_tool_watch_iter(watch))
        }
    }

    // =====================================================================
    // NAT traversal surface — separate `#[napi] impl` block so the outer
    // `#[napi]` on the main `impl NetMesh` doesn't try to register the
    // `*_c_callback` symbols that don't exist without the `nat-traversal`
    // feature. napi-derive collects method names at expansion time
    // regardless of inner `#[cfg]` attributes, so the feature gate has
    // to live on the *impl block* itself. Same structural rationale as
    // the `test-helpers` block below.
    //
    // Framing (plan §5, load-bearing): every user-visible docstring
    // positions NAT traversal as **optimization, not correctness**.
    // Nodes behind NAT can always reach each other via the routed-
    // handshake path; these APIs let the mesh upgrade to a direct
    // path when the NATs allow it. A `nat_type` of `symmetric` or a
    // `traversal: punch-failed` error is not a connectivity failure.
    // =====================================================================

    #[cfg(feature = "nat-traversal")]
    #[napi]
    impl NetMesh {
        /// NAT classification for this mesh, as a stable string:
        /// `"open" | "cone" | "symmetric" | "unknown"`. `unknown`
        /// is the pre-classification state; classification runs
        /// in the background after `start()` once ≥2 peers are
        /// connected.
        #[napi]
        pub fn nat_type(&self) -> Result<String> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            Ok(nat_class_to_string(node.nat_class()))
        }

        /// This mesh's public-facing `ip:port` as observed by a
        /// remote peer, or `null` before classification has
        /// produced an observation. Rides on outbound capability
        /// announcements so peers can attempt direct connects
        /// without a separate discovery round-trip.
        #[napi]
        pub fn reflex_addr(&self) -> Result<Option<String>> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            Ok(node.reflex_addr().map(|addr| addr.to_string()))
        }

        /// NAT classification most recently advertised by
        /// `peer_node_id` (parsed from the `nat:*` tag on their
        /// capability announcement). Returns `"unknown"` when
        /// the peer hasn't announced. The pair-type matrix
        /// treats Unknown as "attempt direct, fall back on
        /// failure," never "don't attempt."
        #[napi]
        pub fn peer_nat_type(&self, peer_node_id: BigInt) -> Result<String> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            // Validate sign + losslessness — raw `get_u64()`
            // silently reinterprets negatives + truncates oversize
            // BigInts, which could target the wrong peer on a
            // malformed caller input. See `crate::common::bigint_u64`
            // for the check.
            let peer_id = crate::common::bigint_u64(peer_node_id)?;
            Ok(nat_class_to_string(node.peer_nat_class(peer_id)))
        }

        /// Send one reflex probe to `peer_node_id` and resolve
        /// with the public `ip:port` the peer observed on the
        /// probe's UDP envelope. Useful for tests and for
        /// diagnosing misclassifications.
        ///
        /// Rejects with an `Error` whose `message` follows the
        /// `traversal: <kind>[: <detail>]` convention (kinds:
        /// `reflex-timeout`, `peer-not-reachable`, `transport`).
        #[napi]
        pub async fn probe_reflex(&self, peer_node_id: BigInt) -> Result<String> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            let peer_id = crate::common::bigint_u64(peer_node_id)?;
            node.probe_reflex(peer_id)
                .await
                .map(|addr| addr.to_string())
                .map_err(traversal_err)
        }

        /// Explicitly re-run the classification sweep. Normally
        /// the background loop handles this; call this after a
        /// suspected NAT rebind (e.g. gateway reboot) to
        /// accelerate re-classification. No-op when fewer than
        /// 2 peers are connected. Never rejects.
        #[napi]
        pub async fn reclassify_nat(&self) -> Result<()> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            node.reclassify_nat().await;
            Ok(())
        }

        /// Cumulative NAT-traversal counters for this mesh.
        /// Object shape: `{ punchesAttempted, punchesSucceeded,
        /// relayFallbacks }` — all u64 bigints, monotonic, never
        /// reset. Useful for telemetry on punch success rate and
        /// relay load.
        #[napi]
        pub fn traversal_stats(&self) -> Result<TraversalStats> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            let snap = node.traversal_stats();
            Ok(TraversalStats {
                punches_attempted: BigInt::from(snap.punches_attempted),
                punches_succeeded: BigInt::from(snap.punches_succeeded),
                relay_fallbacks: BigInt::from(snap.relay_fallbacks),
            })
        }

        /// Establish a session to `peer_node_id` via the
        /// rendezvous path. The pair-type matrix decides between
        /// a direct handshake and a relay-coordinated punch;
        /// either way the returned session is equivalent in
        /// correctness to `connect()`.
        ///
        /// **Optimization, not correctness.** `connect_direct`
        /// always resolves (on punch-failed, the session is
        /// established via the routed-handshake fallback).
        /// Inspect `traversal_stats()` afterward to distinguish
        /// a successful punch from a relay fallback.
        ///
        /// Rejects with `traversal: peer-not-reachable` when we
        /// have no cached reflex for `peer_node_id`, or
        /// `traversal: transport: ...` on a socket-level
        /// handshake error.
        #[napi]
        pub async fn connect_direct(
            &self,
            peer_node_id: BigInt,
            peer_public_key: Buffer,
            coordinator: BigInt,
        ) -> Result<()> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            let pubkey_bytes: &[u8] = peer_public_key.as_ref();
            if pubkey_bytes.len() != 32 {
                return Err(Error::from_reason(format!(
                    "peer_public_key must be 32 bytes, got {}",
                    pubkey_bytes.len()
                )));
            }
            let mut pubkey = [0u8; 32];
            pubkey.copy_from_slice(pubkey_bytes);
            // Validate both ids before the async work — raw
            // `get_u64()` would silently reinterpret negatives
            // or truncate oversize values, picking the wrong
            // peer or coordinator for the rendezvous.
            let peer_id = crate::common::bigint_u64(peer_node_id)?;
            let coord_id = crate::common::bigint_u64(coordinator)?;
            node.connect_direct(peer_id, &pubkey, coord_id)
                .await
                .map_err(traversal_err)?;
            Ok(())
        }

        /// Install a runtime reflex override. Forces `natType()`
        /// to `"open"` and `reflexAddr()` to `external`
        /// immediately, short-circuiting any further classifier
        /// sweeps. Runtime counterpart of the `reflexOverride`
        /// option passed at `create()` time — useful when a
        /// port-forward goes live mid-session or when a stage-4
        /// port-mapping task has just installed a mapping.
        ///
        /// **Optimization, not correctness.** Nodes without an
        /// override still reach every peer via the routed-
        /// handshake path.
        ///
        /// `external` is an `"ip:port"` string — rejects with
        /// `invalid reflex override` if it fails to parse.
        #[napi]
        pub fn set_reflex_override(&self, external: String) -> Result<()> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            let addr: std::net::SocketAddr = external
                .parse()
                .map_err(|e| Error::from_reason(format!("invalid reflex override: {e}")))?;
            node.set_reflex_override(addr);
            Ok(())
        }

        /// Drop a previously-installed reflex override. The
        /// classifier resumes on its normal cadence;
        /// `reflexAddr()` clears to `null` immediately so a
        /// between-sweep read doesn't return a stale override.
        ///
        /// No-op when no override is active — safe to call
        /// unconditionally on shutdown or revoke paths.
        #[napi]
        pub fn clear_reflex_override(&self) -> Result<()> {
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            node.clear_reflex_override();
            Ok(())
        }
    }

    // =====================================================================
    // Test-only helpers — separate `#[napi] impl` block so the outer
    // `#[napi]` on the main `impl NetMesh` doesn't try to register the
    // `test_inject_synthetic_peer_c_callback` symbol that doesn't exist
    // without the `test-helpers` feature. napi-derive collects method
    // names at expansion time regardless of inner `#[cfg]` attributes,
    // so the feature gate has to live on the *impl block* itself.
    // =====================================================================

    #[cfg(feature = "test-helpers")]
    #[napi]
    impl NetMesh {
        /// **Test-only** helper for the vitest groups suite.
        /// Injects a synthetic capability announcement directly
        /// into the local capability index, simulating a peer
        /// announcement without going through a real handshake.
        ///
        /// Gated behind the `test-helpers` feature so it is
        /// **not** exported to production JS consumers. Enabling
        /// `groups` alone does not pull this in; vitest builds with
        /// `--features groups,test-helpers` explicitly. Production
        /// code uses the normal `announce_capabilities` path.
        #[napi]
        pub fn test_inject_synthetic_peer(&self, node_id: BigInt) -> Result<()> {
            use net::adapter::net::behavior::capability::{CapabilityAnnouncement, CapabilitySet};
            use net::adapter::net::identity::EntityId;
            // Validate sign/lossless — a negative or >u64::MAX
            // BigInt would otherwise silently wrap into a garbage
            // node id and corrupt the capability index the test
            // is trying to stage.
            let nid = crate::common::bigint_u64(node_id).map_err(|e| {
                Error::from_reason(format!("test_inject_synthetic_peer: {}", e.reason))
            })?;
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            let eid = EntityId::from_bytes([0u8; 32]);
            node.test_inject_capability_announcement(CapabilityAnnouncement::new(
                nid,
                eid,
                1,
                CapabilitySet::new(),
            ));
            Ok(())
        }

        /// Test-only — same shape as
        /// [`Self::test_inject_synthetic_peer`] but takes an array of
        /// canonical tag strings to install on the synthetic peer.
        /// Used by the Phase 6c capability-aggregation smoke tests
        /// to stage multi-bucket fixtures without spinning up
        /// multiple meshes.
        #[napi]
        pub fn test_inject_synthetic_peer_with_tags(
            &self,
            node_id: BigInt,
            tags: Vec<String>,
        ) -> Result<()> {
            use net::adapter::net::behavior::capability::{CapabilityAnnouncement, CapabilitySet};
            use net::adapter::net::behavior::Tag;
            use net::adapter::net::identity::EntityId;
            let nid = crate::common::bigint_u64(node_id).map_err(|e| {
                Error::from_reason(format!(
                    "test_inject_synthetic_peer_with_tags: {}",
                    e.reason
                ))
            })?;
            let guard = self.load_node()?;
            let node = guard.as_ref().unwrap();
            let mut caps = CapabilitySet::new();
            // Insert directly via the permissive `Tag::parse` so
            // reserved-prefix tags (`scope:region:us-east`, etc.)
            // make it into the synthesized cap set; `add_tag` rejects
            // reserved prefixes by design.
            for s in tags {
                if let Ok(t) = Tag::parse(&s) {
                    caps.tags.insert(t);
                }
            }
            let eid = EntityId::from_bytes([0u8; 32]);
            node.test_inject_capability_announcement(CapabilityAnnouncement::new(
                nid, eid, 1, caps,
            ));
            Ok(())
        }
    }
}

#[cfg(feature = "net")]
pub use mesh_bindings::*;
