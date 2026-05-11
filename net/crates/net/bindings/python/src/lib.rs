//! Python bindings for Net event bus.
//!
//! Provides high-performance event ingestion and consumption for Python.

#[cfg(feature = "cortex")]
mod cortex;
#[cfg(feature = "dataforts")]
mod blob;
// Identity / capabilities / subnets ride the `net` feature as a
// single security unit — they share `adapter::net`'s subprotocol
// dispatch and are operationally inseparable.
#[cfg(feature = "net")]
mod capabilities;
#[cfg(feature = "compute")]
mod compute;
#[cfg(feature = "groups")]
mod groups;
#[cfg(feature = "net")]
mod identity;
// nRPC binding (B3: raw-bytes serve_rpc / call / call_streaming).
// Reuses the cortex feature gate because nRPC is part of the
// cortex / netdb feature unit. Sync handler API; async-Python
// handler support lands as a follow-up phase.
#[cfg(feature = "cortex")]
mod mesh_rpc;
#[cfg(feature = "net")]
mod placement;
#[cfg(feature = "redis")]
mod redis_dedup;
#[cfg(feature = "net")]
mod subnets;

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::sync::Arc;
use std::sync::RwLock;
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
        let bus_guard = self
            .bus
            .read()
            .map_err(|e| PyRuntimeError::new_err(format!("Lock error: {}", e)))?;
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
        let bus_guard = self
            .bus
            .read()
            .map_err(|e| PyRuntimeError::new_err(format!("Lock error: {}", e)))?;
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
        let bus_guard = self
            .bus
            .read()
            .map_err(|e| PyRuntimeError::new_err(format!("Lock error: {}", e)))?;
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
        let bus_guard = self
            .bus
            .read()
            .map_err(|e| PyRuntimeError::new_err(format!("Lock error: {}", e)))?;
        let bus = bus_guard
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("EventBus has been shut down"))?;

        Ok(bus.num_shards())
    }

    /// Get ingestion statistics.
    fn stats(&self) -> PyResult<Stats> {
        let bus_guard = self
            .bus
            .read()
            .map_err(|e| PyRuntimeError::new_err(format!("Lock error: {}", e)))?;
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
        let mut bus_guard = self
            .bus
            .write()
            .map_err(|e| PyRuntimeError::new_err(format!("Lock error: {}", e)))?;
        if let Some(bus) = bus_guard.take() {
            self.runtime
                .block_on(bus.shutdown())
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        }
        Ok(())
    }

    fn __repr__(&self) -> String {
        let bus_guard = self.bus.read().ok();
        if bus_guard.map(|g| g.is_some()).unwrap_or(false) {
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

            // Install shared channel-config registry.
            let channel_configs = Arc::new(ChannelConfigRegistry::new());
            node.set_channel_configs(channel_configs.clone());
            // Install a fresh TokenCache — channel auth needs
            // somewhere to stash tokens presented on subscribe.
            // Callers wanting to share a cache across meshes can
            // build one externally; today each `NetMesh` gets its
            // own.
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
        fn start(&self) -> PyResult<()> {
            let node = self.get_node()?;
            // `MeshNode::start` uses `tokio::spawn` internally, so
            // it must run inside a tokio runtime context. Enter
            // our owned runtime for the duration of the call.
            let _guard = self.runtime.enter();
            node.start();
            Ok(())
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
                .block_on(node.send_to_peer(addr, batch))
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
        ///     require_token: v1 only supports `False` — token
        ///         enforcement arrives with the security plan's
        ///         identity surface.
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
        /// bytes (159 bytes) — attach it when the publisher sets
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
            let index = node.capability_index().clone();
            let eid = EntityId::from_bytes([0u8; 32]);
            index.index(CapabilityAnnouncement::new(
                node_id,
                eid,
                1,
                CapabilitySet::new(),
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
            let capability_index = node.capability_index().clone();
            let wrapper =
                super::placement::PyPlacementFilter::new(id.clone(), predicate, capability_index);
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

        /// Cumulative NAT-traversal counters. Returns a dict:
        /// `{"punches_attempted": int, "punches_succeeded": int,
        /// "relay_fallbacks": int}`. Monotonic — counters never
        /// reset. Useful for telemetry on punch success rate
        /// and relay load. Requires the `nat-traversal` build.
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
            d.set_item("relay_fallbacks", snap.relay_fallbacks)?;
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
        #[cfg(any(feature = "compute", feature = "cortex"))]
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
        #[cfg(any(feature = "compute", feature = "cortex"))]
        pub(crate) fn runtime_arc(&self) -> Arc<Runtime> {
            self.runtime.clone()
        }
    }
}

/// Net Python module.
#[pymodule]
fn _net(m: &Bound<'_, PyModule>) -> PyResult<()> {
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
    m.add("BackpressureError", m.py().get_type::<BackpressureError>())?;
    #[cfg(feature = "net")]
    m.add("NotConnectedError", m.py().get_type::<NotConnectedError>())?;
    #[cfg(feature = "net")]
    m.add("ChannelError", m.py().get_type::<ChannelError>())?;
    #[cfg(feature = "net")]
    m.add("ChannelAuthError", m.py().get_type::<ChannelAuthError>())?;
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
    #[cfg(feature = "cortex")]
    {
        m.add_class::<cortex::PyRedex>()?;
        m.add_class::<cortex::PyRedexFile>()?;
        m.add_class::<cortex::PyRedexTailIter>()?;
        m.add_class::<cortex::PyRedexEvent>()?;
        m.add_class::<cortex::PyWriteToken>()?;
        m.add_class::<cortex::PyTask>()?;
        m.add_class::<cortex::PyTasksAdapter>()?;
        m.add_class::<cortex::PyTaskWatchIter>()?;
        m.add_class::<cortex::PyMemory>()?;
        m.add_class::<cortex::PyMemoriesAdapter>()?;
        m.add_class::<cortex::PyMemoryWatchIter>()?;
        m.add_class::<cortex::PyNetDb>()?;
        m.add("CortexError", m.py().get_type::<cortex::CortexError>())?;
        m.add("NetDbError", m.py().get_type::<cortex::NetDbError>())?;
        m.add("RedexError", m.py().get_type::<cortex::RedexError>())?;
        // nRPC surface (B3 raw-bytes phase). Typed wrappers + retry
        // / hedge / breaker land in a follow-up phase as a Python
        // wrapper module on top of these classes.
        m.add_class::<mesh_rpc::PyMeshRpc>()?;
        m.add_class::<mesh_rpc::PyServeHandle>()?;
        m.add_class::<mesh_rpc::PyRpcStream>()?;
        m.add_class::<mesh_rpc::PyCancellable>()?;
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
    }
    #[cfg(feature = "dataforts")]
    {
        m.add_class::<blob::PyBlobRef>()?;
        m.add_function(wrap_pyfunction!(blob::register_filesystem_blob_adapter, m)?)?;
        m.add_function(wrap_pyfunction!(blob::unregister_blob_adapter, m)?)?;
        m.add_function(wrap_pyfunction!(blob::blob_adapter_registered, m)?)?;
        m.add_function(wrap_pyfunction!(blob::blob_adapter_ids, m)?)?;
        m.add_function(wrap_pyfunction!(blob::blob_publish, m)?)?;
        m.add_function(wrap_pyfunction!(blob::blob_resolve, m)?)?;
        m.add("BlobError", m.py().get_type::<blob::BlobError>())?;
    }
    #[cfg(feature = "compute")]
    {
        m.add_class::<compute::PyDaemonRuntime>()?;
        m.add_class::<compute::PyDaemonHandle>()?;
        m.add_class::<compute::PyCausalEvent>()?;
        m.add_class::<compute::PyMigrationHandle>()?;
        m.add_class::<compute::PyMigrationPhasesIter>()?;
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
    Ok(())
}
