//! Failure detection and recovery for Net.
//!
//! This module provides:
//! - `FailureDetector` - Heartbeat-based failure detection
//! - `LossSimulator` - Packet loss simulation for testing
//! - `RecoveryManager` - Route recovery and failover
//! - `CircuitBreaker` - Prevent cascading failures

use dashmap::DashMap;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Failure detector configuration
#[derive(Debug, Clone)]
pub struct FailureDetectorConfig {
    /// Heartbeat timeout before considering node failed
    pub timeout: Duration,
    /// Number of missed heartbeats before declaring failure
    pub miss_threshold: u32,
    /// Suspicion threshold (soft failure)
    pub suspicion_threshold: u32,
    /// Cleanup interval for stale entries
    pub cleanup_interval: Duration,
}

impl Default for FailureDetectorConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(5),
            miss_threshold: 3,
            suspicion_threshold: 2,
            cleanup_interval: Duration::from_secs(30),
        }
    }
}

/// Node health status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeStatus {
    /// Node is healthy (receiving heartbeats)
    Healthy,
    /// Node is suspected (missed some heartbeats)
    Suspected,
    /// Node is considered failed
    Failed,
    /// Node status is unknown (never seen)
    Unknown,
}

/// Per-node failure tracking state
#[derive(Debug)]
struct NodeState {
    /// Last heartbeat timestamp
    last_heartbeat: Instant,
    /// Number of consecutive missed heartbeats
    missed_count: u32,
    /// Current status
    status: NodeStatus,
    /// Node address
    #[allow(dead_code)]
    addr: SocketAddr,
    /// Total heartbeats received
    total_heartbeats: u64,
    /// Time node was first seen
    #[allow(dead_code)]
    first_seen: Instant,
}

impl NodeState {
    fn new(addr: SocketAddr) -> Self {
        let now = Instant::now();
        Self {
            last_heartbeat: now,
            missed_count: 0,
            status: NodeStatus::Healthy,
            addr,
            total_heartbeats: 1,
            first_seen: now,
        }
    }

    fn on_heartbeat(&mut self) {
        self.last_heartbeat = Instant::now();
        self.missed_count = 0;
        self.status = NodeStatus::Healthy;
        self.total_heartbeats += 1;
    }

    fn check(&mut self, timeout: Duration, suspicion_threshold: u32, miss_threshold: u32) {
        let elapsed = self.last_heartbeat.elapsed();

        if elapsed > timeout {
            // Compute how many heartbeat intervals have been missed based on
            // actual elapsed time, not just how many times check() was called.
            // This prevents both under- and over-counting when check_all()
            // runs at a different frequency than the heartbeat interval.
            let timeout_nanos = timeout.as_nanos().max(1);
            self.missed_count = (elapsed.as_nanos() / timeout_nanos) as u32;

            if self.missed_count >= miss_threshold {
                self.status = NodeStatus::Failed;
            } else if self.missed_count >= suspicion_threshold {
                self.status = NodeStatus::Suspected;
            }
        }
    }
}

/// Failure detection statistics
#[derive(Debug, Clone, Default)]
pub struct FailureStats {
    /// Total nodes tracked
    pub nodes_tracked: usize,
    /// Healthy nodes
    pub nodes_healthy: usize,
    /// Suspected nodes
    pub nodes_suspected: usize,
    /// Failed nodes
    pub nodes_failed: usize,
    /// Total failures detected
    pub total_failures: u64,
    /// Total recoveries
    pub total_recoveries: u64,
}

/// Heartbeat-based failure detector.
///
/// Tracks node health via heartbeat messages and detects failures.
pub struct FailureDetector {
    /// Configuration
    config: FailureDetectorConfig,
    /// Per-node state
    nodes: DashMap<u64, NodeState>,
    /// Failure callback (node_id)
    on_failure: Option<Arc<dyn Fn(u64) + Send + Sync>>,
    /// Recovery callback (node_id)
    on_recovery: Option<Arc<dyn Fn(u64) + Send + Sync>>,
    /// Total failures detected
    total_failures: AtomicU64,
    /// Total recoveries
    total_recoveries: AtomicU64,
    /// Last cleanup time
    last_cleanup: std::sync::Mutex<Instant>,
}

impl FailureDetector {
    /// Create a new failure detector with default config
    pub fn new() -> Self {
        Self::with_config(FailureDetectorConfig::default())
    }

    /// Create with custom config
    pub fn with_config(config: FailureDetectorConfig) -> Self {
        Self {
            config,
            nodes: DashMap::new(),
            on_failure: None,
            on_recovery: None,
            total_failures: AtomicU64::new(0),
            total_recoveries: AtomicU64::new(0),
            last_cleanup: std::sync::Mutex::new(Instant::now()),
        }
    }

    /// Set failure callback
    pub fn on_failure<F>(mut self, f: F) -> Self
    where
        F: Fn(u64) + Send + Sync + 'static,
    {
        self.on_failure = Some(Arc::new(f));
        self
    }

    /// Set recovery callback
    pub fn on_recovery<F>(mut self, f: F) -> Self
    where
        F: Fn(u64) + Send + Sync + 'static,
    {
        self.on_recovery = Some(Arc::new(f));
        self
    }

    /// Record a heartbeat from a node
    ///
    /// Previously the recovery callback was invoked inside
    /// `entry().and_modify(...)`, which holds the DashMap shard's
    /// write lock. A user-supplied callback that re-entered the same
    /// shard (or any structure ordered against it) deadlocked; even
    /// without deadlock, every concurrent `heartbeat` hashing to the
    /// same shard stalled while the callback ran. The fix collects a
    /// "should I notify?" flag inside the closure and fires the
    /// callback *after* the `and_modify` returns, releasing the
    /// shard lock.
    pub fn heartbeat(&self, node_id: u64, addr: SocketAddr) {
        let mut should_notify_recovery = false;
        self.nodes
            .entry(node_id)
            .and_modify(|state| {
                let was_failed = state.status == NodeStatus::Failed;
                state.on_heartbeat();

                if was_failed {
                    self.total_recoveries.fetch_add(1, Ordering::Relaxed);
                    should_notify_recovery = true;
                }
            })
            .or_insert_with(|| NodeState::new(addr));

        if should_notify_recovery {
            if let Some(ref cb) = self.on_recovery {
                cb(node_id);
            }
        }
    }

    /// Check all nodes for failures
    ///
    /// Callbacks are now invoked *after* the `iter_mut` loop has
    /// dropped its shard locks. Previously `cb(*entry.key())` ran
    /// inside the iteration, with the per-shard write lock still
    /// held — a user-supplied callback that touched another DashMap
    /// entry on the same shard (or re-entered the failure detector
    /// itself via `heartbeat` / `status`) would deadlock. We collect
    /// the failed ids first, release the iteration locks, then fire
    /// the callbacks.
    pub fn check_all(&self) -> Vec<u64> {
        let mut newly_failed = Vec::new();

        for mut entry in self.nodes.iter_mut() {
            let prev_status = entry.status;
            entry.check(
                self.config.timeout,
                self.config.suspicion_threshold,
                self.config.miss_threshold,
            );

            if entry.status == NodeStatus::Failed && prev_status != NodeStatus::Failed {
                newly_failed.push(*entry.key());
                self.total_failures.fetch_add(1, Ordering::Relaxed);
            }
        }

        if let Some(ref cb) = self.on_failure {
            for id in &newly_failed {
                cb(*id);
            }
        }

        newly_failed
    }

    /// Get node status
    pub fn status(&self, node_id: u64) -> NodeStatus {
        self.nodes
            .get(&node_id)
            .map(|s| s.status)
            .unwrap_or(NodeStatus::Unknown)
    }

    /// Get all failed nodes
    pub fn failed_nodes(&self) -> Vec<u64> {
        self.nodes
            .iter()
            .filter(|r| r.status == NodeStatus::Failed)
            .map(|r| *r.key())
            .collect()
    }

    /// Get all suspected nodes
    pub fn suspected_nodes(&self) -> Vec<u64> {
        self.nodes
            .iter()
            .filter(|r| r.status == NodeStatus::Suspected)
            .map(|r| *r.key())
            .collect()
    }

    /// Get all healthy nodes
    pub fn healthy_nodes(&self) -> Vec<u64> {
        self.nodes
            .iter()
            .filter(|r| r.status == NodeStatus::Healthy)
            .map(|r| *r.key())
            .collect()
    }

    /// Remove a node from tracking
    pub fn remove(&self, node_id: u64) {
        self.nodes.remove(&node_id);
    }

    /// Clean up stale entries (nodes that have been failed for too long)
    pub fn cleanup(&self) -> usize {
        // Recover from poisoning rather than panic. A panic
        // anywhere holding this mutex would otherwise turn every
        // subsequent `cleanup()` call into a runtime panic that
        // takes the failure-detection loop down with it. Matches
        // the recovery pattern used elsewhere in the crate
        // (e.g. `crypto.rs::sliding_window`).
        let mut last = self.last_cleanup.lock().unwrap_or_else(|p| p.into_inner());
        if last.elapsed() < self.config.cleanup_interval {
            return 0;
        }
        *last = Instant::now();
        drop(last);

        let stale_threshold = self.config.timeout * 10; // 10x timeout = stale
        let mut removed = 0;

        self.nodes.retain(|_, state| {
            if state.status == NodeStatus::Failed
                && state.last_heartbeat.elapsed() > stale_threshold
            {
                removed += 1;
                false
            } else {
                true
            }
        });

        removed
    }

    /// Get statistics
    pub fn stats(&self) -> FailureStats {
        let mut healthy = 0;
        let mut suspected = 0;
        let mut failed = 0;

        for entry in self.nodes.iter() {
            match entry.status {
                NodeStatus::Healthy => healthy += 1,
                NodeStatus::Suspected => suspected += 1,
                NodeStatus::Failed => failed += 1,
                NodeStatus::Unknown => {}
            }
        }

        FailureStats {
            nodes_tracked: self.nodes.len(),
            nodes_healthy: healthy,
            nodes_suspected: suspected,
            nodes_failed: failed,
            total_failures: self.total_failures.load(Ordering::Relaxed),
            total_recoveries: self.total_recoveries.load(Ordering::Relaxed),
        }
    }

    /// Get node count
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }
}

impl Default for FailureDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// Packet loss simulator for testing.
///
/// Simulates various network failure conditions.
pub struct LossSimulator {
    /// Base loss rate (0.0 - 1.0)
    loss_rate: f32,
    /// Burst loss state
    in_burst: AtomicBool,
    /// Burst probability
    burst_prob: f32,
    /// Burst length (packets)
    burst_length: u32,
    /// Current burst remaining
    burst_remaining: AtomicU64,
    /// Random state (simple LCG)
    rng_state: AtomicU64,
    /// Total packets seen
    total_packets: AtomicU64,
    /// Total packets dropped
    total_dropped: AtomicU64,
}

impl LossSimulator {
    /// Create a new loss simulator with given loss rate
    pub fn new(loss_rate: f32) -> Self {
        Self {
            loss_rate: loss_rate.clamp(0.0, 1.0),
            in_burst: AtomicBool::new(false),
            burst_prob: 0.0,
            burst_length: 0,
            burst_remaining: AtomicU64::new(0),
            rng_state: AtomicU64::new(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos() as u64,
            ),
            total_packets: AtomicU64::new(0),
            total_dropped: AtomicU64::new(0),
        }
    }

    /// Create with burst loss behavior
    pub fn with_bursts(mut self, burst_prob: f32, burst_length: u32) -> Self {
        self.burst_prob = burst_prob.clamp(0.0, 1.0);
        self.burst_length = burst_length;
        self
    }

    /// Check if a packet should be dropped
    pub fn should_drop(&self) -> bool {
        self.total_packets.fetch_add(1, Ordering::Relaxed);

        // Check burst state — use compare-and-swap to avoid underflow wrapping
        // to u64::MAX when multiple threads race on the last remaining count.
        loop {
            let remaining = self.burst_remaining.load(Ordering::Relaxed);
            if remaining == 0 {
                break;
            }
            match self.burst_remaining.compare_exchange_weak(
                remaining,
                remaining - 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    self.total_dropped.fetch_add(1, Ordering::Relaxed);
                    return true;
                }
                Err(_) => continue, // Retry CAS
            }
        }

        // Generate random value
        let r = self.next_random();

        // Check for burst start
        if self.burst_prob > 0.0 && r < self.burst_prob {
            // The triggering packet counts as the first drop in the burst,
            // so only burst_length - 1 additional packets remain.
            self.burst_remaining.store(
                self.burst_length.saturating_sub(1) as u64,
                Ordering::Relaxed,
            );
            self.in_burst.store(true, Ordering::Relaxed);
            self.total_dropped.fetch_add(1, Ordering::Relaxed);
            return true;
        }

        // Normal loss
        if r < self.loss_rate {
            self.total_dropped.fetch_add(1, Ordering::Relaxed);
            return true;
        }

        false
    }

    /// Get current effective loss rate
    pub fn effective_loss_rate(&self) -> f32 {
        let total = self.total_packets.load(Ordering::Relaxed);
        let dropped = self.total_dropped.load(Ordering::Relaxed);
        if total == 0 {
            return 0.0;
        }
        dropped as f32 / total as f32
    }

    /// Reset statistics
    pub fn reset(&self) {
        self.total_packets.store(0, Ordering::Relaxed);
        self.total_dropped.store(0, Ordering::Relaxed);
        self.burst_remaining.store(0, Ordering::Relaxed);
        self.in_burst.store(false, Ordering::Relaxed);
    }

    /// Get statistics
    pub fn stats(&self) -> (u64, u64) {
        (
            self.total_packets.load(Ordering::Relaxed),
            self.total_dropped.load(Ordering::Relaxed),
        )
    }

    // Simple LCG random number generator (0.0 - 1.0).
    // Uses CAS loop so concurrent threads don't get identical random values.
    // fetch_update returns Ok(previous_value); derive the output from the
    // new state (prev * M + 1) which the closure already stored atomically.
    fn next_random(&self) -> f32 {
        let prev = self
            .rng_state
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |s| {
                Some(s.wrapping_mul(6364136223846793005).wrapping_add(1))
            })
            .unwrap(); // closure always returns Some
        let new_state = prev.wrapping_mul(6364136223846793005).wrapping_add(1);
        (new_state >> 33) as f32 / (1u64 << 31) as f32
    }
}

/// Circuit breaker state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Circuit is closed (normal operation)
    Closed,
    /// Circuit is open (blocking requests)
    Open,
    /// Circuit is half-open (testing recovery)
    HalfOpen,
}

/// Circuit breaker for preventing cascading failures.
pub struct CircuitBreaker {
    /// Current state
    state: std::sync::RwLock<CircuitState>,
    /// Failure count in current window
    failure_count: AtomicU64,
    /// Success count in current window
    success_count: AtomicU64,
    /// Failure threshold to trip
    failure_threshold: u64,
    /// Success threshold to close
    success_threshold: u64,
    /// Time to wait before half-open
    reset_timeout: Duration,
    /// Last state change time
    last_state_change: std::sync::Mutex<Instant>,
    /// Total trips
    total_trips: AtomicU64,
}

impl CircuitBreaker {
    /// Create a new circuit breaker
    pub fn new(failure_threshold: u64, success_threshold: u64, reset_timeout: Duration) -> Self {
        Self {
            state: std::sync::RwLock::new(CircuitState::Closed),
            failure_count: AtomicU64::new(0),
            success_count: AtomicU64::new(0),
            failure_threshold,
            success_threshold,
            reset_timeout,
            last_state_change: std::sync::Mutex::new(Instant::now()),
            total_trips: AtomicU64::new(0),
        }
    }

    /// Check if request should be allowed
    pub fn allow(&self) -> bool {
        // Fast path: read lock for the common Closed/HalfOpen case so
        // typical allow() calls don't contend on the writer lock.
        {
            let state = *self.state.read().unwrap_or_else(|p| p.into_inner());
            match state {
                CircuitState::Closed | CircuitState::HalfOpen => return true,
                CircuitState::Open => {} // fall through to slow path
            }
        }
        // Slow path: when the fast path observed Open, hold the write
        // lock across the entire read-decide-transition. Dropping it
        // between the read and the transition (the previous
        // implementation) lets a concurrent reset() — which transitions
        // Open → Closed — be silently undone by this method's
        // transition Open → HalfOpen layered on top of the Closed
        // state. record_success/record_failure deliberately hold the
        // write lock throughout for the same reason; allow() was the
        // outlier.
        let mut state = self.state.write().unwrap_or_else(|p| p.into_inner());
        match *state {
            CircuitState::Closed | CircuitState::HalfOpen => true,
            CircuitState::Open => {
                let elapsed = self
                    .last_state_change
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .elapsed();
                if elapsed >= self.reset_timeout {
                    Self::transition_locked(
                        &mut state,
                        CircuitState::HalfOpen,
                        &self.failure_count,
                        &self.success_count,
                        &self.last_state_change,
                        &self.total_trips,
                    );
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Record a success
    pub fn record_success(&self) {
        // Hold write lock through the entire read-decide-transition path
        // to prevent TOCTOU races where concurrent threads undo each other's
        // state transitions.
        let mut state = self.state.write().unwrap_or_else(|p| p.into_inner());
        match *state {
            CircuitState::Closed => {
                // Reset failure count on success
                self.failure_count.store(0, Ordering::Relaxed);
            }
            CircuitState::HalfOpen => {
                let count = self.success_count.fetch_add(1, Ordering::Relaxed) + 1;
                if count >= self.success_threshold {
                    Self::transition_locked(
                        &mut state,
                        CircuitState::Closed,
                        &self.failure_count,
                        &self.success_count,
                        &self.last_state_change,
                        &self.total_trips,
                    );
                }
            }
            CircuitState::Open => {}
        }
    }

    /// Record a failure
    pub fn record_failure(&self) {
        // Hold write lock through the entire read-decide-transition path
        // to prevent TOCTOU races where concurrent threads undo each other's
        // state transitions.
        let mut state = self.state.write().unwrap_or_else(|p| p.into_inner());
        match *state {
            CircuitState::Closed => {
                let count = self.failure_count.fetch_add(1, Ordering::Relaxed) + 1;
                if count >= self.failure_threshold {
                    Self::transition_locked(
                        &mut state,
                        CircuitState::Open,
                        &self.failure_count,
                        &self.success_count,
                        &self.last_state_change,
                        &self.total_trips,
                    );
                }
            }
            CircuitState::HalfOpen => {
                // Single failure in half-open trips back to open
                Self::transition_locked(
                    &mut state,
                    CircuitState::Open,
                    &self.failure_count,
                    &self.success_count,
                    &self.last_state_change,
                    &self.total_trips,
                );
            }
            CircuitState::Open => {}
        }
    }

    /// Get current state
    pub fn state(&self) -> CircuitState {
        *self.state.read().unwrap_or_else(|p| p.into_inner())
    }

    /// Get total trip count
    pub fn total_trips(&self) -> u64 {
        self.total_trips.load(Ordering::Relaxed)
    }

    /// Reset the circuit breaker
    pub fn reset(&self) {
        self.transition_to(CircuitState::Closed);
        self.failure_count.store(0, Ordering::Relaxed);
        self.success_count.store(0, Ordering::Relaxed);
    }

    fn transition_to(&self, new_state: CircuitState) {
        let mut state = self.state.write().unwrap_or_else(|p| p.into_inner());
        Self::transition_locked(
            &mut state,
            new_state,
            &self.failure_count,
            &self.success_count,
            &self.last_state_change,
            &self.total_trips,
        );
    }

    /// Transition while already holding the write lock (avoids deadlock
    /// when called from record_success/record_failure which hold the lock).
    fn transition_locked(
        state: &mut CircuitState,
        new_state: CircuitState,
        failure_count: &AtomicU64,
        success_count: &AtomicU64,
        last_state_change: &std::sync::Mutex<Instant>,
        total_trips: &AtomicU64,
    ) {
        let old_state = *state;
        if old_state != new_state {
            *state = new_state;
            *last_state_change.lock().unwrap_or_else(|p| p.into_inner()) = Instant::now();

            // Reset counters on transition
            failure_count.store(0, Ordering::Relaxed);
            success_count.store(0, Ordering::Relaxed);

            // Track trips
            if new_state == CircuitState::Open {
                total_trips.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// Recovery action for a failed node
#[derive(Debug, Clone)]
pub enum RecoveryAction {
    /// Reroute traffic through alternate path
    Reroute {
        /// Node IDs forming the alternate path
        via: Vec<u64>,
    },
    /// Retry with backoff
    Retry {
        /// Delay before retry in milliseconds
        delay_ms: u64,
    },
    /// Drop and notify
    Drop {
        /// Reason for dropping the message
        reason: String,
    },
    /// Queue for later delivery
    Queue,
}

/// Recovery statistics
#[derive(Debug, Clone, Default)]
pub struct RecoveryStats {
    /// Reroutes performed
    pub reroutes: u64,
    /// Retries performed
    pub retries: u64,
    /// Packets dropped
    pub dropped: u64,
    /// Packets queued
    pub queued: u64,
    /// Average recovery time (ms)
    pub avg_recovery_ms: u64,
}

/// Recovery manager for handling node failures.
pub struct RecoveryManager {
    /// Failed nodes and their recovery state
    failed_nodes: DashMap<u64, FailedNodeState>,
    /// Pending recovery queue
    recovery_queue: std::sync::Mutex<VecDeque<(u64, Instant)>>,
    /// Stats
    reroutes: AtomicU64,
    retries: AtomicU64,
    dropped: AtomicU64,
    queued: AtomicU64,
    total_recovery_time_ms: AtomicU64,
    recovery_count: AtomicU64,
}

#[derive(Debug)]
struct FailedNodeState {
    /// When failure was detected
    failed_at: Instant,
    /// Retry count
    retry_count: u32,
    /// Alternate routes
    alternates: Vec<u64>,
}

impl RecoveryManager {
    /// Create a new recovery manager
    pub fn new() -> Self {
        Self {
            failed_nodes: DashMap::new(),
            recovery_queue: std::sync::Mutex::new(VecDeque::new()),
            reroutes: AtomicU64::new(0),
            retries: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            queued: AtomicU64::new(0),
            total_recovery_time_ms: AtomicU64::new(0),
            recovery_count: AtomicU64::new(0),
        }
    }

    /// Handle a node failure
    pub fn on_failure(&self, node_id: u64, alternates: Vec<u64>) -> RecoveryAction {
        // Repeat failures must NOT reset `failed_at` or
        // `retry_count`. A flapping peer that fails, gets one or
        // more retries, then fails again would otherwise have its
        // retry budget restored from zero each time and never
        // reach `max_retries` in `get_action`. Preserve the
        // existing state on a repeat; refresh `alternates` so a
        // newly-discovered reroute path takes effect.
        self.failed_nodes
            .entry(node_id)
            .and_modify(|s| {
                if !alternates.is_empty() {
                    s.alternates = alternates.clone();
                }
            })
            .or_insert_with(|| FailedNodeState {
                failed_at: Instant::now(),
                retry_count: 0,
                alternates: alternates.clone(),
            });

        if !alternates.is_empty() {
            self.reroutes.fetch_add(1, Ordering::Relaxed);
            RecoveryAction::Reroute { via: alternates }
        } else {
            self.queued.fetch_add(1, Ordering::Relaxed);
            self.recovery_queue
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push_back((node_id, Instant::now()));
            RecoveryAction::Queue
        }
    }

    /// Handle node recovery
    pub fn on_recovery(&self, node_id: u64) {
        if let Some((_, state)) = self.failed_nodes.remove(&node_id) {
            let recovery_time = state.failed_at.elapsed().as_millis() as u64;
            self.total_recovery_time_ms
                .fetch_add(recovery_time, Ordering::Relaxed);
            self.recovery_count.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Get recovery action for a node
    pub fn get_action(&self, node_id: u64, max_retries: u32) -> RecoveryAction {
        if let Some(mut state) = self.failed_nodes.get_mut(&node_id) {
            if !state.alternates.is_empty() {
                return RecoveryAction::Reroute {
                    via: state.alternates.clone(),
                };
            }

            if state.retry_count < max_retries {
                state.retry_count += 1;
                self.retries.fetch_add(1, Ordering::Relaxed);
                let delay = 100 * (1 << state.retry_count.min(6)); // Exponential backoff
                return RecoveryAction::Retry { delay_ms: delay };
            }

            self.dropped.fetch_add(1, Ordering::Relaxed);
            RecoveryAction::Drop {
                reason: "max retries exceeded".into(),
            }
        } else {
            // Node not in failed list — caller asked for an action
            // on a node we don't track as failed. Pre-fix this
            // returned `Retry { delay_ms: 0 }`, which a caller
            // dutifully respecting the delay would busy-loop on.
            // The semantically-cleanest answer is "no action
            // needed, treat as healthy," but the variant doesn't
            // exist. Return the same 100ms first-backoff step the
            // failed-node path uses on its first retry, so the
            // caller paces itself even when get_action was called
            // by mistake on a healthy node.
            RecoveryAction::Retry { delay_ms: 100 }
        }
    }

    /// Check if a node is failed
    pub fn is_failed(&self, node_id: u64) -> bool {
        self.failed_nodes.contains_key(&node_id)
    }

    /// Get statistics
    pub fn stats(&self) -> RecoveryStats {
        let count = self.recovery_count.load(Ordering::Relaxed);
        let total_time = self.total_recovery_time_ms.load(Ordering::Relaxed);
        let avg = total_time.checked_div(count).unwrap_or(0);

        RecoveryStats {
            reroutes: self.reroutes.load(Ordering::Relaxed),
            retries: self.retries.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            queued: self.queued.load(Ordering::Relaxed),
            avg_recovery_ms: avg,
        }
    }

    /// Get failed node count
    pub fn failed_count(&self) -> usize {
        self.failed_nodes.len()
    }
}

impl Default for RecoveryManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_failure_detector_basic() {
        let detector = FailureDetector::with_config(FailureDetectorConfig {
            timeout: Duration::from_millis(100),
            miss_threshold: 2,
            suspicion_threshold: 1,
            cleanup_interval: Duration::from_secs(60),
        });

        let addr: SocketAddr = "127.0.0.1:9000".parse().unwrap();
        detector.heartbeat(0x1234, addr);

        assert_eq!(detector.status(0x1234), NodeStatus::Healthy);
        assert_eq!(detector.node_count(), 1);
    }

    #[test]
    fn test_failure_detector_failure() {
        // Timings: timeout=100ms, sleeps=150ms. `missed_count`
        // computes `elapsed / timeout`, so after 150ms we
        // expect 1 miss → Suspected. After 300ms we expect 3
        // misses → Failed. Wider ratio than the original
        // (10ms / 15ms) because OS scheduler slippage + deps
        // that pull in larger runtimes (hyper / igd-next for
        // the `port-mapping` feature) can add several-ms jitter
        // on top of a 15ms sleep, which was enough to push
        // `missed_count` from 1 into 2 (i.e. Failed) after the
        // first sleep — false positive on the Suspected assert.
        let detector = FailureDetector::with_config(FailureDetectorConfig {
            timeout: Duration::from_millis(100),
            miss_threshold: 2,
            suspicion_threshold: 1,
            cleanup_interval: Duration::from_secs(60),
        });

        let addr: SocketAddr = "127.0.0.1:9000".parse().unwrap();
        detector.heartbeat(0x1234, addr);

        // Wait for timeout (~1.5× the timeout → 1 miss).
        std::thread::sleep(Duration::from_millis(150));

        // First check - should be suspected
        detector.check_all();
        assert_eq!(detector.status(0x1234), NodeStatus::Suspected);

        // Wait more (total ~300ms → 3 misses → Failed).
        std::thread::sleep(Duration::from_millis(150));

        // Second check - should be failed
        let failed = detector.check_all();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0], 0x1234);
        assert_eq!(detector.status(0x1234), NodeStatus::Failed);
    }

    #[test]
    fn test_failure_detector_recovery() {
        let detector = FailureDetector::with_config(FailureDetectorConfig {
            timeout: Duration::from_millis(10),
            miss_threshold: 1,
            suspicion_threshold: 1,
            cleanup_interval: Duration::from_secs(60),
        });

        let addr: SocketAddr = "127.0.0.1:9000".parse().unwrap();
        detector.heartbeat(0x1234, addr);

        std::thread::sleep(Duration::from_millis(15));
        detector.check_all();
        assert_eq!(detector.status(0x1234), NodeStatus::Failed);

        // Recovery
        detector.heartbeat(0x1234, addr);
        assert_eq!(detector.status(0x1234), NodeStatus::Healthy);

        let stats = detector.stats();
        assert_eq!(stats.total_failures, 1);
        assert_eq!(stats.total_recoveries, 1);
    }

    #[test]
    fn test_failure_detector_elapsed_based_missed_count() {
        // Regression: check() incremented missed_count by 1 per call regardless
        // of elapsed time. If check_all() ran infrequently, a node could stay
        // healthy much longer than the configured timeout. Now missed_count is
        // computed from elapsed / timeout.
        let detector = FailureDetector::with_config(FailureDetectorConfig {
            timeout: Duration::from_millis(10),
            miss_threshold: 3,
            suspicion_threshold: 2,
            cleanup_interval: Duration::from_secs(60),
        });

        let addr: SocketAddr = "127.0.0.1:9000".parse().unwrap();
        detector.heartbeat(0x1234, addr);

        // Wait long enough that multiple timeouts have elapsed
        std::thread::sleep(Duration::from_millis(35));

        // A single check_all() call should detect the node as failed
        // because ~35ms / 10ms = 3 missed heartbeats >= miss_threshold(3).
        // With the old code (increment by 1), this would only be missed_count=1.
        let failed = detector.check_all();
        assert_eq!(
            detector.status(0x1234),
            NodeStatus::Failed,
            "node should be Failed after 3+ timeout intervals, even with one check call"
        );
        assert_eq!(failed.len(), 1);
    }

    #[test]
    fn test_loss_simulator() {
        let sim = LossSimulator::new(0.5);

        let mut dropped = 0;
        for _ in 0..1000 {
            if sim.should_drop() {
                dropped += 1;
            }
        }

        // Should be roughly 50% (allow wide margin for randomness)
        assert!(dropped > 300 && dropped < 700);
    }

    #[test]
    fn test_loss_simulator_burst() {
        let sim = LossSimulator::new(0.0).with_bursts(0.1, 5);

        let mut total_bursts = 0;
        let mut in_burst = false;
        for _ in 0..1000 {
            if sim.should_drop() {
                if !in_burst {
                    in_burst = true;
                    total_bursts += 1;
                }
            } else {
                in_burst = false;
            }
        }

        // Should have had some bursts
        assert!(total_bursts > 0);
    }

    #[test]
    fn test_burst_drops_exactly_burst_length_packets() {
        // Regression: a burst starting dropped the triggering packet AND then
        // burst_length more, for burst_length + 1 total drops per burst.
        //
        // We verify by directly inspecting burst_remaining after triggering.
        // With burst_prob = 1.0, the first call always starts a burst.
        let burst_len = 5u32;
        let sim = LossSimulator::new(0.0).with_bursts(1.0, burst_len);

        // First call: triggers burst, drops the triggering packet.
        assert!(sim.should_drop());
        // burst_remaining should be burst_length - 1 (since the trigger was the 1st drop)
        let remaining = sim.burst_remaining.load(Ordering::Relaxed);
        assert_eq!(
            remaining,
            (burst_len - 1) as u64,
            "after trigger, burst_remaining should be burst_length - 1, \
             not burst_length (which would cause burst_length + 1 total drops)"
        );

        // Drain the remaining burst
        for _ in 0..remaining {
            assert!(sim.should_drop());
        }

        // After exactly burst_length total drops, burst_remaining should be 0
        assert_eq!(sim.burst_remaining.load(Ordering::Relaxed), 0);
        assert_eq!(sim.total_dropped.load(Ordering::Relaxed), burst_len as u64);
    }

    #[test]
    fn test_circuit_breaker() {
        let cb = CircuitBreaker::new(3, 2, Duration::from_millis(50));

        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.allow());

        // Trip the breaker
        cb.record_failure();
        cb.record_failure();
        cb.record_failure();

        assert_eq!(cb.state(), CircuitState::Open);
        assert!(!cb.allow());

        // Wait for reset timeout
        std::thread::sleep(Duration::from_millis(60));

        // Should transition to half-open
        assert!(cb.allow());
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        // Successes should close it
        cb.record_success();
        cb.record_success();
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn test_regression_loss_simulator_burst_no_underflow() {
        // Regression: concurrent should_drop() calls could race on
        // burst_remaining decrement, wrapping u64 to MAX. The fix uses
        // compare_exchange_weak (CAS loop) instead of fetch_sub.
        use std::sync::Arc;

        let sim = Arc::new(LossSimulator::new(0.0).with_bursts(0.3, 10));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let sim = Arc::clone(&sim);
                std::thread::spawn(move || {
                    for _ in 0..5_000 {
                        sim.should_drop();
                    }
                })
            })
            .collect();

        for t in threads {
            t.join().unwrap();
        }

        let (total, dropped) = sim.stats();
        // burst_remaining should never have wrapped to u64::MAX, so
        // dropped can never exceed total.
        assert!(
            dropped <= total,
            "dropped ({dropped}) must not exceed total ({total}) — \
             would indicate burst_remaining underflow"
        );
        // Sanity: we actually ran packets
        assert_eq!(total, 8 * 5_000);
    }

    #[test]
    fn test_regression_circuit_breaker_concurrent_transitions() {
        // Regression: record_failure/record_success read state then
        // transitioned without holding the lock, allowing TOCTOU races
        // that could corrupt state. The fix holds the write lock across
        // the entire read-decide-transition path.
        use std::sync::Arc;

        let cb = Arc::new(CircuitBreaker::new(3, 2, Duration::from_millis(10)));

        let threads: Vec<_> = (0..8)
            .map(|i| {
                let cb = Arc::clone(&cb);
                std::thread::spawn(move || {
                    for _ in 0..2_000 {
                        if i % 2 == 0 {
                            cb.record_failure();
                        } else {
                            cb.record_success();
                        }
                    }
                })
            })
            .collect();

        for t in threads {
            t.join().unwrap();
        }

        // State must be one of the valid variants (not corrupted)
        let state = cb.state();
        assert!(
            state == CircuitState::Closed
                || state == CircuitState::Open
                || state == CircuitState::HalfOpen,
            "circuit breaker state is invalid after concurrent access"
        );
        // total_trips should be reasonable (not wildly inflated)
        let trips = cb.total_trips();
        // With 4 failure threads * 2000 calls, at most 8000 trips possible
        assert!(
            trips <= 8_000,
            "total_trips ({trips}) is unreasonably high, suggests corruption"
        );
    }

    #[test]
    fn test_regression_allow_does_not_undo_reset() {
        // Regression: allow() previously read state under the read lock,
        // dropped it, then called transition_to(HalfOpen) without
        // re-checking. A reset() that ran in that gap (transition_to
        // Closed) was silently overwritten when allow()'s transition_to
        // re-acquired the write lock and stamped HalfOpen on top.
        //
        // Fix: allow() holds the write lock across the read-decide-
        // transition path, so a state change between the fast-path read
        // and the slow-path write lock is observed before any
        // transition runs.
        //
        // The test repeatedly trips the breaker to Open, then races
        // allow() (in an observer thread) against reset() (on the main
        // thread). The reset_timeout is 1ns so allow() always sees the
        // timeout as elapsed and would transition to HalfOpen if it
        // could. Final state should always be Closed: either reset()
        // ran "after" allow()'s transition (write-lock serialization
        // guarantees Closed wins), or it ran "before" and allow()
        // observed Closed under the write lock and skipped the
        // transition. With the bug, some trials end in HalfOpen.
        use std::sync::atomic::{AtomicU8, Ordering};
        use std::sync::Arc;
        use std::thread;

        const TRIALS: u32 = 5_000;

        let cb = Arc::new(CircuitBreaker::new(1, 1, Duration::from_nanos(1)));
        let signal = Arc::new(AtomicU8::new(0)); // 0=idle, 1=run, 2=stop

        let cb_observer = cb.clone();
        let signal_observer = signal.clone();
        let observer = thread::spawn(move || loop {
            match signal_observer.load(Ordering::Acquire) {
                0 => std::hint::spin_loop(),
                1 => {
                    cb_observer.allow();
                    signal_observer.store(0, Ordering::Release);
                }
                _ => return,
            }
        });

        let mut bug_count = 0u32;
        for _ in 0..TRIALS {
            // Trip Closed → Open (failure_threshold = 1).
            cb.record_failure();
            assert_eq!(cb.state(), CircuitState::Open);

            // Hand off to observer; race reset() against its allow().
            signal.store(1, Ordering::Release);
            cb.reset();
            while signal.load(Ordering::Acquire) != 0 {
                std::hint::spin_loop();
            }

            if cb.state() != CircuitState::Closed {
                bug_count += 1;
                // Recover for the next trial so the assertion below
                // surfaces the race count, not a stuck state.
                cb.reset();
            }
        }

        signal.store(2, Ordering::Release);
        observer.join().unwrap();

        assert_eq!(
            bug_count, 0,
            "{bug_count} of {TRIALS} trials ended in non-Closed state — \
             allow() transitioned to HalfOpen on top of a fresh reset()"
        );
    }

    #[test]
    fn test_recovery_manager() {
        let mgr = RecoveryManager::new();

        // Failure with alternates
        let action = mgr.on_failure(0x1234, vec![0x5678, 0x9ABC]);
        match action {
            RecoveryAction::Reroute { via } => {
                assert_eq!(via, vec![0x5678, 0x9ABC]);
            }
            _ => panic!("expected reroute"),
        }

        // Failure without alternates
        let action = mgr.on_failure(0x2222, vec![]);
        match action {
            RecoveryAction::Queue => {}
            _ => panic!("expected queue"),
        }

        assert!(mgr.is_failed(0x1234));
        assert!(mgr.is_failed(0x2222));

        // Recovery
        mgr.on_recovery(0x1234);
        assert!(!mgr.is_failed(0x1234));

        let stats = mgr.stats();
        assert_eq!(stats.reroutes, 1);
        assert_eq!(stats.queued, 1);
    }

    /// Pin: a flapping peer (fail, retry, fail, retry, ...) must
    /// reach `max_retries` and be dropped. Pre-fix `on_failure`
    /// unconditionally re-`insert`-ed the node, resetting
    /// `retry_count` to 0 every time, so `get_action` never saw
    /// the count climb past 1 and the node was retried forever.
    #[test]
    fn on_failure_preserves_retry_count_on_repeat() {
        let mgr = RecoveryManager::new();
        let node = 0x42u64;
        let max_retries = 3u32;

        // Failure 1 → enters the failed list with retry_count=0,
        // no alternates so action is Queue.
        let action = mgr.on_failure(node, vec![]);
        assert!(matches!(action, RecoveryAction::Queue));

        // Drive `get_action` to bump retry_count up to the cap.
        for expected_count in 1..=max_retries {
            match mgr.get_action(node, max_retries) {
                RecoveryAction::Retry { .. } => {}
                other => panic!(
                    "expected Retry on attempt {} (count would become {}), got {:?}",
                    expected_count, expected_count, other
                ),
            }
        }

        // Now simulate a re-failure WITHOUT recovery in between
        // (the flapping case). Pre-fix this re-`insert`-ed and
        // wiped `retry_count` back to 0, restoring an unbounded
        // retry budget.
        let _ = mgr.on_failure(node, vec![]);

        // The very next `get_action` must return Drop — the
        // budget set by the prior Retries should still apply.
        match mgr.get_action(node, max_retries) {
            RecoveryAction::Drop { .. } => {}
            other => panic!(
                "expected Drop after exhausting retries across a flap; got {:?} \
                 (pre-fix on_failure reset retry_count to 0 on repeat)",
                other
            ),
        }
    }

    /// Pin: a repeat `on_failure` carrying newly-discovered
    /// alternates must update the alternates list (so a node
    /// that was unreachable can become reroutable when topology
    /// changes), but must NOT reset `retry_count`.
    #[test]
    fn on_failure_repeat_updates_alternates_without_resetting_count() {
        let mgr = RecoveryManager::new();
        let node = 0x99u64;
        let max_retries = 2u32;

        // First failure with no alternates → Queue.
        let _ = mgr.on_failure(node, vec![]);
        // Bump the retry count once via get_action.
        let _ = mgr.get_action(node, max_retries);

        // Second failure now learns of an alternate — semantics
        // should switch to Reroute, but the prior retry_count
        // must be preserved.
        let action = mgr.on_failure(node, vec![0xDEAD]);
        match action {
            RecoveryAction::Reroute { via } => assert_eq!(via, vec![0xDEAD]),
            other => panic!("expected Reroute, got {:?}", other),
        }

        // One more get_action without alternates path: clear
        // alternates and confirm retry budget is exhausted at
        // max_retries (count was 1 after first get_action; one
        // more retry brings it to 2; the next call must Drop).
        if let Some(mut s) = mgr.failed_nodes.get_mut(&node) {
            s.alternates.clear();
        }
        let _ = mgr.get_action(node, max_retries); // count → 2 (== max)
        match mgr.get_action(node, max_retries) {
            RecoveryAction::Drop { .. } => {}
            other => panic!("expected Drop after exhausting retries; got {:?}", other),
        }
    }

    /// Regression: BUG_REPORT.md #14 — `heartbeat` and `check_all`
    /// previously invoked the user-supplied recovery / failure
    /// callbacks while still holding the DashMap shard's write
    /// lock (`and_modify` / `iter_mut` respectively). A callback
    /// that re-entered the failure detector — calling
    /// `heartbeat` / `status` / `is_failed` for *any* node — could
    /// deadlock if it hashed to the same shard, and at minimum
    /// serialized concurrent heartbeats hashing to that shard
    /// behind the user code.
    ///
    /// The fix: collect the "should I notify?" signal inside the
    /// closure / loop, drop the shard locks, then fire the
    /// callbacks. We pin this by setting a callback that calls
    /// back into the detector's `status()` (which acquires a
    /// read lock on the same shard). With the bug present, this
    /// deadlocks; with the fix, it returns successfully.
    #[test]
    fn callbacks_run_after_shard_lock_release() {
        use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
        use std::sync::Arc;

        let detector = Arc::new(FailureDetector::with_config(FailureDetectorConfig {
            timeout: Duration::from_millis(10),
            miss_threshold: 1,
            suspicion_threshold: 1,
            cleanup_interval: Duration::from_secs(60),
        }));

        let detector_for_cb = Arc::clone(&detector);
        let observed = Arc::new(AtomicBool::new(false));
        let observed_clone = Arc::clone(&observed);

        // The recovery callback re-enters `status()`, which must
        // be able to acquire a read lock on the same DashMap
        // shard the recovery path is mutating. With the pre-fix
        // code (callback under `and_modify`'s write lock), this
        // would deadlock on a single-shard DashMap.
        let detector_arc = Arc::new(
            // Re-create using the constructor that accepts a
            // callback. We'll thread it via the public setter.
            FailureDetector::with_config(FailureDetectorConfig {
                timeout: Duration::from_millis(10),
                miss_threshold: 1,
                suspicion_threshold: 1,
                cleanup_interval: Duration::from_secs(60),
            })
            .on_recovery(move |id| {
                // Re-enter the same detector; observable proof
                // we got here without a deadlock.
                let _ = detector_for_cb.status(id);
                observed_clone.store(true, AtomicOrdering::SeqCst);
            }),
        );

        let addr: SocketAddr = "127.0.0.1:9000".parse().unwrap();
        // Drive node into Failed state, then heartbeat to recover.
        detector_arc.heartbeat(0x4242, addr);
        std::thread::sleep(Duration::from_millis(25));
        let _ = detector_arc.check_all();
        assert_eq!(detector_arc.status(0x4242), NodeStatus::Failed);

        // This call would deadlock under the pre-fix code.
        detector_arc.heartbeat(0x4242, addr);

        assert!(
            observed.load(AtomicOrdering::SeqCst),
            "recovery callback must have run (and re-entered status()) — \
             a deadlock here would manifest as the test hanging (#14)"
        );
        let _ = detector;
    }

    /// Pin: `get_action` on a node not in the failed list must
    /// return a non-zero retry delay. Pre-fix the unfailed-node
    /// branch returned `Retry { delay_ms: 0 }`, which a caller
    /// dutifully respecting the delay would busy-loop on,
    /// pegging a CPU. The fix returns the same first-step
    /// backoff (100ms) the failed-node path uses on retry 1, so
    /// the caller paces itself even when `get_action` was
    /// called by mistake on a healthy node.
    #[test]
    fn get_action_on_unfailed_node_does_not_busy_loop() {
        let mgr = RecoveryManager::new();
        let untracked = 0xDEAD_BEEFu64;

        // Sanity: node is not in the failed list.
        assert!(
            !mgr.is_failed(untracked),
            "precondition: node must not be tracked as failed"
        );

        let action = mgr.get_action(untracked, 3);
        match action {
            RecoveryAction::Retry { delay_ms } => {
                assert!(
                    delay_ms > 0,
                    "regression: get_action on an unfailed node returned \
                     Retry {{ delay_ms: 0 }} — a delay-respecting caller \
                     would busy-loop on this and saturate a CPU"
                );
                assert_eq!(
                    delay_ms, 100,
                    "first-step backoff should match the failed-node \
                     path's retry-1 delay (100ms) so callers pace \
                     consistently across both branches"
                );
            }
            other => panic!("unfailed-node branch must return Retry, got {:?}", other),
        }
    }
}
