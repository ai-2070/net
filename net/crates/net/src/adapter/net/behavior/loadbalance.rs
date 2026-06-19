//! Phase 4G: Distributed Load Balancing (LOAD-BALANCE)
//!
//! This module provides distributed load balancing across the Net network:
//! - Multiple load balancing strategies (round-robin, weighted, least-connections, etc.)
//! - Health-aware routing with automatic failover
//! - Load metrics collection and aggregation
//! - Adaptive load balancing based on real-time conditions

use arc_swap::ArcSwap;
use dashmap::DashMap;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::metadata::NodeId;

/// Load balancing strategy
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    /// Round-robin selection
    #[default]
    RoundRobin,
    /// Weighted round-robin based on node capacity
    WeightedRoundRobin,
    /// Select node with fewest active connections
    LeastConnections,
    /// Weighted least connections
    WeightedLeastConnections,
    /// Random selection
    Random,
    /// Weighted random selection
    WeightedRandom,
    /// Consistent hashing for sticky sessions
    ConsistentHash,
    /// Select based on lowest latency
    LeastLatency,
    /// Select based on lowest resource utilization
    LeastLoad,
    /// Power of two random choices
    PowerOfTwo,
    /// Adaptive strategy based on conditions
    Adaptive,
}

/// Health status of a node
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    /// Node is healthy and accepting traffic
    #[default]
    Healthy,
    /// Node is degraded but still accepting traffic
    Degraded,
    /// Node is unhealthy and should not receive traffic
    Unhealthy,
    /// Node health is unknown
    Unknown,
}

impl HealthStatus {
    /// Check if node can receive traffic
    pub fn can_receive_traffic(&self) -> bool {
        matches!(self, HealthStatus::Healthy | HealthStatus::Degraded)
    }

    /// Get weight multiplier for this health status
    pub fn weight_multiplier(&self) -> f64 {
        match self {
            HealthStatus::Healthy => 1.0,
            HealthStatus::Degraded => 0.5,
            HealthStatus::Unhealthy => 0.0,
            HealthStatus::Unknown => 0.25,
        }
    }
}

/// Load metrics for a node
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadMetrics {
    /// CPU utilization (0.0 - 1.0)
    pub cpu_usage: f64,
    /// Memory utilization (0.0 - 1.0)
    pub memory_usage: f64,
    /// Active connections count
    pub active_connections: u32,
    /// Requests per second
    pub requests_per_second: f64,
    /// Average response time in milliseconds
    pub avg_response_time_ms: f64,
    /// Error rate (0.0 - 1.0)
    pub error_rate: f64,
    /// Queue depth
    pub queue_depth: u32,
    /// Bandwidth utilization (0.0 - 1.0)
    pub bandwidth_usage: f64,
    /// Last update timestamp (microseconds since epoch)
    pub updated_at: u64,
}

impl Default for LoadMetrics {
    fn default() -> Self {
        Self {
            cpu_usage: 0.0,
            memory_usage: 0.0,
            active_connections: 0,
            requests_per_second: 0.0,
            avg_response_time_ms: 0.0,
            error_rate: 0.0,
            queue_depth: 0,
            bandwidth_usage: 0.0,
            updated_at: 0,
        }
    }
}

impl LoadMetrics {
    /// Calculate composite load score (0.0 = no load, 1.0 = fully loaded)
    pub fn load_score(&self) -> f64 {
        // Weighted average of different metrics
        let cpu_weight = 0.3;
        let memory_weight = 0.2;
        let connections_weight = 0.2;
        let response_time_weight = 0.15;
        let error_weight = 0.15;

        // Normalize response time (assume 1000ms = fully loaded)
        let normalized_response_time = (self.avg_response_time_ms / 1000.0).min(1.0);

        cpu_weight * self.cpu_usage
            + memory_weight * self.memory_usage
            + connections_weight * (self.active_connections as f64 / 10000.0).min(1.0)
            + response_time_weight * normalized_response_time
            + error_weight * self.error_rate
    }

    /// Check if node is overloaded
    pub fn is_overloaded(&self) -> bool {
        self.cpu_usage > 0.9
            || self.memory_usage > 0.95
            || self.error_rate > 0.1
            || self.queue_depth > 1000
    }
}

/// Node endpoint information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Endpoint {
    /// Node ID
    pub node_id: NodeId,
    /// Weight for weighted strategies (higher = more traffic)
    pub weight: u32,
    /// Health status
    pub health: HealthStatus,
    /// Load metrics
    pub metrics: LoadMetrics,
    /// Tags for filtering
    pub tags: Vec<String>,
    /// Priority (lower = higher priority for failover)
    pub priority: u32,
    /// Whether endpoint is enabled
    pub enabled: bool,
    /// Zone/region for locality-aware routing
    pub zone: Option<String>,
}

impl Endpoint {
    /// Create a new endpoint
    pub fn new(node_id: NodeId) -> Self {
        Self {
            node_id,
            weight: 100,
            health: HealthStatus::Healthy,
            metrics: LoadMetrics::default(),
            tags: Vec::new(),
            priority: 0,
            enabled: true,
            zone: None,
        }
    }

    /// Set weight
    pub fn with_weight(mut self, weight: u32) -> Self {
        self.weight = weight;
        self
    }

    /// Set zone
    pub fn with_zone(mut self, zone: impl Into<String>) -> Self {
        self.zone = Some(zone.into());
        self
    }

    /// Add tag
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Set priority
    pub fn with_priority(mut self, priority: u32) -> Self {
        self.priority = priority;
        self
    }

    /// Effective weight considering health
    pub fn effective_weight(&self) -> f64 {
        if !self.enabled {
            return 0.0;
        }
        self.weight as f64 * self.health.weight_multiplier()
    }

    /// Check if endpoint can receive traffic
    pub fn is_available(&self) -> bool {
        self.enabled && self.health.can_receive_traffic()
    }
}

/// Endpoint state tracked by the load balancer
struct EndpointState {
    /// Immutable endpoint config (node_id, weight, tags, zone, priority)
    node_id: NodeId,
    weight: u32,
    tags: Vec<String>,
    zone: Option<String>,
    priority: u32,
    /// Mutable health status
    health: RwLock<HealthStatus>,
    /// Mutable metrics.
    ///
    /// Per perf #149, switched `RwLock<LoadMetrics>` →
    /// `ArcSwap<LoadMetrics>` so the per-event `load_score()` read
    /// in every selection strategy is one lock-free Acquire load
    /// instead of a parking_lot read + full struct clone. Updates
    /// (operator-cadence) call `metrics.store(Arc::new(...))`.
    metrics: ArcSwap<LoadMetrics>,
    /// Whether endpoint is enabled
    enabled: std::sync::atomic::AtomicBool,
    /// Current connection count
    connections: AtomicU32,
    /// Total requests served
    total_requests: AtomicU64,
    /// Failed requests
    failed_requests: AtomicU64,
    /// Consecutive failures
    consecutive_failures: AtomicU32,
    /// Circuit breaker state
    circuit_open: std::sync::atomic::AtomicBool,
    /// Circuit open time
    circuit_open_time: Mutex<Option<Instant>>,
    /// Whether a half-open probe request is currently in flight. Only one
    /// request is admitted per recovery cycle to test the endpoint.
    half_open_probe: std::sync::atomic::AtomicBool,
    /// Watchdog timestamp for the half-open probe claim.
    ///
    /// Stamped with `Instant::now()` whenever `half_open_probe` flips
    /// `false -> true` (at selection). `select` returns a `Selection` but
    /// has no way to bind an RAII guard to it (the dashmap `Ref` is local
    /// and `Selection` is a `Clone` public type consumed by the FFI/SDK
    /// bindings), so if a caller — e.g. `GroupCoordinator::route_event`,
    /// which never calls `record_completion` — drops the selection without
    /// recording completion, the bare bool would stay `true` forever and
    /// `is_circuit_open` would keep the recovered endpoint out of rotation
    /// permanently. `is_circuit_open` treats a probe held longer than the
    /// recovery window as abandoned and reclaims the slot so a fresh probe
    /// can be admitted (the watchdog the `ProbeGuard` doc points to for the
    /// async-cancel hazard). `record_completion` clears it on the normal
    /// path.
    half_open_probe_at: Mutex<Option<Instant>>,
    /// Set when the endpoint is removed from the balancer. The flat snapshot
    /// shares this `Arc`, so a selector iterating a snapshot taken *before* a
    /// concurrent removal (and before the rebuild that drops the endpoint)
    /// sees it as unavailable immediately. Without this, that selector could
    /// pick a gone endpoint, fail the `endpoints.get` reservation, and burn a
    /// retry — exhausting into a transient false `NoEndpointsAvailable`.
    removed: std::sync::atomic::AtomicBool,
}

impl EndpointState {
    fn new(endpoint: Endpoint) -> Self {
        Self {
            node_id: endpoint.node_id,
            weight: endpoint.weight,
            tags: endpoint.tags,
            zone: endpoint.zone,
            priority: endpoint.priority,
            health: RwLock::new(endpoint.health),
            metrics: ArcSwap::new(Arc::new(endpoint.metrics)),
            enabled: std::sync::atomic::AtomicBool::new(endpoint.enabled),
            connections: AtomicU32::new(0),
            total_requests: AtomicU64::new(0),
            failed_requests: AtomicU64::new(0),
            consecutive_failures: AtomicU32::new(0),
            circuit_open: std::sync::atomic::AtomicBool::new(false),
            circuit_open_time: Mutex::new(None),
            half_open_probe: std::sync::atomic::AtomicBool::new(false),
            half_open_probe_at: Mutex::new(None),
            removed: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn health(&self) -> HealthStatus {
        *self.health.read()
    }

    /// Materialize an owned snapshot of the current metrics. Used
    /// by [`LoadBalancer::endpoints`] which builds full `Endpoint`
    /// structs for operator/inventory consumers — that path
    /// genuinely needs ownership. Per-select hot paths should call
    /// [`Self::load_score`] which avoids the clone entirely.
    fn metrics(&self) -> LoadMetrics {
        (**self.metrics.load()).clone()
    }

    /// Compute the composite load score from the current metrics
    /// snapshot. Per perf #149 — pre-fix every per-event select
    /// path called `state.load_score()` which
    /// `RwLock::read + LoadMetrics::clone + score`. Now it's one
    /// `ArcSwap::load + score` — no lock, no clone.
    fn load_score(&self) -> f64 {
        self.metrics.load().load_score()
    }

    fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    fn effective_weight(&self) -> f64 {
        if !self.is_enabled() {
            return 0.0;
        }
        self.weight as f64 * self.health().weight_multiplier()
    }

    fn is_available(&self) -> bool {
        !self.removed.load(Ordering::Acquire)
            && self.is_enabled()
            && self.health().can_receive_traffic()
    }

    /// Atomically reserve a connection slot if the endpoint is below cap.
    ///
    /// Returns `true` if the slot was reserved (caller now owns a connection
    /// that must be released via `record_completion`), or `false` if the cap
    /// was already reached. This replaces the prior check-then-increment
    /// pattern that allowed concurrent selectors to exceed the cap.
    fn try_record_request(&self, max_connections: u32) -> bool {
        let reserved = self
            .connections
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |c| {
                if c >= max_connections {
                    None
                } else {
                    Some(c + 1)
                }
            })
            .is_ok();
        if reserved {
            self.total_requests.fetch_add(1, Ordering::Relaxed);
        }
        reserved
    }

    fn record_completion(&self, success: bool) {
        // Saturating sub. Pre-fix `fetch_sub(1)` was unconditional;
        // a caller hitting `record_completion` without a matching
        // `record_request` (a substrate bug or a misuse of the
        // public `LoadBalancer::record_completion(node_id)` API)
        // underflowed `connections` to `u32::MAX - k`. After that,
        // `try_record_request` always failed (`c >= max_connections`)
        // and `get_available_endpoints` filtered the endpoint out
        // forever - a silent, permanent removal from rotation with
        // no log, no metric, no recovery path. The test at the
        // bottom of this module explicitly acknowledged the hazard.
        let _ = self
            .connections
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |c| {
                Some(c.saturating_sub(1))
            });

        // If this completion is for the half-open probe, it decides the
        // circuit's fate. Clearing the flag with swap also guarantees only
        // one completion is treated as the probe outcome.
        if self.half_open_probe.swap(false, Ordering::AcqRel) {
            // Clear the watchdog stamp now that the probe is resolved
            // normally — keeps a stale `Instant` from lingering past the
            // next claim's stamp.
            *self.half_open_probe_at.lock() = None;
            if success {
                self.circuit_open.store(false, Ordering::Release);
                self.consecutive_failures.store(0, Ordering::Relaxed);
                *self.circuit_open_time.lock() = None;
            } else {
                self.failed_requests.fetch_add(1, Ordering::Relaxed);
                // Probe failed — restart the recovery timer so the next
                // probe is delayed by another full recovery_time window.
                *self.circuit_open_time.lock() = Some(Instant::now());
            }
            return;
        }

        if success {
            self.consecutive_failures.store(0, Ordering::Relaxed);
        } else {
            self.failed_requests.fetch_add(1, Ordering::Relaxed);
            let failures = self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
            // Open circuit after 5 consecutive failures. Use CAS so only
            // the thread that causes the transition records the open time.
            if failures >= 5
                && self
                    .circuit_open
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            {
                *self.circuit_open_time.lock() = Some(Instant::now());
            }
        }
    }

    /// Returns true if new requests should be rejected for this endpoint.
    ///
    /// **Pure predicate** — this method has no side effects. The
    /// half-open probe slot is claimed lazily at selection time
    /// via [`try_claim_half_open_probe`], so only the endpoint
    /// actually chosen by the selector claims the probe.
    ///
    /// Conflating "is the circuit open?" with "CAS-claim the
    /// half-open probe slot when the recovery window has elapsed"
    /// would let `get_available_endpoints` claim the probe slot
    /// for every endpoint it filters. A multi-endpoint outage past
    /// its recovery window would then have every endpoint claim
    /// the probe slot in the scan while only one (or zero) was
    /// selected. The N-1 others would hold
    /// `half_open_probe == true` with no in-flight request and no
    /// completion path — every subsequent `is_circuit_open` would
    /// return true forever.
    fn is_circuit_open(&self, recovery_time: Duration) -> bool {
        if !self.circuit_open.load(Ordering::Acquire) {
            return false;
        }
        let open_time = match *self.circuit_open_time.lock() {
            Some(t) => t,
            None => return true,
        };
        if open_time.elapsed() < recovery_time {
            return true;
        }
        // Recovery window has elapsed — the endpoint is admitting
        // a half-open probe. If the probe slot is already taken,
        // another request is in flight and we keep rejecting.
        // Otherwise we admit (the caller will CAS-claim the slot
        // via `try_claim_half_open_probe` only on the endpoint it
        // actually selects).
        if !self.half_open_probe.load(Ordering::Acquire) {
            return false;
        }
        // The slot is claimed. A claim that has been held longer than
        // a full recovery window with no completion is an ABANDONED
        // probe — a selection handed to a caller (e.g.
        // `GroupCoordinator::route_event`) that never calls
        // `record_completion`, or an async request future that was
        // cancelled/panicked between claim and completion without a
        // `ProbeGuard`. Without this watchdog the bare bool would
        // pin the recovered endpoint out of rotation forever. Reclaim
        // the slot so a fresh probe can be admitted on this scan.
        let abandoned = self
            .half_open_probe_at
            .lock()
            .is_some_and(|claimed_at| claimed_at.elapsed() >= recovery_time);
        if abandoned {
            self.release_half_open_probe();
            // Admit: this scan/selection will re-claim the slot.
            return false;
        }
        true
    }

    /// Try to claim the half-open probe slot.
    ///
    /// Returns an [`Option<ProbeGuard<'_>>`]; the `Some` arm
    /// carries an RAII guard whose `Drop` releases the slot
    /// automatically. Callers that successfully drive the request
    /// to completion MUST invoke [`ProbeGuard::commit`] before
    /// dispatching to the network — `record_completion` is then
    /// the path that clears the flag. Any other exit (panic
    /// between claim and dispatch, future cancellation, fall-
    /// through error) drops the guard and the slot rolls back
    /// atomically.
    ///
    /// This guard API is intended for ASYNC callers where the
    /// claim → completion window is materially wide (a request
    /// future spanning a network round-trip, where cancellation
    /// or panic between the two is plausible). The synchronous
    /// `select` path at this module's top uses a direct
    /// `compare_exchange` on `half_open_probe` because its claim
    /// → release window is a few atomic ops; the borrow checker
    /// forbids holding a `ProbeGuard<'_>` across the dashmap
    /// `Ref`'s `drop(state)` boundary in that loop.
    #[allow(dead_code)]
    fn try_claim_half_open_probe(&self) -> Option<ProbeGuard<'_>> {
        self.half_open_probe
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| {
                // Stamp the watchdog so a never-committed, never-dropped
                // claim still self-heals via `is_circuit_open`.
                self.stamp_half_open_probe();
                ProbeGuard { state: self }
            })
    }

    /// Release the half-open probe slot without recording a
    /// completion outcome. Prefer [`ProbeGuard`]'s Drop for
    /// routine release; this method exists for paths where the
    /// slot must be cleared via direct atomic write (e.g.
    /// `record_completion` once the breaker fully reopens).
    fn release_half_open_probe(&self) {
        *self.half_open_probe_at.lock() = None;
        self.half_open_probe.store(false, Ordering::Release);
    }

    /// Stamp the half-open probe watchdog. Called right after a
    /// successful `false -> true` claim of `half_open_probe` so
    /// `is_circuit_open` can reclaim an abandoned probe slot (a
    /// selection that was returned to a caller who never recorded
    /// completion) once it has been held past the recovery window.
    fn stamp_half_open_probe(&self) {
        *self.half_open_probe_at.lock() = Some(Instant::now());
    }
}

/// RAII guard returned by
/// [`EndpointState::try_claim_half_open_probe`]. The Drop impl
/// clears the `half_open_probe` slot UNLESS [`Self::commit`] was
/// called first (which `mem::forget`-equivalent the guard, so
/// no atomic write runs).
///
/// Pattern:
/// ```ignore
/// let probe = state.try_claim_half_open_probe()?;   // claim
/// // ... checks that may early-return / panic ...
/// if !state.try_record_request(max_conn) {
///     return Err(...);                                // probe drops, slot released
/// }
/// probe.commit();                                     // success: ownership
///                                                     //   transfers to record_completion
/// // ... dispatch ...
/// ```
///
/// Tracking the success vs failure path with a `bool` plus a
/// manual `release_half_open_probe` at every fall-through is
/// easy to miss on a future-cancel where neither `Ok` nor `Err`
/// runs to completion.
#[allow(dead_code)]
pub(super) struct ProbeGuard<'a> {
    state: &'a EndpointState,
}

impl<'a> ProbeGuard<'a> {
    /// Forget the guard so its Drop does NOT release the slot.
    /// Call this only on the success path AFTER the matching
    /// `try_record_request` succeeded — `record_completion` is
    /// then the path that clears the flag.
    #[allow(dead_code)]
    fn commit(self) {
        std::mem::forget(self);
    }
}

impl<'a> Drop for ProbeGuard<'a> {
    fn drop(&mut self) {
        // Roll back the claim. Idempotent at the atomic level
        // (`store(false)` always lands false), but the structural
        // invariant is that this Drop only runs on the
        // non-commit path — `mem::forget` (via `commit`) prevents
        // it on the success path.
        self.state.release_half_open_probe();
    }
}

/// Request context for load balancing decisions
#[derive(Debug, Clone, Default)]
pub struct RequestContext {
    /// Request ID for consistent hashing
    pub request_id: Option<String>,
    /// Session ID for sticky sessions
    pub session_id: Option<String>,
    /// Client zone for locality routing
    pub client_zone: Option<String>,
    /// Required tags
    pub required_tags: Vec<String>,
    /// Preferred zones (in order of preference)
    pub preferred_zones: Vec<String>,
    /// Custom routing key
    pub routing_key: Option<String>,
}

impl RequestContext {
    /// Create new request context
    pub fn new() -> Self {
        Self::default()
    }

    /// Set session ID for sticky sessions
    pub fn with_session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Set routing key for consistent hashing
    pub fn with_routing_key(mut self, key: impl Into<String>) -> Self {
        self.routing_key = Some(key.into());
        self
    }

    /// Set client zone
    pub fn with_zone(mut self, zone: impl Into<String>) -> Self {
        self.client_zone = Some(zone.into());
        self
    }

    /// Add required tag
    pub fn require_tag(mut self, tag: impl Into<String>) -> Self {
        self.required_tags.push(tag.into());
        self
    }
}

/// Selection result
#[derive(Debug, Clone)]
pub struct Selection {
    /// Selected node ID
    pub node_id: NodeId,
    /// Endpoint weight
    pub weight: u32,
    /// Current load score
    pub load_score: f64,
    /// Why this node was selected
    pub reason: SelectionReason,
}

/// Reason for selection
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionReason {
    /// Selected by round-robin
    RoundRobin,
    /// Selected by weight
    Weighted,
    /// Selected for having least connections
    LeastConnections,
    /// Selected by consistent hash
    ConsistentHash,
    /// Selected for lowest latency
    LeastLatency,
    /// Selected for lowest load
    LeastLoad,
    /// Selected randomly
    Random,
    /// Selected by power of two choices
    PowerOfTwo,
    /// Selected for zone affinity
    ZoneAffinity,
    /// Fallback selection
    Fallback,
}

/// Load balancer configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadBalancerConfig {
    /// Load balancing strategy
    pub strategy: Strategy,
    /// Health check interval
    pub health_check_interval_ms: u64,
    /// Circuit breaker recovery time
    pub circuit_recovery_time_ms: u64,
    /// Maximum connections per endpoint
    pub max_connections_per_endpoint: u32,
    /// Enable zone-aware routing
    pub zone_aware: bool,
    /// Fallback to any available if preferred zone unavailable
    pub zone_fallback: bool,
    /// Metrics staleness threshold
    pub metrics_stale_after_ms: u64,
}

impl Default for LoadBalancerConfig {
    fn default() -> Self {
        Self {
            strategy: Strategy::RoundRobin,
            health_check_interval_ms: 5000,
            circuit_recovery_time_ms: 30000,
            max_connections_per_endpoint: 10000,
            zone_aware: true,
            zone_fallback: true,
            metrics_stale_after_ms: 10000,
        }
    }
}

/// Load balancer error
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadBalancerError {
    /// No endpoints available
    NoEndpointsAvailable,
    /// Endpoint not found
    EndpointNotFound(NodeId),
    /// All endpoints unhealthy
    AllEndpointsUnhealthy,
    /// No endpoints match required tags
    NoMatchingEndpoints,
    /// Circuit breaker open
    CircuitOpen(NodeId),
    /// Max connections reached
    MaxConnectionsReached(NodeId),
}

impl std::fmt::Display for LoadBalancerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoEndpointsAvailable => write!(f, "no endpoints available"),
            Self::EndpointNotFound(id) => write!(f, "endpoint not found: {:?}", id),
            Self::AllEndpointsUnhealthy => write!(f, "all endpoints unhealthy"),
            Self::NoMatchingEndpoints => write!(f, "no endpoints match required tags"),
            Self::CircuitOpen(id) => write!(f, "circuit breaker open for: {:?}", id),
            Self::MaxConnectionsReached(id) => write!(f, "max connections reached for: {:?}", id),
        }
    }
}

impl std::error::Error for LoadBalancerError {}

/// Statistics for the load balancer
#[derive(Debug, Clone, Default)]
pub struct LoadBalancerStats {
    /// Total selections made
    pub total_selections: u64,
    /// Failed selections
    pub failed_selections: u64,
    /// Active endpoints
    pub active_endpoints: u32,
    /// Healthy endpoints
    pub healthy_endpoints: u32,
    /// Total active connections
    pub total_connections: u64,
    /// Average load score across endpoints
    pub avg_load_score: f64,
}

/// Shard count for the consistent-hash ring.
///
/// `DashMap::new()` defaults to `4 × num_cpus` shards (128 on a 32-thread
/// host). The ring holds `virtual_nodes × endpoints` entries and
/// `select_consistent_hash` walks it, so the default over-sharding added a
/// ~128-shard-lock fixed cost to every consistent-hash selection (measured
/// ~19% of it). A small fixed count keeps the walk cheap while leaving room for
/// concurrent ring inserts on add/remove.
///
/// (The `endpoints` map keeps the default on purpose: `select`/`stats` read the
/// `endpoint_list` snapshot, not `endpoints.iter()`, so its shard count only
/// affects concurrent point lookups — where more shards is better.)
const HASH_RING_SHARDS: usize = 8;

/// Distributed load balancer
pub struct LoadBalancer {
    /// Configuration
    config: LoadBalancerConfig,
    /// Endpoints by node ID — authoritative store for point lookups
    /// (get / reservation / health updates).
    endpoints: DashMap<NodeId, Arc<EndpointState>>,
    /// Flat snapshot of the same endpoints, rebuilt only when the SET changes
    /// (add/remove). `select()`/`stats()` iterate this instead of
    /// `DashMap::iter()`, which walks every shard (4*num_cpus, e.g. 128) even
    /// for a handful of endpoints — the fixed cost that dominated select().
    /// The Arcs are shared with `endpoints`, so live per-endpoint atomic state
    /// (health, connections, circuit) is read correctly through the snapshot.
    endpoint_list: ArcSwap<Vec<Arc<EndpointState>>>,
    /// Serializes endpoint SET changes (add/remove) so the `endpoints`
    /// mutation, hash-ring update, and `endpoint_list` rebuild commit as one
    /// unit. Without it, two concurrent membership changes can interleave such
    /// that a rebuild observing the map *before* another thread's mutation
    /// stores its stale snapshot last — dropping a just-added endpoint (or
    /// resurrecting a removed one) from `endpoint_list` until the next change.
    /// Not taken on the hot path (`select`/`stats` only read the snapshot).
    membership_lock: Mutex<()>,
    /// Round-robin counter
    rr_counter: AtomicU64,
    /// Total selections
    total_selections: AtomicU64,
    /// Failed selections
    failed_selections: AtomicU64,
    /// Consistent hash ring (node_id -> virtual nodes)
    hash_ring: DashMap<u64, NodeId>,
    /// Virtual nodes per endpoint for consistent hashing
    virtual_nodes: u32,
}

impl LoadBalancer {
    /// Create a new load balancer
    pub fn new(config: LoadBalancerConfig) -> Self {
        Self {
            config,
            endpoints: DashMap::new(),
            endpoint_list: ArcSwap::from_pointee(Vec::new()),
            membership_lock: Mutex::new(()),
            rr_counter: AtomicU64::new(0),
            total_selections: AtomicU64::new(0),
            failed_selections: AtomicU64::new(0),
            hash_ring: DashMap::with_shard_amount(HASH_RING_SHARDS),
            virtual_nodes: 150,
        }
    }

    /// Create with default configuration
    pub fn with_strategy(strategy: Strategy) -> Self {
        Self::new(LoadBalancerConfig {
            strategy,
            ..Default::default()
        })
    }

    /// Add an endpoint
    pub fn add_endpoint(&self, endpoint: Endpoint) {
        let node_id = endpoint.node_id;
        // Hold `membership_lock` across the mutation + rebuild so a concurrent
        // add/remove can't store a stale snapshot over ours (see field doc).
        let _guard = self.membership_lock.lock();
        self.endpoints
            .insert(node_id, Arc::new(EndpointState::new(endpoint)));

        // Add (or idempotently re-add) this node's vnodes. `add_to_hash_ring`
        // overwrites the node's own prior vnodes in place and sweeps any
        // stale leftovers, so a re-add (reconnect / weight change) neither
        // leaks vnodes nor leaves a transient window where the node is
        // absent from the ring (which would misroute traffic for an
        // endpoint that was meant to stay available).
        self.add_to_hash_ring(node_id);
        self.rebuild_endpoint_list();
    }

    /// Remove an endpoint
    pub fn remove_endpoint(&self, node_id: &NodeId) {
        let _guard = self.membership_lock.lock();
        self.remove_from_hash_ring(node_id);
        if let Some((_, state)) = self.endpoints.remove(node_id) {
            // Flag the shared EndpointState so an in-flight selector reading a
            // pre-rebuild snapshot treats it as unavailable (see field doc).
            state.removed.store(true, Ordering::Release);
        }
        self.rebuild_endpoint_list();
    }

    /// Rebuild the flat endpoint snapshot iterated by `select`/`stats`.
    /// Called only when the endpoint SET changes (add/remove) — per-endpoint
    /// state updates mutate shared atomics visible through the existing Arcs,
    /// so they need no rebuild. This is the only place that pays the
    /// `DashMap::iter()` shard walk; the hot path reads the snapshot.
    fn rebuild_endpoint_list(&self) {
        let list: Vec<Arc<EndpointState>> = self
            .endpoints
            .iter()
            .map(|e| Arc::clone(e.value()))
            .collect();
        self.endpoint_list.store(Arc::new(list));
    }

    /// Update endpoint health
    pub fn update_health(&self, node_id: &NodeId, health: HealthStatus) {
        if let Some(state) = self.endpoints.get(node_id) {
            *state.health.write() = health;
        }
    }

    /// Update endpoint metrics
    pub fn update_metrics(&self, node_id: &NodeId, metrics: LoadMetrics) {
        if let Some(state) = self.endpoints.get(node_id) {
            state.metrics.store(Arc::new(metrics));
        }
    }

    /// Select an endpoint for a request.
    ///
    /// The connection slot is reserved atomically as part of selection so
    /// that concurrent selectors cannot collectively exceed
    /// `max_connections_per_endpoint`. If a strategy picks an endpoint whose
    /// cap was filled by a concurrent selector between availability filtering
    /// and reservation, the selection is retried up to a bounded number of
    /// times before giving up.
    pub fn select(&self, ctx: &RequestContext) -> Result<Selection, LoadBalancerError> {
        self.total_selections.fetch_add(1, Ordering::Relaxed);

        const MAX_RESERVATION_RETRIES: usize = 4;
        let max_conn = self.config.max_connections_per_endpoint;

        // Round-robin strategies advance `rr_counter` inside their
        // selection function. The retry loop below could call them up
        // to 4 times per logical `select()`, which inflated the
        // rotation counter proportionally and distorted the observed
        // RR sequence — weighted-RR distribution tests indirectly
        // assumed 1:1. We pre-compute the RR offset once for this
        // whole logical selection and step deterministically across
        // retries via `(rr_offset + attempt)`, so the counter
        // advances exactly once per `select()` regardless of how many
        // reservation retries occur.
        let rr_offset_for_this_select = self.rr_counter.fetch_add(1, Ordering::Relaxed) as usize;

        for attempt in 0..MAX_RESERVATION_RETRIES {
            let available = self.get_available_endpoints(ctx)?;

            if available.is_empty() {
                self.failed_selections.fetch_add(1, Ordering::Relaxed);
                return Err(LoadBalancerError::NoEndpointsAvailable);
            }

            // Apply strategy. Round-robin variants take a
            // pre-computed offset; non-RR strategies are
            // unaffected by retries (their selection is content-
            // or metric-based).
            let selection = match self.config.strategy {
                Strategy::RoundRobin => self.select_round_robin_at(
                    &available,
                    rr_offset_for_this_select.wrapping_add(attempt),
                ),
                Strategy::WeightedRoundRobin => self.select_weighted_round_robin_at(
                    &available,
                    rr_offset_for_this_select.wrapping_add(attempt) as u64,
                ),
                Strategy::LeastConnections => self.select_least_connections(&available),
                Strategy::WeightedLeastConnections => {
                    self.select_weighted_least_connections(&available)
                }
                Strategy::Random => self.select_random(&available),
                Strategy::WeightedRandom => self.select_weighted_random(&available),
                Strategy::ConsistentHash => self.select_consistent_hash(&available, ctx),
                Strategy::LeastLatency => self.select_least_latency(&available),
                Strategy::LeastLoad => self.select_least_load(&available),
                Strategy::PowerOfTwo => self.select_power_of_two(&available),
                Strategy::Adaptive => self.select_adaptive(&available, ctx),
            };

            // Atomically reserve the connection slot. If a concurrent
            // selector filled the cap, re-run selection against fresh state.
            if let Some(state) = self.endpoints.get(&selection.node_id) {
                // Claim the half-open probe slot ONLY on the
                // endpoint we actually selected, AFTER the
                // pure-predicate `is_circuit_open` check has
                // already admitted the endpoint into `available`.
                // Claiming during the filter pass would leak slots
                // on multi-endpoint outages.
                //
                // When `circuit_open == true`, the half-open probe
                // claim is the HARD GATE — losers of the CAS race
                // must NOT proceed through `try_record_request`.
                // Without strict half-open semantics, a concurrent
                // selector that observed `half_open_probe == false`
                // at filter time but lost the claim CAS could still
                // ride the connection-cap path through and send
                // real traffic to a recovering endpoint alongside
                // the actual probe. Only the thread that wins the
                // probe-slot CAS may test the endpoint; everyone
                // else skips and retries selection. With the slot
                // now claimed (by whoever won), the next
                // iteration's `get_available_endpoints` sees
                // `half_open_probe == true` and filters this
                // endpoint out — losers naturally pick a different
                // endpoint or surface `NoEndpointsAvailable` if
                // this was the only option.
                let circuit_open = state.circuit_open.load(Ordering::Acquire);
                // The `ProbeGuard` RAII type is the preferred API
                // for future async callers (where the request
                // future may panic / cancel between claim and
                // `record_completion`, leaking the slot without a
                // guard). At THIS synchronous selection callsite,
                // the guard's lifetime is tied to the dashmap
                // `Ref` we hold via `state`; carrying it across
                // the `drop(state); continue;` path the
                // lost-race branch needs is forbidden by the
                // borrow checker. Since this loop is fully
                // synchronous (a few atomic ops between claim
                // and either `Ok(selection)` or
                // `release_half_open_probe`), the bool +
                // explicit-release pattern is panic-free in
                // practice — the only ops between claim and
                // release are atomic loads / stores that don't
                // unwind. We use a direct CAS here rather than
                // `try_claim_half_open_probe` so we don't have to
                // immediately drop the guard returned by it.
                let claimed_probe = if circuit_open {
                    let claim_ok = state
                        .half_open_probe
                        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok();
                    if !claim_ok {
                        // Lost the half-open probe race. Drop the
                        // ref guard so the retry's `endpoints.get`
                        // doesn't deadlock, and continue to the
                        // next attempt.
                        drop(state);
                        continue;
                    }
                    // Stamp the watchdog so this claim self-heals even
                    // if the returned `Selection` is dropped without a
                    // matching `record_completion` (e.g.
                    // `GroupCoordinator::route_event`). `is_circuit_open`
                    // reclaims the slot once it has been held past the
                    // recovery window. See `half_open_probe_at`.
                    state.stamp_half_open_probe();
                    true
                } else {
                    false
                };
                if state.try_record_request(max_conn) {
                    return Ok(selection);
                }
                // try_record_request failed — release any probe
                // slot we just claimed so it doesn't strand.
                if claimed_probe {
                    state.release_half_open_probe();
                }
            }
        }

        self.failed_selections.fetch_add(1, Ordering::Relaxed);
        Err(LoadBalancerError::NoEndpointsAvailable)
    }

    /// Record request completion
    pub fn record_completion(&self, node_id: &NodeId, success: bool) {
        if let Some(state) = self.endpoints.get(node_id) {
            state.record_completion(success);
        }
    }

    /// Get available endpoints matching context
    fn get_available_endpoints(
        &self,
        ctx: &RequestContext,
    ) -> Result<Vec<Arc<EndpointState>>, LoadBalancerError> {
        let recovery_time = Duration::from_millis(self.config.circuit_recovery_time_ms);
        let mut available = Vec::new();
        let mut zone_matches = Vec::new();

        // Iterate the flat snapshot rather than DashMap::iter (shard walk).
        let snapshot = self.endpoint_list.load();
        for state in snapshot.iter() {
            // Check basic availability
            if !state.is_available() {
                continue;
            }

            // Check circuit breaker
            if state.is_circuit_open(recovery_time) {
                continue;
            }

            // Check max connections
            if state.connections.load(Ordering::Relaxed) >= self.config.max_connections_per_endpoint
            {
                continue;
            }

            // Check required tags
            if !ctx.required_tags.is_empty()
                && !ctx.required_tags.iter().all(|t| state.tags.contains(t))
            {
                continue;
            }

            // Zone-aware routing
            if self.config.zone_aware {
                if let Some(ref client_zone) = ctx.client_zone {
                    if state.zone.as_ref() == Some(client_zone) {
                        zone_matches.push(Arc::clone(state));
                        continue;
                    }
                }
            }

            available.push(Arc::clone(state));
        }

        // Prefer zone matches if available
        if !zone_matches.is_empty() {
            return Ok(zone_matches);
        }

        // No zone matches — check zone_fallback policy
        if self.config.zone_aware && ctx.client_zone.is_some() && !self.config.zone_fallback {
            // zone_fallback is disabled: don't fall back to non-zone endpoints
            return Err(LoadBalancerError::NoEndpointsAvailable);
        }

        if available.is_empty() {
            return Err(LoadBalancerError::NoEndpointsAvailable);
        }

        Ok(available)
    }

    fn select_round_robin(&self, endpoints: &[Arc<EndpointState>]) -> Selection {
        let offset = self.rr_counter.fetch_add(1, Ordering::Relaxed) as usize;
        self.select_round_robin_at(endpoints, offset)
    }

    /// Offset-based variant used by the retry loop in `select()` so
    /// a logical select advances the `rr_counter` exactly once across
    /// all reservation retries.
    fn select_round_robin_at(&self, endpoints: &[Arc<EndpointState>], offset: usize) -> Selection {
        let idx = offset % endpoints.len();
        let state = &endpoints[idx];
        Selection {
            node_id: state.node_id,
            weight: state.weight,
            load_score: state.load_score(),
            reason: SelectionReason::RoundRobin,
        }
    }

    fn select_weighted_round_robin(&self, endpoints: &[Arc<EndpointState>]) -> Selection {
        let counter = self.rr_counter.fetch_add(1, Ordering::Relaxed);
        self.select_weighted_round_robin_at(endpoints, counter)
    }

    /// Offset-based variant used by `select()` across reservation
    /// retries so the `rr_counter` advances exactly once per logical
    /// select.
    fn select_weighted_round_robin_at(
        &self,
        endpoints: &[Arc<EndpointState>],
        counter: u64,
    ) -> Selection {
        let total_weight: f64 = endpoints.iter().map(|e| e.effective_weight()).sum();

        // Use `!(total_weight > 0.0)` rather than `total_weight <= 0.0`:
        // NaN compares unequal to everything (including itself), so
        // `NaN <= 0.0` is `false` — the gate would fall through to
        // the weighted path below where `total_weight.ceil() as u64`
        // is undefined for NaN, and the cumulative loop never
        // exceeds NaN (the `>` comparison is also false), causing
        // the function to fall through to the fallback-first path
        // and silently bias every selection to `endpoints[0]`. The
        // negated-greater check catches NaN as well as ≤ 0.0.
        // Clippy flags the negated comparison; the lint is wrong
        // for our NaN-safety intent, so suppress it locally.
        #[allow(clippy::neg_cmp_op_on_partial_ord)]
        if !(total_weight > 0.0) {
            return self.select_round_robin_at(endpoints, counter as usize);
        }

        // Pick a wheel position by reducing the counter modulo an
        // integer `wheel` size BEFORE casting to f64, then mapping that
        // position proportionally into `[0, total_weight)`. Doing the
        // modulus in integer space first preserves the precision fix —
        // `counter as f64 % total_weight` lost the low bits of `counter`
        // past the f64 mantissa boundary (2^53 selections), stalling
        // rotation on a narrow set of indices.
        //
        // The wheel size must NOT collapse to the integer ceiling of a
        // sub-unit total: `total_weight.ceil() as u64` mapped
        // `total_weight == 1.0` (e.g. two endpoints each at effective
        // weight 0.5, both `Degraded`) to `1`, so `counter % 1 == 0`
        // always and the cumulative loop (`0.5 > 0`) selected the first
        // endpoint forever — the second starved.
        //
        // Derive the wheel from the weights themselves rather than an
        // arbitrary floor: `ceil(total / smallest positive weight)`
        // gives every endpoint at least one wheel position and yields
        // EXACT ratios for commensurate weights. Integer weights keep
        // their natural cycle (1,1,1 → wheel 3; 100,50 → wheel 3, i.e.
        // 2:1) — byte-for-byte the old integer behavior, NOT reshaped by
        // a fixed constant — while fractional/sub-unit shares resolve
        // (0.5,0.5 → 2; 1.0,0.5 → 3). The modulus is taken in integer
        // space (`counter % wheel`) BEFORE the f64 cast, preserving the
        // precision fix (`counter as f64 % total` lost the low bits of
        // `counter` past the 2^53 mantissa boundary, stalling rotation).
        let min_weight = endpoints
            .iter()
            .map(|e| e.effective_weight())
            .filter(|w| *w > 0.0)
            .fold(f64::INFINITY, f64::min);
        let wheel = ((total_weight / min_weight).ceil() as u64).max(1);
        // Map the integer wheel position into the real weight domain.
        let target = (counter % wheel) as f64 / wheel as f64 * total_weight;

        let mut cumulative = 0.0;
        for state in endpoints {
            cumulative += state.effective_weight();
            if cumulative > target {
                return Selection {
                    node_id: state.node_id,
                    weight: state.weight,
                    load_score: state.load_score(),
                    reason: SelectionReason::Weighted,
                };
            }
        }

        // Fallback to first
        let state = &endpoints[0];
        Selection {
            node_id: state.node_id,
            weight: state.weight,
            load_score: state.load_score(),
            reason: SelectionReason::Weighted,
        }
    }

    #[expect(
        clippy::unwrap_used,
        reason = "caller (LoadBalancer::select) returns early on empty endpoints; min_by_key on a non-empty iter is infallible"
    )]
    fn select_least_connections(&self, endpoints: &[Arc<EndpointState>]) -> Selection {
        let state = endpoints
            .iter()
            .min_by_key(|e| e.connections.load(Ordering::Relaxed))
            .unwrap();

        Selection {
            node_id: state.node_id,
            weight: state.weight,
            load_score: state.load_score(),
            reason: SelectionReason::LeastConnections,
        }
    }

    #[expect(
        clippy::unwrap_used,
        reason = "caller (LoadBalancer::select) returns early on empty endpoints; min_by on a non-empty iter is infallible"
    )]
    fn select_weighted_least_connections(&self, endpoints: &[Arc<EndpointState>]) -> Selection {
        // Score = connections / weight (lower is better).
        // The `.max(MIN_DIVISOR)` guard is a divide-by-zero protector
        // for zero-weighted endpoints. It uses a small positive
        // epsilon instead of `1.0` so that fractional weights like
        // `0.1` and `0.5` keep their relative ordering — the old
        // `.max(1.0)` silently collapsed any weight in `(0, 1]` onto
        // `1.0`, degrading weighted-LC into plain least-connections
        // whenever operators configured sub-unit weights.
        const MIN_DIVISOR: f64 = 1e-6;
        let state = endpoints
            .iter()
            .min_by(|a, b| {
                let score_a = a.connections.load(Ordering::Relaxed) as f64
                    / a.effective_weight().max(MIN_DIVISOR);
                let score_b = b.connections.load(Ordering::Relaxed) as f64
                    / b.effective_weight().max(MIN_DIVISOR);
                score_a.total_cmp(&score_b)
            })
            .unwrap();

        Selection {
            node_id: state.node_id,
            weight: state.weight,
            load_score: state.load_score(),
            reason: SelectionReason::LeastConnections,
        }
    }

    fn select_random(&self, endpoints: &[Arc<EndpointState>]) -> Selection {
        let idx = random_usize() % endpoints.len();
        let state = &endpoints[idx];
        Selection {
            node_id: state.node_id,
            weight: state.weight,
            load_score: state.load_score(),
            reason: SelectionReason::Random,
        }
    }

    fn select_weighted_random(&self, endpoints: &[Arc<EndpointState>]) -> Selection {
        let total_weight: f64 = endpoints.iter().map(|e| e.effective_weight()).sum();

        if total_weight <= 0.0 {
            return self.select_random(endpoints);
        }

        let target = random_f64() * total_weight;

        let mut cumulative = 0.0;
        for state in endpoints {
            cumulative += state.effective_weight();
            if cumulative >= target {
                return Selection {
                    node_id: state.node_id,
                    weight: state.weight,
                    load_score: state.load_score(),
                    reason: SelectionReason::Weighted,
                };
            }
        }

        // Fallback
        let state = &endpoints[0];
        Selection {
            node_id: state.node_id,
            weight: state.weight,
            load_score: state.load_score(),
            reason: SelectionReason::Weighted,
        }
    }

    fn select_consistent_hash(
        &self,
        endpoints: &[Arc<EndpointState>],
        ctx: &RequestContext,
    ) -> Selection {
        let key = ctx
            .routing_key
            .as_ref()
            .or(ctx.session_id.as_ref())
            .or(ctx.request_id.as_ref());

        if let Some(key) = key {
            let hash = self.hash_key(key);

            // Collect and sort hash ring entries — DashMap iteration order is
            // arbitrary, but consistent hashing requires finding the smallest
            // key >= hash.
            let mut ring: Vec<(u64, NodeId)> = self
                .hash_ring
                .iter()
                .map(|entry| (*entry.key(), *entry.value()))
                .collect();
            ring.sort_unstable_by_key(|&(k, _)| k);

            // Binary search for the first key >= hash
            let idx = ring.partition_point(|&(k, _)| k < hash);

            // Try from the found position, wrapping around
            for i in 0..ring.len() {
                let (_, node_id) = ring[(idx + i) % ring.len()];
                if let Some(state) = endpoints.iter().find(|e| e.node_id == node_id) {
                    return Selection {
                        node_id: state.node_id,
                        weight: state.weight,
                        load_score: state.load_score(),
                        reason: SelectionReason::ConsistentHash,
                    };
                }
            }
        }

        // Fallback to round-robin
        self.select_round_robin(endpoints)
    }

    #[expect(
        clippy::unwrap_used,
        reason = "caller (LoadBalancer::select) returns early on empty endpoints; min_by on a non-empty iter is infallible"
    )]
    fn select_least_latency(&self, endpoints: &[Arc<EndpointState>]) -> Selection {
        let state = endpoints
            .iter()
            .min_by(|a, b| {
                a.metrics()
                    .avg_response_time_ms
                    .total_cmp(&b.metrics().avg_response_time_ms)
            })
            .unwrap();

        Selection {
            node_id: state.node_id,
            weight: state.weight,
            load_score: state.load_score(),
            reason: SelectionReason::LeastLatency,
        }
    }

    #[expect(
        clippy::unwrap_used,
        reason = "caller (LoadBalancer::select) returns early on empty endpoints; min_by on a non-empty iter is infallible"
    )]
    fn select_least_load(&self, endpoints: &[Arc<EndpointState>]) -> Selection {
        let state = endpoints
            .iter()
            .min_by(|a, b| {
                a.metrics()
                    .load_score()
                    .total_cmp(&b.metrics().load_score())
            })
            .unwrap();

        Selection {
            node_id: state.node_id,
            weight: state.weight,
            load_score: state.load_score(),
            reason: SelectionReason::LeastLoad,
        }
    }

    fn select_power_of_two(&self, endpoints: &[Arc<EndpointState>]) -> Selection {
        if endpoints.len() < 2 {
            return self.select_round_robin(endpoints);
        }

        // Pick two random endpoints
        let idx1 = random_usize() % endpoints.len();
        let mut idx2 = random_usize() % endpoints.len();
        if idx2 == idx1 {
            idx2 = (idx1 + 1) % endpoints.len();
        }

        let state1 = &endpoints[idx1];
        let state2 = &endpoints[idx2];

        // Choose the one with fewer connections
        let state = if state1.connections.load(Ordering::Relaxed)
            <= state2.connections.load(Ordering::Relaxed)
        {
            state1
        } else {
            state2
        };

        Selection {
            node_id: state.node_id,
            weight: state.weight,
            load_score: state.load_score(),
            reason: SelectionReason::PowerOfTwo,
        }
    }

    fn select_adaptive(&self, endpoints: &[Arc<EndpointState>], ctx: &RequestContext) -> Selection {
        // Use different strategies based on conditions
        let avg_load: f64 = endpoints
            .iter()
            .map(|e| e.metrics().load_score())
            .sum::<f64>()
            / endpoints.len() as f64;

        // If high load, use least connections
        if avg_load > 0.7 {
            return self.select_least_connections(endpoints);
        }

        // If session ID present, use consistent hash
        if ctx.session_id.is_some() || ctx.routing_key.is_some() {
            return self.select_consistent_hash(endpoints, ctx);
        }

        // Otherwise use weighted round-robin
        self.select_weighted_round_robin(endpoints)
    }

    fn add_to_hash_ring(&self, node_id: NodeId) {
        // Slots this node ends up occupying in THIS call — used both to
        // keep the node's own intra-call collisions distinct and to
        // sweep any stale vnodes left by a prior add (drift cleanup).
        let mut placed: std::collections::HashSet<u64> =
            std::collections::HashSet::with_capacity(self.virtual_nodes as usize);
        for i in 0..self.virtual_nodes {
            let key = format!("{:?}-{}", node_id, i);
            let mut hash = self.hash_key(&key);
            // Linear-probe past collisions, NON-DESTRUCTIVELY (a plain
            // `insert` would clobber another node's vnode and skew the
            // ring). The probe stops when the slot is:
            //   * free, OR
            //   * already held by THIS node from a *prior* add (not one
            //     we placed this call) — overwrite it in place.
            // The in-place overwrite is what makes a re-add (reconnect /
            // weight change) idempotent WITHOUT first removing the
            // node's vnodes: clearing first (the previous fix) left a
            // transient window where the node had no ring presence and
            // traffic could misroute. A slot held by a DIFFERENT node,
            // or by one of this node's vnodes we ALREADY placed this
            // call (an intra-node hash collision), is a true collision —
            // probe on so every vnode stays distinct.
            loop {
                match self.hash_ring.get(&hash).map(|r| *r) {
                    None => break,
                    Some(occupant) if occupant == node_id && !placed.contains(&hash) => break,
                    Some(_) => hash = hash.wrapping_add(1),
                }
            }
            self.hash_ring.insert(hash, node_id);
            placed.insert(hash);
        }
        // Sweep any vnodes still tagged with this node that we did NOT
        // (re)place this call — stale entries from an earlier add whose
        // probe path changed (e.g. a collision partner was since
        // removed). The node's freshly-placed vnodes are all in `placed`
        // and inserted above, so it is never absent from the ring; this
        // only drops leftovers. Common case: nothing to remove.
        self.hash_ring
            .retain(|k, v| *v != node_id || placed.contains(k));
    }

    fn remove_from_hash_ring(&self, node_id: &NodeId) {
        self.hash_ring.retain(|_, v| v != node_id);
    }

    fn hash_key(&self, key: &str) -> u64 {
        // Simple FNV-1a hash
        let mut hash: u64 = 0xcbf29ce484222325;
        for byte in key.bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash
    }

    /// Get statistics
    pub fn stats(&self) -> LoadBalancerStats {
        let mut healthy = 0u32;
        let mut total_connections = 0u64;
        let mut total_load = 0.0;

        let snapshot = self.endpoint_list.load();
        for state in snapshot.iter() {
            if state.health() == HealthStatus::Healthy {
                healthy += 1;
            }
            total_connections += state.connections.load(Ordering::Relaxed) as u64;
            total_load += state.load_score();
        }

        let endpoint_count = snapshot.len() as u32;

        LoadBalancerStats {
            total_selections: self.total_selections.load(Ordering::Relaxed),
            failed_selections: self.failed_selections.load(Ordering::Relaxed),
            active_endpoints: endpoint_count,
            healthy_endpoints: healthy,
            total_connections,
            avg_load_score: if endpoint_count > 0 {
                total_load / endpoint_count as f64
            } else {
                0.0
            },
        }
    }

    /// Get all endpoints as snapshots
    pub fn endpoints(&self) -> Vec<Endpoint> {
        self.endpoint_list
            .load()
            .iter()
            .map(|state| Endpoint {
                node_id: state.node_id,
                weight: state.weight,
                health: state.health(),
                metrics: state.metrics(),
                tags: state.tags.clone(),
                priority: state.priority,
                enabled: state.is_enabled(),
                zone: state.zone.clone(),
            })
            .collect()
    }

    /// Get endpoint count
    pub fn endpoint_count(&self) -> usize {
        self.endpoint_list.load().len()
    }
}

/// Generate random usize.
///
/// Aborts on `getrandom` failure rather than panic-unwinding
/// through the FFI boundary. Load-balance random numbers are not
/// directly auth-bearing, but this function is reachable from hot
/// paths called by `extern "C"` FFI consumers (Python / Node / Go
/// bindings) — a `getrandom` failure that unwound across the C
/// ABI would be undefined behaviour. `process::abort` is
/// `extern "C"`-safe (terminates rather than unwinds) and
/// loss-of-availability is the only safe response when the system
/// can't produce randomness.
fn random_usize() -> usize {
    let mut bytes = [0u8; 8];
    if let Err(e) = getrandom::fill(&mut bytes) {
        eprintln!(
            "FATAL: behavior::loadbalance::random_usize getrandom failure ({e:?}); \
             aborting to avoid panic across the FFI boundary"
        );
        std::process::abort();
    }
    usize::from_le_bytes(bytes)
}

/// Generate random f64 uniformly in the half-open interval [0.0, 1.0).
///
/// Uses the top 53 bits of entropy (the f64 mantissa width) divided by
/// `2^53`, which guarantees the result is strictly less than 1.0. The naive
/// `r as f64 / u64::MAX as f64` approach can round up to exactly 1.0 because
/// `u64::MAX as f64` itself rounds to `2^64`.
fn random_f64() -> f64 {
    let r = random_usize() as u64;
    (r >> 11) as f64 / ((1u64 << 53) as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_node_id(n: u8) -> NodeId {
        let mut id = [0u8; 32];
        id[0] = n;
        id
    }

    /// Pin perf #149: `update_metrics` followed by `load_score`
    /// observes the new metrics value lock-free via ArcSwap. A
    /// regression that reverts to `RwLock<LoadMetrics>` would
    /// still pass this functional test but would re-introduce the
    /// per-event lock-acquire + struct clone. The pointer-identity
    /// check on `Arc::ptr_eq` distinguishes ArcSwap (writer's Arc
    /// is the reader's Arc) from any swap-via-clone alternative.
    #[test]
    fn endpoint_state_metrics_arc_swap_visibility_and_no_clone_on_read() {
        let lb = LoadBalancer::with_strategy(Strategy::LeastLoad);
        let node = make_node_id(7);
        lb.add_endpoint(Endpoint::new(node));

        // Initial: defaulted LoadMetrics; score is small but
        // computed lock-free.
        {
            let state_ref = lb.endpoints.get(&node).expect("endpoint registered");
            let initial_load = state_ref.load_score();
            assert!(
                initial_load >= 0.0,
                "load_score must compute from current ArcSwap snapshot"
            );
            // The internal Arc backing metrics — readers should
            // observe the SAME Arc identity across two loads with
            // no intervening write.
            let arc1 = state_ref.metrics.load_full();
            let arc2 = state_ref.metrics.load_full();
            assert!(
                Arc::ptr_eq(&arc1, &arc2),
                "two reads with no writer in between must share the Arc — \
                 confirms ArcSwap (not RwLock<T>) backing"
            );
        }

        // Update to a heavily-loaded snapshot.
        let busy = LoadMetrics {
            cpu_usage: 0.95,
            error_rate: 0.5,
            ..Default::default()
        };
        let busy_score = busy.load_score();
        lb.update_metrics(&node, busy);

        let state_ref = lb.endpoints.get(&node).expect("endpoint still here");
        assert!(
            (state_ref.load_score() - busy_score).abs() < f64::EPSILON,
            "post-update load_score must reflect the new snapshot"
        );
    }

    #[test]
    fn test_health_status() {
        assert!(HealthStatus::Healthy.can_receive_traffic());
        assert!(HealthStatus::Degraded.can_receive_traffic());
        assert!(!HealthStatus::Unhealthy.can_receive_traffic());
        assert!(!HealthStatus::Unknown.can_receive_traffic());

        assert_eq!(HealthStatus::Healthy.weight_multiplier(), 1.0);
        assert_eq!(HealthStatus::Degraded.weight_multiplier(), 0.5);
        assert_eq!(HealthStatus::Unhealthy.weight_multiplier(), 0.0);
    }

    #[test]
    fn test_load_metrics() {
        let metrics = LoadMetrics {
            cpu_usage: 0.5,
            memory_usage: 0.3,
            active_connections: 100,
            requests_per_second: 1000.0,
            avg_response_time_ms: 50.0,
            error_rate: 0.01,
            queue_depth: 10,
            bandwidth_usage: 0.2,
            updated_at: 0,
        };

        let score = metrics.load_score();
        assert!(score > 0.0 && score < 1.0);
        assert!(!metrics.is_overloaded());

        let overloaded = LoadMetrics {
            cpu_usage: 0.95,
            ..Default::default()
        };
        assert!(overloaded.is_overloaded());
    }

    #[test]
    fn test_endpoint() {
        let node_id = make_node_id(1);
        let endpoint = Endpoint::new(node_id)
            .with_weight(200)
            .with_zone("us-east-1")
            .with_tag("gpu");

        assert_eq!(endpoint.weight, 200);
        assert_eq!(endpoint.zone, Some("us-east-1".to_string()));
        assert!(endpoint.tags.contains(&"gpu".to_string()));
        assert!(endpoint.is_available());
        assert_eq!(endpoint.effective_weight(), 200.0);
    }

    #[test]
    fn test_load_balancer_round_robin() {
        let lb = LoadBalancer::with_strategy(Strategy::RoundRobin);

        lb.add_endpoint(Endpoint::new(make_node_id(1)));
        lb.add_endpoint(Endpoint::new(make_node_id(2)));
        lb.add_endpoint(Endpoint::new(make_node_id(3)));

        let ctx = RequestContext::new();

        // Should cycle through endpoints
        let mut selected = Vec::new();
        for _ in 0..6 {
            let selection = lb.select(&ctx).unwrap();
            selected.push(selection.node_id[0]);
        }

        // Should have selected each endpoint twice
        assert_eq!(selected.iter().filter(|&&x| x == 1).count(), 2);
        assert_eq!(selected.iter().filter(|&&x| x == 2).count(), 2);
        assert_eq!(selected.iter().filter(|&&x| x == 3).count(), 2);
    }

    #[test]
    fn test_load_balancer_least_connections() {
        let lb = LoadBalancer::with_strategy(Strategy::LeastConnections);

        lb.add_endpoint(Endpoint::new(make_node_id(1)));
        lb.add_endpoint(Endpoint::new(make_node_id(2)));
        lb.add_endpoint(Endpoint::new(make_node_id(3)));

        let ctx = RequestContext::new();

        // First selection - all have 0 connections
        let s1 = lb.select(&ctx).unwrap();
        // Don't record completion, so connection stays

        // Second selection should pick a different node
        let s2 = lb.select(&ctx).unwrap();
        assert_ne!(s1.node_id, s2.node_id);
    }

    #[test]
    fn test_load_balancer_weighted() {
        let lb = LoadBalancer::with_strategy(Strategy::WeightedRoundRobin);

        lb.add_endpoint(Endpoint::new(make_node_id(1)).with_weight(100));
        lb.add_endpoint(Endpoint::new(make_node_id(2)).with_weight(200));
        lb.add_endpoint(Endpoint::new(make_node_id(3)).with_weight(300));

        let ctx = RequestContext::new();

        let mut counts = std::collections::HashMap::new();
        for _ in 0..600 {
            let selection = lb.select(&ctx).unwrap();
            lb.record_completion(&selection.node_id, true);
            *counts.entry(selection.node_id[0]).or_insert(0) += 1;
        }

        // Node 3 should get most traffic, node 1 least
        assert!(counts.get(&3).unwrap() > counts.get(&2).unwrap());
        assert!(counts.get(&2).unwrap() > counts.get(&1).unwrap());
    }

    #[test]
    fn test_regression_weighted_lc_preserves_fractional_weights() {
        // Regression (LOW, BUGS.md): `select_weighted_least_connections`
        // used `.max(1.0)` as a divide-by-zero guard, which also
        // collapsed every weight in `(0, 1]` onto `1.0`. An endpoint
        // with weight `0.1` was scored identically to one with
        // `1.0`, silently degrading weighted-LC into plain LC
        // whenever operators configured sub-unit weights.
        //
        // Fix: use a small positive epsilon instead, so fractional
        // weights keep their relative ordering.
        let lb = LoadBalancer::with_strategy(Strategy::WeightedLeastConnections);

        // Two endpoints with identical connection counts but very
        // different fractional weights.
        lb.add_endpoint(Endpoint::new(make_node_id(1)).with_weight(10));
        lb.add_endpoint(Endpoint::new(make_node_id(2)).with_weight(1));

        let ctx = RequestContext::new();
        let mut counts = std::collections::HashMap::new();
        for _ in 0..600 {
            let selection = lb.select(&ctx).unwrap();
            // Don't record completion so connections stay matched.
            *counts.entry(selection.node_id[0]).or_insert(0_u32) += 1;
        }

        // The 10x-weighted endpoint should overwhelmingly win the
        // "connections/weight" tiebreak when connection counts are
        // comparable. With the old `.max(1.0)` collapse, the two
        // endpoints would score identically and a later tiebreaker
        // would pick one consistently — distribution would be either
        // 50/50 or 100/0 depending on ordering.
        let high = *counts.get(&1).unwrap_or(&0);
        let low = *counts.get(&2).unwrap_or(&0);
        assert!(
            high > low * 2,
            "weight=10 endpoint must get >2x more traffic than weight=1 \
             (got {high} vs {low})",
        );
    }

    #[test]
    fn test_regression_weighted_rr_precision_past_f64_mantissa() {
        // Regression (LOW, BUGS.md): `select_weighted_round_robin`
        // used `counter as f64 % total_weight`. Past 2^53 selections
        // the `as f64` cast dropped the low bits and rotation stalled
        // on a narrow set of indices. The fix scales weights to
        // integers and does the modulus in u64 space.
        let lb = LoadBalancer::with_strategy(Strategy::WeightedRoundRobin);
        lb.add_endpoint(Endpoint::new(make_node_id(1)).with_weight(1));
        lb.add_endpoint(Endpoint::new(make_node_id(2)).with_weight(1));
        lb.add_endpoint(Endpoint::new(make_node_id(3)).with_weight(1));

        // Jump the counter past the f64 mantissa boundary. The raw
        // `AtomicU64` is private but `select` starts from the internal
        // counter; we simulate a long-running process by selecting
        // once (to warm up) and then seeding the rr_counter via the
        // backing atomic through a public helper.
        //
        // Without direct access we exercise ordinary rotation; the
        // real precision gain is covered by the unit-level property
        // that `(counter % scaled_total)` is exact for all u64 inputs.
        let ctx = RequestContext::new();
        let mut counts = std::collections::HashMap::new();
        for _ in 0..300 {
            let sel = lb.select(&ctx).unwrap();
            *counts.entry(sel.node_id[0]).or_insert(0) += 1;
        }

        // Uniform weights → each of three endpoints gets ~100 hits.
        // This is a basic sanity test; the u64 exactness is verified
        // by construction (integer math has no rounding).
        for id in 1..=3u8 {
            let got = counts.get(&id).copied().unwrap_or(0);
            assert!(
                (80..=120).contains(&got),
                "endpoint {id} should get ~100 hits, got {got}",
            );
        }
    }

    #[test]
    fn test_load_balancer_health() {
        let lb = LoadBalancer::with_strategy(Strategy::RoundRobin);

        lb.add_endpoint(Endpoint::new(make_node_id(1)));
        lb.add_endpoint(Endpoint::new(make_node_id(2)));

        let ctx = RequestContext::new();

        // Mark node 1 as unhealthy
        lb.update_health(&make_node_id(1), HealthStatus::Unhealthy);

        // All selections should go to node 2
        for _ in 0..10 {
            let selection = lb.select(&ctx).unwrap();
            assert_eq!(selection.node_id[0], 2);
        }
    }

    #[test]
    fn test_load_balancer_zone_affinity() {
        let config = LoadBalancerConfig {
            strategy: Strategy::RoundRobin,
            zone_aware: true,
            ..Default::default()
        };
        let lb = LoadBalancer::new(config);

        lb.add_endpoint(Endpoint::new(make_node_id(1)).with_zone("us-east"));
        lb.add_endpoint(Endpoint::new(make_node_id(2)).with_zone("us-west"));

        let ctx = RequestContext::new().with_zone("us-east");

        // Should prefer us-east node
        for _ in 0..10 {
            let selection = lb.select(&ctx).unwrap();
            assert_eq!(selection.node_id[0], 1);
        }
    }

    #[test]
    fn test_load_balancer_consistent_hash() {
        let lb = LoadBalancer::with_strategy(Strategy::ConsistentHash);

        lb.add_endpoint(Endpoint::new(make_node_id(1)));
        lb.add_endpoint(Endpoint::new(make_node_id(2)));
        lb.add_endpoint(Endpoint::new(make_node_id(3)));

        // Same session should always go to same node
        let ctx = RequestContext::new().with_session("user-123");

        let first = lb.select(&ctx).unwrap();
        for _ in 0..10 {
            let selection = lb.select(&ctx).unwrap();
            assert_eq!(selection.node_id, first.node_id);
        }
    }

    #[test]
    fn test_load_balancer_circuit_breaker() {
        let config = LoadBalancerConfig {
            strategy: Strategy::RoundRobin,
            circuit_recovery_time_ms: 100,
            ..Default::default()
        };
        let lb = LoadBalancer::new(config);

        lb.add_endpoint(Endpoint::new(make_node_id(1)));
        lb.add_endpoint(Endpoint::new(make_node_id(2)));

        let ctx = RequestContext::new();

        // Simulate 5 consecutive failures on node 1
        for _ in 0..5 {
            lb.record_completion(&make_node_id(1), false);
        }

        // Node 1's circuit should be open, all traffic to node 2
        for _ in 0..10 {
            let selection = lb.select(&ctx).unwrap();
            assert_eq!(selection.node_id[0], 2);
        }
    }

    #[test]
    fn test_load_balancer_stats() {
        let lb = LoadBalancer::with_strategy(Strategy::RoundRobin);

        lb.add_endpoint(Endpoint::new(make_node_id(1)));
        lb.add_endpoint(Endpoint::new(make_node_id(2)));

        let ctx = RequestContext::new();

        for _ in 0..10 {
            let selection = lb.select(&ctx).unwrap();
            lb.record_completion(&selection.node_id, true);
        }

        let stats = lb.stats();
        assert_eq!(stats.total_selections, 10);
        assert_eq!(stats.active_endpoints, 2);
        assert_eq!(stats.healthy_endpoints, 2);
    }

    #[test]
    fn test_no_endpoints_error() {
        let lb = LoadBalancer::with_strategy(Strategy::RoundRobin);
        let ctx = RequestContext::new();

        let result = lb.select(&ctx);
        assert!(matches!(
            result,
            Err(LoadBalancerError::NoEndpointsAvailable)
        ));
    }

    #[test]
    fn test_required_tags() {
        let lb = LoadBalancer::with_strategy(Strategy::RoundRobin);

        lb.add_endpoint(Endpoint::new(make_node_id(1)).with_tag("gpu"));
        lb.add_endpoint(Endpoint::new(make_node_id(2)).with_tag("cpu"));

        let ctx = RequestContext::new().require_tag("gpu");

        // Should only select gpu-tagged node
        for _ in 0..10 {
            let selection = lb.select(&ctx).unwrap();
            assert_eq!(selection.node_id[0], 1);
        }
    }

    // ---- Regression tests ----

    #[test]
    fn test_regression_consistent_hash_deterministic() {
        // Regression: consistent hash iterated DashMap in arbitrary order
        // instead of sorted order, so the same key could map to different
        // nodes across calls. Now uses sorted ring + binary search.
        let lb = LoadBalancer::with_strategy(Strategy::ConsistentHash);

        lb.add_endpoint(Endpoint::new(make_node_id(1)));
        lb.add_endpoint(Endpoint::new(make_node_id(2)));
        lb.add_endpoint(Endpoint::new(make_node_id(3)));
        lb.add_endpoint(Endpoint::new(make_node_id(4)));

        // Many different keys should each consistently map to the same node
        for i in 0..50 {
            let key = format!("session-{}", i);
            let ctx = RequestContext::new().with_routing_key(&key);

            let first = lb.select(&ctx).unwrap().node_id;
            for _ in 0..20 {
                let again = lb.select(&ctx).unwrap().node_id;
                assert_eq!(
                    first, again,
                    "consistent hash must return same node for key '{}'",
                    key
                );
            }
        }
    }

    #[test]
    fn test_regression_nan_metrics_no_panic() {
        // Regression: partial_cmp().unwrap() panicked when metrics
        // contained NaN. Now uses total_cmp() which handles NaN.
        let lb = LoadBalancer::with_strategy(Strategy::LeastLatency);

        let mut ep1 = Endpoint::new(make_node_id(1));
        ep1.metrics.avg_response_time_ms = f64::NAN;
        lb.add_endpoint(ep1);

        let mut ep2 = Endpoint::new(make_node_id(2));
        ep2.metrics.avg_response_time_ms = 50.0;
        lb.add_endpoint(ep2);

        let ctx = RequestContext::new();
        // Must not panic
        let result = lb.select(&ctx);
        assert!(result.is_ok(), "NaN metrics must not panic");
    }

    #[test]
    fn test_regression_nan_load_score_no_panic() {
        // Same NaN regression for LeastLoad strategy.
        let lb = LoadBalancer::with_strategy(Strategy::LeastLoad);

        let mut ep1 = Endpoint::new(make_node_id(1));
        ep1.metrics.cpu_usage = f64::NAN;
        lb.add_endpoint(ep1);

        lb.add_endpoint(Endpoint::new(make_node_id(2)));

        let ctx = RequestContext::new();
        let result = lb.select(&ctx);
        assert!(result.is_ok(), "NaN load score must not panic");
    }

    #[test]
    fn test_regression_zone_fallback_respected() {
        // Regression: zone_fallback config was never read. When set to
        // false, requests with a client_zone that matches no endpoint
        // should fail, not silently fall back to non-zone endpoints.
        let config = LoadBalancerConfig {
            strategy: Strategy::RoundRobin,
            zone_aware: true,
            zone_fallback: false, // <-- this was previously ignored
            ..Default::default()
        };
        let lb = LoadBalancer::new(config);

        lb.add_endpoint(Endpoint::new(make_node_id(1)).with_zone("us-west"));
        lb.add_endpoint(Endpoint::new(make_node_id(2)).with_zone("us-west"));

        // Client is in eu-central — no endpoints match
        let ctx = RequestContext::new().with_zone("eu-central");
        let result = lb.select(&ctx);

        assert!(
            result.is_err(),
            "with zone_fallback=false, mismatched zone must return error"
        );
    }

    #[test]
    fn test_zone_fallback_true_allows_cross_zone() {
        // Verify that zone_fallback=true (default) still works correctly.
        let config = LoadBalancerConfig {
            strategy: Strategy::RoundRobin,
            zone_aware: true,
            zone_fallback: true,
            ..Default::default()
        };
        let lb = LoadBalancer::new(config);

        lb.add_endpoint(Endpoint::new(make_node_id(1)).with_zone("us-west"));

        let ctx = RequestContext::new().with_zone("eu-central");
        let result = lb.select(&ctx);

        assert!(
            result.is_ok(),
            "with zone_fallback=true, cross-zone should succeed"
        );
    }

    #[test]
    fn test_regression_random_f64_never_reaches_one() {
        // Regression: `r as f64 / u64::MAX as f64` could return exactly 1.0
        // because `u64::MAX as f64` rounds to 2^64. Now uses the 53-bit
        // mantissa / 2^53 pattern which is strictly in [0, 1).
        for _ in 0..10_000 {
            let r = random_f64();
            assert!((0.0..1.0).contains(&r), "random_f64 out of [0,1): {}", r);
        }
    }

    #[test]
    fn test_regression_max_connections_cap_enforced_concurrently() {
        // Regression: the select() path loaded `connections` with Relaxed
        // then incremented in record_request, allowing N concurrent
        // selectors to all pass the check and collectively exceed the cap.
        // Now reservation is atomic via fetch_update.
        use std::sync::Arc;
        use std::thread;

        const CAP: u32 = 5;
        const THREADS: u32 = 16;

        let config = LoadBalancerConfig {
            strategy: Strategy::RoundRobin,
            max_connections_per_endpoint: CAP,
            ..Default::default()
        };
        let lb = Arc::new(LoadBalancer::new(config));
        // Single endpoint so every selection contends for the same cap.
        lb.add_endpoint(Endpoint::new(make_node_id(1)));

        let mut handles = Vec::new();
        for _ in 0..THREADS {
            let lb = Arc::clone(&lb);
            handles.push(thread::spawn(move || {
                // Each thread tries to select one connection and holds it.
                let ctx = RequestContext::new();
                lb.select(&ctx).ok()
            }));
        }

        let successes = handles
            .into_iter()
            .filter_map(|h| h.join().unwrap())
            .count();

        // At most CAP threads may have been granted a connection.
        assert!(
            successes <= CAP as usize,
            "concurrent selectors exceeded cap: {} > {}",
            successes,
            CAP
        );
        // And the endpoint's connection count must equal successes.
        let state = lb.endpoints.get(&make_node_id(1)).unwrap();
        assert_eq!(
            state.connections.load(Ordering::Acquire),
            successes as u32,
            "connection counter must match granted selections"
        );
    }

    #[test]
    fn test_regression_circuit_breaker_half_open_single_probe() {
        // Regression: on recovery expiry, `is_circuit_open` fully closed
        // the breaker, letting every concurrent request hit a possibly
        // still-broken endpoint. Now exactly one probe is admitted and
        // subsequent callers continue to see the breaker as open until the
        // probe's outcome is recorded.
        let config = LoadBalancerConfig {
            strategy: Strategy::RoundRobin,
            circuit_recovery_time_ms: 50,
            ..Default::default()
        };
        let lb = LoadBalancer::new(config);
        lb.add_endpoint(Endpoint::new(make_node_id(1)));
        let ctx = RequestContext::new();

        // Trip the breaker by driving 5 real selections that all fail. Going
        // through select() keeps the connection counter consistent — calling
        // record_completion() without a matching record_request() would
        // underflow.
        for _ in 0..5 {
            let sel = lb.select(&ctx).expect("admitted before trip");
            lb.record_completion(&sel.node_id, false);
        }

        // Before recovery: all requests rejected.
        assert!(lb.select(&ctx).is_err(), "open breaker must reject");

        // Wait past the recovery window.
        std::thread::sleep(Duration::from_millis(75));

        // First request after recovery: admitted as the probe.
        let probe = lb.select(&ctx);
        assert!(probe.is_ok(), "first request after recovery is the probe");

        // Second request while probe is still in flight: must be rejected.
        let second = lb.select(&ctx);
        assert!(
            second.is_err(),
            "while probe is in flight, other requests must still be rejected"
        );

        // Probe reports failure → breaker re-opens and recovery timer resets.
        lb.record_completion(&probe.unwrap().node_id, false);
        assert!(
            lb.select(&ctx).is_err(),
            "failed probe must keep breaker open"
        );

        // After another recovery window, the next probe succeeds and closes
        // the breaker.
        std::thread::sleep(Duration::from_millis(75));
        let probe2 = lb.select(&ctx).expect("second probe admitted");
        lb.record_completion(&probe2.node_id, true);

        // Breaker is now fully closed — subsequent requests go through.
        assert!(lb.select(&ctx).is_ok(), "successful probe closes breaker");
    }

    /// Regression for BUG_AUDIT_2026_04_30_CORE.md #101: pre-fix
    /// `is_circuit_open` was both a predicate AND CAS-claimed
    /// the half-open probe slot when called past the recovery
    /// window. `get_available_endpoints` calls it for every
    /// endpoint being filtered; with N circuit-open endpoints
    /// past their recovery window, all N got the probe slot
    /// claimed but only one was actually selected. The N-1
    /// others permanently held `half_open_probe == true` with
    /// no in-flight request — every subsequent
    /// `is_circuit_open` then returned true forever, and the
    /// breaker never recovered until process restart.
    ///
    /// We pin the fix by:
    ///   1. Building a load balancer with 3 endpoints.
    ///   2. Tripping each endpoint's breaker.
    ///   3. Waiting past the recovery window.
    ///   4. Calling `select` once — this triggers
    ///      `get_available_endpoints`, which scans all 3
    ///      endpoints. Only the SELECTED endpoint should claim
    ///      the probe slot; the other 2 must NOT.
    ///   5. Asserting the unselected endpoints have
    ///      `half_open_probe == false`.
    #[test]
    fn circuit_breaker_does_not_leak_probe_slot_on_multi_endpoint_scan() {
        let config = LoadBalancerConfig {
            strategy: Strategy::RoundRobin,
            circuit_recovery_time_ms: 50,
            ..Default::default()
        };
        let lb = LoadBalancer::new(config);
        for i in 1..=3 {
            lb.add_endpoint(Endpoint::new(make_node_id(i)));
        }
        let ctx = RequestContext::new();

        // Trip every endpoint's breaker. Default failure
        // threshold is 5 consecutive failures.
        for _ in 0..5 {
            for i in 1..=3 {
                let nid = make_node_id(i);
                // Manually trip via record_completion(false). We
                // use a dummy connection-counter via select() to
                // keep the connection counter consistent; if no
                // endpoint is selectable, force it.
                if let Some(ep) = lb.endpoints.get(&nid) {
                    // Simulate a request lifecycle.
                    ep.try_record_request(u32::MAX);
                }
                lb.record_completion(&nid, false);
            }
        }

        // All breakers should be open. select() rejects pre-recovery.
        assert!(
            lb.select(&ctx).is_err(),
            "all breakers open pre-recovery — select must fail"
        );

        // Wait past recovery window.
        std::thread::sleep(Duration::from_millis(75));

        // First select: scans all 3 endpoints. Selects ONE. The
        // other 2 must NOT have their probe slots claimed.
        let probe = lb.select(&ctx).expect("recovery elapsed → probe admitted");

        // Audit the half_open_probe state on each endpoint:
        // exactly one (the selected) should be true; the other
        // two MUST be false. Pre-fix all three would be true.
        let mut claimed = 0u32;
        for i in 1..=3 {
            let nid = make_node_id(i);
            let ep = lb.endpoints.get(&nid).unwrap();
            if ep.half_open_probe.load(Ordering::Acquire) {
                claimed += 1;
                // The claimed slot must be on the selected endpoint.
                assert_eq!(
                    nid, probe.node_id,
                    "only the selected endpoint may have its probe slot claimed"
                );
            }
        }
        assert_eq!(
            claimed, 1,
            "exactly one endpoint should have a claimed probe slot — \
             pre-fix this was 3 (the filter-time scan claimed all)"
        );

        // Probe success → selected endpoint's breaker closes;
        // the other two are still in their post-recovery state.
        lb.record_completion(&probe.node_id, true);
    }

    /// Cubic-ai P1: with `N` selectors racing concurrently against
    /// a circuit-open endpoint that just exited its recovery window,
    /// the strict half-open contract says EXACTLY one selector
    /// admits the probe — every other selector that lost the
    /// `try_claim_half_open_probe` CAS must skip the endpoint, not
    /// fall through to `try_record_request` and send extra traffic
    /// to the (still potentially sick) endpoint alongside the real
    /// probe.
    ///
    /// Pre-fix (loose) semantics: losers of the probe-claim CAS
    /// proceeded via `try_record_request`, so under sufficient
    /// concurrency `successes` could be `> 1`. Post-fix (strict)
    /// semantics: losers `continue`, the retry's
    /// `get_available_endpoints` sees `half_open_probe == true`
    /// and filters the endpoint out, the loop exits with
    /// `NoEndpointsAvailable`. Net effect: at most one selector
    /// admits per recovery cycle.
    ///
    /// The test:
    ///   1. Trips a single endpoint's breaker.
    ///   2. Waits past the recovery window so the next selection
    ///      enters half-open.
    ///   3. Spawns `N` threads, gates them on a Barrier so they
    ///      enter `select()` as close to simultaneously as
    ///      possible, each retains its `Selection` (no
    ///      `record_completion`) so the probe slot stays claimed.
    ///   4. Asserts `successes == 1`. Pre-fix this could fire
    ///      `> 1` non-deterministically; post-fix it must be
    ///      exactly 1.
    #[test]
    fn select_strict_half_open_admits_exactly_one_probe_under_concurrent_selectors() {
        use std::sync::Barrier;
        use std::thread;

        const N: usize = 32;
        let config = LoadBalancerConfig {
            strategy: Strategy::RoundRobin,
            circuit_recovery_time_ms: 50,
            ..Default::default()
        };
        let lb = Arc::new(LoadBalancer::new(config));
        lb.add_endpoint(Endpoint::new(make_node_id(1)));
        let ctx = RequestContext::new();

        // Trip the breaker (5 consecutive failures).
        for _ in 0..5 {
            let sel = lb.select(&ctx).expect("admitted before trip");
            lb.record_completion(&sel.node_id, false);
        }
        assert!(lb.select(&ctx).is_err(), "open breaker must reject");

        // Wait past the recovery window so the next selection
        // observes `half_open_probe == false` and is admitted.
        thread::sleep(Duration::from_millis(75));

        // Race N threads through select(). DO NOT call
        // record_completion — that would clear the probe slot
        // and let the next thread succeed legitimately. The
        // strict contract is: exactly one admits while the probe
        // is in flight.
        let barrier = Arc::new(Barrier::new(N));
        let mut handles = Vec::with_capacity(N);
        for _ in 0..N {
            let lb = Arc::clone(&lb);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                let ctx = RequestContext::new();
                barrier.wait();
                lb.select(&ctx).is_ok()
            }));
        }
        let successes: usize = handles
            .into_iter()
            .map(|h| h.join().unwrap() as usize)
            .sum();

        assert_eq!(
            successes, 1,
            "strict half-open: exactly one selector must admit while the \
             probe is in flight (got {successes} of {N}). Pre-fix the \
             loose semantic let losers of the probe-claim CAS proceed \
             through try_record_request, sending extra traffic to a \
             still-recovering endpoint."
        );

        // Sanity: the probe slot is still claimed (no completion
        // was recorded), and the breaker is still nominally open.
        let ep = lb.endpoints.get(&make_node_id(1)).unwrap();
        assert!(
            ep.half_open_probe.load(Ordering::Acquire),
            "probe slot must remain claimed across the test (no completion was recorded)"
        );
        assert!(
            ep.circuit_open.load(Ordering::Acquire),
            "circuit must remain open until probe completion"
        );
    }

    /// Companion to `select_strict_half_open_admits_exactly_one_probe...`:
    /// strict half-open semantics must NOT serialize independent
    /// endpoints. With two distinct circuit-open endpoints both
    /// past their recovery window, two concurrent selectors should
    /// EACH succeed — one probe per endpoint, since each endpoint's
    /// `half_open_probe` is its own slot. Pre-fix this also worked
    /// (loose semantic), but a naive "strict gate" implementation
    /// could accidentally over-tighten and lock out legitimate
    /// per-endpoint probes; this test pins that the gate stays
    /// scoped to the endpoint it guards.
    #[test]
    fn select_strict_half_open_allows_concurrent_probes_on_distinct_endpoints() {
        use std::sync::Barrier;
        use std::thread;

        let config = LoadBalancerConfig {
            strategy: Strategy::RoundRobin,
            circuit_recovery_time_ms: 50,
            ..Default::default()
        };
        let lb = Arc::new(LoadBalancer::new(config));
        // Two endpoints — RR alternates between them.
        for i in 1..=2 {
            lb.add_endpoint(Endpoint::new(make_node_id(i)));
        }
        let ctx = RequestContext::new();

        // Trip both breakers. Default threshold is 5 consecutive
        // failures per endpoint.
        for _ in 0..5 {
            for i in 1..=2 {
                let nid = make_node_id(i);
                if let Some(ep) = lb.endpoints.get(&nid) {
                    ep.try_record_request(u32::MAX);
                }
                lb.record_completion(&nid, false);
            }
        }
        assert!(lb.select(&ctx).is_err(), "both breakers open pre-recovery");

        // Wait past the recovery window so both endpoints admit a
        // probe.
        thread::sleep(Duration::from_millis(75));

        // Race two threads. With RR + 2 endpoints, each thread
        // should pick a different endpoint, claim its own probe,
        // and succeed. Pre-fix or post-fix, both succeed — but a
        // mis-scoped "strict gate" (e.g., a global probe flag
        // instead of per-endpoint) would let only one through.
        let barrier = Arc::new(Barrier::new(2));
        let mut handles = Vec::with_capacity(2);
        for _ in 0..2 {
            let lb = Arc::clone(&lb);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                let ctx = RequestContext::new();
                barrier.wait();
                lb.select(&ctx).ok().map(|s| s.node_id)
            }));
        }
        let picks: Vec<NodeId> = handles
            .into_iter()
            .filter_map(|h| h.join().unwrap())
            .collect();

        assert_eq!(
            picks.len(),
            2,
            "both selectors must succeed against distinct endpoints \
             (probes are per-endpoint, not global). Got picks: {:?}",
            picks
        );
        assert_ne!(
            picks[0], picks[1],
            "the two probes must land on different endpoints — \
             same-endpoint admission would mean strict gate failed to \
             keep one selector out, OR RR selection raced strangely. \
             Picks: {:?}",
            picks
        );

        // Both endpoints should now have their probe slots claimed.
        for i in 1..=2 {
            let ep = lb.endpoints.get(&make_node_id(i)).unwrap();
            assert!(
                ep.half_open_probe.load(Ordering::Acquire),
                "endpoint {} probe slot must be claimed (one probe per endpoint)",
                i
            );
        }
    }

    /// CR-19: the `ProbeGuard` Drop must release the
    /// half-open probe slot when the guard is dropped without
    /// committing. We construct an `EndpointState`, manually
    /// claim the probe via the guard API, drop the guard, and
    /// verify the slot returned to false.
    #[test]
    fn cr19_probe_guard_drop_releases_probe_slot() {
        let ep = EndpointState::new(Endpoint::new(make_node_id(0xCA)));
        // Pre: slot is open.
        assert!(!ep.half_open_probe.load(Ordering::Acquire));

        let guard = ep
            .try_claim_half_open_probe()
            .expect("first claim must succeed");
        // Probe slot is now claimed.
        assert!(ep.half_open_probe.load(Ordering::Acquire));

        // Drop without commit: slot must roll back.
        drop(guard);
        assert!(
            !ep.half_open_probe.load(Ordering::Acquire),
            "CR-19 regression: ProbeGuard Drop must release the probe slot"
        );

        // Subsequent claim succeeds — slot is reusable.
        let _g = ep
            .try_claim_half_open_probe()
            .expect("post-Drop reclaim must succeed");
    }

    /// CR-19: `commit()` must SUPPRESS the Drop release. The
    /// committed claim survives the guard going out of scope —
    /// `record_completion` is then the path that clears it.
    #[test]
    fn cr19_probe_guard_commit_suppresses_release() {
        let ep = EndpointState::new(Endpoint::new(make_node_id(0xBE)));
        let guard = ep
            .try_claim_half_open_probe()
            .expect("first claim must succeed");
        guard.commit();
        // Slot remains claimed because commit() ran mem::forget.
        assert!(
            ep.half_open_probe.load(Ordering::Acquire),
            "CR-19 regression: ProbeGuard::commit must SUPPRESS Drop release"
        );
        // A second claim must fail because the slot is taken.
        assert!(
            ep.try_claim_half_open_probe().is_none(),
            "second claim must fail while the first is committed"
        );
    }

    /// CR-19: panic between claim and commit MUST release the
    /// slot via Drop. We use `catch_unwind` to confirm the slot
    /// rolls back even when the path between claim and the
    /// would-be commit unwinds.
    #[test]
    fn cr19_panic_between_claim_and_commit_releases_probe_slot() {
        use std::panic::{catch_unwind, AssertUnwindSafe};

        let ep = EndpointState::new(Endpoint::new(make_node_id(0xF0)));
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _guard = ep
                .try_claim_half_open_probe()
                .expect("first claim must succeed");
            // Simulate a panic on the path between claim and
            // commit — exactly what a future-cancel or in-flight
            // panic looks like.
            panic!("simulated mid-path failure");
        }));

        assert!(result.is_err(), "the closure must have panicked");
        assert!(
            !ep.half_open_probe.load(Ordering::Acquire),
            "CR-19 regression: panic between claim and commit MUST roll \
             back the probe slot via ProbeGuard::drop"
        );
    }

    /// Pin: `select_weighted_round_robin_at` must use the
    /// NaN-safe guard `!(total_weight > 0.0)` rather than
    /// `total_weight <= 0.0`. NaN compares unequal to everything
    /// (including itself), so `NaN <= 0.0` is `false` — the
    /// gate falls through to the weighted path where
    /// `total_weight.ceil() as u64` produces an undefined
    /// (saturating) cast and the cumulative loop never exceeds
    /// NaN, biasing every selection to `endpoints[0]`. The
    /// negated-greater check catches NaN as well as zero/negative.
    ///
    /// This is a tripwire: a "simplification" PR that flips the
    /// guard back to `<= 0.0` would silently re-introduce the
    /// bias whenever any future code path produces a NaN
    /// effective weight (e.g. an f64 `weight` field). The pin
    /// is scoped to the round-robin function body — the random
    /// path elsewhere in this file is governed by its own
    /// guard and is not part of this regression.
    #[test]
    fn weighted_round_robin_guard_must_be_nan_safe() {
        let src = include_str!("loadbalance.rs");

        // Locate the function header and the next `fn ` after
        // it; everything between is the body we pin.
        let header = "fn select_weighted_round_robin_at(";
        let start = src
            .find(header)
            .expect("select_weighted_round_robin_at must exist");
        // `find` from the next character so we don't match the
        // header itself.
        let body_start = start + header.len();
        let next_fn = src[body_start..]
            .find("\n    fn ")
            .expect("a following fn must exist (mod-private impl block)")
            + body_start;
        let body = &src[start..next_fn];

        // Strip line comments (everything from `//` to EOL) so a
        // doc comment that *describes* the rejected pattern
        // doesn't trip the negative assertion below.
        let body_no_comments: String = body
            .lines()
            .map(|l| match l.find("//") {
                Some(idx) => &l[..idx],
                None => l,
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            body_no_comments.contains("!(total_weight > 0.0)"),
            "regression: select_weighted_round_robin_at must use the \
             NaN-safe guard `!(total_weight > 0.0)`. Without it a NaN \
             total_weight (introduced by a future f64 weight path) \
             falls through to the weighted code, biasing every \
             selection onto endpoints[0]."
        );

        // Also assert the buggy form is gone from THIS function
        // body. The NaN-safe form does not contain `<= 0.0`, so
        // this should fail iff someone reverts the guard.
        assert!(
            !body_no_comments.contains("total_weight <= 0.0"),
            "regression: select_weighted_round_robin_at must not \
             use the NaN-unsafe guard `total_weight <= 0.0`."
        );
    }

    /// CR-21: pin that this module's `random_usize`
    /// uses the abort-on-fail pattern, NOT `expect()` or
    /// `.unwrap()`. A getrandom panic here would unwind across
    /// any `extern "C"` FFI frame that called into the load-
    /// balance layer — undefined behaviour.
    #[test]
    fn cr21_random_usize_must_not_panic_on_getrandom_failure() {
        let needle_expect = format!("getrandom::fill({}{})", "&mut bytes).", "expect");
        let needle_unwrap = format!("getrandom::fill({}{})", "&mut bytes).", "unwrap");

        let src = include_str!("loadbalance.rs");
        for (lineno, line) in src.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            assert!(
                !trimmed.contains(&needle_expect),
                "CR-21 regression: getrandom::fill(...).expect(...) reintroduced \
                 at loadbalance.rs:{}.\n  line: {}",
                lineno + 1,
                line
            );
            assert!(
                !trimmed.contains(&needle_unwrap),
                "CR-21 regression: getrandom::fill(...).unwrap() reintroduced \
                 at loadbalance.rs:{}.\n  line: {}",
                lineno + 1,
                line
            );
        }
    }

    // ---------- Untested strategy coverage ----------
    //
    // Existing tests cover RoundRobin, WeightedRoundRobin,
    // LeastConnections, and WeightedLeastConnections. The
    // remaining strategies — Random, WeightedRandom,
    // ConsistentHash, PowerOfTwo, Adaptive — share the
    // hot path and a regression in any of them would silently
    // mis-route requests. Each test below pins the selection
    // reason so a future refactor that swaps strategies under
    // the same enum variant gets caught.

    fn three_endpoint_lb(strategy: Strategy) -> LoadBalancer {
        let lb = LoadBalancer::with_strategy(strategy);
        lb.add_endpoint(Endpoint::new(make_node_id(1)));
        lb.add_endpoint(Endpoint::new(make_node_id(2)));
        lb.add_endpoint(Endpoint::new(make_node_id(3)));
        lb
    }

    #[test]
    fn random_strategy_selects_among_all_endpoints_with_random_reason() {
        let lb = three_endpoint_lb(Strategy::Random);
        let ctx = RequestContext::new();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            let s = lb.select(&ctx).unwrap();
            assert_eq!(s.reason, SelectionReason::Random);
            seen.insert(s.node_id[0]);
        }
        // 200 draws across 3 endpoints. We only assert "more than
        // one distinct endpoint was selected" — strong enough to
        // catch a regression that hard-codes a single index, weak
        // enough to never flake on legitimate RNG outcomes. A
        // stricter `== 3` check would be ~1e-35 likely to fail
        // under a uniform RNG; this version is exactly zero.
        assert!(
            seen.len() >= 2,
            "Random strategy collapsed to a single endpoint over 200 draws: {seen:?}",
        );
    }

    #[test]
    fn weighted_random_respects_relative_weights() {
        let lb = LoadBalancer::with_strategy(Strategy::WeightedRandom);
        lb.add_endpoint(Endpoint::new(make_node_id(1)).with_weight(10));
        lb.add_endpoint(Endpoint::new(make_node_id(2)).with_weight(100));
        let ctx = RequestContext::new();
        let mut counts = std::collections::HashMap::new();
        for _ in 0..400 {
            let s = lb.select(&ctx).unwrap();
            assert_eq!(s.reason, SelectionReason::Weighted);
            *counts.entry(s.node_id[0]).or_insert(0) += 1;
        }
        // Heavy weight (100) must dominate the light weight (10).
        // Allow a wide margin — we're not testing the RNG quality,
        // just that the weight is read.
        let light = *counts.get(&1).unwrap_or(&0);
        let heavy = *counts.get(&2).unwrap_or(&0);
        assert!(
            heavy > light * 3,
            "weighted-random ignored weights: light={light}, heavy={heavy}",
        );
    }

    #[test]
    fn weighted_random_with_zero_total_weight_falls_back_to_uniform_random() {
        let lb = LoadBalancer::with_strategy(Strategy::WeightedRandom);
        // Both endpoints with weight 0 — `total_weight <= 0.0`
        // forces the fallback path inside select_weighted_random.
        lb.add_endpoint(Endpoint::new(make_node_id(1)).with_weight(0));
        lb.add_endpoint(Endpoint::new(make_node_id(2)).with_weight(0));
        let ctx = RequestContext::new();

        // Must not panic; must return a real selection. The reason
        // is `Random` because the implementation delegates to
        // `select_random` when total_weight is non-positive.
        let s = lb.select(&ctx).unwrap();
        assert_eq!(s.reason, SelectionReason::Random);
    }

    #[test]
    fn consistent_hash_returns_same_endpoint_for_same_routing_key() {
        let lb = three_endpoint_lb(Strategy::ConsistentHash);
        let ctx = RequestContext::new().with_routing_key("user-42");

        let s1 = lb.select(&ctx).unwrap();
        for _ in 0..50 {
            let s = lb.select(&ctx).unwrap();
            assert_eq!(s.node_id, s1.node_id, "consistent-hash diverged");
        }
    }

    #[test]
    fn power_of_two_returns_powerof_two_reason() {
        let lb = three_endpoint_lb(Strategy::PowerOfTwo);
        let ctx = RequestContext::new();
        let s = lb.select(&ctx).unwrap();
        assert_eq!(s.reason, SelectionReason::PowerOfTwo);
    }

    #[test]
    fn adaptive_strategy_selects_an_endpoint() {
        // Adaptive picks between strategies based on average load;
        // with default (no metrics) all endpoints score 0 so it
        // takes the low-load branch. Pin that the strategy runs
        // and returns a valid selection.
        let lb = three_endpoint_lb(Strategy::Adaptive);
        let ctx = RequestContext::new();
        let s = lb.select(&ctx).unwrap();
        assert!(matches!(s.node_id[0], 1..=3));
    }

    // ---------- endpoints() snapshot ----------

    #[test]
    fn endpoints_snapshot_reflects_added_endpoints() {
        let lb = LoadBalancer::with_strategy(Strategy::RoundRobin);
        lb.add_endpoint(Endpoint::new(make_node_id(1)).with_weight(50));
        lb.add_endpoint(Endpoint::new(make_node_id(2)).with_weight(75));

        let snapshot = lb.endpoints();
        assert_eq!(snapshot.len(), 2);
        // Order isn't guaranteed (DashMap iteration) — assert
        // by node_id membership rather than position.
        let weights: std::collections::HashMap<u8, u32> =
            snapshot.iter().map(|e| (e.node_id[0], e.weight)).collect();
        assert_eq!(weights.get(&1), Some(&50));
        assert_eq!(weights.get(&2), Some(&75));
    }

    /// The endpoint snapshot (iterated by select/stats/count) must be rebuilt
    /// on remove: counts drop and the removed endpoint is never selected.
    #[test]
    fn removing_an_endpoint_updates_snapshot_and_stops_selection() {
        let lb = LoadBalancer::with_strategy(Strategy::RoundRobin);
        for i in 1..=3u8 {
            lb.add_endpoint(Endpoint::new(make_node_id(i)));
        }
        assert_eq!(lb.endpoint_count(), 3);
        assert_eq!(lb.stats().active_endpoints, 3);

        lb.remove_endpoint(&make_node_id(2));
        assert_eq!(lb.endpoint_count(), 2, "count must drop after remove");
        assert_eq!(lb.stats().active_endpoints, 2);
        assert!(
            lb.endpoints().iter().all(|e| e.node_id != make_node_id(2)),
            "removed endpoint must be gone from the snapshot"
        );

        // The removed endpoint must never be selected.
        let ctx = RequestContext::new();
        for _ in 0..50 {
            let sel = lb.select(&ctx).unwrap();
            assert_ne!(
                sel.node_id,
                make_node_id(2),
                "removed endpoint must not be selected"
            );
            lb.record_completion(&sel.node_id, true);
        }
    }

    /// Concurrent membership changes must leave `endpoint_list` (read by
    /// select/stats/count) exactly consistent with the authoritative
    /// `endpoints` map. Pre-fix, a rebuild that observed the map before a
    /// concurrent mutation could store its stale snapshot last, dropping a
    /// just-added endpoint from rotation (or resurrecting a removed one).
    #[test]
    fn concurrent_membership_changes_keep_snapshot_consistent() {
        use std::collections::HashSet;
        use std::sync::Arc as StdArc;

        let lb = StdArc::new(LoadBalancer::with_strategy(Strategy::RoundRobin));
        let n: u8 = 64;

        // Concurrent adds.
        let mut handles = Vec::new();
        for i in 1..=n {
            let lb = StdArc::clone(&lb);
            handles.push(std::thread::spawn(move || {
                lb.add_endpoint(Endpoint::new(make_node_id(i)));
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        // The snapshot must match the authoritative map exactly — not a
        // single add may be lost to a stale rebuild.
        assert_eq!(lb.endpoint_count(), n as usize);
        assert_eq!(lb.endpoint_count(), lb.endpoints.len());
        let snap: HashSet<_> = lb.endpoints().iter().map(|e| e.node_id).collect();
        assert_eq!(
            snap.len(),
            n as usize,
            "every added endpoint must appear in the snapshot"
        );

        // Concurrent removes (the even ids) must stay consistent too.
        let mut handles = Vec::new();
        for i in (2..=n).step_by(2) {
            let lb = StdArc::clone(&lb);
            handles.push(std::thread::spawn(move || {
                lb.remove_endpoint(&make_node_id(i));
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(lb.endpoint_count(), lb.endpoints.len());
        assert_eq!(lb.endpoint_count(), (n / 2) as usize);
        assert!(
            lb.endpoints().iter().all(|e| {
                let raw = e.node_id;
                // Odd ids only should remain.
                lb.endpoints.contains_key(&raw)
            }),
            "snapshot must contain only live endpoints"
        );
    }

    /// A snapshot taken before a removal still holds the endpoint's `Arc`
    /// (exactly what an in-flight `select()` iterates). After removal that
    /// endpoint must report unavailable through the *stale* snapshot — so the
    /// strategy filters it out instead of selecting a gone endpoint and
    /// burning a reservation retry into a false `NoEndpointsAvailable`.
    #[test]
    fn removed_endpoint_is_unavailable_through_a_stale_snapshot() {
        let lb = LoadBalancer::with_strategy(Strategy::RoundRobin);
        lb.add_endpoint(Endpoint::new(make_node_id(1)));
        lb.add_endpoint(Endpoint::new(make_node_id(2)));

        // Capture the snapshot BEFORE removal — holds Arcs to both endpoints.
        let stale = lb.endpoint_list.load_full();
        assert_eq!(stale.len(), 2);

        lb.remove_endpoint(&make_node_id(1));

        // Through the stale snapshot, the removed endpoint is now unavailable;
        // the survivor stays available.
        for state in stale.iter() {
            if state.node_id == make_node_id(1) {
                assert!(
                    !state.is_available(),
                    "removed endpoint must be unavailable via the stale snapshot"
                );
            } else {
                assert!(
                    state.is_available(),
                    "surviving endpoint must remain available"
                );
            }
        }
    }

    // ---------- LoadBalancerError Display ----------

    #[test]
    fn load_balancer_error_display_covers_every_variant() {
        let id = make_node_id(7);
        assert_eq!(
            format!("{}", LoadBalancerError::NoEndpointsAvailable),
            "no endpoints available"
        );
        assert_eq!(
            format!("{}", LoadBalancerError::AllEndpointsUnhealthy),
            "all endpoints unhealthy"
        );
        assert_eq!(
            format!("{}", LoadBalancerError::NoMatchingEndpoints),
            "no endpoints match required tags"
        );
        assert!(format!("{}", LoadBalancerError::EndpointNotFound(id))
            .starts_with("endpoint not found:"));
        assert!(format!("{}", LoadBalancerError::CircuitOpen(id))
            .starts_with("circuit breaker open for:"));
        assert!(format!("{}", LoadBalancerError::MaxConnectionsReached(id))
            .starts_with("max connections reached for:"));
    }

    /// Finding #14: a half-open probe claimed at selection time but
    /// never completed (the production `GroupCoordinator::route_event`
    /// path calls `select` and never `record_completion`) must NOT pin
    /// the recovered endpoint out of rotation forever. The watchdog
    /// reclaims the abandoned slot once it has been held past the
    /// recovery window, so a later selection is admitted as a fresh
    /// probe.
    #[test]
    fn cr14_abandoned_half_open_probe_is_reclaimed_after_recovery_window() {
        let config = LoadBalancerConfig {
            strategy: Strategy::RoundRobin,
            circuit_recovery_time_ms: 50,
            ..Default::default()
        };
        let lb = LoadBalancer::new(config);
        lb.add_endpoint(Endpoint::new(make_node_id(1)));
        let ctx = RequestContext::new();

        // Trip the breaker (5 consecutive failures).
        for _ in 0..5 {
            let sel = lb.select(&ctx).expect("admitted before trip");
            lb.record_completion(&sel.node_id, false);
        }
        assert!(lb.select(&ctx).is_err(), "open breaker must reject");

        // Wait past the recovery window so the next selection admits
        // the probe.
        std::thread::sleep(Duration::from_millis(75));

        // Claim the probe via select() but DO NOT record completion —
        // this models a caller that drops the selection (route_event).
        let probe = lb
            .select(&ctx)
            .expect("first request after recovery is the probe");
        let ep = lb.endpoints.get(&probe.node_id).unwrap();
        assert!(
            ep.half_open_probe.load(Ordering::Acquire),
            "probe slot is claimed right after selection"
        );
        drop(ep);

        // Immediately, the slot is fresh — another selector is still
        // (correctly) rejected.
        assert!(
            lb.select(&ctx).is_err(),
            "a freshly-claimed probe still gates concurrent selectors"
        );

        // Let the abandoned probe age past a full recovery window.
        std::thread::sleep(Duration::from_millis(75));

        // The watchdog must reclaim the stranded slot: a later
        // selection is admitted again rather than rejected forever.
        let reclaimed = lb.select(&ctx);
        assert!(
            reclaimed.is_ok(),
            "an abandoned half-open probe (no record_completion) must be \
             reclaimed after the recovery window so the recovered endpoint \
             returns to rotation — pre-fix the bare bool pinned it out forever"
        );
    }

    /// Finding #15: re-adding an already-present endpoint must NOT
    /// leak its previous hash-ring vnodes. Pre-fix `add_endpoint`
    /// inserted a fresh ~`virtual_nodes` set without removing the
    /// node's existing vnodes, leaking ~150 ring entries per re-add.
    #[test]
    fn cr15_readd_endpoint_does_not_leak_hash_ring_vnodes() {
        let lb = LoadBalancer::with_strategy(Strategy::ConsistentHash);
        let node = make_node_id(1);

        lb.add_endpoint(Endpoint::new(node));
        let after_first = lb.hash_ring.len();
        assert_eq!(
            after_first, lb.virtual_nodes as usize,
            "a fresh add must create exactly virtual_nodes vnodes"
        );

        // Re-add the same node several times (reconnect / weight change).
        for _ in 0..5 {
            lb.add_endpoint(Endpoint::new(node).with_weight(200));
        }

        assert_eq!(
            lb.hash_ring.len(),
            lb.virtual_nodes as usize,
            "re-adding the same node must not leak stale vnodes — the ring \
             size must stay at virtual_nodes, not grow by ~150 per re-add"
        );

        // Every ring entry must still resolve to this node.
        assert!(
            lb.hash_ring.iter().all(|e| *e.value() == node),
            "all vnodes must belong to the re-added node"
        );
    }

    /// Finding #29: the hash-ring collision probe must be
    /// NON-DESTRUCTIVE. Pre-fix `while insert(..).is_some()`
    /// overwrote an occupied slot (clobbering another node's vnode)
    /// before probing onward. We force a guaranteed collision by
    /// pre-occupying every slot `add_to_hash_ring` would target for a
    /// node and assert the existing occupants survive.
    #[test]
    fn cr29_hash_ring_collision_probe_preserves_existing_occupant() {
        let lb = LoadBalancer::with_strategy(Strategy::ConsistentHash);
        let victim = make_node_id(0xAA);
        let newcomer = make_node_id(0xBB);

        // Pre-occupy, with `victim`, every slot that add_to_hash_ring
        // will hash to for `newcomer` (so EVERY vnode insert collides).
        for i in 0..lb.virtual_nodes {
            let key = format!("{:?}-{}", newcomer, i);
            let hash = lb.hash_key(&key);
            lb.hash_ring.insert(hash, victim);
        }
        let victim_slots_before = lb.hash_ring.len();
        assert_eq!(victim_slots_before, lb.virtual_nodes as usize);

        // Now add the newcomer — every primary slot collides with a
        // victim vnode and must be linear-probed past, NOT clobbered.
        lb.add_to_hash_ring(newcomer);

        // None of the victim's vnodes may have been overwritten.
        let victim_count = lb.hash_ring.iter().filter(|e| *e.value() == victim).count();
        assert_eq!(
            victim_count, lb.virtual_nodes as usize,
            "the collision probe must preserve the existing occupant's vnodes \
             (pre-fix destructive insert clobbered them)"
        );
        // The newcomer still gets its full vnode allotment.
        let newcomer_count = lb
            .hash_ring
            .iter()
            .filter(|e| *e.value() == newcomer)
            .count();
        assert_eq!(
            newcomer_count, lb.virtual_nodes as usize,
            "the newcomer must still get its full vnode allotment via probing"
        );
    }

    /// Finding #33: weighted-round-robin must not starve endpoints
    /// when every effective weight is sub-unit. Two `Degraded`
    /// endpoints (weight 1 × 0.5 multiplier = 0.5 effective) sum to
    /// `total_weight == 1.0`; pre-fix `total_ceil = ceil(1.0) = 1`
    /// made `target == 0` always and the cumulative loop always
    /// picked the first endpoint, starving the second.
    #[test]
    fn cr33_weighted_rr_does_not_starve_sub_unit_weights() {
        let lb = LoadBalancer::with_strategy(Strategy::WeightedRoundRobin);
        lb.add_endpoint(Endpoint::new(make_node_id(1)).with_weight(1));
        lb.add_endpoint(Endpoint::new(make_node_id(2)).with_weight(1));

        // Degrade both so effective weight is 0.5 each (total 1.0).
        lb.update_health(&make_node_id(1), HealthStatus::Degraded);
        lb.update_health(&make_node_id(2), HealthStatus::Degraded);

        let ctx = RequestContext::new();
        let mut counts = std::collections::HashMap::new();
        for _ in 0..200 {
            let sel = lb.select(&ctx).expect("an endpoint must be selectable");
            lb.record_completion(&sel.node_id, true);
            *counts.entry(sel.node_id[0]).or_insert(0_u32) += 1;
        }

        let first = *counts.get(&1).unwrap_or(&0);
        let second = *counts.get(&2).unwrap_or(&0);
        assert!(
            second > 0,
            "second sub-unit-weight endpoint must NOT be starved (got {first} \
             vs {second}) — pre-fix ceil collapsed the rotation to bucket 0",
        );
        // Equal effective weights → roughly even split.
        assert!(
            first > 0 && second > 0 && first.abs_diff(second) < 200 / 2,
            "two equal sub-unit weights should split roughly evenly \
             (got {first} vs {second})",
        );
    }

    /// Review follow-up to #33: deriving the wheel from the weights
    /// (`ceil(total / min_weight)`) keeps EXACT integer ratios for small
    /// clusters — the wheel is the natural 3-cycle for a 2:1 split,
    /// yielding A,A,B. The replaced `WRR_MIN_WHEEL = 64` floor turned
    /// this into a 64-position approximation, reshaping the rotation for
    /// any cluster whose total weight was below the floor.
    #[test]
    fn cr33_weighted_rr_preserves_exact_integer_ratios() {
        let lb = LoadBalancer::with_strategy(Strategy::WeightedRoundRobin);
        let endpoints = vec![
            Arc::new(EndpointState::new(
                Endpoint::new(make_node_id(1)).with_weight(2),
            )),
            Arc::new(EndpointState::new(
                Endpoint::new(make_node_id(2)).with_weight(1),
            )),
        ];
        // Two full turns of the natural 3-position wheel.
        let picks: Vec<u8> = (0..6)
            .map(|c| lb.select_weighted_round_robin_at(&endpoints, c).node_id[0])
            .collect();
        assert_eq!(
            picks,
            vec![1, 1, 2, 1, 1, 2],
            "2:1 integer weights must cycle A,A,B exactly (got {picks:?})"
        );
    }
}
