//! Context Fabric (CTXT-FABRIC) - Phase 4F
//!
//! Provides distributed context propagation across the Net network:
//! - Request context with trace IDs and spans
//! - Context inheritance and propagation
//! - Distributed baggage (key-value propagation)
//! - Context scopes with automatic cleanup
//! - Sampling and rate limiting for tracing

use dashmap::DashMap;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::NodeId;

/// Generate random bytes using getrandom.
///
/// Aborts on `getrandom` failure rather than panic-unwinding
/// through the FFI boundary. Trace IDs are not directly
/// auth-bearing, but this function is reachable from hot paths
/// called by `extern "C"` FFI consumers (Python / Node / Go
/// bindings) — a `getrandom` failure (kernel rng exhaustion,
/// container-restricted /dev/urandom) that unwound across the C
/// ABI would be undefined behaviour. `process::abort` is
/// `extern "C"`-safe (terminates rather than unwinds) and
/// loss-of-availability is the only safe response when the
/// system can't produce randomness.
///
/// The diagnostic uses a fallible `writeln!` rather than
/// `eprintln!` because the latter panics if the underlying
/// stderr write fails (closed fd, sandboxed process). A panic
/// here would defeat the whole point of the abort path —
/// unwinding across the FFI boundary that we're trying to
/// protect — so we ignore any write error and proceed straight
/// to `abort()`.
fn random_u64() -> u64 {
    let mut bytes = [0u8; 8];
    if let Err(e) = getrandom::fill(&mut bytes) {
        use std::io::Write;
        let _ = writeln!(
            std::io::stderr(),
            "FATAL: behavior::context::random_u64 getrandom failure ({e:?}); \
             aborting to avoid panic across the FFI boundary"
        );
        std::process::abort();
    }
    u64::from_le_bytes(bytes)
}

/// Generate random f64 between 0.0 and 1.0
fn random_f64() -> f64 {
    let r = random_u64();
    (r as f64) / (u64::MAX as f64)
}

/// Simple percent-encode a string for baggage propagation
fn percent_encode(s: &str) -> String {
    let mut result = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(b as char);
            }
            _ => {
                result.push_str(&format!("%{:02X}", b));
            }
        }
    }
    result
}

/// Simple percent-decode a string
fn percent_decode(s: &str) -> Option<String> {
    let mut result = Vec::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if hex.len() != 2 {
                return None;
            }
            let byte = u8::from_str_radix(&hex, 16).ok()?;
            result.push(byte);
        } else {
            result.push(c as u8);
        }
    }

    String::from_utf8(result).ok()
}

/// Unique trace identifier (128-bit for W3C compatibility)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TraceId {
    /// High 64 bits of the 128-bit trace ID
    pub high: u64,
    /// Low 64 bits of the 128-bit trace ID
    pub low: u64,
}

impl TraceId {
    /// Generate a new random trace ID
    pub fn generate() -> Self {
        Self {
            high: random_u64(),
            low: random_u64(),
        }
    }

    /// Create from hex string (32 characters)
    pub fn from_hex(s: &str) -> Option<Self> {
        if s.len() != 32 {
            return None;
        }
        let high = u64::from_str_radix(&s[0..16], 16).ok()?;
        let low = u64::from_str_radix(&s[16..32], 16).ok()?;
        Some(Self { high, low })
    }

    /// Convert to hex string
    pub fn to_hex(&self) -> String {
        format!("{:016x}{:016x}", self.high, self.low)
    }
}

impl Default for TraceId {
    fn default() -> Self {
        Self::generate()
    }
}

/// Unique span identifier (64-bit)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct SpanId(pub u64);

impl SpanId {
    /// Generate a new random span ID
    pub fn generate() -> Self {
        Self(random_u64())
    }

    /// Parse a span ID from a 16-character hex string
    pub fn from_hex(s: &str) -> Option<Self> {
        u64::from_str_radix(s, 16).ok().map(Self)
    }

    /// Convert to hex string
    pub fn to_hex(&self) -> String {
        format!("{:016x}", self.0)
    }
}

/// Trace flags (W3C compatible)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceFlags(pub u8);

impl TraceFlags {
    /// Bit flag indicating the trace is sampled
    pub const SAMPLED: u8 = 0x01;

    /// Create flags with the sampled bit set
    pub fn sampled() -> Self {
        Self(Self::SAMPLED)
    }

    /// Create flags with no bits set (not sampled)
    pub fn not_sampled() -> Self {
        Self(0)
    }

    /// Returns true if the sampled flag is set
    pub fn is_sampled(&self) -> bool {
        self.0 & Self::SAMPLED != 0
    }
}

impl Default for TraceFlags {
    fn default() -> Self {
        Self::sampled()
    }
}

/// Span kind (role in the trace)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SpanKind {
    /// Internal operation
    #[default]
    Internal,
    /// Server handling a request
    Server,
    /// Client making a request
    Client,
    /// Producer sending a message
    Producer,
    /// Consumer receiving a message
    Consumer,
}

/// Span status
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SpanStatus {
    /// Status has not been set
    #[default]
    Unset,
    /// Span completed successfully
    Ok,
    /// Span completed with an error
    Error {
        /// Human-readable error description
        message: String,
    },
}

/// A span represents a unit of work in a trace
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Span {
    /// Unique span ID
    pub span_id: SpanId,
    /// Parent span ID (None for root spans)
    pub parent_span_id: Option<SpanId>,
    /// Trace this span belongs to
    pub trace_id: TraceId,
    /// Human-readable name
    pub name: String,
    /// Kind of span
    pub kind: SpanKind,
    /// Start timestamp (microseconds since Unix epoch)
    pub start_time_us: u64,
    /// End timestamp (microseconds since Unix epoch)
    pub end_time_us: Option<u64>,
    /// Span attributes
    pub attributes: HashMap<String, AttributeValue>,
    /// Status
    pub status: SpanStatus,
    /// Events that occurred during the span
    pub events: Vec<SpanEvent>,
    /// Links to other spans
    pub links: Vec<SpanLink>,
    /// Node that created this span
    pub node_id: NodeId,
}

impl Span {
    /// Create a new root span within the given trace
    pub fn new(trace_id: TraceId, name: impl Into<String>, node_id: NodeId) -> Self {
        Self {
            span_id: SpanId::generate(),
            parent_span_id: None,
            trace_id,
            name: name.into(),
            kind: SpanKind::Internal,
            start_time_us: now_micros(),
            end_time_us: None,
            attributes: HashMap::new(),
            status: SpanStatus::Unset,
            events: Vec::new(),
            links: Vec::new(),
            node_id,
        }
    }

    /// Set the parent span ID on this span
    pub fn with_parent(mut self, parent: SpanId) -> Self {
        self.parent_span_id = Some(parent);
        self
    }

    /// Set the kind of this span
    pub fn with_kind(mut self, kind: SpanKind) -> Self {
        self.kind = kind;
        self
    }

    /// Insert a key-value attribute on this span
    pub fn set_attribute(&mut self, key: impl Into<String>, value: impl Into<AttributeValue>) {
        self.attributes.insert(key.into(), value.into());
    }

    /// Record a named event on this span at the current time
    pub fn add_event(&mut self, name: impl Into<String>) {
        self.events.push(SpanEvent {
            name: name.into(),
            timestamp_us: now_micros(),
            attributes: HashMap::new(),
        });
    }

    /// Record a named event with additional attributes on this span
    pub fn add_event_with_attributes(
        &mut self,
        name: impl Into<String>,
        attributes: HashMap<String, AttributeValue>,
    ) {
        self.events.push(SpanEvent {
            name: name.into(),
            timestamp_us: now_micros(),
            attributes,
        });
    }

    /// Add a causal link to another span in a different trace
    pub fn add_link(&mut self, trace_id: TraceId, span_id: SpanId) {
        self.links.push(SpanLink {
            trace_id,
            span_id,
            attributes: HashMap::new(),
        });
    }

    /// Mark this span as successfully completed
    pub fn set_ok(&mut self) {
        self.status = SpanStatus::Ok;
    }

    /// Mark this span as failed with the given error message
    pub fn set_error(&mut self, message: impl Into<String>) {
        self.status = SpanStatus::Error {
            message: message.into(),
        };
    }

    /// Record the end timestamp if not already set
    pub fn end(&mut self) {
        if self.end_time_us.is_none() {
            self.end_time_us = Some(now_micros());
        }
    }

    /// Return the elapsed duration in microseconds, if the span has ended
    pub fn duration_us(&self) -> Option<u64> {
        self.end_time_us
            .map(|end| end.saturating_sub(self.start_time_us))
    }
}

/// Attribute value types
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AttributeValue {
    /// A UTF-8 string value
    String(String),
    /// A signed 64-bit integer value
    Int(i64),
    /// A 64-bit floating-point value
    Float(f64),
    /// A boolean value
    Bool(bool),
    /// An array of UTF-8 string values
    StringArray(Vec<String>),
    /// An array of signed 64-bit integer values
    IntArray(Vec<i64>),
    /// An array of 64-bit floating-point values
    FloatArray(Vec<f64>),
    /// An array of boolean values
    BoolArray(Vec<bool>),
}

impl From<String> for AttributeValue {
    fn from(s: String) -> Self {
        Self::String(s)
    }
}

impl From<&str> for AttributeValue {
    fn from(s: &str) -> Self {
        Self::String(s.to_string())
    }
}

impl From<i64> for AttributeValue {
    fn from(n: i64) -> Self {
        Self::Int(n)
    }
}

impl From<i32> for AttributeValue {
    fn from(n: i32) -> Self {
        Self::Int(n as i64)
    }
}

impl From<f64> for AttributeValue {
    fn from(n: f64) -> Self {
        Self::Float(n)
    }
}

impl From<bool> for AttributeValue {
    fn from(b: bool) -> Self {
        Self::Bool(b)
    }
}

/// An event that occurred during a span
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpanEvent {
    /// Human-readable event name
    pub name: String,
    /// Timestamp when the event occurred (microseconds since Unix epoch)
    pub timestamp_us: u64,
    /// Additional attributes describing the event
    pub attributes: HashMap<String, AttributeValue>,
}

/// A link to another span (e.g., for batched operations)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpanLink {
    /// Trace ID of the linked span
    pub trace_id: TraceId,
    /// Span ID of the linked span
    pub span_id: SpanId,
    /// Optional attributes describing the relationship
    pub attributes: HashMap<String, AttributeValue>,
}

/// Baggage is key-value pairs that propagate across the network
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Baggage {
    items: HashMap<String, BaggageItem>,
}

/// A single key-value entry carried in distributed baggage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaggageItem {
    /// The propagated string value for this baggage entry
    pub value: String,
    /// Optional properties metadata associated with this entry
    pub metadata: Option<String>,
}

impl Baggage {
    /// Create a new empty baggage container
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a baggage entry by key
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.items.insert(
            key.into(),
            BaggageItem {
                value: value.into(),
                metadata: None,
            },
        );
    }

    /// Insert or replace a baggage entry with associated metadata properties
    pub fn set_with_metadata(
        &mut self,
        key: impl Into<String>,
        value: impl Into<String>,
        metadata: impl Into<String>,
    ) {
        self.items.insert(
            key.into(),
            BaggageItem {
                value: value.into(),
                metadata: Some(metadata.into()),
            },
        );
    }

    /// Look up a baggage value by key
    pub fn get(&self, key: &str) -> Option<&str> {
        self.items.get(key).map(|item| item.value.as_str())
    }

    /// Look up a baggage value and its optional metadata by key
    pub fn get_with_metadata(&self, key: &str) -> Option<(&str, Option<&str>)> {
        self.items
            .get(key)
            .map(|item| (item.value.as_str(), item.metadata.as_deref()))
    }

    /// Remove a baggage entry by key and return its value
    pub fn remove(&mut self, key: &str) -> Option<String> {
        self.items.remove(key).map(|item| item.value)
    }

    /// Iterate over all baggage entries as (key, value) pairs
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.items
            .iter()
            .map(|(k, v)| (k.as_str(), v.value.as_str()))
    }

    /// Return the number of baggage entries
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Return true if there are no baggage entries
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Merge another baggage into this one (other takes precedence)
    pub fn merge(&mut self, other: &Baggage) {
        for (key, item) in &other.items {
            self.items.insert(key.clone(), item.clone());
        }
    }
}

/// Request context that propagates across the network
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Context {
    /// Trace ID for distributed tracing
    pub trace_id: TraceId,
    /// Current span ID
    pub span_id: SpanId,
    /// Parent span ID (for hierarchy)
    pub parent_span_id: Option<SpanId>,
    /// Trace flags
    pub trace_flags: TraceFlags,
    /// Trace state (vendor-specific key-value pairs)
    pub trace_state: HashMap<String, String>,
    /// Baggage that propagates with the request
    pub baggage: Baggage,
    /// Deadline for this request (microseconds since Unix epoch)
    pub deadline_us: Option<u64>,
    /// Originating node
    pub origin_node: NodeId,
    /// Request ID (application-level)
    pub request_id: Option<String>,
    /// Correlation ID (for related requests)
    pub correlation_id: Option<String>,
    /// Hop count (increases with each network hop)
    pub hop_count: u32,
    /// Maximum allowed hops
    pub max_hops: Option<u32>,
}

impl Context {
    /// Create a new root context originating from the given node
    pub fn new(origin_node: NodeId) -> Self {
        Self {
            trace_id: TraceId::generate(),
            span_id: SpanId::generate(),
            parent_span_id: None,
            trace_flags: TraceFlags::sampled(),
            trace_state: HashMap::new(),
            baggage: Baggage::new(),
            deadline_us: None,
            origin_node,
            request_id: None,
            correlation_id: None,
            hop_count: 0,
            max_hops: None,
        }
    }

    /// Create a child context for a new span
    pub fn child(&self, new_span_name: &str) -> Self {
        let _ = new_span_name; // Used for logging/tracing, not stored in context
        Self {
            trace_id: self.trace_id,
            span_id: SpanId::generate(),
            parent_span_id: Some(self.span_id),
            trace_flags: self.trace_flags,
            trace_state: self.trace_state.clone(),
            baggage: self.baggage.clone(),
            deadline_us: self.deadline_us,
            origin_node: self.origin_node,
            request_id: self.request_id.clone(),
            correlation_id: self.correlation_id.clone(),
            hop_count: self.hop_count,
            max_hops: self.max_hops,
        }
    }

    /// Create a context for sending to another node
    pub fn for_remote(&self) -> Self {
        Self {
            trace_id: self.trace_id,
            span_id: SpanId::generate(),
            parent_span_id: Some(self.span_id),
            trace_flags: self.trace_flags,
            trace_state: self.trace_state.clone(),
            baggage: self.baggage.clone(),
            deadline_us: self.deadline_us,
            origin_node: self.origin_node,
            request_id: self.request_id.clone(),
            correlation_id: self.correlation_id.clone(),
            hop_count: self.hop_count.saturating_add(1),
            max_hops: self.max_hops,
        }
    }

    /// Set a timeout from now
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.deadline_us = Some(now_micros() + timeout.as_micros() as u64);
        self
    }

    /// Set an absolute deadline
    pub fn with_deadline(mut self, deadline_us: u64) -> Self {
        self.deadline_us = Some(deadline_us);
        self
    }

    /// Check if the context has expired
    pub fn is_expired(&self) -> bool {
        self.deadline_us
            .map(|deadline| now_micros() > deadline)
            .unwrap_or(false)
    }

    /// Get remaining time until deadline
    pub fn remaining(&self) -> Option<Duration> {
        self.deadline_us.and_then(|deadline| {
            let now = now_micros();
            if now >= deadline {
                None
            } else {
                Some(Duration::from_micros(deadline - now))
            }
        })
    }

    /// Check if we've exceeded max hops
    pub fn exceeded_hops(&self) -> bool {
        self.max_hops
            .map(|max| self.hop_count >= max)
            .unwrap_or(false)
    }

    /// Set max hops
    pub fn with_max_hops(mut self, max: u32) -> Self {
        self.max_hops = Some(max);
        self
    }

    /// Set request ID
    pub fn with_request_id(mut self, id: impl Into<String>) -> Self {
        self.request_id = Some(id.into());
        self
    }

    /// Set correlation ID
    pub fn with_correlation_id(mut self, id: impl Into<String>) -> Self {
        self.correlation_id = Some(id.into());
        self
    }

    /// Encode to W3C traceparent header format
    pub fn to_traceparent(&self) -> String {
        format!(
            "00-{}-{}-{:02x}",
            self.trace_id.to_hex(),
            self.span_id.to_hex(),
            self.trace_flags.0
        )
    }

    /// Parse from W3C traceparent header
    pub fn from_traceparent(header: &str, origin_node: NodeId) -> Option<Self> {
        let parts: Vec<&str> = header.split('-').collect();
        if parts.len() != 4 || parts[0] != "00" {
            return None;
        }

        let trace_id = TraceId::from_hex(parts[1])?;
        let span_id = SpanId::from_hex(parts[2])?;
        let flags = u8::from_str_radix(parts[3], 16).ok()?;

        Some(Self {
            trace_id,
            span_id: SpanId::generate(),
            parent_span_id: Some(span_id),
            trace_flags: TraceFlags(flags),
            trace_state: HashMap::new(),
            baggage: Baggage::new(),
            deadline_us: None,
            origin_node,
            request_id: None,
            correlation_id: None,
            hop_count: 1,
            max_hops: None,
        })
    }
}

/// Sampling strategy for traces
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SamplingStrategy {
    /// Always sample
    AlwaysOn,
    /// Never sample
    AlwaysOff,
    /// Sample a fixed ratio (0.0 to 1.0)
    Ratio(f64),
    /// Sample based on rate limit (max per second)
    RateLimited {
        /// Maximum number of traces to sample per second
        max_per_second: u32,
    },
    /// Parent-based sampling (inherit from parent)
    ParentBased,
    /// Custom sampler by name
    Custom(String),
}

impl Default for SamplingStrategy {
    fn default() -> Self {
        Self::Ratio(0.1) // 10% default sampling
    }
}

/// Sampler that decides whether to sample a trace
#[derive(Debug)]
pub struct Sampler {
    strategy: SamplingStrategy,
    count: AtomicU64,
    last_reset: Mutex<Instant>,
}

impl Sampler {
    /// Create a new sampler with the given strategy
    pub fn new(strategy: SamplingStrategy) -> Self {
        Self {
            strategy,
            count: AtomicU64::new(0),
            last_reset: Mutex::new(Instant::now()),
        }
    }

    /// Decide whether to sample a new trace, given the parent's sampling decision
    pub fn should_sample(&self, parent_sampled: Option<bool>) -> bool {
        match &self.strategy {
            SamplingStrategy::AlwaysOn => true,
            SamplingStrategy::AlwaysOff => false,
            SamplingStrategy::Ratio(ratio) => random_f64() < *ratio,
            SamplingStrategy::RateLimited { max_per_second } => {
                let mut last_reset = self.last_reset.lock();
                let now = Instant::now();

                // Reset counter every second
                if now.duration_since(*last_reset) >= Duration::from_secs(1) {
                    self.count.store(0, Ordering::Relaxed);
                    *last_reset = now;
                }

                let current = self.count.fetch_add(1, Ordering::Relaxed);
                current < *max_per_second as u64
            }
            SamplingStrategy::ParentBased => parent_sampled.unwrap_or(true),
            SamplingStrategy::Custom(_) => true, // Custom samplers default to true
        }
    }
}

/// Context error types
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextError {
    /// Context has expired
    Expired,
    /// Maximum hops exceeded
    MaxHopsExceeded,
    /// Context not found
    NotFound,
    /// Invalid trace ID
    InvalidTraceId,
    /// Storage capacity exceeded
    CapacityExceeded,
}

impl std::fmt::Display for ContextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Expired => write!(f, "context has expired"),
            Self::MaxHopsExceeded => write!(f, "maximum hops exceeded"),
            Self::NotFound => write!(f, "context not found"),
            Self::InvalidTraceId => write!(f, "invalid trace ID"),
            Self::CapacityExceeded => write!(f, "storage capacity exceeded"),
        }
    }
}

impl std::error::Error for ContextError {}

/// Entry in the context store with metadata
#[derive(Debug)]
struct ContextEntry {
    context: Context,
    created_at: Instant,
    spans: Vec<Span>,
}

/// Statistics for the context store
#[derive(Debug, Clone, Default)]
pub struct ContextStoreStats {
    /// Number of traces currently being tracked
    pub active_traces: u64,
    /// Total number of spans across all active traces
    pub total_spans: u64,
    /// Cumulative count of traces that were sampled
    pub sampled_traces: u64,
    /// Cumulative count of traces dropped due to capacity limits
    pub dropped_traces: u64,
    /// Cumulative count of traces removed due to TTL expiry
    pub expired_traces: u64,
}

/// Store for active contexts and traces
pub struct ContextStore {
    /// Active contexts by trace ID
    contexts: DashMap<TraceId, ContextEntry>,
    /// Maximum number of traces to store
    max_traces: usize,
    /// Maximum spans per trace
    max_spans_per_trace: usize,
    /// TTL for traces
    trace_ttl: Duration,
    /// Sampler
    sampler: Sampler,
    /// Stats
    sampled_count: AtomicU64,
    dropped_count: AtomicU64,
    expired_count: AtomicU64,
    /// Authoritative atomic counter so the "is the store full?"
    /// check can be a CAS-with-cap rather than a
    /// `dashmap.len() >= max` racy probe. Bumped on insert via
    /// `try_reserve_slot` (CAS), decremented on eviction
    /// (`cleanup_expired`, explicit removal). DashMap's own
    /// `len()` is the source of truth for queries; this counter
    /// exists only to gate admission atomically.
    active_count: std::sync::atomic::AtomicUsize,
}

impl ContextStore {
    /// Create a new store with the given capacity limits and TTL
    pub fn new(max_traces: usize, max_spans_per_trace: usize, trace_ttl: Duration) -> Self {
        Self {
            contexts: DashMap::new(),
            max_traces,
            max_spans_per_trace,
            trace_ttl,
            sampler: Sampler::new(SamplingStrategy::default()),
            sampled_count: AtomicU64::new(0),
            dropped_count: AtomicU64::new(0),
            expired_count: AtomicU64::new(0),
            active_count: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Atomically reserve a slot if `active_count < max_traces`.
    ///
    /// Returns an [`Option<SlotReservation<'_>>`]; the `Some` arm
    /// carries an RAII guard whose `Drop` releases the reservation
    /// automatically. The success-path caller MUST invoke
    /// [`SlotReservation::commit`] to keep the slot once the
    /// matching insert lands. Any other exit (early return, error,
    /// panic between reserve and insert) drops the guard, which
    /// undoes the reservation atomically. A `bool` return with a
    /// manual `release_slot` call on every failure path would be
    /// easy to miss and would leak a slot permanently across an
    /// `active_count` underflow guard.
    ///
    /// This is the admission gate. A `dashmap.len() >= max` probe
    /// would lose the race against concurrent inserters.
    fn try_reserve_slot(&self) -> Option<SlotReservation<'_>> {
        use std::sync::atomic::Ordering;
        // Fetch-update CAS loop: only commit if `cur < max`.
        let ok = self
            .active_count
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |cur| {
                if cur < self.max_traces {
                    Some(cur + 1)
                } else {
                    None
                }
            })
            .is_ok();
        if ok {
            Some(SlotReservation { store: self })
        } else {
            None
        }
    }

    /// Release a slot reserved by `try_reserve_slot` — used for the
    /// post-insert duplicate-trace-id detection path in
    /// `continue_context` (where the reservation is committed but
    /// the matching insert turned out to be a no-op overwrite of
    /// an existing entry). Most callers should rely on
    /// [`SlotReservation`]'s automatic Drop release instead.
    fn release_slot(&self) {
        use std::sync::atomic::Ordering;
        self.active_count
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |cur| {
                Some(cur.saturating_sub(1))
            })
            .ok();
    }
}

/// RAII guard returned by [`ContextStore::try_reserve_slot`].
/// The Drop impl decrements `active_count` UNLESS [`Self::commit`]
/// was called first (which `mem::forget`-equivalent the guard, so
/// no decrement runs).
///
/// Pattern:
/// ```ignore
/// let guard = self.try_reserve_slot()?;       // reserve
/// // ... do work that may panic / early-return ...
/// // success path:
/// guard.commit();                              // keep the slot
/// ```
///
/// On any non-commit exit (panic, error, early return) the slot
/// reservation is rolled back automatically — the admission cap
/// stays accurate even when the matching insert never lands.
pub(super) struct SlotReservation<'a> {
    store: &'a ContextStore,
}

impl<'a> SlotReservation<'a> {
    /// Forget the guard so its Drop does NOT release the slot.
    /// Call this only on the success path AFTER the matching
    /// insert has landed.
    fn commit(self) {
        // `mem::forget` skips the Drop impl; the reserved slot
        // stays counted against `active_count`, where it
        // correctly reflects the live entry.
        std::mem::forget(self);
    }
}

impl<'a> Drop for SlotReservation<'a> {
    fn drop(&mut self) {
        // Roll back the reservation. `release_slot` is itself
        // saturating so a double-Drop (which `mem::forget`
        // already prevents structurally) would not underflow.
        self.store.release_slot();
    }
}

impl ContextStore {
    /// Override the default sampling strategy for this store
    pub fn with_sampling(mut self, strategy: SamplingStrategy) -> Self {
        self.sampler = Sampler::new(strategy);
        self
    }

    /// Create a new context and register it
    ///
    /// Capacity admission goes through the atomic
    /// `try_reserve_slot` CAS rather than a `contexts.len() >= max`
    /// probe. Two threads inserting at exactly capacity could
    /// otherwise both observe `len < max` after a
    /// `cleanup_expired` and both insert, growing the map past
    /// `max_traces`. The slot is reserved atomically before the
    /// insert; if the reserve fails after a `cleanup_expired`
    /// retry, we surface `CapacityExceeded`.
    pub fn create_context(&self, origin_node: NodeId) -> Result<Context, ContextError> {
        // Hold the reservation as a RAII guard. Any path out
        // before `guard.commit()` (early return, panic in
        // `Context::new` / `should_sample`, future refactor that
        // adds another fallible step) drops the guard and the
        // slot is released automatically.
        let guard = match self.try_reserve_slot() {
            Some(g) => g,
            None => {
                self.cleanup_expired();
                match self.try_reserve_slot() {
                    Some(g) => g,
                    None => {
                        self.dropped_count.fetch_add(1, Ordering::Relaxed);
                        return Err(ContextError::CapacityExceeded);
                    }
                }
            }
        };

        let ctx = Context::new(origin_node);

        // Check if we should sample this trace
        if !self.sampler.should_sample(None) {
            let mut unsampled = ctx.clone();
            unsampled.trace_flags = TraceFlags::not_sampled();
            // Sampling-skip path: no insert happens — `guard`'s
            // Drop releases the slot. No manual release needed.
            return Ok(unsampled);
        }

        self.sampled_count.fetch_add(1, Ordering::Relaxed);

        self.contexts.insert(
            ctx.trace_id,
            ContextEntry {
                context: ctx.clone(),
                created_at: Instant::now(),
                spans: Vec::new(),
            },
        );

        // Insert succeeded — commit the reservation so its slot
        // stays counted against `active_count`.
        guard.commit();

        Ok(ctx)
    }

    /// Continue a context from a remote node
    pub fn continue_context(&self, ctx: Context) -> Result<Context, ContextError> {
        // Check if expired
        if ctx.is_expired() {
            return Err(ContextError::Expired);
        }

        // Check hop count
        if ctx.exceeded_hops() {
            return Err(ContextError::MaxHopsExceeded);
        }

        // If already tracking this trace, just return
        if self.contexts.contains_key(&ctx.trace_id) {
            return Ok(ctx);
        }

        // RAII reserve. Drop releases on any non-commit exit
        // (sampling-skip, panic, error).
        let guard = match self.try_reserve_slot() {
            Some(g) => g,
            None => {
                self.cleanup_expired();
                match self.try_reserve_slot() {
                    Some(g) => g,
                    None => {
                        self.dropped_count.fetch_add(1, Ordering::Relaxed);
                        return Err(ContextError::CapacityExceeded);
                    }
                }
            }
        };

        // Check sampling (parent-based)
        if !self
            .sampler
            .should_sample(Some(ctx.trace_flags.is_sampled()))
        {
            // Sampling-skip path — `guard` Drop releases the slot.
            return Ok(ctx);
        }

        self.sampled_count.fetch_add(1, Ordering::Relaxed);

        // Two threads racing on the same `trace_id` both pass the
        // `contains_key` check above (TOCTOU) and both reserve a
        // slot via `try_reserve_slot`. `DashMap::insert` overwrites
        // the existing entry and returns the prior value; the
        // losing thread did not actually grow the map, so its
        // reservation is a leak. Commit the guard FIRST (so we
        // own the reservation) then inspect the return: when a
        // prior entry existed, manually release one slot to keep
        // `active_count` in lockstep with the map size.
        let prev = self.contexts.insert(
            ctx.trace_id,
            ContextEntry {
                context: ctx.clone(),
                created_at: Instant::now(),
                spans: Vec::new(),
            },
        );
        guard.commit();
        if prev.is_some() {
            // Insert was an overwrite, not a growth — undo the
            // reservation we just committed.
            self.release_slot();
        }

        Ok(ctx)
    }

    /// Add a span to a trace
    pub fn add_span(&self, span: Span) -> Result<(), ContextError> {
        if let Some(mut entry) = self.contexts.get_mut(&span.trace_id) {
            if entry.spans.len() < self.max_spans_per_trace {
                entry.spans.push(span);
            }
            Ok(())
        } else {
            Err(ContextError::NotFound)
        }
    }

    /// Get a context by trace ID
    pub fn get_context(&self, trace_id: &TraceId) -> Option<Context> {
        self.contexts
            .get(trace_id)
            .map(|entry| entry.context.clone())
    }

    /// Get all spans for a trace
    pub fn get_spans(&self, trace_id: &TraceId) -> Vec<Span> {
        self.contexts
            .get(trace_id)
            .map(|entry| entry.spans.clone())
            .unwrap_or_default()
    }

    /// Complete a trace and return all spans
    ///
    /// Also releases the `active_count` slot so the
    /// `try_reserve_slot` admission gate can re-admit.
    pub fn complete_trace(&self, trace_id: &TraceId) -> Option<(Context, Vec<Span>)> {
        let removed = self
            .contexts
            .remove(trace_id)
            .map(|(_, entry)| (entry.context, entry.spans));
        if removed.is_some() {
            self.release_slot();
        }
        removed
    }

    /// Cleanup expired traces
    ///
    /// Every successful removal also releases an `active_count`
    /// slot so the `try_reserve_slot` admission gate can re-admit
    /// work as soon as expired entries are reclaimed.
    pub fn cleanup_expired(&self) {
        let now = Instant::now();
        let mut expired = Vec::new();

        for entry in self.contexts.iter() {
            if now.duration_since(entry.created_at) > self.trace_ttl {
                expired.push(*entry.key());
            }
        }

        for trace_id in expired {
            if self.contexts.remove(&trace_id).is_some() {
                self.expired_count.fetch_add(1, Ordering::Relaxed);
                self.release_slot();
            }
        }
    }

    /// Get statistics
    pub fn stats(&self) -> ContextStoreStats {
        let mut total_spans = 0;
        for entry in self.contexts.iter() {
            total_spans += entry.spans.len() as u64;
        }

        ContextStoreStats {
            active_traces: self.contexts.len() as u64,
            total_spans,
            sampled_traces: self.sampled_count.load(Ordering::Relaxed),
            dropped_traces: self.dropped_count.load(Ordering::Relaxed),
            expired_traces: self.expired_count.load(Ordering::Relaxed),
        }
    }
}

/// Context scope for automatic span management
pub struct ContextScope<'a> {
    store: &'a ContextStore,
    span: Span,
    finished: bool,
}

impl<'a> ContextScope<'a> {
    /// Create a new scope that automatically records a span on drop
    pub fn new(store: &'a ContextStore, ctx: &Context, name: &str, node_id: NodeId) -> Self {
        let mut span = Span::new(ctx.trace_id, name, node_id);
        if let Some(parent) = ctx.parent_span_id {
            span = span.with_parent(parent);
        }

        Self {
            store,
            span,
            finished: false,
        }
    }

    /// Set the kind of the underlying span
    pub fn with_kind(mut self, kind: SpanKind) -> Self {
        self.span.kind = kind;
        self
    }

    /// Set an attribute on the underlying span
    pub fn set_attribute(&mut self, key: impl Into<String>, value: impl Into<AttributeValue>) {
        self.span.set_attribute(key, value);
    }

    /// Record a named event on the underlying span
    pub fn add_event(&mut self, name: impl Into<String>) {
        self.span.add_event(name);
    }

    /// Mark the underlying span as successfully completed
    pub fn set_ok(&mut self) {
        self.span.set_ok();
    }

    /// Mark the underlying span as failed with the given message
    pub fn set_error(&mut self, message: impl Into<String>) {
        self.span.set_error(message);
    }

    /// Explicitly finish the scope and submit the span to the store
    pub fn finish(mut self) {
        self.span.end();
        let _ = self.store.add_span(self.span.clone());
        self.finished = true;
    }

    /// Return a reference to the underlying span
    pub fn span(&self) -> &Span {
        &self.span
    }
}

impl<'a> Drop for ContextScope<'a> {
    fn drop(&mut self) {
        if !self.finished {
            self.span.end();
            let _ = self.store.add_span(self.span.clone());
        }
    }
}

/// A lightweight propagation context for network transmission
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropagationContext {
    /// W3C traceparent
    pub traceparent: String,
    /// W3C tracestate (optional vendor-specific data)
    pub tracestate: Option<String>,
    /// Serialized baggage
    pub baggage: Option<String>,
    /// Deadline (microseconds since epoch)
    pub deadline_us: Option<u64>,
    /// Hop count
    pub hop_count: u32,
    /// Max hops
    pub max_hops: Option<u32>,
}

impl PropagationContext {
    /// Serialize a `Context` into a wire-ready propagation envelope
    pub fn from_context(ctx: &Context) -> Self {
        let tracestate = if ctx.trace_state.is_empty() {
            None
        } else {
            Some(
                ctx.trace_state
                    .iter()
                    .map(|(k, v)| format!("{}={}", k, v))
                    .collect::<Vec<_>>()
                    .join(","),
            )
        };

        let baggage = if ctx.baggage.is_empty() {
            None
        } else {
            Some(
                ctx.baggage
                    .iter()
                    .map(|(k, v)| format!("{}={}", k, percent_encode(v)))
                    .collect::<Vec<_>>()
                    .join(","),
            )
        };

        Self {
            traceparent: ctx.to_traceparent(),
            tracestate,
            baggage,
            deadline_us: ctx.deadline_us,
            hop_count: ctx.hop_count,
            max_hops: ctx.max_hops,
        }
    }

    /// Deserialize this propagation envelope back into a `Context` for the given node
    pub fn to_context(&self, origin_node: NodeId) -> Option<Context> {
        let mut ctx = Context::from_traceparent(&self.traceparent, origin_node)?;

        // Parse tracestate
        if let Some(ref ts) = self.tracestate {
            for pair in ts.split(',') {
                if let Some((k, v)) = pair.split_once('=') {
                    ctx.trace_state.insert(k.to_string(), v.to_string());
                }
            }
        }

        // Parse baggage
        if let Some(ref bg) = self.baggage {
            for pair in bg.split(',') {
                if let Some((k, v)) = pair.split_once('=') {
                    if let Some(decoded) = percent_decode(v) {
                        ctx.baggage.set(k, decoded);
                    }
                }
            }
        }

        ctx.deadline_us = self.deadline_us;
        ctx.hop_count = self.hop_count;
        ctx.max_hops = self.max_hops;

        Some(ctx)
    }
}

/// Helper to get current time in microseconds
fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_node_id() -> NodeId {
        [1u8; 32]
    }

    #[test]
    fn test_trace_id() {
        let id = TraceId::generate();
        let hex = id.to_hex();
        assert_eq!(hex.len(), 32);

        let parsed = TraceId::from_hex(&hex).unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn test_span_id() {
        let id = SpanId::generate();
        let hex = id.to_hex();
        assert_eq!(hex.len(), 16);

        let parsed = SpanId::from_hex(&hex).unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn test_span_lifecycle() {
        let trace_id = TraceId::generate();
        let node_id = test_node_id();

        let mut span = Span::new(trace_id, "test_operation", node_id);
        span.set_attribute("key", "value");
        span.add_event("started");

        assert!(span.end_time_us.is_none());
        span.end();
        assert!(span.end_time_us.is_some());
        assert!(span.duration_us().is_some());
    }

    #[test]
    fn test_baggage() {
        let mut baggage = Baggage::new();
        baggage.set("user_id", "12345");
        baggage.set_with_metadata("tenant", "acme", "priority=high");

        assert_eq!(baggage.get("user_id"), Some("12345"));
        assert_eq!(
            baggage.get_with_metadata("tenant"),
            Some(("acme", Some("priority=high")))
        );

        let mut other = Baggage::new();
        other.set("user_id", "67890");
        other.set("request_id", "abc");

        baggage.merge(&other);
        assert_eq!(baggage.get("user_id"), Some("67890"));
        assert_eq!(baggage.get("request_id"), Some("abc"));
    }

    #[test]
    fn test_context_creation() {
        let node_id = test_node_id();
        let ctx = Context::new(node_id);

        assert!(!ctx.is_expired());
        assert!(!ctx.exceeded_hops());
        assert_eq!(ctx.hop_count, 0);
    }

    #[test]
    fn test_context_child() {
        let node_id = test_node_id();
        let parent = Context::new(node_id);
        let child = parent.child("child_operation");

        assert_eq!(child.trace_id, parent.trace_id);
        assert_eq!(child.parent_span_id, Some(parent.span_id));
        assert_eq!(child.hop_count, parent.hop_count);
    }

    #[test]
    fn test_context_remote() {
        let node_id = test_node_id();
        let local = Context::new(node_id);
        let remote = local.for_remote();

        assert_eq!(remote.trace_id, local.trace_id);
        assert_eq!(remote.parent_span_id, Some(local.span_id));
        assert_eq!(remote.hop_count, local.hop_count + 1);
    }

    #[test]
    fn test_context_timeout() {
        let node_id = test_node_id();
        let ctx = Context::new(node_id).with_timeout(Duration::from_millis(100));

        assert!(!ctx.is_expired());
        assert!(ctx.remaining().is_some());

        let expired = Context::new(node_id).with_timeout(Duration::from_nanos(1));
        std::thread::sleep(Duration::from_millis(1));
        assert!(expired.is_expired());
    }

    #[test]
    fn test_context_max_hops() {
        let node_id = test_node_id();
        let mut ctx = Context::new(node_id).with_max_hops(3);

        assert!(!ctx.exceeded_hops());

        ctx.hop_count = 3;
        assert!(ctx.exceeded_hops());
    }

    #[test]
    fn test_traceparent() {
        let node_id = test_node_id();
        let ctx = Context::new(node_id);
        let traceparent = ctx.to_traceparent();

        assert!(traceparent.starts_with("00-"));

        let parsed = Context::from_traceparent(&traceparent, node_id).unwrap();
        assert_eq!(parsed.trace_id, ctx.trace_id);
        assert_eq!(parsed.parent_span_id, Some(ctx.span_id));
        assert_eq!(parsed.hop_count, 1);
    }

    #[test]
    fn test_sampler_always_on() {
        let sampler = Sampler::new(SamplingStrategy::AlwaysOn);
        for _ in 0..100 {
            assert!(sampler.should_sample(None));
        }
    }

    #[test]
    fn test_sampler_always_off() {
        let sampler = Sampler::new(SamplingStrategy::AlwaysOff);
        for _ in 0..100 {
            assert!(!sampler.should_sample(None));
        }
    }

    #[test]
    fn test_sampler_parent_based() {
        let sampler = Sampler::new(SamplingStrategy::ParentBased);
        assert!(sampler.should_sample(Some(true)));
        assert!(!sampler.should_sample(Some(false)));
        assert!(sampler.should_sample(None)); // No parent defaults to true
    }

    #[test]
    fn test_context_store() {
        let store = ContextStore::new(100, 1000, Duration::from_secs(60))
            .with_sampling(SamplingStrategy::AlwaysOn);

        let node_id = test_node_id();
        let ctx = store.create_context(node_id).unwrap();

        assert!(store.get_context(&ctx.trace_id).is_some());

        let mut span = Span::new(ctx.trace_id, "test", node_id);
        span.end();
        store.add_span(span).unwrap();

        let spans = store.get_spans(&ctx.trace_id);
        assert_eq!(spans.len(), 1);

        let (completed_ctx, completed_spans) = store.complete_trace(&ctx.trace_id).unwrap();
        assert_eq!(completed_ctx.trace_id, ctx.trace_id);
        assert_eq!(completed_spans.len(), 1);

        assert!(store.get_context(&ctx.trace_id).is_none());
    }

    #[test]
    fn test_propagation_context() {
        let node_id = test_node_id();
        let mut ctx = Context::new(node_id)
            .with_timeout(Duration::from_secs(30))
            .with_max_hops(10);

        ctx.baggage.set("user", "alice");
        ctx.trace_state.insert("vendor".into(), "data".into());

        let prop = PropagationContext::from_context(&ctx);
        let restored = prop.to_context(node_id).unwrap();

        assert_eq!(restored.trace_id, ctx.trace_id);
        assert_eq!(restored.baggage.get("user"), Some("alice"));
        assert_eq!(restored.max_hops, Some(10));
    }

    #[test]
    fn test_context_store_capacity() {
        let store = ContextStore::new(2, 10, Duration::from_secs(60))
            .with_sampling(SamplingStrategy::AlwaysOn);

        let node_id = test_node_id();

        let ctx1 = store.create_context(node_id).unwrap();
        let ctx2 = store.create_context(node_id).unwrap();

        // Third should fail due to capacity
        assert!(matches!(
            store.create_context(node_id),
            Err(ContextError::CapacityExceeded)
        ));

        // Complete one to make room
        store.complete_trace(&ctx1.trace_id);

        // Now should succeed
        assert!(store.create_context(node_id).is_ok());

        // Cleanup the second one too
        store.complete_trace(&ctx2.trace_id);
    }

    // ========================================================================
    // create_context capacity check must be atomic
    // ========================================================================

    /// Concurrent `create_context` calls must not grow `contexts` past
    /// `max_traces`. Pre-fix, two threads could each observe
    /// `len < max` after a `cleanup_expired` and both insert,
    /// producing `len > max`. The new atomic `try_reserve_slot` CAS
    /// gate guarantees the cap is hard.
    #[test]
    fn create_context_concurrent_inserts_do_not_exceed_max_traces() {
        use std::sync::Arc;
        use std::thread;

        const MAX_TRACES: usize = 32;
        let store = Arc::new(
            ContextStore::new(MAX_TRACES, 10, Duration::from_secs(60))
                .with_sampling(SamplingStrategy::AlwaysOn),
        );

        let node_id = test_node_id();
        let n_threads = 16;
        let attempts_per_thread = 8; // 16 * 8 = 128 total attempts

        let barrier = Arc::new(std::sync::Barrier::new(n_threads));
        let mut handles = Vec::new();
        for _ in 0..n_threads {
            let store = store.clone();
            let barrier = barrier.clone();
            handles.push(thread::spawn(move || {
                barrier.wait();
                for _ in 0..attempts_per_thread {
                    let _ = store.create_context(node_id);
                }
            }));
        }
        for h in handles {
            h.join().expect("thread panicked");
        }

        let stats = store.stats();
        assert!(
            stats.active_traces <= MAX_TRACES as u64,
            "active_traces ({}) exceeded MAX_TRACES ({}) — admission gate \
             must hold under concurrent inserts",
            stats.active_traces,
            MAX_TRACES,
        );
        // Also verify dropped_traces accounts for at least some
        // attempts that were rejected at capacity.
        assert!(
            stats.dropped_traces > 0,
            "with 128 attempts and a cap of 32, some inserts must have been dropped",
        );
    }

    /// Two threads calling `continue_context` with the SAME trace_id
    /// must not strand `active_count` slots when `DashMap::insert`
    /// overwrites a prior entry. Pre-fix the duplicate-insert path
    /// reserved a slot but never released it on overwrite, so each
    /// duplicate `continue_context` permanently consumed one slot
    /// of capacity even though the map size never grew past 1. After
    /// `n` duplicates against the same trace_id the store would
    /// refuse new admissions despite `contexts.len() == 1`.
    #[test]
    fn continue_context_duplicate_trace_id_does_not_leak_capacity() {
        const MAX_TRACES: usize = 4;
        let store = ContextStore::new(MAX_TRACES, 10, Duration::from_secs(60))
            .with_sampling(SamplingStrategy::AlwaysOn);
        let node_id = test_node_id();

        // Build a single context once and replay it `MAX_TRACES * 4`
        // times. Pre-fix this stranded `MAX_TRACES * 4 - 1` slots and
        // the next fresh `create_context` would hit CapacityExceeded.
        let ctx = Context::new(node_id);
        for _ in 0..(MAX_TRACES * 4) {
            store
                .continue_context(ctx.clone())
                .expect("duplicate continue_context must succeed");
        }

        // Map only ever holds the one entry.
        assert_eq!(
            store.stats().active_traces,
            1,
            "duplicate continue_context must not grow the map",
        );

        // The store still has room for `MAX_TRACES - 1` brand-new
        // traces. Pre-fix this loop tripped CapacityExceeded on the
        // first iteration because every duplicate had silently
        // consumed a slot.
        for _ in 0..(MAX_TRACES - 1) {
            store
                .create_context(node_id)
                .expect("active_count must reflect map size, not duplicate-insert count");
        }
    }

    /// `complete_trace` releases an `active_count` slot so the
    /// store can re-admit after a trace finishes. Without this,
    /// the atomic counter would leak slots and the
    /// admission gate would refuse new traces even after the
    /// `contexts` map shrinks.
    #[test]
    fn complete_trace_re_admits_capacity() {
        let store = ContextStore::new(2, 10, Duration::from_secs(60))
            .with_sampling(SamplingStrategy::AlwaysOn);
        let node_id = test_node_id();

        let ctx1 = store.create_context(node_id).unwrap();
        let _ctx2 = store.create_context(node_id).unwrap();
        // At cap → next create must be rejected.
        assert!(matches!(
            store.create_context(node_id),
            Err(ContextError::CapacityExceeded)
        ));

        // Complete one trace; the slot must be released and a new
        // create must succeed.
        store.complete_trace(&ctx1.trace_id);
        assert!(
            store.create_context(node_id).is_ok(),
            "complete_trace must release a slot for re-admission",
        );
    }

    #[test]
    fn test_context_store_stats() {
        let store = ContextStore::new(100, 1000, Duration::from_secs(60))
            .with_sampling(SamplingStrategy::AlwaysOn);

        let node_id = test_node_id();

        let ctx = store.create_context(node_id).unwrap();

        let mut span = Span::new(ctx.trace_id, "op1", node_id);
        span.end();
        store.add_span(span).unwrap();

        let mut span2 = Span::new(ctx.trace_id, "op2", node_id);
        span2.end();
        store.add_span(span2).unwrap();

        let stats = store.stats();
        assert_eq!(stats.active_traces, 1);
        assert_eq!(stats.total_spans, 2);
        assert_eq!(stats.sampled_traces, 1);
    }

    /// CR-14: pin that an early-return path between
    /// `try_reserve_slot` and the matching insert correctly
    /// rolls back the reservation via the `SlotReservation` guard's
    /// Drop impl. Pre-CR-14 each early-return path had to manually
    /// call `release_slot()` — easy to miss, leaks a slot
    /// permanently across `active_count` underflow guard.
    ///
    /// We exercise the sampling-skip path which bails out BEFORE
    /// calling `commit()` on the guard. Pre-CR-14 the code had a
    /// manual `release_slot` here; with the guard it's automatic
    /// — the test ensures the release still happens.
    #[test]
    fn cr14_sampling_skip_releases_reservation_via_drop_guard() {
        // Sampler that ALWAYS skips — every reserved slot must
        // be released via the guard's Drop.
        let store = ContextStore::new(8, 100, std::time::Duration::from_secs(60))
            .with_sampling(SamplingStrategy::AlwaysOff);

        let node = test_node_id();
        for _ in 0..50 {
            let _ = store.create_context(node).unwrap();
        }

        let stats = store.stats();
        assert_eq!(
            stats.active_traces, 0,
            "all 50 contexts were sampling-skipped; the SlotReservation \
             Drop guard must have released every reservation. Got \
             active_traces = {} (CR-14 regression).",
            stats.active_traces
        );
    }

    /// CR-14: pin that a panic between reserve and commit ALSO
    /// releases the slot via Drop. We use `catch_unwind` to
    /// observe the panic without poisoning the test harness, then
    /// verify `active_count` rolled back.
    ///
    /// Cubic P2: read `active_count` directly rather than
    /// `stats().active_traces`. The latter is `contexts.len()` —
    /// the DashMap size — and a leaked reservation would bump the
    /// atomic but never reach an insert, leaving the map size
    /// unchanged and silently masking the regression.
    #[test]
    fn cr14_panic_between_reserve_and_commit_releases_slot() {
        use std::panic::{catch_unwind, AssertUnwindSafe};
        use std::sync::atomic::Ordering;

        let store = ContextStore::new(8, 100, std::time::Duration::from_secs(60));
        let initial_active = store.active_count.load(Ordering::Relaxed);

        // Synthesize "reserve then panic before commit" via direct
        // guard manipulation — mirrors what would happen if a
        // future refactor added a fallible step between reserve
        // and `guard.commit()`.
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _guard = store
                .try_reserve_slot()
                .expect("first reserve must succeed against an empty store");
            // Simulate a panic on the path between reserve and
            // commit. The guard's Drop runs as part of unwind.
            panic!("simulated mid-path failure");
        }));

        assert!(result.is_err(), "the closure must have panicked");
        let after_active = store.active_count.load(Ordering::Relaxed);
        assert_eq!(
            after_active, initial_active,
            "CR-14 regression: panic between reserve and commit MUST roll \
             back the slot reservation via SlotReservation::drop. \
             Got active before={} after={}",
            initial_active, after_active
        );
    }

    /// CR-21: pin that this module's `random_u64`
    /// uses the abort-on-fail pattern, NOT `expect()` or
    /// `.unwrap()`. A getrandom panic here would unwind across
    /// any `extern "C"` FFI frame that called into the trace
    /// context layer — undefined behaviour. Source-level
    /// tripwire (assemble forbidden token at runtime so the test
    /// file doesn't trigger itself).
    #[test]
    fn cr21_random_u64_must_not_panic_on_getrandom_failure() {
        // Forbidden shapes — assembled at runtime to prevent the
        // test's own source from triggering the scan.
        let needle_expect = format!("getrandom::fill({}{})", "&mut bytes).", "expect");
        let needle_unwrap = format!("getrandom::fill({}{})", "&mut bytes).", "unwrap");

        let src = include_str!("context.rs");
        for (lineno, line) in src.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            assert!(
                !trimmed.contains(&needle_expect),
                "CR-21 regression: getrandom::fill(...).expect(...) reintroduced \
                 at context.rs:{}. Use the abort-on-fail pattern (fallible \
                 writeln to stderr + std::process::abort).\n  line: {}",
                lineno + 1,
                line
            );
            assert!(
                !trimmed.contains(&needle_unwrap),
                "CR-21 regression: getrandom::fill(...).unwrap() reintroduced \
                 at context.rs:{}. Use the abort-on-fail pattern (fallible \
                 writeln to stderr + std::process::abort).\n  line: {}",
                lineno + 1,
                line
            );
        }
    }

    // ---------- Span builder / lifecycle coverage ----------

    #[test]
    fn span_with_parent_and_kind_set_fields() {
        let parent = SpanId::generate();
        let span = Span::new(TraceId::generate(), "child", test_node_id())
            .with_parent(parent)
            .with_kind(SpanKind::Server);
        assert_eq!(span.parent_span_id, Some(parent));
        assert_eq!(span.kind, SpanKind::Server);
    }

    #[test]
    fn span_set_ok_and_set_error_update_status() {
        let mut span = Span::new(TraceId::generate(), "op", test_node_id());
        span.set_ok();
        assert!(matches!(span.status, SpanStatus::Ok));

        span.set_error("boom");
        match &span.status {
            SpanStatus::Error { message } => assert_eq!(message, "boom"),
            other => panic!("expected Error, got {:?}", other),
        }
    }

    #[test]
    fn span_add_event_with_attributes_and_add_link_populate_collections() {
        let mut span = Span::new(TraceId::generate(), "op", test_node_id());

        let mut attrs = HashMap::new();
        attrs.insert("k".into(), AttributeValue::from("v"));
        span.add_event_with_attributes("evt", attrs);
        assert_eq!(span.events.len(), 1);
        assert_eq!(span.events[0].name, "evt");
        assert!(span.events[0].attributes.contains_key("k"));

        let other_trace = TraceId::generate();
        let other_span = SpanId::generate();
        span.add_link(other_trace, other_span);
        assert_eq!(span.links.len(), 1);
        assert_eq!(span.links[0].trace_id, other_trace);
        assert_eq!(span.links[0].span_id, other_span);
    }

    // ---------- ContextError Display ----------

    #[test]
    fn context_error_display_covers_every_variant() {
        assert_eq!(format!("{}", ContextError::Expired), "context has expired");
        assert_eq!(
            format!("{}", ContextError::MaxHopsExceeded),
            "maximum hops exceeded"
        );
        assert_eq!(format!("{}", ContextError::NotFound), "context not found");
        assert_eq!(
            format!("{}", ContextError::InvalidTraceId),
            "invalid trace ID"
        );
        assert_eq!(
            format!("{}", ContextError::CapacityExceeded),
            "storage capacity exceeded"
        );
    }

    // ---------- percent_encode / percent_decode ----------

    #[test]
    fn percent_codec_roundtrips_ascii_and_unicode_and_punctuation() {
        for input in [
            "",
            "plain",
            "with space",
            "weird/chars?&=",
            "trailing space ",
            "key=value;meta=other",
            // Unicode bytes get encoded byte-by-byte.
            "café",
        ] {
            let encoded = percent_encode(input);
            // Unreserved chars survive; everything else is %HH.
            assert!(!encoded.contains(' '));
            let decoded =
                percent_decode(&encoded).unwrap_or_else(|| panic!("decode failed: {}", encoded));
            assert_eq!(decoded, input, "roundtrip mismatch for {input:?}");
        }
    }

    #[test]
    fn percent_decode_rejects_truncated_hex_escape() {
        // `%4` is missing the second hex digit — the decoder must
        // surface None rather than silently consuming a partial
        // escape (which would corrupt baggage propagation).
        assert_eq!(percent_decode("%4"), None);
        // Non-hex characters after `%` also fail.
        assert_eq!(percent_decode("%ZZ"), None);
    }

    // ---------- ContextScope RAII + explicit finish ----------

    /// Build a store whose sampler is forced to AlwaysOn so
    /// `create_context` deterministically inserts the trace.
    /// The default sampler is `Ratio(0.1)` — most contexts go
    /// unsampled (not stored), and `add_span` then returns
    /// `NotFound` regardless of the scope's behavior. Inside
    /// the tests mod we can swap the private `sampler` field.
    fn store_with_always_on_sampler() -> ContextStore {
        let mut store = ContextStore::new(64, 64, Duration::from_secs(60));
        store.sampler = Sampler::new(SamplingStrategy::AlwaysOn);
        store
    }

    #[test]
    fn context_scope_drop_records_span_into_store() {
        let store = store_with_always_on_sampler();
        let ctx = store.create_context(test_node_id()).unwrap();
        let trace_id = ctx.trace_id;

        // Pre: no spans yet.
        assert!(store.get_spans(&trace_id).is_empty());

        // Drop the scope without calling finish — the Drop impl
        // must end the span and push it to the store.
        {
            let _scope = ContextScope::new(&store, &ctx, "auto", test_node_id());
        }
        let spans = store.get_spans(&trace_id);
        assert_eq!(spans.len(), 1, "Drop must push the span");
        assert!(spans[0].end_time_us.is_some(), "Drop must end() the span");
    }

    #[test]
    fn context_scope_finish_records_span_and_suppresses_drop() {
        let store = store_with_always_on_sampler();
        let ctx = store.create_context(test_node_id()).unwrap();
        let trace_id = ctx.trace_id;

        let mut scope = ContextScope::new(&store, &ctx, "explicit", test_node_id());
        scope.set_ok();
        scope.finish();

        // Exactly one span — `finish` set finished=true so Drop
        // didn't push a duplicate.
        let spans = store.get_spans(&trace_id);
        assert_eq!(spans.len(), 1);
        assert!(matches!(spans[0].status, SpanStatus::Ok));
    }
}
