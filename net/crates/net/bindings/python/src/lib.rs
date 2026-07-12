//! Python bindings for Net event bus.
//!
//! Provides high-performance event ingestion and consumption for Python.

#[cfg(feature = "aggregator")]
mod aggregator;
mod async_bridge;
#[cfg(feature = "dataforts")]
mod blob;
mod capability_aggregation;
#[cfg(feature = "cortex")]
mod cortex;
#[cfg(feature = "dataforts")]
mod transport;
// Identity / capabilities / subnets ride the `net` feature as a
// single security unit — they share `adapter::net`'s subprotocol
// dispatch and are operationally inseparable.
#[cfg(feature = "net")]
mod capabilities;
#[cfg(feature = "compute")]
mod compute;
// Local consent surface — CapabilityId, ConsentPolicy, and the
// lock-protocol PinStore, graduated to net-mesh-sdk by the MCP
// bridge SDK plan's P0 and bound here in P1. Pure local-state
// primitives (no mesh dependency), so the feature pulls only the
// net-sdk dep itself.
#[cfg(feature = "consent")]
mod consent;
#[cfg(feature = "groups")]
mod groups;
// MCP bridge pure helpers — classify + lower_tool only (the bridge's
// forwarding/keychain internals are never bound).
#[cfg(feature = "net")]
mod identity;
#[cfg(feature = "mcp")]
mod mcp_helpers;
// Native consent-gated capability gateway (search / describe / invoke over an
// embedded NetMesh node) — the `HERMES_INTEGRATION_PLAN.md` Phase 1 enabler.
// Needs both a live node (`net`) and the bridge's gateway + shared gate
// (`mcp`), so it is gated on both.
#[cfg(all(feature = "net", feature = "mcp"))]
mod capability_gateway;
// Outbound HTTP-402 client (`PAYMENTS_LANGUAGE_SDKS_PLAN` WS-P2): pay an
// external x402 HTTP API through the same spend policy as the gateway.
// Behind `payments-http` (opt-in — it pulls reqwest via
// `net-payments/http-facilitator`).
#[cfg(feature = "payments-http")]
mod payment_http;
// Provider-side payment surface: pricing a capability (+ charging for it).
// Behind `payments` (needs the net-payments core types).
#[cfg(feature = "payments")]
mod payment_provider;
// Delegated agent identity (`HERMES_INTEGRATION_PLAN.md` Phase 3): the
// DelegationChain (root → machine → gateway → subagent) + shared
// RevocationRegistry + child-`Identity` derivation. Thin wrappers over
// `net_sdk::delegation`; needs the SDK's `net` feature.
#[cfg(feature = "delegation")]
mod delegation;
// Device enrollment (Hermes V2 Phase 1): the invite → join → approve handshake
// + the operator device-lifecycle facade. Thin wrappers over
// `net_sdk::{enrollment,operator,devices}`; shares the `delegation` gate (same
// SDK `net` surface, returns `DelegationChain` handles).
#[cfg(feature = "delegation")]
mod enrollment;
// Publish this node's OWN local tools as mesh capabilities (Hermes V2 Phase 2):
// the inverse of `net wrap`, backed by a Python async callback. Thin wrappers
// over `net_mcp::wrap::ServerPublisher::publish_tools`; gated on `publish`
// (mcp + delegation + cortex).
#[cfg(feature = "publish")]
mod publish;
// Agent-to-agent task handoff (Hermes V2 Phase 3): serve the A2A task lifecycle
// backed by a Python async task-executor callback + submit/status/cancel by node
// id. Thin wrappers over `net_sdk::{a2a,mesh_a2a}`; gated on `a2a`
// (delegation + cortex).
#[cfg(feature = "a2a")]
mod a2a;
// nRPC binding (B3: raw-bytes serve_rpc / call / call_streaming).
// Reuses the cortex feature gate because nRPC is part of the
// cortex / netdb feature unit. Sync handler API; async-Python
// handler support lands as a follow-up phase.
#[cfg(feature = "cortex")]
mod mesh_rpc;
// MeshDB query layer (Python SDK slice 1: factory AST +
// in-memory ChainReader + sync runner). Gated behind the
// crate's own `meshdb` Cargo feature so non-MeshDB builds
// stay slim.
#[cfg(feature = "meshdb")]
mod meshdb;
// MeshOS daemon-author SDK (Phase 2 slice 1: register / control
// receive / publish_log / graceful_shutdown). Builds on `compute`
// for the `MeshDaemon` trait + the `Identity` wrapper.
#[cfg(feature = "meshos")]
mod meshos;
// Deck SDK — operator-side bindings (Phase 4 slice 1). Builds on
// `meshos` for the supervisor runtime accessors.
#[cfg(feature = "deck")]
mod deck;
#[cfg(feature = "net")]
mod placement;
#[cfg(feature = "redis")]
mod redis_dedup;
#[cfg(feature = "net")]
mod subnets;

use parking_lot::RwLock;
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::sync::Arc;
use tokio::runtime::Runtime;

use net::{
    config::{AdapterConfig, BackpressureMode, EventBusConfig},
    consumer::Ordering,
    event::RawEvent,
    ConsumeRequest, EventBus, Filter,
};

#[cfg(feature = "redis")]
use net::config::RedisAdapterConfig;

#[cfg(feature = "jetstream")]
use net::config::JetStreamAdapterConfig;

#[cfg(feature = "net")]
use net::adapter::net::{NetAdapterConfig, ReliabilityConfig, StaticKeypair};

/// Result of an ingestion operation.
#[pyclass(from_py_object)]
#[derive(Clone)]
pub struct IngestResult {
    #[pyo3(get)]
    pub shard_id: u16,
    #[pyo3(get)]
    pub timestamp: u64,
}

#[pymethods]
impl IngestResult {
    fn __repr__(&self) -> String {
        format!(
            "IngestResult(shard_id={}, timestamp={})",
            self.shard_id, self.timestamp
        )
    }
}

/// A stored event returned from polling.
#[pyclass(from_py_object)]
#[derive(Clone)]
pub struct StoredEvent {
    #[pyo3(get)]
    pub id: String,
    #[pyo3(get)]
    pub raw: String,
    #[pyo3(get)]
    pub insertion_ts: u64,
    #[pyo3(get)]
    pub shard_id: u16,
}

#[pymethods]
impl StoredEvent {
    fn __repr__(&self) -> String {
        format!(
            "StoredEvent(id='{}', shard_id={}, insertion_ts={})",
            self.id, self.shard_id, self.insertion_ts
        )
    }

    /// Parse the raw JSON into a Python dict.
    fn parse(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let json_module = py.import("json")?;
        let result = json_module.call_method1("loads", (&self.raw,))?;
        Ok(result.into())
    }
}

/// Poll response containing events and cursor.
#[pyclass(from_py_object)]
#[derive(Clone)]
pub struct PollResponse {
    #[pyo3(get)]
    pub events: Vec<StoredEvent>,
    #[pyo3(get)]
    pub next_id: Option<String>,
    #[pyo3(get)]
    pub has_more: bool,
}

#[pymethods]
impl PollResponse {
    fn __repr__(&self) -> String {
        format!(
            "PollResponse(events=[...{}], next_id={:?}, has_more={})",
            self.events.len(),
            self.next_id,
            self.has_more
        )
    }

    fn __len__(&self) -> usize {
        self.events.len()
    }

    fn __iter__(slf: PyRef<'_, Self>) -> PyResult<Py<EventIterator>> {
        let iter = EventIterator {
            events: slf.events.clone(),
            index: 0,
        };
        Py::new(slf.py(), iter)
    }
}

#[pyclass]
struct EventIterator {
    events: Vec<StoredEvent>,
    index: usize,
}

#[pymethods]
impl EventIterator {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__(mut slf: PyRefMut<'_, Self>) -> Option<StoredEvent> {
        if slf.index < slf.events.len() {
            let event = slf.events[slf.index].clone();
            slf.index += 1;
            Some(event)
        } else {
            None
        }
    }
}

/// Ingestion statistics.
#[pyclass(from_py_object)]
#[derive(Clone)]
pub struct Stats {
    #[pyo3(get)]
    pub events_ingested: u64,
    #[pyo3(get)]
    pub events_dropped: u64,
}

#[pymethods]
impl Stats {
    fn __repr__(&self) -> String {
        format!(
            "Stats(events_ingested={}, events_dropped={})",
            self.events_ingested, self.events_dropped
        )
    }
}

/// Net keypair for encrypted UDP transport.
#[pyclass(from_py_object)]
#[derive(Clone)]
pub struct NetKeypair {
    /// Hex-encoded 32-byte public key
    #[pyo3(get)]
    pub public_key: String,
    /// Hex-encoded 32-byte secret key
    #[pyo3(get)]
    pub secret_key: String,
}

#[pymethods]
impl NetKeypair {
    fn __repr__(&self) -> String {
        format!(
            "NetKeypair(public_key='{}...', secret_key='[REDACTED]')",
            &self.public_key[..self.public_key.len().min(8)]
        )
    }
}

/// Generate a new Net keypair for encrypted UDP transport.
///
/// Returns a NetKeypair with hex-encoded public and secret keys.
/// Use this to generate keys for a responder, then share the public key
/// with the initiator.
///
/// Returns:
///     NetKeypair with public_key and secret_key attributes
#[cfg(feature = "net")]
#[pyfunction]
fn generate_net_keypair() -> NetKeypair {
    let keypair = StaticKeypair::generate();
    NetKeypair {
        public_key: hex::encode(keypair.public_key()),
        secret_key: hex::encode(keypair.secret_key()),
    }
}

/// High-performance event bus for Python.
///
/// Example usage:
/// ```python
/// from net import Net
///
/// # Create event bus
/// bus = Net(num_shards=4)
///
/// # Ingest events (fast path with raw JSON string)
/// bus.ingest_raw('{"token": "hello", "index": 0}')
///
/// # Or ingest a dict (convenience method)
/// bus.ingest({"token": "world", "index": 1})
///
/// # Poll events
/// response = bus.poll(limit=100)
/// for event in response:
///     print(event.raw)
///
/// bus.shutdown()
/// ```
#[pyclass]
pub struct Net {
    bus: Arc<RwLock<Option<EventBus>>>,
    runtime: Arc<Runtime>,
}

#[pymethods]
impl Net {
    /// Create a new Net event bus.
    ///
    /// Args:
    ///     num_shards: Number of shards (defaults to CPU core count)
    ///     ring_buffer_capacity: Ring buffer capacity per shard (must be power of 2)
    ///     backpressure_mode: One of "drop_newest", "drop_oldest", "fail_producer"
    ///     redis_url: Redis connection URL (e.g., "redis://localhost:6379")
    ///     redis_prefix: Stream key prefix (default: "net")
    ///     redis_pipeline_size: Maximum commands per pipeline (default: 1000)
    ///     redis_pool_size: Connection pool size (default: num_shards)
    ///     redis_connect_timeout_ms: Connection timeout in milliseconds (default: 5000)
    ///     redis_command_timeout_ms: Command timeout in milliseconds (default: 1000)
    ///     redis_max_stream_len: Maximum stream length, unlimited if not set
    ///     jetstream_url: NATS JetStream URL (e.g., "nats://localhost:4222")
    ///     jetstream_prefix: Stream name prefix (default: "net")
    ///     jetstream_connect_timeout_ms: Connection timeout in milliseconds (default: 5000)
    ///     jetstream_request_timeout_ms: Request timeout in milliseconds (default: 5000)
    ///     jetstream_max_messages: Maximum messages per stream, unlimited if not set
    ///     jetstream_max_bytes: Maximum bytes per stream, unlimited if not set
    ///     jetstream_max_age_ms: Maximum age for messages in milliseconds, unlimited if not set
    ///     jetstream_replicas: Number of stream replicas (default: 1)
    ///     net_bind_addr: Net local bind address (e.g., "127.0.0.1:9000")
    ///     net_peer_addr: Net remote peer address (e.g., "127.0.0.1:9001")
    ///     net_psk: Hex-encoded 32-byte pre-shared key
    ///     net_role: Connection role - "initiator" or "responder"
    ///     net_peer_public_key: Hex-encoded peer's public key (required for initiator)
    ///     net_secret_key: Hex-encoded secret key (required for responder)
    ///     net_public_key: Hex-encoded public key (required for responder)
    ///     net_reliability: Reliability mode - "none", "light", or "full" (default: "none")
    ///     net_heartbeat_interval_ms: Heartbeat interval in milliseconds (default: 5000)
    ///     net_session_timeout_ms: Session timeout in milliseconds (default: 30000)
    ///     net_batched_io: Enable batched I/O for Linux (default: False)
    ///     net_packet_pool_size: Packet pool size (default: 64)
    #[new]
    #[pyo3(signature = (num_shards=None, ring_buffer_capacity=None, backpressure_mode=None, redis_url=None, redis_prefix=None, redis_pipeline_size=None, redis_pool_size=None, redis_connect_timeout_ms=None, redis_command_timeout_ms=None, redis_max_stream_len=None, jetstream_url=None, jetstream_prefix=None, jetstream_connect_timeout_ms=None, jetstream_request_timeout_ms=None, jetstream_max_messages=None, jetstream_max_bytes=None, jetstream_max_age_ms=None, jetstream_replicas=None, net_bind_addr=None, net_peer_addr=None, net_psk=None, net_role=None, net_peer_public_key=None, net_secret_key=None, net_public_key=None, net_reliability=None, net_heartbeat_interval_ms=None, net_session_timeout_ms=None, net_batched_io=None, net_packet_pool_size=None))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        num_shards: Option<u16>,
        ring_buffer_capacity: Option<usize>,
        backpressure_mode: Option<&str>,
        redis_url: Option<&str>,
        redis_prefix: Option<&str>,
        redis_pipeline_size: Option<usize>,
        redis_pool_size: Option<usize>,
        redis_connect_timeout_ms: Option<u64>,
        redis_command_timeout_ms: Option<u64>,
        redis_max_stream_len: Option<usize>,
        jetstream_url: Option<&str>,
        jetstream_prefix: Option<&str>,
        jetstream_connect_timeout_ms: Option<u64>,
        jetstream_request_timeout_ms: Option<u64>,
        jetstream_max_messages: Option<i64>,
        jetstream_max_bytes: Option<i64>,
        jetstream_max_age_ms: Option<u64>,
        jetstream_replicas: Option<usize>,
        net_bind_addr: Option<&str>,
        net_peer_addr: Option<&str>,
        net_psk: Option<&str>,
        net_role: Option<&str>,
        net_peer_public_key: Option<&str>,
        net_secret_key: Option<&str>,
        net_public_key: Option<&str>,
        net_reliability: Option<&str>,
        net_heartbeat_interval_ms: Option<u64>,
        net_session_timeout_ms: Option<u64>,
        net_batched_io: Option<bool>,
        net_packet_pool_size: Option<usize>,
    ) -> PyResult<Self> {
        let runtime = Runtime::new().map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

        let mut builder = EventBusConfig::builder();

        if let Some(n) = num_shards {
            builder = builder.num_shards(n);
        }
        if let Some(cap) = ring_buffer_capacity {
            builder = builder.ring_buffer_capacity(cap);
        }
        if let Some(mode) = backpressure_mode {
            let bp_mode = match mode {
                "drop_newest" => BackpressureMode::DropNewest,
                "drop_oldest" => BackpressureMode::DropOldest,
                "fail_producer" => BackpressureMode::FailProducer,
                _ => {
                    return Err(PyValueError::new_err(format!(
                        "Invalid backpressure mode: {}",
                        mode
                    )));
                }
            };
            builder = builder.backpressure_mode(bp_mode);
        }

        // Configure Redis adapter if URL is provided
        if let Some(url) = redis_url {
            #[cfg(feature = "redis")]
            {
                use std::time::Duration;
                let mut redis_config = RedisAdapterConfig::new(url);
                if let Some(prefix) = redis_prefix {
                    redis_config = redis_config.with_prefix(prefix);
                }
                if let Some(pipeline_size) = redis_pipeline_size {
                    redis_config = redis_config.with_pipeline_size(pipeline_size);
                }
                if let Some(pool_size) = redis_pool_size {
                    redis_config = redis_config.with_pool_size(pool_size);
                }
                if let Some(connect_timeout_ms) = redis_connect_timeout_ms {
                    redis_config = redis_config
                        .with_connect_timeout(Duration::from_millis(connect_timeout_ms));
                }
                if let Some(command_timeout_ms) = redis_command_timeout_ms {
                    redis_config = redis_config
                        .with_command_timeout(Duration::from_millis(command_timeout_ms));
                }
                if let Some(max_stream_len) = redis_max_stream_len {
                    redis_config = redis_config.with_max_stream_len(max_stream_len);
                }
                builder = builder.adapter(AdapterConfig::Redis(redis_config));
            }
            #[cfg(not(feature = "redis"))]
            {
                let _ = (
                    url,
                    redis_prefix,
                    redis_pipeline_size,
                    redis_pool_size,
                    redis_connect_timeout_ms,
                    redis_command_timeout_ms,
                    redis_max_stream_len,
                );
                return Err(PyRuntimeError::new_err(
                    "Redis support not enabled. Rebuild with --features redis",
                ));
            }
        } else if let Some(url) = jetstream_url {
            #[cfg(feature = "jetstream")]
            {
                use std::time::Duration;
                let mut js_config = JetStreamAdapterConfig::new(url);
                if let Some(prefix) = jetstream_prefix {
                    js_config = js_config.with_prefix(prefix);
                }
                if let Some(connect_timeout_ms) = jetstream_connect_timeout_ms {
                    js_config =
                        js_config.with_connect_timeout(Duration::from_millis(connect_timeout_ms));
                }
                if let Some(request_timeout_ms) = jetstream_request_timeout_ms {
                    js_config =
                        js_config.with_request_timeout(Duration::from_millis(request_timeout_ms));
                }
                if let Some(max_messages) = jetstream_max_messages {
                    js_config = js_config.with_max_messages(max_messages);
                }
                if let Some(max_bytes) = jetstream_max_bytes {
                    js_config = js_config.with_max_bytes(max_bytes);
                }
                if let Some(max_age_ms) = jetstream_max_age_ms {
                    js_config = js_config.with_max_age(Duration::from_millis(max_age_ms));
                }
                if let Some(replicas) = jetstream_replicas {
                    js_config = js_config.with_replicas(replicas);
                }
                builder = builder.adapter(AdapterConfig::JetStream(js_config));
            }
            #[cfg(not(feature = "jetstream"))]
            {
                let _ = (
                    url,
                    jetstream_prefix,
                    jetstream_connect_timeout_ms,
                    jetstream_request_timeout_ms,
                    jetstream_max_messages,
                    jetstream_max_bytes,
                    jetstream_max_age_ms,
                    jetstream_replicas,
                );
                return Err(PyRuntimeError::new_err(
                    "JetStream support not enabled. Rebuild with --features jetstream",
                ));
            }
        } else if let Some(bind_addr_str) = net_bind_addr {
            #[cfg(feature = "net")]
            {
                use std::time::Duration;

                let bind_addr: std::net::SocketAddr = bind_addr_str
                    .parse()
                    .map_err(|e| PyValueError::new_err(format!("Invalid net_bind_addr: {}", e)))?;

                let peer_addr: std::net::SocketAddr = net_peer_addr
                    .ok_or_else(|| PyValueError::new_err("net_peer_addr is required"))?
                    .parse()
                    .map_err(|e| PyValueError::new_err(format!("Invalid net_peer_addr: {}", e)))?;

                let psk_hex =
                    net_psk.ok_or_else(|| PyValueError::new_err("net_psk is required"))?;
                let psk: [u8; 32] = hex::decode(psk_hex)
                    .map_err(|e| PyValueError::new_err(format!("Invalid net_psk hex: {}", e)))?
                    .try_into()
                    .map_err(|_| PyValueError::new_err("net_psk must be exactly 32 bytes"))?;

                let role = net_role.ok_or_else(|| PyValueError::new_err("net_role is required"))?;

                let mut net_config = match role {
                    "initiator" => {
                        let peer_pubkey_hex = net_peer_public_key.ok_or_else(|| {
                            PyValueError::new_err("net_peer_public_key is required for initiator")
                        })?;
                        let peer_pubkey: [u8; 32] = hex::decode(peer_pubkey_hex)
                            .map_err(|e| {
                                PyValueError::new_err(format!(
                                    "Invalid net_peer_public_key hex: {}",
                                    e
                                ))
                            })?
                            .try_into()
                            .map_err(|_| {
                                PyValueError::new_err(
                                    "net_peer_public_key must be exactly 32 bytes",
                                )
                            })?;
                        NetAdapterConfig::initiator(bind_addr, peer_addr, psk, peer_pubkey)
                    }
                    "responder" => {
                        let secret_key_hex = net_secret_key.ok_or_else(|| {
                            PyValueError::new_err("net_secret_key is required for responder")
                        })?;
                        let public_key_hex = net_public_key.ok_or_else(|| {
                            PyValueError::new_err("net_public_key is required for responder")
                        })?;
                        let secret_key: [u8; 32] = hex::decode(secret_key_hex)
                            .map_err(|e| {
                                PyValueError::new_err(format!("Invalid net_secret_key hex: {}", e))
                            })?
                            .try_into()
                            .map_err(|_| {
                                PyValueError::new_err("net_secret_key must be exactly 32 bytes")
                            })?;
                        let public_key: [u8; 32] = hex::decode(public_key_hex)
                            .map_err(|e| {
                                PyValueError::new_err(format!("Invalid net_public_key hex: {}", e))
                            })?
                            .try_into()
                            .map_err(|_| {
                                PyValueError::new_err("net_public_key must be exactly 32 bytes")
                            })?;
                        let keypair = StaticKeypair::from_keys(secret_key, public_key);
                        NetAdapterConfig::responder(bind_addr, peer_addr, psk, keypair)
                    }
                    _ => {
                        return Err(PyValueError::new_err(format!(
                            "Invalid net_role: {}. Use 'initiator' or 'responder'",
                            role
                        )));
                    }
                };

                // Apply optional settings
                if let Some(reliability) = net_reliability {
                    net_config = net_config.with_reliability(match reliability {
                        "light" => ReliabilityConfig::Light,
                        "full" => ReliabilityConfig::Full,
                        _ => ReliabilityConfig::None,
                    });
                }
                if let Some(interval_ms) = net_heartbeat_interval_ms {
                    net_config =
                        net_config.with_heartbeat_interval(Duration::from_millis(interval_ms));
                }
                if let Some(timeout_ms) = net_session_timeout_ms {
                    net_config = net_config.with_session_timeout(Duration::from_millis(timeout_ms));
                }
                if let Some(batched) = net_batched_io {
                    net_config = net_config.with_batched_io(batched);
                }
                if let Some(pool_size) = net_packet_pool_size {
                    net_config = net_config.with_pool_size(pool_size);
                }

                builder = builder.adapter(AdapterConfig::Net(Box::new(net_config)));
            }
            #[cfg(not(feature = "net"))]
            {
                let _ = (
                    bind_addr_str,
                    net_peer_addr,
                    net_psk,
                    net_role,
                    net_peer_public_key,
                    net_secret_key,
                    net_public_key,
                    net_reliability,
                    net_heartbeat_interval_ms,
                    net_session_timeout_ms,
                    net_batched_io,
                    net_packet_pool_size,
                );
                return Err(PyRuntimeError::new_err(
                    "Net support not enabled. Rebuild with --features net",
                ));
            }
        }

        let config = builder
            .build()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;

        let bus = runtime
            .block_on(EventBus::new(config))
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

        Ok(Net {
            bus: Arc::new(RwLock::new(Some(bus))),
            runtime: Arc::new(runtime),
        })
    }

    /// Ingest a raw JSON string (fastest path).
    ///
    /// This is the recommended method for high-throughput ingestion.
    /// The JSON string is stored directly without parsing.
    ///
    /// Args:
    ///     json: JSON string to ingest
    ///
    /// Returns:
    ///     IngestResult with shard_id and timestamp
    fn ingest_raw(&self, json: &str) -> PyResult<IngestResult> {
        let bus_guard = self.bus.read();
        let bus = bus_guard
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("EventBus has been shut down"))?;

        let raw = RawEvent::from_str(json);
        let (shard_id, ts) = bus
            .ingest_raw(raw)
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

        Ok(IngestResult {
            shard_id,
            timestamp: ts,
        })
    }

    /// Ingest a Python dict (convenience method).
    ///
    /// The dict is serialized to JSON before ingestion.
    /// For maximum performance, use `ingest_raw` with pre-serialized JSON.
    ///
    /// Args:
    ///     event: Dict to ingest (will be JSON serialized)
    ///
    /// Returns:
    ///     IngestResult with shard_id and timestamp
    fn ingest(&self, py: Python<'_>, event: &Bound<'_, PyDict>) -> PyResult<IngestResult> {
        let json_module = py.import("json")?;
        let json_str: String = json_module.call_method1("dumps", (event,))?.extract()?;
        self.ingest_raw(&json_str)
    }

    /// Ingest multiple raw JSON strings in a batch.
    ///
    /// Args:
    ///     events: List of JSON strings to ingest
    ///
    /// Returns:
    ///     Number of successfully ingested events
    fn ingest_raw_batch(&self, events: Vec<String>) -> PyResult<usize> {
        let bus_guard = self.bus.read();
        let bus = bus_guard
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("EventBus has been shut down"))?;

        let raw_events: Vec<RawEvent> = events.iter().map(|s| RawEvent::from_str(s)).collect();
        let count = bus.ingest_raw_batch(raw_events);

        Ok(count)
    }

    /// Poll events from the bus.
    ///
    /// Args:
    ///     limit: Maximum number of events to return
    ///     cursor: Optional cursor to resume from
    ///     filter: Optional JSON filter expression
    ///     ordering: Event ordering - "none" (default, fastest) or "insertion_ts" (cross-shard ordering)
    ///
    /// Returns:
    ///     PollResponse with events and pagination cursor
    #[pyo3(signature = (limit, cursor=None, filter=None, ordering=None))]
    fn poll(
        &self,
        limit: usize,
        cursor: Option<&str>,
        filter: Option<&str>,
        ordering: Option<&str>,
    ) -> PyResult<PollResponse> {
        let bus_guard = self.bus.read();
        let bus = bus_guard
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("EventBus has been shut down"))?;

        let mut request = ConsumeRequest::new(limit);

        if let Some(c) = cursor {
            request = request.from(c);
        }

        if let Some(f) = filter {
            let filter_obj: Filter =
                serde_json::from_str(f).map_err(|e| PyValueError::new_err(e.to_string()))?;
            request = request.filter(filter_obj);
        }

        if let Some(ord) = ordering {
            let ordering_mode = match ord {
                "none" => Ordering::None,
                "insertion_ts" => Ordering::InsertionTs,
                _ => {
                    return Err(PyValueError::new_err(format!(
                        "Invalid ordering: {}. Use 'none' or 'insertion_ts'",
                        ord
                    )));
                }
            };
            request = request.ordering(ordering_mode);
        }

        let response = self
            .runtime
            .block_on(bus.poll(request))
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

        let events: Vec<StoredEvent> = response
            .events
            .into_iter()
            .map(|e| {
                let raw = e.raw_str().unwrap_or("").to_string();
                StoredEvent {
                    id: e.id,
                    raw,
                    insertion_ts: e.insertion_ts,
                    shard_id: e.shard_id,
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
    fn num_shards(&self) -> PyResult<u16> {
        let bus_guard = self.bus.read();
        let bus = bus_guard
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("EventBus has been shut down"))?;

        Ok(bus.num_shards())
    }

    /// Get ingestion statistics.
    fn stats(&self) -> PyResult<Stats> {
        let bus_guard = self.bus.read();
        let bus = bus_guard
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("EventBus has been shut down"))?;

        let stats = bus.stats();
        Ok(Stats {
            events_ingested: stats
                .events_ingested
                .load(std::sync::atomic::Ordering::Relaxed),
            events_dropped: stats
                .events_dropped
                .load(std::sync::atomic::Ordering::Relaxed),
        })
    }

    /// Gracefully shutdown the event bus.
    fn shutdown(&self) -> PyResult<()> {
        let mut bus_guard = self.bus.write();
        if let Some(bus) = bus_guard.take() {
            self.runtime
                .block_on(bus.shutdown())
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        }
        Ok(())
    }

    fn __repr__(&self) -> String {
        if self.bus.read().is_some() {
            "Net(active)".to_string()
        } else {
            "Net(shutdown)".to_string()
        }
    }

    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    #[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __exit__(
        &self,
        _exc_type: Option<&Bound<'_, pyo3::types::PyType>>,
        _exc_val: Option<&Bound<'_, pyo3::types::PyAny>>,
        _exc_tb: Option<&Bound<'_, pyo3::types::PyAny>>,
    ) -> PyResult<bool> {
        self.shutdown()?;
        Ok(false)
    }
}

// ============================================================================
// MeshNode bindings
// ============================================================================

#[cfg(feature = "net")]
pyo3::create_exception!(
    _net,
    BackpressureError,
    pyo3::exceptions::PyException,
    "Raised when a stream's per-stream in-flight window is full. The \
     caller's events were NOT sent — decide whether to drop, retry, \
     or buffer at the app layer. See send_with_retry / send_blocking \
     for two built-in policies."
);

#[cfg(feature = "net")]
pyo3::create_exception!(
    _net,
    NotConnectedError,
    pyo3::exceptions::PyException,
    "Raised when a stream's peer session is gone (never connected, \
     disconnected, or the stream was closed)."
);

#[cfg(feature = "net")]
pyo3::create_exception!(
    _net,
    ChannelError,
    pyo3::exceptions::PyException,
    "Raised when a channel operation fails for a reason other than \
     auth: invalid name / visibility, unknown channel, rate limit, \
     transport failure. Authorization-specific rejections raise the \
     subclass `ChannelAuthError`."
);

#[cfg(feature = "net")]
pyo3::create_exception!(
    _net,
    ChannelAuthError,
    ChannelError,
    "Raised when a Subscribe / Unsubscribe is rejected because the \
     publisher's ACL denied the subscriber. Subclass of \
     `ChannelError`."
);

#[cfg(feature = "net")]
mod mesh_bindings {
    use super::*;
    use net::adapter::net::DEFAULT_STREAM_WINDOW_BYTES;
    use net::adapter::net::{
        ChannelConfig as InnerChannelConfig, ChannelConfigRegistry, ChannelId,
        ChannelName as InnerChannelName, ChannelPublisher, EntityKeypair, MeshNode, MeshNodeConfig,
        OnFailure as InnerOnFailure, PublishConfig as InnerPublishConfig,
        PublishReport as InnerPublishReport, Reliability, Stream as CoreStream, StreamConfig,
        StreamError, Visibility as InnerVisibility,
    };
    use net::adapter::Adapter;
    use std::time::Duration;

    pub(crate) fn stream_error_to_py(e: StreamError) -> PyErr {
        match e {
            StreamError::Backpressure => {
                super::BackpressureError::new_err("stream would block (queue full)")
            }
            StreamError::NotConnected => super::NotConnectedError::new_err("stream not connected"),
            StreamError::Transport(msg) => {
                PyRuntimeError::new_err(format!("stream transport error: {}", msg))
            }
        }
    }

    /// Convert the core `NatClass` enum to the stable string form
    /// used on the Python boundary. Stable vocabulary per plan §5:
    /// `"open" | "cone" | "symmetric" | "unknown"`. Kept in sync
    /// with the NAPI + Go bindings — callers do
    /// `mesh.nat_type() == "open"` against these strings.
    #[cfg(feature = "nat-traversal")]
    pub(crate) fn nat_class_to_string(
        class: net::adapter::net::traversal::classify::NatClass,
    ) -> String {
        use net::adapter::net::traversal::classify::NatClass;
        match class {
            NatClass::Open => "open",
            NatClass::Cone => "cone",
            NatClass::Symmetric => "symmetric",
            NatClass::Unknown => "unknown",
        }
        .to_string()
    }

    /// Format a core `TraversalError` into a `PyErr` whose message
    /// follows the `traversal: <kind>[: <detail>]` convention.
    /// Mirrors the `migration: <kind>` pattern already used by the
    /// compute surface; callers branch on the stable `kind` prefix.
    #[cfg(feature = "nat-traversal")]
    pub(crate) fn traversal_py_err(e: net::adapter::net::traversal::TraversalError) -> PyErr {
        use net::adapter::net::traversal::TraversalError;
        let body = match &e {
            TraversalError::Transport(msg) => format!("transport: {msg}"),
            TraversalError::RendezvousRejected(msg) => format!("rendezvous-rejected: {msg}"),
            _ => e.kind().to_string(),
        };
        PyRuntimeError::new_err(format!("traversal: {body}"))
    }

    fn parse_visibility(s: &str) -> PyResult<InnerVisibility> {
        match s {
            "subnet-local" => Ok(InnerVisibility::SubnetLocal),
            "parent-visible" => Ok(InnerVisibility::ParentVisible),
            "exported" => Ok(InnerVisibility::Exported),
            "global" => Ok(InnerVisibility::Global),
            other => Err(super::ChannelError::new_err(format!(
                "channel: invalid visibility {:?} (expected subnet-local | parent-visible | exported | global)",
                other
            ))),
        }
    }

    fn parse_reliability_cfg(s: &str) -> PyResult<Reliability> {
        match s {
            "reliable" => Ok(Reliability::Reliable),
            "fire_and_forget" => Ok(Reliability::FireAndForget),
            other => Err(super::ChannelError::new_err(format!(
                "channel: invalid reliability {:?}",
                other
            ))),
        }
    }

    fn parse_on_failure(s: &str) -> PyResult<InnerOnFailure> {
        match s {
            "best_effort" => Ok(InnerOnFailure::BestEffort),
            "fail_fast" => Ok(InnerOnFailure::FailFast),
            "collect" => Ok(InnerOnFailure::Collect),
            other => Err(super::ChannelError::new_err(format!(
                "channel: invalid on_failure {:?}",
                other
            ))),
        }
    }

    fn publish_report_to_pydict<'py>(
        py: Python<'py>,
        report: InnerPublishReport,
    ) -> PyResult<Bound<'py, pyo3::types::PyDict>> {
        let dict = pyo3::types::PyDict::new(py);
        dict.set_item("attempted", report.attempted)?;
        dict.set_item("delivered", report.delivered)?;
        let errors = pyo3::types::PyList::empty(py);
        for (node_id, err) in report.errors {
            let entry = pyo3::types::PyDict::new(py);
            entry.set_item("node_id", node_id)?;
            entry.set_item("message", format!("{}", err))?;
            errors.append(entry)?;
        }
        dict.set_item("errors", errors)?;
        Ok(dict)
    }

    /// Translate an `AdapterError` from a channel op into the right
    /// typed Python error: `ChannelAuthError` for the `Unauthorized`
    /// AckReason, `ChannelError` for every other rejection / network
    /// failure.
    pub(crate) fn adapter_to_channel_pyerr(err: net::error::AdapterError) -> PyErr {
        use net::error::AdapterError;
        if let AdapterError::Connection(ref msg) = err {
            let prefix = "membership request rejected: ";
            if let Some(tail) = msg.strip_prefix(prefix) {
                let reason = tail.trim();
                if reason == "Some(Unauthorized)" {
                    return super::ChannelAuthError::new_err("channel: unauthorized");
                }
                if reason == "Some(UnknownChannel)" {
                    return super::ChannelError::new_err("channel: unknown channel");
                }
                if reason == "Some(RateLimited)" {
                    return super::ChannelError::new_err("channel: rate limited");
                }
                if reason == "Some(TooManyChannels)" {
                    return super::ChannelError::new_err("channel: too many channels");
                }
                return super::ChannelError::new_err(format!("channel: rejected ({})", reason));
            }
        }
        super::ChannelError::new_err(format!("channel: {}", err))
    }

    /// Handle to an open stream. Opaque to Python callers.
    ///
    /// Async equivalent: :class:`AsyncNetStream` — awaitable
    /// `send` / `send_with_retry` / `send_blocking`.
    #[pyclass]
    pub struct NetStream {
        pub(crate) peer_node_id: u64,
        pub(crate) stream_id: u64,
        pub(crate) core: CoreStream,
    }

    #[pymethods]
    impl NetStream {
        #[getter]
        fn peer_node_id(&self) -> u64 {
            self.peer_node_id
        }
        #[getter]
        fn stream_id(&self) -> u64 {
            self.stream_id
        }
        fn __repr__(&self) -> String {
            format!(
                "NetStream(peer_node_id={:#x}, stream_id={:#x})",
                self.peer_node_id, self.stream_id
            )
        }
    }

    /// Snapshot of per-stream stats.
    #[pyclass(from_py_object)]
    #[derive(Clone)]
    pub struct NetStreamStats {
        #[pyo3(get)]
        pub tx_seq: u64,
        #[pyo3(get)]
        pub rx_seq: u64,
        #[pyo3(get)]
        pub inbound_pending: u64,
        #[pyo3(get)]
        pub last_activity_ns: u64,
        #[pyo3(get)]
        pub active: bool,
        #[pyo3(get)]
        pub backpressure_events: u64,
        #[pyo3(get)]
        pub tx_credit_remaining: u32,
        #[pyo3(get)]
        pub tx_window: u32,
        #[pyo3(get)]
        pub credit_grants_received: u64,
        #[pyo3(get)]
        pub credit_grants_sent: u64,
    }

    pub(crate) fn parse_reliability(s: Option<&str>) -> PyResult<Reliability> {
        match s {
            None | Some("fire_and_forget") => Ok(Reliability::FireAndForget),
            Some("reliable") => Ok(Reliability::Reliable),
            Some(other) => Err(PyValueError::new_err(format!(
                "unknown reliability mode {:?}; expected \"fire_and_forget\" or \"reliable\"",
                other
            ))),
        }
    }

    /// A multi-peer mesh node for Python.
    ///
    /// Manages encrypted connections to multiple peers over a single
    /// UDP socket with automatic failure detection and rerouting.
    ///
    /// Async equivalent: :class:`AsyncNetMesh` — same `MeshNode`,
    /// awaitable I/O methods.
    ///
    /// ```python
    /// from net import NetMesh
    ///
    /// node = NetMesh("127.0.0.1:9000", "00" * 32)
    /// print(f"public key: {node.public_key}")
    ///
    /// node.connect("127.0.0.1:9001", peer_pubkey, 0x2222)
    /// node.start()
    ///
    /// node.push_to("127.0.0.1:9001", '{"token":"hi"}')
    ///
    /// events = node.poll(100)
    /// node.shutdown()
    /// ```
    #[pyclass]
    pub struct NetMesh {
        /// Live `MeshNode` held via `Arc` so the compute feature's
        /// `DaemonRuntime` (and any future Arc-sharing consumer)
        /// can hold a second reference without copying the live
        /// socket or handshake table. Shutdown drops this Arc —
        /// any remaining clones held by a `DaemonRuntime` observe
        /// a shut-down node the next time they call into it.
        node: Option<Arc<MeshNode>>,
        runtime: Arc<Runtime>,
        /// Shared channel config registry installed on the MeshNode
        /// at construction; `register_channel` inserts into this same
        /// Arc so the core's membership-ACL path sees every add.
        channel_configs: Arc<ChannelConfigRegistry>,
    }

    /// Build the core `MatchCriteria` from flat Python kwargs (so callers
    /// never touch the internal `CapabilityQuery` / policy enum shapes).
    #[allow(clippy::too_many_arguments)]
    fn build_gang_criteria(
        tags_all: Vec<String>,
        tags_any: Vec<String>,
        tag_groups_all: Vec<Vec<String>>,
        region: Option<String>,
        min_units: Option<usize>,
        max_load: Option<f64>,
        max_p50_latency_us: Option<u32>,
        require_all: Vec<String>,
        require_any: Vec<String>,
        selection: Option<String>,
        load_band_target: Option<f64>,
        prefer_capability: Option<String>,
    ) -> PyResult<::net::adapter::net::behavior::gang::MatchCriteria> {
        use ::net::adapter::net::behavior::fold::{CapabilityFilter, CapabilityQuery};
        use ::net::adapter::net::behavior::gang::{MatchCriteria, NumericFilter, SelectionPolicy};
        let sel = match selection.as_deref() {
            None | Some("least_loaded") => SelectionPolicy::LeastLoaded,
            Some("pack") => SelectionPolicy::Pack,
            Some("lowest_id") => SelectionPolicy::LowestId,
            Some("load_band") => SelectionPolicy::LoadBand(load_band_target.unwrap_or(0.5) as f32),
            Some(other) => {
                return Err(PyValueError::new_err(format!(
                    "unknown selection policy {other:?}"
                )))
            }
        };
        Ok(MatchCriteria {
            capability: CapabilityQuery::Composite(CapabilityFilter {
                tags_all,
                tags_any,
                tag_groups_all,
                region,
                ..Default::default()
            }),
            numeric: NumericFilter {
                min_units: min_units.unwrap_or(0),
                max_load: max_load.map(|v| v as f32),
                max_p50_latency_us,
                require_all,
                require_any,
            },
            selection: sel,
            prefer_capability,
        })
    }

    fn claim_outcome_str(o: ::net::adapter::net::behavior::gang::ClaimOutcome) -> &'static str {
        use ::net::adapter::net::behavior::gang::ClaimOutcome;
        match o {
            ClaimOutcome::Won => "won",
            ClaimOutcome::Lost => "lost",
        }
    }

    #[pymethods]
    impl NetMesh {
        /// Create a new mesh node.
        ///
        /// Args:
        ///     bind_addr: Local bind address (e.g., "127.0.0.1:9000")
        ///     psk: Hex-encoded 32-byte pre-shared key
        ///     heartbeat_interval_ms: Heartbeat interval (default: 5000)
        ///     session_timeout_ms: Session timeout (default: 30000)
        ///     num_shards: Number of inbound shards (default: 4)
        #[new]
        #[pyo3(signature = (
            bind_addr,
            psk,
            heartbeat_interval_ms=None,
            session_timeout_ms=None,
            num_shards=None,
            identity_seed=None,
            capability_gc_interval_ms=None,
            require_signed_capabilities=None,
            subnet=None,
            subnet_policy=None,
            reflex_override=None,
            try_port_mapping=None,
            auto_direct_upgrade=None,
            permissive_channels=None,
        ))]
        #[allow(clippy::too_many_arguments)]
        fn new(
            bind_addr: &str,
            psk: &str,
            heartbeat_interval_ms: Option<u64>,
            session_timeout_ms: Option<u64>,
            num_shards: Option<u16>,
            identity_seed: Option<&[u8]>,
            capability_gc_interval_ms: Option<u64>,
            require_signed_capabilities: Option<bool>,
            subnet: Option<Vec<u32>>,
            subnet_policy: Option<&Bound<'_, PyDict>>,
            // reflex_override: pin this mesh's public reflex to
            // the supplied external "ip:port". Classification is
            // skipped; the node starts in "open" with this
            // reflex on its capability announcements. Silently
            // ignored when the cdylib was built without
            // `--features nat-traversal`.
            reflex_override: Option<&str>,
            // try_port_mapping: opt into opportunistic UPnP /
            // NAT-PMP / PCP at startup. When True, the mesh
            // spawns a port-mapping task that installs + renews
            // a mapping on the operator's router. Optimization,
            // not correctness — silently ignored when the cdylib
            // was built without `--features port-mapping`.
            try_port_mapping: Option<bool>,
            // auto_direct_upgrade: enable the background
            // direct-path upgrade — relay-routed sessions are
            // opportunistically re-handshaked over a direct path
            // and migrated (guarded by the migration contract so
            // in-flight work is never dropped). Optimization,
            // not correctness; default False. Silently ignored
            // when built without `--features nat-traversal`.
            auto_direct_upgrade: Option<bool>,
            // permissive_channels: opt out of the strict
            // `ChannelConfigRegistry` install. When True, no
            // registry is set on the MeshNode, so the substrate's
            // `authorize_subscribe` treats all channels as
            // permissive (matches the Rust default in
            // tests/integration_nrpc_mesh.rs). When False or None
            // (default), the strict registry is installed and
            // every subscribed channel must be `register_channel`'d
            // first. Test-only knob — production code should
            // keep the strict default.
            permissive_channels: Option<bool>,
        ) -> PyResult<Self> {
            let addr: std::net::SocketAddr = bind_addr
                .parse()
                .map_err(|e| PyValueError::new_err(format!("invalid address: {}", e)))?;

            let psk_bytes = hex::decode(psk)
                .map_err(|e| PyValueError::new_err(format!("invalid PSK hex: {}", e)))?;
            if psk_bytes.len() != 32 {
                return Err(PyValueError::new_err("PSK must be 32 bytes (64 hex chars)"));
            }
            let mut psk_arr = [0u8; 32];
            psk_arr.copy_from_slice(&psk_bytes);

            let mut config = MeshNodeConfig::new(addr, psk_arr);
            if let Some(ms) = heartbeat_interval_ms {
                config = config.with_heartbeat_interval(Duration::from_millis(ms));
            }
            if let Some(ms) = session_timeout_ms {
                config = config.with_session_timeout(Duration::from_millis(ms));
            }
            if let Some(n) = num_shards {
                config = config.with_num_shards(n);
            }
            if let Some(ms) = capability_gc_interval_ms {
                config = config.with_capability_gc_interval(Duration::from_millis(ms));
            }
            if let Some(b) = require_signed_capabilities {
                config = config.with_require_signed_capabilities(b);
            }
            if let Some(levels) = subnet {
                let id = super::subnets::subnet_id_from_py(levels)?;
                config = config.with_subnet(id);
            }
            if let Some(policy_dict) = subnet_policy {
                let policy = Arc::new(super::subnets::subnet_policy_from_py(policy_dict)?);
                config = config.with_subnet_policy(policy);
            }
            #[cfg(feature = "nat-traversal")]
            if let Some(external_str) = reflex_override {
                let external: std::net::SocketAddr = external_str
                    .parse()
                    .map_err(|e| PyValueError::new_err(format!("invalid reflex_override: {e}")))?;
                config = config.with_reflex_override(external);
            }
            // Silently accept + ignore the kwarg in builds without
            // `nat-traversal` so Python callers compiled against a
            // full-feature wheel can fall back to a thin wheel
            // without an exception on an unknown kwarg.
            #[cfg(not(feature = "nat-traversal"))]
            let _ = reflex_override;
            #[cfg(feature = "port-mapping")]
            if try_port_mapping == Some(true) {
                config = config.with_try_port_mapping(true);
            }
            // Same drop-on-the-floor pattern as reflex_override
            // above — thin wheels accept the kwarg and ignore it.
            #[cfg(not(feature = "port-mapping"))]
            let _ = try_port_mapping;
            #[cfg(feature = "nat-traversal")]
            if auto_direct_upgrade == Some(true) {
                config = config.with_auto_direct_upgrade(true);
            }
            #[cfg(not(feature = "nat-traversal"))]
            let _ = auto_direct_upgrade;

            let runtime = Arc::new(
                Runtime::new().map_err(|e| PyRuntimeError::new_err(format!("runtime: {}", e)))?,
            );

            // Derive the mesh's keypair from the caller-supplied
            // seed when present — lets a caller-side `Identity.
            // from_seed(same_seed)` issue tokens whose `subject`
            // matches this mesh's entity id. Otherwise generate.
            let identity = match identity_seed {
                Some(seed) => {
                    if seed.len() != 32 {
                        return Err(PyValueError::new_err(format!(
                            "identity_seed must be 32 bytes, got {}",
                            seed.len()
                        )));
                    }
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(seed);
                    EntityKeypair::from_bytes(arr)
                }
                None => EntityKeypair::generate(),
            };
            let mut node = runtime
                .block_on(MeshNode::new(identity, config))
                .map_err(|e| PyRuntimeError::new_err(format!("MeshNode: {}", e)))?;

            // Install shared channel-config registry. Tests can
            // opt out with `permissive_channels=True` to match the
            // Rust integration-test default (no registry → no ACL).
            let channel_configs = Arc::new(ChannelConfigRegistry::new());
            if !permissive_channels.unwrap_or(false) {
                node.set_channel_configs(channel_configs.clone());
            }
            // Install a fresh TokenCache — channel auth needs
            // somewhere to stash tokens presented on subscribe.
            //
            // **Known limitation.** Each `NetMesh` gets its own
            // `TokenCache`. A caller's `Identity.install_token(...)`
            // against the same seed binds to a different
            // `Arc<TokenCache>` than the mesh's cache, so
            // subscribe-with-token from a separately-built
            // `Identity` silently fails verification until the
            // token is explicitly re-installed via
            // `NetMesh.subscribe_channel(..., token=bytes)`.
            // Sharing across meshes is tracked separately — it
            // needs a per-seed registry on the Python side so a
            // second `NetMesh.__new__` with the same seed picks
            // up the existing cache.
            node.set_token_cache(Arc::new(net::adapter::net::identity::TokenCache::new()));

            Ok(Self {
                node: Some(Arc::new(node)),
                runtime,
                channel_configs,
            })
        }

        /// Get this node's Noise public key (hex-encoded).
        #[getter]
        fn public_key(&self) -> PyResult<String> {
            let node = self.get_node()?;
            Ok(hex::encode(node.public_key()))
        }

        /// Get this node's 32-byte ed25519 entity id. Matches
        /// `Identity.from_seed(seed).entity_id` when the mesh was
        /// constructed with `identity_seed=seed`.
        #[getter]
        fn entity_id(&self) -> PyResult<Vec<u8>> {
            let node = self.get_node()?;
            Ok(node.entity_id().as_bytes().to_vec())
        }

        /// Get this node's ID.
        #[getter]
        fn node_id(&self) -> PyResult<u64> {
            let node = self.get_node()?;
            Ok(node.node_id())
        }

        /// This node's bound local UDP address (e.g. ``"127.0.0.1:54321"``).
        /// With a ``:0`` bind this resolves the OS-assigned port, so a peer can
        /// be told where to `connect`.
        #[getter]
        fn local_addr(&self) -> PyResult<String> {
            Ok(self.get_node()?.local_addr().to_string())
        }

        /// Connect to a peer (initiator side).
        #[pyo3(signature = (peer_addr, peer_public_key, peer_node_id))]
        fn connect(
            &self,
            py: Python<'_>,
            peer_addr: &str,
            peer_public_key: &str,
            peer_node_id: u64,
        ) -> PyResult<()> {
            let addr: std::net::SocketAddr = peer_addr
                .parse()
                .map_err(|e| PyValueError::new_err(format!("invalid address: {}", e)))?;

            let pubkey_bytes = hex::decode(peer_public_key)
                .map_err(|e| PyValueError::new_err(format!("invalid hex: {}", e)))?;
            if pubkey_bytes.len() != 32 {
                return Err(PyValueError::new_err("public key must be 32 bytes"));
            }
            let mut pubkey = [0u8; 32];
            pubkey.copy_from_slice(&pubkey_bytes);

            // Grab an Arc to the node before detaching so we don't
            // need `&self` while the GIL is released. Releasing the
            // GIL is load-bearing for the two-mesh handshake case:
            // the peer's `accept` blocks on its own thread while
            // holding the GIL, and our blocking connect on this
            // thread would otherwise deadlock that thread's GIL
            // acquire at the end of handshake.
            let node = self
                .node
                .as_ref()
                .cloned()
                .ok_or_else(|| PyRuntimeError::new_err("MeshNode has been shut down"))?;
            let runtime = self.runtime.clone();
            py.detach(move || {
                runtime
                    .block_on(node.connect(addr, &pubkey, peer_node_id))
                    .map_err(|e| PyRuntimeError::new_err(format!("connect: {}", e)))?;
                Ok(())
            })
        }

        /// Accept an incoming connection (responder side).
        fn accept(&self, py: Python<'_>, peer_node_id: u64) -> PyResult<String> {
            // Release the GIL while blocking on the accept future —
            // without this, a caller running `accept` in a thread
            // would pin the GIL and starve a concurrent `connect` on
            // the main thread (same reason as `connect` above).
            let node = self
                .node
                .as_ref()
                .cloned()
                .ok_or_else(|| PyRuntimeError::new_err("MeshNode has been shut down"))?;
            let runtime = self.runtime.clone();
            py.detach(move || {
                let (addr, _) = runtime
                    .block_on(node.accept(peer_node_id))
                    .map_err(|e| PyRuntimeError::new_err(format!("accept: {}", e)))?;
                Ok(addr.to_string())
            })
        }

        /// Start the receive loop and heartbeats.
        ///
        /// Uses `start_arc` (like the C FFI and the Rust SDK) so
        /// the Arc-scoped lifecycle loops run too: periodic
        /// capability re-announce (with the reflex-diff
        /// re-classify trigger) and — when `auto_direct_upgrade`
        /// is set — the background direct-path upgrade.
        fn start(&self) -> PyResult<()> {
            let node = self
                .node
                .as_ref()
                .ok_or_else(|| PyRuntimeError::new_err("MeshNode has been shut down"))?;
            // `MeshNode::start` uses `tokio::spawn` internally, so
            // it must run inside a tokio runtime context. Enter
            // our owned runtime for the duration of the call.
            let _guard = self.runtime.enter();
            node.start_arc();
            Ok(())
        }

        /// The invite `rendezvous` locator for this node (address + Noise
        /// static key + node id), to pass to `OperatorEnrollment.invite`.
        /// Devices dial it via `join`. (Hermes V2 Phase 1.)
        #[cfg(feature = "delegation")]
        fn rendezvous_string(&self) -> PyResult<String> {
            Ok(crate::enrollment::mesh_rendezvous_string(
                self.node_arc_clone()?,
            ))
        }

        /// Device-side enrollment: enroll `device`'s key into the mesh named by
        /// the `invite` string, under `name` + `tags`, returning the verified
        /// `root -> device` `DelegationChain`. This node must be `start()`ed and
        /// built with `permissive_channels=True` (the enrollment nRPC uses
        /// dynamic per-caller reply channels the strict registry rejects).
        #[cfg(feature = "delegation")]
        fn join(
            &self,
            py: Python<'_>,
            device: &crate::identity::Identity,
            invite: &str,
            name: &str,
            tags: Vec<String>,
        ) -> PyResult<crate::delegation::PyDelegationChain> {
            crate::enrollment::mesh_join(
                py,
                self.node_arc_clone()?,
                &self.runtime,
                device,
                invite.to_string(),
                name.to_string(),
                tags,
            )
        }

        /// Operator-side: serve enrollment on this node (auto — the invite is
        /// the authorization). Hold the returned handle for as long as
        /// enrollment should stay open. This node must be `start()`ed and built
        /// with `permissive_channels=True` (the enrollment nRPC uses dynamic
        /// per-caller reply channels the strict registry rejects).
        #[cfg(feature = "delegation")]
        #[pyo3(signature = (operator, grant_ttl_seconds, max_depth=None))]
        fn serve_enrollment_auto(
            &self,
            operator: &crate::enrollment::PyOperatorEnrollment,
            grant_ttl_seconds: u64,
            max_depth: Option<u8>,
        ) -> PyResult<crate::enrollment::PyEnrollmentServeHandle> {
            let depth = max_depth.unwrap_or(net_sdk::delegation::DEFAULT_DELEGATION_DEPTH);
            crate::enrollment::mesh_serve_enrollment_auto(
                self.node_arc_clone()?,
                &self.runtime,
                operator.arc(),
                std::time::Duration::from_secs(grant_ttl_seconds),
                depth,
            )
        }

        /// Device-side renewal: refresh the grant carried by `enrollment` over
        /// the mesh, returning the verified fresh `root -> device`
        /// `DelegationChain`. This node must be `start()`ed and built with
        /// `permissive_channels=True`. (Requires the `delegation` feature.)
        #[cfg(feature = "delegation")]
        fn renew(
            &self,
            py: Python<'_>,
            enrollment: &crate::enrollment::PyDeviceEnrollment,
        ) -> PyResult<crate::delegation::PyDelegationChain> {
            crate::enrollment::mesh_renew(py, self.node_arc_clone()?, &self.runtime, enrollment)
        }

        /// Publish this node's OWN local tools as mesh capabilities (V2 Phase 2)
        /// — the inverse of `net wrap`. `tools` is a list of
        /// `(name, description|None, input_schema_json)` (the input schema as a
        /// JSON string). `callback` is an **async** callable
        /// `async (tool_name: str, args_json: str) -> str | (str, bool)` invoked
        /// when a consumer calls a tool; its return is the tool's text output
        /// (a `(text, is_error)` tuple flags a tool-level error). A consumer
        /// discovers + invokes these through the ordinary
        /// `AsyncCapabilityGateway`. `owner_origin` scopes admission: an
        /// `origin_hash` admits only that caller; `None` admits only **this
        /// node itself** (fail-closed default — the tools are backed by an
        /// arbitrary local callback). Pass `allow_any_caller=True` to
        /// explicitly admit every mesh peer (overrides `owner_origin`; gate
        /// invocations yourself, e.g. with an approval callback). Returns a
        /// handle that must be held to keep the tools published. This node
        /// must be `start()`ed and built with `permissive_channels=True`.
        /// (Requires the `publish` feature.)
        #[cfg(feature = "publish")]
        #[pyo3(signature = (tools, callback, version=String::new(), owner_origin=None, allow_any_caller=false))]
        fn publish_tools(
            &self,
            py: Python<'_>,
            tools: Vec<(String, Option<String>, String)>,
            callback: pyo3::Py<pyo3::PyAny>,
            version: String,
            owner_origin: Option<u64>,
            allow_any_caller: bool,
        ) -> PyResult<crate::publish::PyLocalPublicationHandle> {
            crate::publish::mesh_publish_tools(
                py,
                self.node_arc_clone()?,
                self.runtime.clone(),
                tools,
                callback,
                version,
                owner_origin,
                allow_any_caller,
            )
        }

        /// Serve the agent-to-agent (A2A) task lifecycle on this node (V2 Phase
        /// 3), backed by a Python **async** task-executor `callback`
        /// `async (task_id, prompt, context_refs, tags) -> str` returning the
        /// result's artifact ref. A sibling in-root agent hands off a job with
        /// `submit_task`. Hold the returned handle to keep accepting tasks. This
        /// node must be `start()`ed. (Requires the `a2a` feature.)
        #[cfg(feature = "a2a")]
        fn serve_a2a(
            &self,
            callback: pyo3::Py<pyo3::PyAny>,
        ) -> PyResult<crate::a2a::PyA2aServeHandle> {
            crate::a2a::mesh_serve_a2a(self.node_arc_clone()?, self.runtime.clone(), callback)
        }

        /// Hand off a task to the executor at `target_node_id`: `prompt` plus
        /// optional Datafort `context_refs` (the executor doesn't share your
        /// memory) and routing `tags`. Returns the accepted task id; raises if
        /// the executor rejected it. The node must already be connected to
        /// `target_node_id`. (Requires the `a2a` feature.)
        #[cfg(feature = "a2a")]
        #[pyo3(signature = (target_node_id, prompt, context_refs=Vec::new(), tags=Vec::new()))]
        fn submit_task(
            &self,
            py: Python<'_>,
            target_node_id: u64,
            prompt: String,
            context_refs: Vec<String>,
            tags: Vec<String>,
        ) -> PyResult<String> {
            crate::a2a::mesh_submit_task(
                py,
                self.node_arc_clone()?,
                self.runtime.clone(),
                target_node_id,
                prompt,
                context_refs,
                tags,
            )
        }

        /// The executor's status for `task_id` as a JSON string
        /// (`{brief, state, updated_at}`), or ``None`` if unknown. (Requires the
        /// `a2a` feature.)
        #[cfg(feature = "a2a")]
        fn task_status(
            &self,
            py: Python<'_>,
            target_node_id: u64,
            task_id: String,
        ) -> PyResult<Option<String>> {
            crate::a2a::mesh_task_status(
                py,
                self.node_arc_clone()?,
                self.runtime.clone(),
                target_node_id,
                task_id,
            )
        }

        /// Cancel `task_id` on the executor; returns whether it was in flight.
        /// The executor's coroutine is cancelled — the remote work stops.
        /// (Requires the `a2a` feature.)
        #[cfg(feature = "a2a")]
        fn cancel_task(
            &self,
            py: Python<'_>,
            target_node_id: u64,
            task_id: String,
        ) -> PyResult<bool> {
            crate::a2a::mesh_cancel_task(
                py,
                self.node_arc_clone()?,
                self.runtime.clone(),
                target_node_id,
                task_id,
            )
        }

        /// Send raw JSON to a direct peer.
        fn push_to(&self, peer_addr: &str, json: &str) -> PyResult<bool> {
            let node = self.get_node()?;
            let addr: std::net::SocketAddr = peer_addr
                .parse()
                .map_err(|e| PyValueError::new_err(format!("invalid address: {}", e)))?;

            let batch = net::event::Batch {
                shard_id: 0,
                events: vec![net::event::InternalEvent::new(
                    bytes::Bytes::copy_from_slice(json.as_bytes()),
                    0,
                    0,
                )],
                sequence_start: 0,
                process_nonce: net::event::batch_process_nonce(),
            };

            self.runtime
                .block_on(node.send_to_peer(addr, &batch))
                .map_err(|e| PyRuntimeError::new_err(format!("send: {}", e)))?;
            Ok(true)
        }

        /// Poll for received events.
        fn poll(&self, limit: usize) -> PyResult<Vec<StoredEvent>> {
            let node = self.get_node()?;
            let result = self
                .runtime
                .block_on(node.poll_shard(0, None, limit))
                .map_err(|e| PyRuntimeError::new_err(format!("poll: {}", e)))?;

            Ok(result
                .events
                .into_iter()
                .map(|e| {
                    let raw = e.raw_str().unwrap_or("").to_string();
                    StoredEvent {
                        id: e.id,
                        raw,
                        insertion_ts: e.insertion_ts,
                        shard_id: e.shard_id,
                    }
                })
                .collect())
        }

        /// Add a route.
        fn add_route(&self, dest_node_id: u64, next_hop_addr: &str) -> PyResult<()> {
            let node = self.get_node()?;
            let addr: std::net::SocketAddr = next_hop_addr
                .parse()
                .map_err(|e| PyValueError::new_err(format!("invalid address: {}", e)))?;
            node.router().add_route(dest_node_id, addr);
            Ok(())
        }

        /// Number of connected peers.
        fn peer_count(&self) -> PyResult<usize> {
            Ok(self.get_node()?.peer_count())
        }

        /// Number of nodes discovered via pingwave.
        fn discovered_nodes(&self) -> PyResult<usize> {
            Ok(self.get_node()?.proximity_graph().node_count())
        }

        // ─── Stream API ────────────────────────────────────────────

        /// Open (or look up) a logical stream to a connected peer.
        ///
        /// Repeated calls for the same (peer, stream_id) are
        /// idempotent — the first open wins; later differing configs
        /// are logged and ignored.
        ///
        /// Args:
        ///     peer_node_id: node_id of a peer this node is connected to.
        ///     stream_id: caller-chosen opaque u64.
        ///     reliability: "fire_and_forget" (default) or "reliable".
        ///     window_bytes: initial send-credit window in bytes.
        ///         Defaults to DEFAULT_STREAM_WINDOW_BYTES (64 KB)
        ///         when unset — v2 backpressure is ON out of the
        ///         box. Pass 0 to restore the v1 unbounded behavior.
        ///     fairness_weight: fair-scheduler quantum multiplier.
        #[pyo3(signature = (
            peer_node_id,
            stream_id,
            reliability=None,
            window_bytes=DEFAULT_STREAM_WINDOW_BYTES,
            fairness_weight=1
        ))]
        fn open_stream(
            &self,
            peer_node_id: u64,
            stream_id: u64,
            reliability: Option<&str>,
            window_bytes: u32,
            fairness_weight: u8,
        ) -> PyResult<NetStream> {
            let node = self.get_node()?;
            let rel = parse_reliability(reliability)?;
            let config = StreamConfig::new()
                .with_reliability(rel)
                .with_window_bytes(window_bytes)
                .with_fairness_weight(fairness_weight);
            let core = node
                .open_stream(peer_node_id, stream_id, config)
                .map_err(|e| PyRuntimeError::new_err(format!("open_stream: {}", e)))?;
            Ok(NetStream {
                peer_node_id,
                stream_id,
                core,
            })
        }

        /// Close a stream. Idempotent.
        fn close_stream(&self, peer_node_id: u64, stream_id: u64) -> PyResult<()> {
            let node = self.get_node()?;
            node.close_stream(peer_node_id, stream_id);
            Ok(())
        }

        /// Send a batch of events on an explicit stream. Each event is
        /// a `bytes` payload.
        ///
        /// Raises:
        ///     BackpressureError: stream's in-flight window is full (no
        ///         events sent — the caller decides what to do).
        ///     NotConnectedError: stream's peer session is gone.
        ///     RuntimeError: underlying transport failure.
        fn send_on_stream(
            &self,
            py: Python<'_>,
            stream: &NetStream,
            events: Vec<Vec<u8>>,
        ) -> PyResult<()> {
            let node = self.get_node()?;
            let payloads: Vec<bytes::Bytes> = events.into_iter().map(bytes::Bytes::from).collect();
            // Release the GIL while the runtime is actually awaiting the
            // socket send. Without this, every other Python thread is
            // blocked for the duration — matters even for a single round
            // trip under contention, matters a lot for `send_with_retry`
            // / `send_blocking`.
            py.detach(|| {
                self.runtime
                    .block_on(node.send_on_stream(&stream.core, &payloads))
            })
            .map_err(stream_error_to_py)
        }

        /// Send events, retrying on `BackpressureError` with 5 ms → 200 ms
        /// exponential backoff up to `max_retries` times. Transport
        /// errors and `NotConnectedError` are raised immediately.
        #[pyo3(signature = (stream, events, max_retries=8))]
        fn send_with_retry(
            &self,
            py: Python<'_>,
            stream: &NetStream,
            events: Vec<Vec<u8>>,
            max_retries: u32,
        ) -> PyResult<()> {
            let node = self.get_node()?;
            let payloads: Vec<bytes::Bytes> = events.into_iter().map(bytes::Bytes::from).collect();
            py.detach(|| {
                self.runtime.block_on(node.send_with_retry(
                    &stream.core,
                    &payloads,
                    max_retries as usize,
                ))
            })
            .map_err(stream_error_to_py)
        }

        /// Block the calling task until the send succeeds or a
        /// transport error occurs. Retries `BackpressureError` with
        /// 5 ms → 200 ms exponential backoff up to 4096 times (~13 min
        /// worst case) — effectively "block until the network lets up"
        /// for practical workloads, but with a hard upper bound so
        /// runaway pressure can't hang the caller forever. Use
        /// `send_with_retry` for a tighter bound.
        ///
        /// Releases the GIL for the duration of the block — retries
        /// can take arbitrarily long under sustained backpressure, so
        /// other Python threads must be free to run (GC, signals,
        /// other worker threads).
        fn send_blocking(
            &self,
            py: Python<'_>,
            stream: &NetStream,
            events: Vec<Vec<u8>>,
        ) -> PyResult<()> {
            let node = self.get_node()?;
            let payloads: Vec<bytes::Bytes> = events.into_iter().map(bytes::Bytes::from).collect();
            py.detach(|| {
                self.runtime
                    .block_on(node.send_blocking(&stream.core, &payloads))
            })
            .map_err(stream_error_to_py)
        }

        /// Snapshot of per-stream stats. Returns None if the peer or
        /// stream isn't registered.
        fn stream_stats(
            &self,
            peer_node_id: u64,
            stream_id: u64,
        ) -> PyResult<Option<NetStreamStats>> {
            let node = self.get_node()?;
            Ok(node
                .stream_stats(peer_node_id, stream_id)
                .map(|s| NetStreamStats {
                    tx_seq: s.tx_seq,
                    rx_seq: s.rx_seq,
                    inbound_pending: s.inbound_pending,
                    last_activity_ns: s.last_activity_ns,
                    active: s.active,
                    backpressure_events: s.backpressure_events,
                    tx_credit_remaining: s.tx_credit_remaining,
                    tx_window: s.tx_window,
                    credit_grants_received: s.credit_grants_received,
                    credit_grants_sent: s.credit_grants_sent,
                }))
        }

        // =====================================================
        // Channels (distributed pub/sub)
        // =====================================================

        /// Register a channel on this node. Subscribers must pass the
        /// publisher-side ACL (built from this config) before being
        /// added to the roster.
        ///
        /// Args:
        ///     name: Canonical channel name (not the u16 hash).
        ///     visibility: One of 'subnet-local', 'parent-visible',
        ///         'exported', 'global'. Default 'global'.
        ///     reliable: Default reliability for streams on this
        ///         channel.
        ///     require_token: Require a valid token chain to publish
        ///         or subscribe. On its own (no `token_roots`) this
        ///         fails every authorization closed — pass
        ///         `token_roots` to anchor a root of trust.
        ///     token_roots: List of 32-byte entity ids whose signature
        ///         may root a presented token chain. Setting this turns
        ///         on token enforcement and anchors the channel: a
        ///         chain is honored only if its root link was issued by
        ///         one of these entities.
        ///     priority: 0 = lowest.
        ///     max_rate_pps: Rate cap in packets per second.
        ///
        /// Raises:
        ///     ChannelError: invalid name / visibility.
        #[pyo3(signature = (
            name,
            *,
            visibility = None,
            reliable = None,
            require_token = None,
            token_roots = None,
            priority = None,
            max_rate_pps = None,
            publish_caps = None,
            subscribe_caps = None,
        ))]
        #[allow(clippy::too_many_arguments)]
        fn register_channel(
            &self,
            name: &str,
            visibility: Option<&str>,
            reliable: Option<bool>,
            require_token: Option<bool>,
            token_roots: Option<Vec<Vec<u8>>>,
            priority: Option<u8>,
            max_rate_pps: Option<u32>,
            publish_caps: Option<&Bound<'_, PyDict>>,
            subscribe_caps: Option<&Bound<'_, PyDict>>,
        ) -> PyResult<()> {
            let channel = InnerChannelName::new(name).map_err(|e| {
                super::ChannelError::new_err(format!("channel: invalid name: {}", e))
            })?;
            let mut cfg = InnerChannelConfig::new(ChannelId::new(channel));
            if let Some(v) = visibility {
                cfg = cfg.with_visibility(parse_visibility(v)?);
            }
            if let Some(r) = reliable {
                cfg = cfg.with_reliable(r);
            }
            if let Some(t) = require_token {
                cfg = cfg.with_require_token(t);
            }
            if let Some(roots) = token_roots {
                let mut parsed = Vec::with_capacity(roots.len());
                for bytes in roots {
                    let arr: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
                        ChannelError::new_err(format!(
                            "channel: token_roots entry must be 32 bytes, got {}",
                            bytes.len()
                        ))
                    })?;
                    parsed.push(net::adapter::net::identity::EntityId::from_bytes(arr));
                }
                cfg = cfg.with_token_roots(parsed);
            }
            if let Some(p) = priority {
                cfg = cfg.with_priority(p);
            }
            if let Some(pps) = max_rate_pps {
                cfg = cfg.with_rate_limit(pps);
            }
            if let Some(filter_dict) = publish_caps {
                let filter = super::capabilities::capability_filter_from_py(filter_dict)?;
                cfg = cfg.with_publish_caps(filter);
            }
            if let Some(filter_dict) = subscribe_caps {
                let filter = super::capabilities::capability_filter_from_py(filter_dict)?;
                cfg = cfg.with_subscribe_caps(filter);
            }
            self.channel_configs.insert(cfg);
            Ok(())
        }

        /// Ask `publisher_node_id` to add this node to `channel`'s
        /// subscriber set. Blocks until the publisher's `Ack`
        /// arrives or the membership-ack timeout elapses.
        ///
        /// Optional `token` is the serialized `PermissionToken`
        /// bytes (161 bytes) — attach it when the publisher sets
        /// `require_token=True` on the channel, or when the
        /// caller's caps don't satisfy `subscribe_caps` on their
        /// own.
        ///
        /// Raises:
        ///     ChannelAuthError: publisher rejected as unauthorized.
        ///     ChannelError: other rejection / transport failure.
        ///     TokenError: supplied `token` is malformed / bad signature.
        #[pyo3(signature = (publisher_node_id, channel, token = None))]
        fn subscribe_channel(
            &self,
            publisher_node_id: u64,
            channel: &str,
            token: Option<&[u8]>,
        ) -> PyResult<()> {
            let node = self.get_node()?;
            let name = InnerChannelName::new(channel).map_err(|e| {
                super::ChannelError::new_err(format!("channel: invalid name: {}", e))
            })?;
            match token {
                Some(bytes) => {
                    let parsed = net::adapter::net::identity::PermissionToken::from_bytes(bytes)
                        .map_err(super::identity::token_err)?;
                    self.runtime
                        .block_on(node.subscribe_channel_with_token(
                            publisher_node_id,
                            name,
                            parsed,
                        ))
                        .map_err(adapter_to_channel_pyerr)
                }
                None => self
                    .runtime
                    .block_on(node.subscribe_channel(publisher_node_id, name))
                    .map_err(adapter_to_channel_pyerr),
            }
        }

        /// Mirror of `subscribe_channel`. Idempotent on the publisher
        /// side — unsubscribing a non-member returns `None`.
        fn unsubscribe_channel(&self, publisher_node_id: u64, channel: &str) -> PyResult<()> {
            let node = self.get_node()?;
            let name = InnerChannelName::new(channel).map_err(|e| {
                super::ChannelError::new_err(format!("channel: invalid name: {}", e))
            })?;
            self.runtime
                .block_on(node.unsubscribe_channel(publisher_node_id, name))
                .map_err(adapter_to_channel_pyerr)
        }

        /// Publish one payload to every subscriber of `channel`.
        /// Returns a `PublishReport` dict:
        ///
        ///     {
        ///       "attempted":  <int>,
        ///       "delivered":  <int>,
        ///       "errors":     [{"node_id": <int>, "message": <str>}, ...]
        ///     }
        ///
        /// Args:
        ///     channel: Channel name.
        ///     payload: Bytes to publish.
        ///     reliability: 'reliable' | 'fire_and_forget'. Default
        ///         'fire_and_forget'.
        ///     on_failure: 'best_effort' | 'fail_fast' | 'collect'.
        ///         Default 'best_effort'.
        ///     max_inflight: Concurrent per-peer sends. Default 32.
        #[pyo3(signature = (
            channel,
            payload,
            *,
            reliability = None,
            on_failure = None,
            max_inflight = None,
        ))]
        fn publish<'py>(
            &self,
            py: Python<'py>,
            channel: &str,
            payload: &[u8],
            reliability: Option<&str>,
            on_failure: Option<&str>,
            max_inflight: Option<u32>,
        ) -> PyResult<Bound<'py, pyo3::types::PyDict>> {
            let node = self.get_node()?;
            let name = InnerChannelName::new(channel).map_err(|e| {
                super::ChannelError::new_err(format!("channel: invalid name: {}", e))
            })?;
            let mut pub_cfg = InnerPublishConfig {
                reliability: Reliability::FireAndForget,
                on_failure: InnerOnFailure::BestEffort,
                max_inflight: 32,
            };
            if let Some(r) = reliability {
                pub_cfg.reliability = parse_reliability_cfg(r)?;
            }
            if let Some(f) = on_failure {
                pub_cfg.on_failure = parse_on_failure(f)?;
            }
            if let Some(n) = max_inflight {
                pub_cfg.max_inflight = n as usize;
            }
            let publisher = ChannelPublisher::new(name, pub_cfg);
            let payload_bytes = bytes::Bytes::copy_from_slice(payload);
            let report: InnerPublishReport = self
                .runtime
                .block_on(node.publish(&publisher, payload_bytes))
                .map_err(adapter_to_channel_pyerr)?;
            publish_report_to_pydict(py, report)
        }

        // =====================================================
        // Capability announcements
        // =====================================================

        /// Announce this node's capabilities to every directly-
        /// connected peer. Also self-indexes, so `find_nodes` on this
        /// same node matches on the announcement.
        ///
        /// Multi-hop propagation is deferred — peers more than one
        /// hop away will not see the announcement.
        ///
        /// CR-12: release the GIL across the broadcast. The
        /// underlying `node.announce_capabilities` issues UDP/QUIC
        /// frames to every directly-connected peer; without
        /// `py.detach` every other Python thread blocks for the
        /// network round-trip. Sibling sync paths (`call`,
        /// `find_service_nodes`) already follow this pattern.
        fn announce_capabilities(&self, py: Python<'_>, caps: &Bound<'_, PyDict>) -> PyResult<()> {
            let node = self.get_node()?;
            // capability_set_from_py touches Python objects; must
            // run while we still hold the GIL.
            let core = super::capabilities::capability_set_from_py(caps)?;
            py.detach(|| self.runtime.block_on(node.announce_capabilities(core)))
                .map_err(|e| PyRuntimeError::new_err(format!("capability: {}", e)))
        }

        // ---- Gang-claim resource-island scheduler ----

        /// Publish this node's island-topology record (its host is
        /// forced to this node). Self-indexed locally so the node's own
        /// scheduler sees it, then broadcast; returns the peer count.
        /// `capabilities` are resident tags (e.g. `"model:<hex>"`).
        fn publish_island_topology(
            &self,
            py: Python<'_>,
            id: u64,
            units: Vec<u32>,
            capabilities: Vec<String>,
            load: f64,
            p50_latency_us: u32,
        ) -> PyResult<usize> {
            use ::net::adapter::net::behavior::fold::{IslandRecord, UnitSet};
            let node = self.get_node()?;
            let record = IslandRecord {
                id,
                units: UnitSet::new(units),
                host: 0, // forced to this node by publish
                capabilities,
                load: load as f32,
                p50_latency_us,
            };
            py.detach(|| self.runtime.block_on(node.publish_island_topology(record)))
                .map_err(|e| PyRuntimeError::new_err(format!("gang: {}", e)))
        }

        /// Match islands against the criteria over this node's folds
        /// (read-only; no claim). Best island first. `tags_*` filter the
        /// host capability match; `require_*` filter the island's
        /// resident capabilities.
        #[pyo3(signature = (tags_all, tags_any=Vec::new(), tag_groups_all=Vec::new(), region=None, min_units=None, max_load=None, max_p50_latency_us=None, require_all=Vec::new(), require_any=Vec::new(), selection=None, load_band_target=None, prefer_capability=None))]
        #[allow(clippy::too_many_arguments)]
        fn match_islands(
            &self,
            tags_all: Vec<String>,
            tags_any: Vec<String>,
            tag_groups_all: Vec<Vec<String>>,
            region: Option<String>,
            min_units: Option<usize>,
            max_load: Option<f64>,
            max_p50_latency_us: Option<u32>,
            require_all: Vec<String>,
            require_any: Vec<String>,
            selection: Option<String>,
            load_band_target: Option<f64>,
            prefer_capability: Option<String>,
        ) -> PyResult<Vec<u64>> {
            let node = self.get_node()?;
            let mc = build_gang_criteria(
                tags_all,
                tags_any,
                tag_groups_all,
                region,
                min_units,
                max_load,
                max_p50_latency_us,
                require_all,
                require_any,
                selection,
                load_band_target,
                prefer_capability,
            )?;
            Ok(node.match_islands(&mc))
        }

        /// Reserve `island` until `until_unix_us` (wall-clock micros).
        /// Returns `"won"` / `"lost"`.
        fn reserve_island(
            &self,
            py: Python<'_>,
            island: u64,
            until_unix_us: u64,
        ) -> PyResult<String> {
            let node = self.get_node()?;
            let outcome = py
                .detach(|| {
                    self.runtime
                        .block_on(node.reserve_island(island, until_unix_us))
                })
                .map_err(|e| PyRuntimeError::new_err(format!("gang: {}", e)))?;
            Ok(claim_outcome_str(outcome).to_string())
        }

        /// Release `island` this node holds. Returns `"won"` / `"lost"`
        /// (`"lost"` if this node wasn't the holder).
        fn release_island(&self, py: Python<'_>, island: u64) -> PyResult<String> {
            let node = self.get_node()?;
            let outcome = py
                .detach(|| self.runtime.block_on(node.release_island(island)))
                .map_err(|e| PyRuntimeError::new_err(format!("gang: {}", e)))?;
            Ok(claim_outcome_str(outcome).to_string())
        }

        /// Match + reserve the first available island in one call.
        /// Returns its id, or `None` when nothing matched / all
        /// contended.
        #[pyo3(signature = (tags_all, until_unix_us, tags_any=Vec::new(), tag_groups_all=Vec::new(), region=None, min_units=None, max_load=None, max_p50_latency_us=None, require_all=Vec::new(), require_any=Vec::new(), selection=None, load_band_target=None, prefer_capability=None))]
        #[allow(clippy::too_many_arguments)]
        fn claim_island(
            &self,
            py: Python<'_>,
            tags_all: Vec<String>,
            until_unix_us: u64,
            tags_any: Vec<String>,
            tag_groups_all: Vec<Vec<String>>,
            region: Option<String>,
            min_units: Option<usize>,
            max_load: Option<f64>,
            max_p50_latency_us: Option<u32>,
            require_all: Vec<String>,
            require_any: Vec<String>,
            selection: Option<String>,
            load_band_target: Option<f64>,
            prefer_capability: Option<String>,
        ) -> PyResult<Option<u64>> {
            let node = self.get_node()?;
            let mc = build_gang_criteria(
                tags_all,
                tags_any,
                tag_groups_all,
                region,
                min_units,
                max_load,
                max_p50_latency_us,
                require_all,
                require_any,
                selection,
                load_band_target,
                prefer_capability,
            )?;
            py.detach(|| self.runtime.block_on(node.claim_island(&mc, until_unix_us)))
                .map_err(|e| PyRuntimeError::new_err(format!("gang: {}", e)))
        }

        /// **Test-only** helper for the groups test suite.
        /// Injects a synthetic capability announcement directly
        /// into the local capability index, simulating a peer
        /// announcement without going through a real handshake.
        ///
        /// Production code should NOT use this — the mesh's
        /// normal `announce_capabilities` path is what peers
        /// broadcast through at runtime. This exists so
        /// `test_groups.py` can stage enough placement candidates
        /// for `ReplicaGroup` / `ForkGroup` / `StandbyGroup`
        /// `place_with_spread` calls without spinning up a 3-node
        /// handshake in every test.
        #[cfg(feature = "groups")]
        fn _test_inject_synthetic_peer(&self, node_id: u64) -> PyResult<()> {
            use net::adapter::net::behavior::capability::{CapabilityAnnouncement, CapabilitySet};
            use net::adapter::net::identity::EntityId;
            let node = self.get_node()?;
            let eid = EntityId::from_bytes([0u8; 32]);
            node.test_inject_capability_announcement(CapabilityAnnouncement::new(
                node_id,
                eid,
                1,
                CapabilitySet::new(),
            ));
            Ok(())
        }

        /// Test-only — same shape as `_test_inject_synthetic_peer`
        /// but takes a list of canonical tag strings to install on
        /// the synthetic peer. Used by the Phase 6c
        /// capability-aggregation smoke tests so the fixture can
        /// stage multi-bucket data without spinning up multiple
        /// meshes.
        #[cfg(feature = "groups")]
        fn _test_inject_synthetic_peer_with_tags(
            &self,
            node_id: u64,
            tags: Vec<String>,
        ) -> PyResult<()> {
            use net::adapter::net::behavior::capability::{CapabilityAnnouncement, CapabilitySet};
            use net::adapter::net::behavior::Tag;
            use net::adapter::net::identity::EntityId;
            let node = self.get_node()?;
            let mut caps = CapabilitySet::new();
            // Insert via the permissive `Tag::parse` so reserved-
            // prefix tags (`scope:region:us-east`, etc.) make it
            // into the synthesized cap set; `add_tag` rejects
            // reserved prefixes by design.
            for s in tags {
                if let Ok(t) = Tag::parse(&s) {
                    caps.tags.insert(t);
                }
            }
            let eid = EntityId::from_bytes([0u8; 32]);
            node.test_inject_capability_announcement(CapabilityAnnouncement::new(
                node_id, eid, 1, caps,
            ));
            Ok(())
        }

        /// Query the local capability index. Returns node ids
        /// (including our own when we self-match) whose latest
        /// announcement matches `filter`.
        fn find_nodes(&self, filter: &Bound<'_, PyDict>) -> PyResult<Vec<u64>> {
            let node = self.get_node()?;
            let core = super::capabilities::capability_filter_from_py(filter)?;
            Ok(node.find_nodes_by_filter(&core))
        }

        /// Scoped variant of [`Self::find_nodes`]. Filters candidates
        /// through a `scope` dict derived from each peer's `scope:*`
        /// reserved tags. See
        /// `super::capabilities::scope_filter_from_py` for the
        /// accepted dict shapes.
        ///
        /// Untagged peers stay visible under most filters by design;
        /// peers tagged `scope:subnet-local` only show up under
        /// `{"kind": "same_subnet"}`.
        fn find_nodes_scoped(
            &self,
            filter: &Bound<'_, PyDict>,
            scope: &Bound<'_, PyDict>,
        ) -> PyResult<Vec<u64>> {
            let node = self.get_node()?;
            let core = super::capabilities::capability_filter_from_py(filter)?;
            let owned = super::capabilities::scope_filter_from_py(scope)?;
            Ok(super::capabilities::with_scope_filter(&owned, |sf| {
                node.find_nodes_by_filter_scoped(&core, sf)
            }))
        }

        /// Walk the local capability fold for every published AI
        /// tool. Returns a list of dicts mirroring the substrate's
        /// `ToolDescriptor` (one row per `(tool_id, version)`
        /// slot, with `node_count` filled in by the aggregating
        /// walk).
        ///
        /// One in-memory pass; no network. Schemas come back as
        /// JSON-encoded strings on `descriptor["input_schema"]` /
        /// `descriptor["output_schema"]` — call `json.loads(...)`
        /// for the parsed shape that adapter packages consume when
        /// lowering into provider-specific tool definitions.
        ///
        /// Mirror of the Rust SDK's `Mesh::list_tools(None)`. v1
        /// always walks unfiltered; matcher-pushdown lands in a
        /// follow-up that adds a `matcher` arg.
        ///
        /// Gated on the `tool` Cargo feature (default-on).
        ///
        /// Returns a JSON-encoded list of descriptors as a Python
        /// string. The `net.tool.list_tools` wrapper parses it once
        /// with `json.loads`. ToolDescriptor already derives
        /// `Serialize`, so this is a single `serde_json::to_string`
        /// instead of 12 fallible `PyDict::set_item` calls per
        /// descriptor — meaningful on every watch_tools poll.
        #[cfg(feature = "tool")]
        fn list_tools(&self) -> PyResult<String> {
            let node = self.get_node()?;
            let descriptors = node.list_tools(None);
            serde_json::to_string(&descriptors)
                .map_err(|e| PyValueError::new_err(format!("list_tools serialize failed: {e}")))
        }

        /// Event-driven watch over the local capability fold's tool
        /// view. Returns an `AsyncToolWatchIter` (PEP 525 async
        /// iterator) yielding one JSON-encoded `ToolListChange` per
        /// addition / removal / publisher-count change — delivered the
        /// moment the fold mutates, not on a timer. The
        /// `net.tool.watch_tools` wrapper parses each JSON change into
        /// the matching dataclass.
        ///
        /// `interval_ms` is a debounce ceiling, NOT a poll cadence:
        /// `None`/`0` is pure event-driven (idle fold = zero periodic
        /// work); a positive value additionally guarantees a re-diff at
        /// least every `interval_ms` as a safety net.
        ///
        /// Mirror of the Rust SDK's `Mesh::watch_tools` and the Node
        /// `watchTools`. Gated on the `tool` Cargo feature (default-on).
        #[cfg(feature = "tool")]
        #[pyo3(signature = (interval_ms=None))]
        fn watch_tools(
            &self,
            interval_ms: Option<u64>,
        ) -> PyResult<super::cortex::PyAsyncToolWatchIter> {
            let node = self.node_arc_clone()?;
            let interval = match interval_ms {
                Some(ms) if ms > 0 => Some(std::time::Duration::from_millis(ms)),
                _ => None,
            };
            // `watch_tools` spawns the substrate diff task, so it must be
            // called inside a tokio runtime context. The calling thread
            // here is the asyncio/GIL thread, not a tokio worker — enter
            // the shared runtime for the spawn. The guard only scopes the
            // `tokio::spawn`; the task then lives on the runtime's workers.
            let rt = self.runtime_arc();
            let _guard = rt.enter();
            let watch = node.watch_tools(None, interval);
            Ok(super::cortex::new_async_tool_watch_iter(watch))
        }

        /// Bucketed aggregation over the local capability fold —
        /// `Fold::aggregate(matcher, group_by, agg)`. Arguments are
        /// JSON-encoded tagged unions; the sdk-py wrappers ship
        /// dataclasses that emit the right shape. Returns
        /// `list[dict]` sorted lex by bucket key. Phase 6c-A of
        /// `MULTIFOLD_PHASE_6C_CAPACITY_AGGREGATION.md`.
        ///
        /// `matcher_json = None` walks every entry.
        #[pyo3(signature = (matcher_json, group_by_json, aggregation_json))]
        fn capability_aggregate(
            &self,
            py: Python<'_>,
            matcher_json: Option<&str>,
            group_by_json: &str,
            aggregation_json: &str,
        ) -> PyResult<Py<PyAny>> {
            let node = self.get_node()?;
            super::capability_aggregation::aggregate(
                py,
                node.capability_fold(),
                matcher_json,
                group_by_json,
                aggregation_json,
            )
        }

        /// Capacity-ranked materialized view over the local
        /// capability fold — `Fold::capacity_ranking(query,
        /// rtt_lookup)`. `query_json` is a JSON-encoded
        /// `CapacityQuery`; `rtt_map` is a Python `dict[int, int]`
        /// mapping `node_id -> rtt_ms` (`None` / empty disables the
        /// RTT filter regardless of `query.max_rtt_ms`).
        ///
        /// Faulty entries are always excluded; rows return sorted by
        /// `available` desc, ties broken on bucket asc, truncated to
        /// `query.limit`. Phase 6c-B.
        #[pyo3(signature = (query_json, rtt_map = None))]
        fn capability_capacity_ranking(
            &self,
            py: Python<'_>,
            query_json: &str,
            rtt_map: Option<&Bound<'_, PyDict>>,
        ) -> PyResult<Py<PyAny>> {
            let node = self.get_node()?;
            super::capability_aggregation::capacity_ranking(
                py,
                node.capability_fold(),
                query_json,
                rtt_map,
            )
        }

        // ── SDK Phase 7 slice 3 — custom PlacementFilter callbacks ──
        //
        // Python contract: `predicate(candidate: dict) -> bool` where
        // `candidate = { "node_id": int, "tags": list[str],
        // "metadata": dict[str, str] }`. Returning `True` keeps the
        // candidate (placement-score 1.0); `False` / exception /
        // non-bool vetoes it. The predicate runs per candidate per
        // placement decision under the GIL — keep it tight.
        //
        // Bridge wrapper lives in `super::placement::PyPlacementFilter`;
        // registration goes through the substrate's
        // `global_placement_filter_registry` singleton.

        /// Register a Python placement-filter predicate under `id`.
        ///
        /// Returns `True` if registration succeeded; `False` if `id`
        /// is already registered. The SDK's
        /// `placement_filter_from_fn` generates unique IDs by
        /// counter, so collisions are an SDK-side concern. Use
        /// `unregister_placement_filter` first if you need to swap
        /// the predicate behind a stable id.
        fn register_placement_filter(
            &self,
            py: Python<'_>,
            id: String,
            predicate: Py<PyAny>,
        ) -> PyResult<bool> {
            use net::adapter::net::behavior::placement::PlacementFilter;
            use net::adapter::net::behavior::placement_registry::global_placement_filter_registry;

            // P2-K: validate the predicate is callable BEFORE
            // wrapping + registering. Pre-fix any object — None, a
            // dict, a non-callable instance — would register
            // successfully and only surface failure at first
            // dispatch (where `PyPlacementFilter::placement_score`
            // raises `TypeError`, the wrapper translates to None,
            // and every candidate gets vetoed silently). Caller-
            // side `TypeError` at registration is the right shape
            // — same contract as `napi`'s
            // `function-arg-required-but-not-provided` validation.
            if !predicate.bind(py).is_callable() {
                return Err(pyo3::exceptions::PyTypeError::new_err(
                    "predicate must be callable as predicate(candidate: dict) -> bool",
                ));
            }

            let node = self.get_node()?;
            let capability_fold = node.capability_fold().clone();
            let wrapper =
                super::placement::PyPlacementFilter::new(id.clone(), predicate, capability_fold);
            let arc: std::sync::Arc<dyn PlacementFilter> = std::sync::Arc::new(wrapper);
            // SDK Phase 7 polish: `"python"` binding label drives the
            // `dataforts_placement_callback_invocations_total{binding="python"}`
            // counter on the substrate registry.
            Ok(global_placement_filter_registry().register(id, arc, "python"))
        }

        /// Drop the placement-filter registration under `id`.
        ///
        /// Returns `True` if `id` was registered. Existing
        /// `Arc<dyn PlacementFilter>` clones held by in-flight
        /// scheduler calls keep the predicate alive until those
        /// calls finish — see the registry docs.
        fn unregister_placement_filter(&self, id: String) -> bool {
            use net::adapter::net::behavior::placement_registry::global_placement_filter_registry;
            global_placement_filter_registry().unregister(&id)
        }

        /// Whether `id` is currently registered. Mainly for tests.
        fn has_placement_filter(&self, id: String) -> bool {
            use net::adapter::net::behavior::placement_registry::global_placement_filter_registry;
            global_placement_filter_registry().contains(&id)
        }

        // ── NAT traversal ──────────────────────────────────────
        //
        // Framing (plan §5, load-bearing): every user-visible
        // docstring positions NAT traversal as **optimization,
        // not correctness**. Nodes behind NAT can always reach
        // each other through the mesh's routed-handshake path.
        // A `nat_type` of `"symmetric"` or a
        // `traversal: punch-failed` error is not a connectivity
        // failure — traffic just keeps riding the relay.

        /// NAT classification for this mesh, as a stable string:
        /// `"open" | "cone" | "symmetric" | "unknown"`.
        /// `"unknown"` is the pre-classification state;
        /// classification runs in the background after `start()`
        /// once ≥2 peers are connected. Requires the
        /// `nat-traversal` build.
        #[cfg(feature = "nat-traversal")]
        fn nat_type(&self) -> PyResult<String> {
            let node = self.get_node()?;
            Ok(nat_class_to_string(node.nat_class()))
        }

        /// This mesh's public-facing `ip:port` as observed by a
        /// remote peer, or `None` before classification has
        /// produced an observation. Piggybacks on outbound
        /// capability announcements so peers can attempt direct
        /// connects without a separate discovery round-trip.
        /// Requires the `nat-traversal` build.
        #[cfg(feature = "nat-traversal")]
        fn reflex_addr(&self) -> PyResult<Option<String>> {
            let node = self.get_node()?;
            Ok(node.reflex_addr().map(|a| a.to_string()))
        }

        /// NAT classification most recently advertised by
        /// `peer_node_id` (parsed from the `nat:*` tag on their
        /// capability announcement). Returns `"unknown"` when
        /// the peer hasn't announced. The pair-type matrix
        /// treats Unknown as "attempt direct, fall back on
        /// failure," never "don't attempt." Requires the
        /// `nat-traversal` build.
        #[cfg(feature = "nat-traversal")]
        fn peer_nat_type(&self, peer_node_id: u64) -> PyResult<String> {
            let node = self.get_node()?;
            Ok(nat_class_to_string(node.peer_nat_class(peer_node_id)))
        }

        /// Send one reflex probe to `peer_node_id` and return the
        /// public `ip:port` the peer observed on the probe's UDP
        /// envelope. Useful for tests and for diagnosing
        /// misclassifications.
        ///
        /// Raises `RuntimeError` whose message follows the
        /// `traversal: <kind>[: <detail>]` convention (kinds:
        /// `reflex-timeout`, `peer-not-reachable`, `transport`)
        /// — mirrors the pattern used by `migration:` errors.
        /// Requires the `nat-traversal` build.
        #[cfg(feature = "nat-traversal")]
        fn probe_reflex(&self, py: Python<'_>, peer_node_id: u64) -> PyResult<String> {
            let node = self
                .node
                .as_ref()
                .cloned()
                .ok_or_else(|| PyRuntimeError::new_err("MeshNode has been shut down"))?;
            let runtime = self.runtime.clone();
            py.detach(move || {
                runtime
                    .block_on(node.probe_reflex(peer_node_id))
                    .map(|a| a.to_string())
                    .map_err(traversal_py_err)
            })
        }

        /// Explicitly re-run the classification sweep. Normally
        /// the background loop handles this; call this after a
        /// suspected NAT rebind (gateway reboot, address change)
        /// to accelerate re-classification. No-op when fewer
        /// than 2 peers are connected. Never raises.
        /// Requires the `nat-traversal` build.
        #[cfg(feature = "nat-traversal")]
        fn reclassify_nat(&self, py: Python<'_>) -> PyResult<()> {
            let node = self
                .node
                .as_ref()
                .cloned()
                .ok_or_else(|| PyRuntimeError::new_err("MeshNode has been shut down"))?;
            let runtime = self.runtime.clone();
            py.detach(move || {
                runtime.block_on(node.reclassify_nat());
            });
            Ok(())
        }

        /// Cumulative NAT-traversal counters — the full stage-5
        /// snapshot. Returns a dict with:
        ///
        /// - `punches_attempted` / `punches_succeeded` /
        ///   `punches_failed` (derived: attempted - succeeded) /
        ///   `relay_fallbacks` — punch outcome counters,
        /// - `punch_timeouts` / `punch_rejections` /
        ///   `rendezvous_no_relay` — failure *cause* counters
        ///   (include pre-mediation failures; not a partition of
        ///   `punches_failed`),
        /// - `upgrades_attempted` / `upgrades_succeeded` /
        ///   `upgrades_deferred_busy` — background direct-path
        ///   upgrade activity,
        /// - `port_mapping_active` (bool) /
        ///   `port_mapping_external` (str | None) /
        ///   `port_mapping_renewals` — port-mapping state.
        ///
        /// Base counters are monotonic and never reset;
        /// `punches_failed` is derived and can decrease while a
        /// punch is in flight, and `port_mapping_renewals` resets
        /// on each fresh install — difference only the base
        /// counters for rates. Requires the `nat-traversal` build.
        #[cfg(feature = "nat-traversal")]
        fn traversal_stats<'py>(
            &self,
            py: Python<'py>,
        ) -> PyResult<Bound<'py, pyo3::types::PyDict>> {
            let node = self.get_node()?;
            let snap = node.traversal_stats();
            let d = pyo3::types::PyDict::new(py);
            d.set_item("punches_attempted", snap.punches_attempted)?;
            d.set_item("punches_succeeded", snap.punches_succeeded)?;
            d.set_item("punches_failed", snap.punches_failed)?;
            d.set_item("relay_fallbacks", snap.relay_fallbacks)?;
            d.set_item("punch_timeouts", snap.punch_timeouts)?;
            d.set_item("punch_rejections", snap.punch_rejections)?;
            d.set_item("rendezvous_no_relay", snap.rendezvous_no_relay)?;
            d.set_item("upgrades_attempted", snap.upgrades_attempted)?;
            d.set_item("upgrades_succeeded", snap.upgrades_succeeded)?;
            d.set_item("upgrades_deferred_busy", snap.upgrades_deferred_busy)?;
            d.set_item("port_mapping_active", snap.port_mapping_active)?;
            d.set_item(
                "port_mapping_external",
                snap.port_mapping_external.map(|a| a.to_string()),
            )?;
            d.set_item("port_mapping_renewals", snap.port_mapping_renewals)?;
            Ok(d)
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
        /// Raises `RuntimeError` with
        /// `traversal: peer-not-reachable` when we have no
        /// cached reflex for `peer_node_id`, or
        /// `traversal: transport: ...` on a socket-level
        /// handshake error. Requires the `nat-traversal` build.
        #[cfg(feature = "nat-traversal")]
        #[pyo3(signature = (peer_node_id, peer_public_key, coordinator))]
        fn connect_direct(
            &self,
            py: Python<'_>,
            peer_node_id: u64,
            peer_public_key: &str,
            coordinator: u64,
        ) -> PyResult<()> {
            let pubkey_bytes = hex::decode(peer_public_key)
                .map_err(|e| PyValueError::new_err(format!("invalid hex: {}", e)))?;
            if pubkey_bytes.len() != 32 {
                return Err(PyValueError::new_err("public key must be 32 bytes"));
            }
            let mut pubkey = [0u8; 32];
            pubkey.copy_from_slice(&pubkey_bytes);

            let node = self
                .node
                .as_ref()
                .cloned()
                .ok_or_else(|| PyRuntimeError::new_err("MeshNode has been shut down"))?;
            let runtime = self.runtime.clone();
            py.detach(move || {
                runtime
                    .block_on(node.connect_direct(peer_node_id, &pubkey, coordinator))
                    .map_err(traversal_py_err)?;
                Ok(())
            })
        }

        /// Like `connect_direct`, but auto-selects the rendezvous
        /// coordinator: the relay currently forwarding to the
        /// peer, then a `relay-capable` mutual peer, then any
        /// mutual peer.
        ///
        /// **Optimization, not correctness.** Punch-needing pairs
        /// with no coordinator candidate raise `RuntimeError`
        /// with `traversal: rendezvous-no-relay` — the caller
        /// simply stays on the routed path, which is always
        /// available. Requires the `nat-traversal` build.
        #[cfg(feature = "nat-traversal")]
        #[pyo3(signature = (peer_node_id, peer_public_key))]
        fn connect_direct_auto(
            &self,
            py: Python<'_>,
            peer_node_id: u64,
            peer_public_key: &str,
        ) -> PyResult<()> {
            let pubkey_bytes = hex::decode(peer_public_key)
                .map_err(|e| PyValueError::new_err(format!("invalid hex: {}", e)))?;
            if pubkey_bytes.len() != 32 {
                return Err(PyValueError::new_err("public key must be 32 bytes"));
            }
            let mut pubkey = [0u8; 32];
            pubkey.copy_from_slice(&pubkey_bytes);

            let node = self
                .node
                .as_ref()
                .cloned()
                .ok_or_else(|| PyRuntimeError::new_err("MeshNode has been shut down"))?;
            let runtime = self.runtime.clone();
            py.detach(move || {
                runtime
                    .block_on(node.connect_direct_auto(peer_node_id, &pubkey))
                    .map_err(traversal_py_err)?;
                Ok(())
            })
        }

        /// Install a runtime reflex override. Forces `nat_type()`
        /// to `"open"` and `reflex_addr()` to `external`
        /// immediately, short-circuiting any further classifier
        /// sweeps. Runtime counterpart of the `reflex_override`
        /// constructor kwarg — useful when a port-forward goes
        /// live mid-session or when a stage-4 port-mapping task
        /// has just installed a mapping.
        ///
        /// **Optimization, not correctness.** Nodes without an
        /// override still reach every peer via the routed-
        /// handshake path.
        ///
        /// `external` is an "ip:port" string. Raises
        /// `ValueError` if it fails to parse.
        #[cfg(feature = "nat-traversal")]
        fn set_reflex_override(&self, external: &str) -> PyResult<()> {
            let node = self.get_node()?;
            let addr: std::net::SocketAddr = external
                .parse()
                .map_err(|e| PyValueError::new_err(format!("invalid reflex override: {e}")))?;
            node.set_reflex_override(addr);
            Ok(())
        }

        /// Drop a previously-installed reflex override. The
        /// classifier resumes on its normal cadence;
        /// `reflex_addr()` clears to `None` immediately so a
        /// between-sweep read doesn't return a stale override.
        ///
        /// No-op when no override is active — safe to call
        /// unconditionally on shutdown or revoke paths.
        #[cfg(feature = "nat-traversal")]
        fn clear_reflex_override(&self) -> PyResult<()> {
            let node = self.get_node()?;
            node.clear_reflex_override();
            Ok(())
        }

        /// Shutdown the mesh node. Idempotent — a second call is a no-op.
        fn shutdown(&mut self) -> PyResult<()> {
            let Some(node) = self.node.take() else {
                return Ok(());
            };
            self.runtime
                .block_on(node.shutdown())
                .map_err(|e| PyRuntimeError::new_err(format!("shutdown: {}", e)))?;
            Ok(())
        }

        fn __repr__(&self) -> String {
            if let Some(node) = &self.node {
                format!(
                    "NetMesh(addr={}, peers={}, nodes={})",
                    node.local_addr(),
                    node.peer_count(),
                    node.proximity_graph().node_count()
                )
            } else {
                "NetMesh(shutdown)".to_string()
            }
        }

        fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
            slf
        }

        fn __exit__(
            &mut self,
            _exc_type: Option<&Bound<'_, PyAny>>,
            _exc_val: Option<&Bound<'_, PyAny>>,
            _exc_tb: Option<&Bound<'_, PyAny>>,
        ) -> PyResult<bool> {
            self.shutdown()?;
            Ok(false)
        }
    }

    impl NetMesh {
        fn get_node(&self) -> PyResult<&MeshNode> {
            self.node
                .as_deref()
                .ok_or_else(|| PyRuntimeError::new_err("MeshNode has been shut down"))
        }

        /// Clone the `Arc<MeshNode>` backing this `NetMesh`. Used
        /// by sibling-feature modules (`compute`, `mesh_rpc`) to
        /// share the live mesh node without opening a second
        /// socket.
        // Also reachable under `dataforts` (the transport bindings clone
        // the node Arc); `dataforts` enables `cortex`, so this gate
        // already covers it.
        #[cfg(any(feature = "compute", feature = "cortex", feature = "aggregator"))]
        pub(crate) fn node_arc_clone(&self) -> PyResult<Arc<MeshNode>> {
            self.node
                .as_ref()
                .cloned()
                .ok_or_else(|| PyRuntimeError::new_err("MeshNode has been shut down"))
        }

        /// Shared `ChannelConfigRegistry`. Currently consumed by
        /// `compute` only; nRPC's `serve_rpc` auto-registers via
        /// the SDK glue without needing per-binding access. Kept
        /// gated on either feature so the accessor is available
        /// if `mesh_rpc` ever needs it.
        #[cfg(any(feature = "compute", feature = "cortex"))]
        #[cfg_attr(all(feature = "cortex", not(feature = "compute")), allow(dead_code))]
        pub(crate) fn channel_configs_arc(&self) -> Arc<ChannelConfigRegistry> {
            self.channel_configs.clone()
        }

        /// Shared tokio runtime. `DaemonRuntime` and `MeshRpc`
        /// both use this for async method bridging so we don't
        /// spin up a second runtime per mesh.
        #[cfg(any(feature = "compute", feature = "cortex", feature = "aggregator"))]
        pub(crate) fn runtime_arc(&self) -> Arc<Runtime> {
            self.runtime.clone()
        }
    }

    // ====================================================================
    // AsyncNetMesh / AsyncNetStream — T1-A1 + T1-A2.
    //
    // Parallel async surface over the same `Arc<MeshNode>` the sync
    // `NetMesh` / `NetStream` wrap. Constructor accepts the sync
    // `NetMesh` (cheap `Arc::clone`); handshakes, subscriptions,
    // capability announcements, and stream registrations done via
    // one side are visible to the other. Async I/O methods return
    // Python awaitables via the pyo3-async-runtimes bridge.
    //
    // No substrate cancel-token plumbing here — `MeshNode::connect`,
    // `subscribe_channel`, `publish`, etc. don't expose cancel-token
    // hooks (yet). asyncio task-cancel still works: dropping the
    // pyo3-async-runtimes-spawned task drops the underlying tokio
    // future, which the substrate handles at session/stream level.
    // Cancel-token wiring lands when the substrate exposes it for
    // these surfaces.
    // ====================================================================

    /// Async sibling of [`NetStream`]. Same opaque handle to a
    /// logical stream; awaitable `send*` methods live here so users
    /// can write ``await stream.send(payloads)`` directly instead
    /// of routing through `AsyncNetMesh.send_on_stream(...)`.
    ///
    /// Sync equivalent: :class:`NetStream`.
    #[pyclass(name = "AsyncNetStream", module = "_net")]
    pub struct AsyncNetStream {
        peer_node_id: u64,
        stream_id: u64,
        core: CoreStream,
        node: Arc<MeshNode>,
    }

    #[pymethods]
    impl AsyncNetStream {
        #[getter]
        fn peer_node_id(&self) -> u64 {
            self.peer_node_id
        }
        #[getter]
        fn stream_id(&self) -> u64 {
            self.stream_id
        }
        fn __repr__(&self) -> String {
            format!(
                "AsyncNetStream(peer_node_id={:#x}, stream_id={:#x})",
                self.peer_node_id, self.stream_id
            )
        }

        /// Send a batch of payloads. Returns an awaitable that
        /// resolves to ``None`` on success.
        ///
        /// Raises:
        ///     BackpressureError: per-stream window is full.
        ///     NotConnectedError: peer session is gone.
        ///     RuntimeError: transport failure.
        fn send<'py>(&self, py: Python<'py>, events: Vec<Vec<u8>>) -> PyResult<Bound<'py, PyAny>> {
            let node = self.node.clone();
            let core = self.core.clone();
            let payloads: Vec<bytes::Bytes> = events.into_iter().map(bytes::Bytes::from).collect();
            pyo3_async_runtimes::tokio::future_into_py(py, async move {
                node.send_on_stream(&core, &payloads)
                    .await
                    .map_err(stream_error_to_py)
            })
        }

        /// Send with up to ``max_retries`` retries on
        /// `BackpressureError` (5 ms → 200 ms exponential).
        /// Transport errors raise immediately.
        #[pyo3(signature = (events, max_retries=8))]
        fn send_with_retry<'py>(
            &self,
            py: Python<'py>,
            events: Vec<Vec<u8>>,
            max_retries: u32,
        ) -> PyResult<Bound<'py, PyAny>> {
            let node = self.node.clone();
            let core = self.core.clone();
            let payloads: Vec<bytes::Bytes> = events.into_iter().map(bytes::Bytes::from).collect();
            pyo3_async_runtimes::tokio::future_into_py(py, async move {
                node.send_with_retry(&core, &payloads, max_retries as usize)
                    .await
                    .map_err(stream_error_to_py)
            })
        }

        /// Block-until-success send. Retries up to 4096 times under
        /// sustained backpressure (~13 min worst case). Use
        /// :meth:`send_with_retry` for a tighter bound.
        fn send_blocking<'py>(
            &self,
            py: Python<'py>,
            events: Vec<Vec<u8>>,
        ) -> PyResult<Bound<'py, PyAny>> {
            let node = self.node.clone();
            let core = self.core.clone();
            let payloads: Vec<bytes::Bytes> = events.into_iter().map(bytes::Bytes::from).collect();
            pyo3_async_runtimes::tokio::future_into_py(py, async move {
                node.send_blocking(&core, &payloads)
                    .await
                    .map_err(stream_error_to_py)
            })
        }
    }

    /// Newtype that lets `MeshNode::publish`'s `PublishReport` be
    /// returned from a future and converted to the same `PyDict`
    /// shape the sync `NetMesh.publish` method returns. The
    /// conversion runs under the GIL on the Python awaitable's
    /// resume step.
    struct AsyncPublishReportWrap(InnerPublishReport);

    impl<'py> pyo3::IntoPyObject<'py> for AsyncPublishReportWrap {
        type Target = pyo3::types::PyAny;
        type Output = Bound<'py, Self::Target>;
        type Error = PyErr;
        fn into_pyobject(self, py: Python<'py>) -> Result<Self::Output, Self::Error> {
            Ok(publish_report_to_pydict(py, self.0)?.into_any())
        }
    }

    /// Async sibling of [`NetMesh`]. Shares the same `Arc<MeshNode>`
    /// as the source sync `NetMesh`; handshakes / channels /
    /// capabilities / streams are interoperable across both shapes.
    /// Network methods return Python awaitables; sync helpers
    /// (getters, `find_nodes`, etc.) stay sync.
    ///
    /// Sync equivalent: :class:`NetMesh`.
    #[pyclass(name = "AsyncNetMesh", module = "_net")]
    pub struct AsyncNetMesh {
        node: Arc<MeshNode>,
    }

    #[pymethods]
    impl AsyncNetMesh {
        /// Build against an existing `NetMesh`. Cheap
        /// (`Arc::clone`); the underlying `MeshNode` is shared.
        #[new]
        fn new(mesh: &NetMesh) -> PyResult<Self> {
            let node = mesh
                .node
                .as_ref()
                .cloned()
                .ok_or_else(|| PyRuntimeError::new_err("NetMesh has been shut down"))?;
            Ok(Self { node })
        }

        #[getter]
        fn public_key(&self) -> String {
            hex::encode(self.node.public_key())
        }
        #[getter]
        fn entity_id(&self) -> Vec<u8> {
            self.node.entity_id().as_bytes().to_vec()
        }
        #[getter]
        fn node_id(&self) -> u64 {
            self.node.node_id()
        }
        fn peer_count(&self) -> usize {
            self.node.peer_count()
        }
        fn discovered_nodes(&self) -> usize {
            self.node.proximity_graph().node_count()
        }

        fn __repr__(&self) -> String {
            format!(
                "AsyncNetMesh(addr={}, peers={}, nodes={})",
                self.node.local_addr(),
                self.node.peer_count(),
                self.node.proximity_graph().node_count()
            )
        }

        /// Start the receive loop + heartbeats. Sync — internal
        /// `tokio::spawn`, no network round-trip.
        ///
        /// `start_arc`, matching the sync `NetMesh.start`: enables
        /// the Arc-scoped lifecycle loops (periodic re-announce +
        /// the opt-in background direct-path upgrade).
        fn start(&self) -> PyResult<()> {
            let handle = crate::async_bridge::runtime()
                .ok_or_else(|| PyRuntimeError::new_err("async bridge not initialized"))?;
            let _guard = handle.enter();
            self.node.start_arc();
            Ok(())
        }

        /// Initiate a handshake to `peer_addr`. Returns an
        /// awaitable that resolves to ``None`` on success.
        #[pyo3(signature = (peer_addr, peer_public_key, peer_node_id))]
        fn connect<'py>(
            &self,
            py: Python<'py>,
            peer_addr: &str,
            peer_public_key: &str,
            peer_node_id: u64,
        ) -> PyResult<Bound<'py, PyAny>> {
            let addr: std::net::SocketAddr = peer_addr
                .parse()
                .map_err(|e| PyValueError::new_err(format!("invalid address: {}", e)))?;
            let pubkey_bytes = hex::decode(peer_public_key)
                .map_err(|e| PyValueError::new_err(format!("invalid hex: {}", e)))?;
            if pubkey_bytes.len() != 32 {
                return Err(PyValueError::new_err("public key must be 32 bytes"));
            }
            let mut pubkey = [0u8; 32];
            pubkey.copy_from_slice(&pubkey_bytes);
            let node = self.node.clone();
            pyo3_async_runtimes::tokio::future_into_py(py, async move {
                node.connect(addr, &pubkey, peer_node_id)
                    .await
                    .map_err(|e| PyRuntimeError::new_err(format!("connect: {}", e)))
            })
        }

        /// Accept an incoming connection. Returns an awaitable
        /// resolving to the peer's observed `ip:port` as a string.
        fn accept<'py>(&self, py: Python<'py>, peer_node_id: u64) -> PyResult<Bound<'py, PyAny>> {
            let node = self.node.clone();
            pyo3_async_runtimes::tokio::future_into_py(py, async move {
                let (addr, _) = node
                    .accept(peer_node_id)
                    .await
                    .map_err(|e| PyRuntimeError::new_err(format!("accept: {}", e)))?;
                Ok::<String, PyErr>(addr.to_string())
            })
        }

        /// Send raw JSON to a direct peer.
        fn push_to<'py>(
            &self,
            py: Python<'py>,
            peer_addr: &str,
            json: String,
        ) -> PyResult<Bound<'py, PyAny>> {
            let addr: std::net::SocketAddr = peer_addr
                .parse()
                .map_err(|e| PyValueError::new_err(format!("invalid address: {}", e)))?;
            let node = self.node.clone();
            pyo3_async_runtimes::tokio::future_into_py(py, async move {
                let batch = net::event::Batch {
                    shard_id: 0,
                    events: vec![net::event::InternalEvent::new(
                        bytes::Bytes::copy_from_slice(json.as_bytes()),
                        0,
                        0,
                    )],
                    sequence_start: 0,
                    process_nonce: net::event::batch_process_nonce(),
                };
                node.send_to_peer(addr, &batch)
                    .await
                    .map(|_| true)
                    .map_err(|e| PyRuntimeError::new_err(format!("send: {}", e)))
            })
        }

        /// Poll for received events.
        fn poll<'py>(&self, py: Python<'py>, limit: usize) -> PyResult<Bound<'py, PyAny>> {
            let node = self.node.clone();
            pyo3_async_runtimes::tokio::future_into_py(py, async move {
                let result = node
                    .poll_shard(0, None, limit)
                    .await
                    .map_err(|e| PyRuntimeError::new_err(format!("poll: {}", e)))?;
                let events: Vec<StoredEvent> = result
                    .events
                    .into_iter()
                    .map(|e| {
                        let raw = e.raw_str().unwrap_or("").to_string();
                        StoredEvent {
                            id: e.id,
                            raw,
                            insertion_ts: e.insertion_ts,
                            shard_id: e.shard_id,
                        }
                    })
                    .collect();
                Ok::<Vec<StoredEvent>, PyErr>(events)
            })
        }

        /// Subscribe to a channel on `publisher_node_id`. Optional
        /// `token` carries a `PermissionToken` for capability auth.
        #[pyo3(signature = (publisher_node_id, channel, token=None))]
        fn subscribe_channel<'py>(
            &self,
            py: Python<'py>,
            publisher_node_id: u64,
            channel: &str,
            token: Option<&[u8]>,
        ) -> PyResult<Bound<'py, PyAny>> {
            let name = InnerChannelName::new(channel).map_err(|e| {
                super::ChannelError::new_err(format!("channel: invalid name: {}", e))
            })?;
            let parsed_token = match token {
                Some(bytes) => Some(
                    net::adapter::net::identity::PermissionToken::from_bytes(bytes)
                        .map_err(super::identity::token_err)?,
                ),
                None => None,
            };
            let node = self.node.clone();
            pyo3_async_runtimes::tokio::future_into_py(py, async move {
                match parsed_token {
                    Some(t) => node
                        .subscribe_channel_with_token(publisher_node_id, name, t)
                        .await
                        .map_err(adapter_to_channel_pyerr),
                    None => node
                        .subscribe_channel(publisher_node_id, name)
                        .await
                        .map_err(adapter_to_channel_pyerr),
                }
            })
        }

        /// Unsubscribe from a channel on `publisher_node_id`.
        fn unsubscribe_channel<'py>(
            &self,
            py: Python<'py>,
            publisher_node_id: u64,
            channel: &str,
        ) -> PyResult<Bound<'py, PyAny>> {
            let name = InnerChannelName::new(channel).map_err(|e| {
                super::ChannelError::new_err(format!("channel: invalid name: {}", e))
            })?;
            let node = self.node.clone();
            pyo3_async_runtimes::tokio::future_into_py(py, async move {
                node.unsubscribe_channel(publisher_node_id, name)
                    .await
                    .map_err(adapter_to_channel_pyerr)
            })
        }

        /// Publish a payload to every subscriber. Returns an
        /// awaitable resolving to the same dict shape as
        /// :meth:`NetMesh.publish`.
        #[pyo3(signature = (
            channel,
            payload,
            *,
            reliability = None,
            on_failure = None,
            max_inflight = None,
        ))]
        fn publish<'py>(
            &self,
            py: Python<'py>,
            channel: &str,
            payload: &[u8],
            reliability: Option<&str>,
            on_failure: Option<&str>,
            max_inflight: Option<u32>,
        ) -> PyResult<Bound<'py, PyAny>> {
            let name = InnerChannelName::new(channel).map_err(|e| {
                super::ChannelError::new_err(format!("channel: invalid name: {}", e))
            })?;
            let mut pub_cfg = InnerPublishConfig {
                reliability: Reliability::FireAndForget,
                on_failure: InnerOnFailure::BestEffort,
                max_inflight: 32,
            };
            if let Some(r) = reliability {
                pub_cfg.reliability = parse_reliability_cfg(r)?;
            }
            if let Some(f) = on_failure {
                pub_cfg.on_failure = parse_on_failure(f)?;
            }
            if let Some(n) = max_inflight {
                pub_cfg.max_inflight = n as usize;
            }
            let publisher = ChannelPublisher::new(name, pub_cfg);
            let payload_bytes = bytes::Bytes::copy_from_slice(payload);
            let node = self.node.clone();
            pyo3_async_runtimes::tokio::future_into_py(py, async move {
                let report = node
                    .publish(&publisher, payload_bytes)
                    .await
                    .map_err(adapter_to_channel_pyerr)?;
                Ok::<AsyncPublishReportWrap, PyErr>(AsyncPublishReportWrap(report))
            })
        }

        /// Broadcast a capability announcement to every directly-
        /// connected peer and self-index for `find_nodes` matches.
        fn announce_capabilities<'py>(
            &self,
            py: Python<'py>,
            caps: &Bound<'py, PyDict>,
        ) -> PyResult<Bound<'py, PyAny>> {
            let core = super::capabilities::capability_set_from_py(caps)?;
            let node = self.node.clone();
            pyo3_async_runtimes::tokio::future_into_py(py, async move {
                node.announce_capabilities(core)
                    .await
                    .map_err(|e| PyRuntimeError::new_err(format!("capability: {}", e)))
            })
        }

        /// Open (or look up) a stream to a connected peer. Sync —
        /// stream registration is local; no network round-trip.
        /// Repeated calls for the same (peer, stream_id) are
        /// idempotent.
        #[pyo3(signature = (
            peer_node_id,
            stream_id,
            reliability=None,
            window_bytes=DEFAULT_STREAM_WINDOW_BYTES,
            fairness_weight=1
        ))]
        fn open_stream(
            &self,
            peer_node_id: u64,
            stream_id: u64,
            reliability: Option<&str>,
            window_bytes: u32,
            fairness_weight: u8,
        ) -> PyResult<AsyncNetStream> {
            let rel = parse_reliability(reliability)?;
            let config = StreamConfig::new()
                .with_reliability(rel)
                .with_window_bytes(window_bytes)
                .with_fairness_weight(fairness_weight);
            let core = self
                .node
                .open_stream(peer_node_id, stream_id, config)
                .map_err(|e| PyRuntimeError::new_err(format!("open_stream: {}", e)))?;
            Ok(AsyncNetStream {
                peer_node_id,
                stream_id,
                core,
                node: self.node.clone(),
            })
        }

        /// Close a stream. Sync — idempotent local operation.
        fn close_stream(&self, peer_node_id: u64, stream_id: u64) {
            self.node.close_stream(peer_node_id, stream_id);
        }

        /// Snapshot of per-stream stats.
        fn stream_stats(&self, peer_node_id: u64, stream_id: u64) -> Option<NetStreamStats> {
            self.node
                .stream_stats(peer_node_id, stream_id)
                .map(|s| NetStreamStats {
                    tx_seq: s.tx_seq,
                    rx_seq: s.rx_seq,
                    inbound_pending: s.inbound_pending,
                    last_activity_ns: s.last_activity_ns,
                    active: s.active,
                    backpressure_events: s.backpressure_events,
                    tx_credit_remaining: s.tx_credit_remaining,
                    tx_window: s.tx_window,
                    credit_grants_received: s.credit_grants_received,
                    credit_grants_sent: s.credit_grants_sent,
                })
        }

        /// Query the local capability index.
        fn find_nodes(&self, filter: &Bound<'_, PyDict>) -> PyResult<Vec<u64>> {
            let core = super::capabilities::capability_filter_from_py(filter)?;
            Ok(self.node.find_nodes_by_filter(&core))
        }

        /// Shutdown the underlying mesh node. Idempotent. Returns
        /// an awaitable.
        fn shutdown<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
            let node = self.node.clone();
            pyo3_async_runtimes::tokio::future_into_py(py, async move {
                node.shutdown()
                    .await
                    .map_err(|e| PyRuntimeError::new_err(format!("shutdown: {}", e)))
            })
        }
    }
}

/// Net Python module.
#[pymodule]
fn _net(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Initialize the tokio↔asyncio bridge once per process. Every
    // `Async*` class landing in waves T1+ spawns Python awaitables
    // via `pyo3_async_runtimes::tokio::future_into_py`, which
    // requires the bridge to be initialized first. Sync bindings
    // (`Net`, `MeshRpc`, etc.) keep their per-instance runtimes
    // for now; T1+ slices may migrate to share the bridge runtime.
    async_bridge::init().map_err(|e| {
        pyo3::exceptions::PyRuntimeError::new_err(format!("async bridge init: {e}"))
    })?;
    m.add_class::<Net>()?;
    m.add_class::<IngestResult>()?;
    m.add_class::<StoredEvent>()?;
    m.add_class::<PollResponse>()?;
    m.add_class::<Stats>()?;
    m.add_class::<NetKeypair>()?;
    #[cfg(feature = "redis")]
    m.add_class::<redis_dedup::PyRedisStreamDedup>()?;
    #[cfg(feature = "net")]
    m.add_function(wrap_pyfunction!(generate_net_keypair, m)?)?;
    #[cfg(feature = "net")]
    m.add_class::<mesh_bindings::NetMesh>()?;
    #[cfg(feature = "net")]
    m.add_class::<mesh_bindings::NetStream>()?;
    #[cfg(feature = "net")]
    m.add_class::<mesh_bindings::NetStreamStats>()?;
    #[cfg(feature = "net")]
    m.add_class::<mesh_bindings::AsyncNetMesh>()?;
    #[cfg(feature = "net")]
    m.add_class::<mesh_bindings::AsyncNetStream>()?;
    #[cfg(feature = "net")]
    m.add("BackpressureError", m.py().get_type::<BackpressureError>())?;
    #[cfg(feature = "net")]
    m.add("NotConnectedError", m.py().get_type::<NotConnectedError>())?;
    #[cfg(feature = "net")]
    m.add("ChannelError", m.py().get_type::<ChannelError>())?;
    #[cfg(feature = "net")]
    m.add("ChannelAuthError", m.py().get_type::<ChannelAuthError>())?;
    #[cfg(feature = "consent")]
    {
        m.add_class::<consent::PyCapabilityId>()?;
        m.add_class::<consent::PyConsentPolicy>()?;
        m.add_class::<consent::PyPinStore>()?;
        m.add_class::<consent::PyAsyncPinStore>()?;
        m.add_class::<consent::PyAsyncPinWatcher>()?;
        m.add_class::<consent::PyPinChange>()?;
        m.add_function(wrap_pyfunction!(consent::credential_requires_consent, m)?)?;
        m.add_function(wrap_pyfunction!(consent::default_pin_store_path, m)?)?;
        m.add("PinsError", m.py().get_type::<consent::PinsError>())?;
    }
    #[cfg(feature = "mcp")]
    {
        m.add_function(wrap_pyfunction!(mcp_helpers::classify_mcp_server, m)?)?;
        m.add_function(wrap_pyfunction!(mcp_helpers::lower_mcp_tool, m)?)?;
    }
    #[cfg(all(feature = "net", feature = "mcp"))]
    {
        m.add_class::<capability_gateway::PyCapabilityGateway>()?;
        m.add_class::<capability_gateway::PyAsyncCapabilityGateway>()?;
    }
    #[cfg(feature = "payments-http")]
    {
        m.add_class::<payment_http::PyPaymentHttpClient>()?;
        m.add_class::<payment_http::PyAsyncPaymentHttpClient>()?;
    }
    #[cfg(feature = "payments")]
    {
        m.add_function(wrap_pyfunction!(payment_provider::build_pricing_terms, m)?)?;
        #[cfg(feature = "publish")]
        m.add_class::<payment_provider::PyPaymentProvider>()?;
    }
    #[cfg(feature = "net")]
    {
        m.add_class::<identity::Identity>()?;
        m.add_function(wrap_pyfunction!(identity::parse_token, m)?)?;
        m.add_function(wrap_pyfunction!(identity::verify_token, m)?)?;
        m.add_function(wrap_pyfunction!(identity::token_is_expired, m)?)?;
        m.add_function(wrap_pyfunction!(identity::delegate_token, m)?)?;
        m.add_function(wrap_pyfunction!(identity::channel_hash, m)?)?;
        m.add_function(wrap_pyfunction!(capabilities::normalize_gpu_vendor, m)?)?;
        m.add(
            "IdentityError",
            m.py().get_type::<identity::IdentityError>(),
        )?;
        m.add("TokenError", m.py().get_type::<identity::TokenError>())?;
    }
    #[cfg(feature = "delegation")]
    {
        m.add_class::<delegation::PyDelegationChain>()?;
        m.add_class::<delegation::PyRevocationRegistry>()?;
        m.add_function(wrap_pyfunction!(delegation::derive_child_identity, m)?)?;
        m.add_function(wrap_pyfunction!(
            delegation::default_revocation_store_path,
            m
        )?)?;
        m.add(
            "GATEWAY_DELEGATION_CHANNEL",
            delegation::GATEWAY_DELEGATION_CHANNEL,
        )?;
        // Device enrollment (V2 Phase 1).
        m.add_class::<enrollment::PyInviteToken>()?;
        m.add_class::<enrollment::PyJoinRequest>()?;
        m.add_class::<enrollment::PyJoinOutcome>()?;
        m.add_class::<enrollment::PyDeviceRecord>()?;
        m.add_class::<enrollment::PyOperatorEnrollment>()?;
        m.add_class::<enrollment::PyEnrollmentServeHandle>()?;
        m.add_class::<enrollment::PyDeviceEnrollment>()?;
        m.add_function(wrap_pyfunction!(enrollment::fingerprint, m)?)?;
    }
    #[cfg(feature = "publish")]
    {
        // Local tool publication (V2 Phase 2, Slice B).
        m.add_class::<publish::PyLocalPublicationHandle>()?;
    }
    #[cfg(feature = "a2a")]
    {
        // Agent-to-agent task handoff (V2 Phase 3).
        m.add_class::<a2a::PyA2aServeHandle>()?;
    }
    #[cfg(feature = "cortex")]
    {
        m.add_class::<cortex::PyRedex>()?;
        m.add_class::<cortex::PyRedexFile>()?;
        m.add_class::<cortex::PyRedexTailIter>()?;
        m.add_class::<cortex::PyAsyncRedexFile>()?;
        m.add_class::<cortex::PyAsyncRedexTailIter>()?;
        m.add_class::<cortex::PyRedexEvent>()?;
        m.add_class::<cortex::PyWriteToken>()?;
        m.add_class::<cortex::PyTask>()?;
        m.add_class::<cortex::PyTasksAdapter>()?;
        m.add_class::<cortex::PyTaskWatchIter>()?;
        m.add_class::<cortex::PyAsyncTasksAdapter>()?;
        m.add_class::<cortex::PyAsyncTaskWatchIter>()?;
        m.add_class::<cortex::PyWorkflowAdapter>()?;
        m.add_class::<cortex::PyWorkflowTaskState>()?;
        m.add_class::<cortex::PyWorkflowStatusCounts>()?;
        m.add_class::<cortex::PyShardGroup>()?;
        m.add_class::<cortex::PyJoinResult>()?;
        m.add_class::<cortex::PyTriggerEngine>()?;
        m.add_class::<cortex::PyTriggerAction>()?;
        m.add_class::<cortex::PyMemory>()?;
        m.add_class::<cortex::PyMemoriesAdapter>()?;
        m.add_class::<cortex::PyMemoryWatchIter>()?;
        m.add_class::<cortex::PyAsyncMemoriesAdapter>()?;
        m.add_class::<cortex::PyAsyncMemoryWatchIter>()?;
        #[cfg(feature = "tool")]
        m.add_class::<cortex::PyAsyncToolWatchIter>()?;
        m.add_class::<cortex::PyNetDb>()?;
        m.add("CortexError", m.py().get_type::<cortex::CortexError>())?;
        m.add("NetDbError", m.py().get_type::<cortex::NetDbError>())?;
        m.add("RedexError", m.py().get_type::<cortex::RedexError>())?;
        // nRPC surface (B3 raw-bytes phase). Typed wrappers + retry
        // / hedge / breaker land in a follow-up phase as a Python
        // wrapper module on top of these classes.
        m.add_class::<mesh_rpc::PyMeshRpc>()?;
        m.add_class::<mesh_rpc::PyAsyncMeshRpc>()?;
        m.add_class::<mesh_rpc::PyAsyncRpcStream>()?;
        m.add_class::<mesh_rpc::PyAsyncClientStreamCall>()?;
        m.add_class::<mesh_rpc::PyAsyncDuplexCall>()?;
        m.add_class::<mesh_rpc::PyAsyncDuplexSink>()?;
        m.add_class::<mesh_rpc::PyAsyncDuplexStream>()?;
        m.add_class::<mesh_rpc::PyServeHandle>()?;
        m.add_class::<mesh_rpc::PyRpcStream>()?;
        m.add_class::<mesh_rpc::PyCancellable>()?;
        m.add_class::<mesh_rpc::PyRpcCallEvent>()?;
        m.add_class::<mesh_rpc::PyServiceMetrics>()?;
        m.add_class::<mesh_rpc::PyRpcMetricsSnapshot>()?;
        m.add("RpcError", m.py().get_type::<mesh_rpc::RpcError>())?;
        m.add(
            "RpcNoRouteError",
            m.py().get_type::<mesh_rpc::RpcNoRouteError>(),
        )?;
        m.add(
            "RpcTimeoutError",
            m.py().get_type::<mesh_rpc::RpcTimeoutError>(),
        )?;
        m.add(
            "RpcServerError",
            m.py().get_type::<mesh_rpc::RpcServerError>(),
        )?;
        m.add(
            "RpcTransportError",
            m.py().get_type::<mesh_rpc::RpcTransportError>(),
        )?;
        m.add(
            "RpcCodecError",
            m.py().get_type::<mesh_rpc::RpcCodecError>(),
        )?;
        m.add("RpcAppError", m.py().get_type::<mesh_rpc::RpcAppError>())?;
        m.add(
            "RpcCancelledError",
            m.py().get_type::<mesh_rpc::RpcCancelledError>(),
        )?;
        m.add(
            "RpcCapabilityDeniedError",
            m.py().get_type::<mesh_rpc::RpcCapabilityDeniedError>(),
        )?;
    }
    #[cfg(feature = "dataforts")]
    {
        m.add_class::<blob::PyBlobRef>()?;
        m.add_class::<blob::PyBandwidthClass>()?;
        m.add_class::<blob::PyChunkingStrategy>()?;
        m.add_class::<blob::PyEncoding>()?;
        m.add_class::<blob::PyMeshBlobAdapter>()?;
        m.add_class::<blob::PyAsyncMeshBlobAdapter>()?;
        m.add(
            "DATAFORTS_BLOB_TREE_SUPPORTED",
            blob::DATAFORTS_BLOB_TREE_SUPPORTED,
        )?;
        m.add(
            "DATAFORTS_BLOB_CDC_SUPPORTED",
            blob::DATAFORTS_BLOB_CDC_SUPPORTED,
        )?;
        m.add(
            "DATAFORTS_BLOB_ERASURE_SUPPORTED",
            blob::DATAFORTS_BLOB_ERASURE_SUPPORTED,
        )?;
        m.add(
            "DATAFORTS_BLOB_BANDWIDTH_CLASS_SUPPORTED",
            blob::DATAFORTS_BLOB_BANDWIDTH_CLASS_SUPPORTED,
        )?;
        m.add_function(wrap_pyfunction!(blob::register_filesystem_blob_adapter, m)?)?;
        m.add_function(wrap_pyfunction!(blob::register_blob_adapter, m)?)?;
        m.add_function(wrap_pyfunction!(blob::unregister_blob_adapter, m)?)?;
        m.add_function(wrap_pyfunction!(blob::blob_adapter_registered, m)?)?;
        m.add_function(wrap_pyfunction!(blob::blob_adapter_ids, m)?)?;
        m.add_function(wrap_pyfunction!(blob::blob_publish, m)?)?;
        m.add_function(wrap_pyfunction!(blob::blob_resolve, m)?)?;
        m.add_function(wrap_pyfunction!(blob::async_blob_publish, m)?)?;
        m.add_function(wrap_pyfunction!(blob::async_blob_resolve, m)?)?;
        m.add("BlobError", m.py().get_type::<blob::BlobError>())?;

        // Transport surface (blob + directory transfer over the
        // fairscheduler stream transport — Transport SDK plan T-D).
        transport::register(m)?;

        // Register an atexit hook so the global blob-adapter
        // registry is drained while the interpreter is still
        // alive. Python-implemented adapters hold a Py<PyAny>;
        // dropping one after interpreter finalization aborts the
        // process via PyO3's safety guard. Draining here on
        // shutdown frees those refs while the GIL is still
        // acquirable.
        let py = m.py();
        let drain_fn = wrap_pyfunction!(blob::_drain_blob_adapters, m)?;
        let atexit = py.import("atexit")?;
        atexit.call_method1("register", (drain_fn,))?;
    }
    #[cfg(feature = "compute")]
    {
        m.add_class::<compute::PyDaemonRuntime>()?;
        m.add_class::<compute::PyDaemonHandle>()?;
        m.add_class::<compute::PyCausalEvent>()?;
        m.add_class::<compute::PyMigrationHandle>()?;
        m.add_class::<compute::PyMigrationPhasesIter>()?;
        m.add_class::<compute::PyAsyncDaemonRuntime>()?;
        m.add_class::<compute::PyAsyncMigrationHandle>()?;
        m.add("DaemonError", m.py().get_type::<compute::DaemonError>())?;
        m.add(
            "MigrationError",
            m.py().get_type::<compute::MigrationError>(),
        )?;
    }
    #[cfg(feature = "groups")]
    {
        m.add_class::<groups::PyReplicaGroup>()?;
        m.add_class::<groups::PyForkGroup>()?;
        m.add_class::<groups::PyStandbyGroup>()?;
        m.add("GroupError", m.py().get_type::<groups::GroupError>())?;
    }
    #[cfg(feature = "meshdb")]
    {
        m.add_class::<meshdb::PyMeshQuery>()?;
        m.add_class::<meshdb::PyResultRow>()?;
        m.add_class::<meshdb::PyExecuteOptions>()?;
        m.add_class::<meshdb::PyCachePolicy>()?;
        m.add_class::<meshdb::PyInMemoryChainReader>()?;
        m.add_class::<meshdb::PyMeshQueryRunner>()?;
        m.add_class::<meshdb::PyAsyncMeshQueryRunner>()?;
        m.add_class::<meshdb::PyAggregateResult>()?;
        m.add_class::<meshdb::PyGroupKey>()?;
        m.add_class::<meshdb::PyJoinedRow>()?;
        m.add_class::<meshdb::PyWindowBoundary>()?;
        m.add_class::<meshdb::PyLineageEntry>()?;
        m.add_class::<meshdb::PyPredicate>()?;
        m.add_class::<meshdb::PyQueryBuilder>()?;
        m.add("MeshDbError", m.py().get_type::<meshdb::MeshDbError>())?;
    }
    #[cfg(feature = "meshos")]
    {
        m.add_class::<meshos::PyMeshOsDaemonSdk>()?;
        m.add_class::<meshos::PyMeshOsDaemonHandle>()?;
        m.add_class::<meshos::PyAsyncMeshOsDaemonSdk>()?;
        m.add_class::<meshos::PyAsyncMeshOsDaemonHandle>()?;
        m.add(
            "MeshOsSdkError",
            m.py().get_type::<meshos::MeshOsSdkError>(),
        )?;
    }
    #[cfg(feature = "aggregator")]
    {
        m.add_class::<aggregator::PyRegistryClient>()?;
        m.add_class::<aggregator::PyFoldQueryClient>()?;
        m.add_class::<aggregator::PyAsyncRegistryClient>()?;
        m.add_class::<aggregator::PyAsyncFoldQueryClient>()?;
        m.add(
            "RegistryClientError",
            m.py().get_type::<aggregator::RegistryClientError>(),
        )?;
        m.add(
            "UnknownTemplate",
            m.py().get_type::<aggregator::UnknownTemplate>(),
        )?;
        m.add(
            "DuplicateGroupName",
            m.py().get_type::<aggregator::DuplicateGroupName>(),
        )?;
        m.add(
            "SpawnRejected",
            m.py().get_type::<aggregator::SpawnRejected>(),
        )?;
        m.add(
            "SpawnNotSupported",
            m.py().get_type::<aggregator::SpawnNotSupported>(),
        )?;
        m.add(
            "FoldQueryClientError",
            m.py().get_type::<aggregator::FoldQueryClientError>(),
        )?;
        m.add(
            "UnknownFoldKind",
            m.py().get_type::<aggregator::UnknownFoldKind>(),
        )?;
    }
    #[cfg(feature = "deck")]
    {
        m.add_class::<deck::PyDeckClient>()?;
        m.add_class::<deck::PyAdminCommands>()?;
        m.add_class::<deck::PySnapshotStream>()?;
        m.add_class::<deck::PyStatusSummaryStream>()?;
        m.add_class::<deck::PyAsyncSnapshotStream>()?;
        m.add_class::<deck::PyAsyncStatusSummaryStream>()?;
        m.add_class::<deck::PyAsyncDeckClient>()?;
        m.add_class::<deck::PyAsyncAdminCommands>()?;
        m.add_class::<deck::PyAsyncIceCommands>()?;
        m.add_class::<deck::PyAsyncIceProposal>()?;
        m.add_class::<deck::PyAsyncSimulatedIceProposal>()?;
        m.add_class::<deck::PyOperatorIdentity>()?;
        m.add_class::<deck::PyLogStream>()?;
        m.add_class::<deck::PyFailureStream>()?;
        m.add_class::<deck::PyAuditQuery>()?;
        m.add_class::<deck::PyAuditStream>()?;
        m.add_class::<deck::PyIceCommands>()?;
        m.add_class::<deck::PyIceProposal>()?;
        m.add_class::<deck::PySimulatedIceProposal>()?;
        m.add_class::<deck::PyOperatorRegistry>()?;
        m.add_class::<deck::PyAdminVerifier>()?;
        m.add("DeckSdkError", m.py().get_type::<deck::DeckSdkError>())?;
    }
    Ok(())
}
