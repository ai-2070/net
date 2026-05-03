//! Phase 4I: Safety Envelope Enforcement (SAFETY)
//!
//! This module provides hard safety limits that cannot be bypassed:
//! - Resource quotas (concurrent requests, tokens, memory, time)
//! - Rate limits (global, per-source, token-based)
//! - Content policies (pattern filtering, size limits, external hooks)
//! - Audit logging with configurable sinks
//! - Kill switches for emergency shutdown
//!
//! All enforcement is designed for the hot path with minimal overhead.

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use super::metadata::NodeId;

// ============================================================================
// Safety Envelope Configuration
// ============================================================================

/// Safety envelope configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetyEnvelope {
    /// Unique envelope ID
    pub id: String,
    /// Resource limits
    pub resource_limits: ResourceEnvelope,
    /// Rate limits
    pub rate_limits: RateEnvelope,
    /// Content policies
    pub content_policies: Vec<ContentPolicy>,
    /// Audit configuration
    pub audit: AuditConfig,
    /// Kill switch state
    pub kill_switch: KillSwitchConfig,
    /// Enforcement mode
    pub mode: EnforcementMode,
}

impl Default for SafetyEnvelope {
    fn default() -> Self {
        Self {
            id: "default".to_string(),
            resource_limits: ResourceEnvelope::default(),
            rate_limits: RateEnvelope::default(),
            content_policies: Vec::new(),
            audit: AuditConfig::default(),
            kill_switch: KillSwitchConfig::default(),
            mode: EnforcementMode::Enforce,
        }
    }
}

/// Resource limits envelope
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceEnvelope {
    /// Max concurrent requests
    pub max_concurrent: u32,
    /// Max tokens per request
    pub max_tokens_per_request: u32,
    /// Max memory per request (MB)
    pub max_memory_mb: u32,
    /// Max execution time (ms)
    pub max_time_ms: u32,
    /// Max total cost per hour (in cents)
    pub max_cost_per_hour_cents: u32,
}

impl Default for ResourceEnvelope {
    fn default() -> Self {
        Self {
            max_concurrent: 1000,
            max_tokens_per_request: 128_000,
            max_memory_mb: 16_384,
            max_time_ms: 300_000,            // 5 minutes
            max_cost_per_hour_cents: 10_000, // $100/hour
        }
    }
}

/// Rate limits envelope
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateEnvelope {
    /// Requests per minute (global)
    pub global_rpm: u32,
    /// Requests per minute (per source)
    pub per_source_rpm: u32,
    /// Tokens per minute
    pub tokens_per_minute: u64,
    /// Burst multiplier (allows temporary burst above limit)
    pub burst_multiplier: f32,
}

impl Default for RateEnvelope {
    fn default() -> Self {
        Self {
            global_rpm: 10_000,
            per_source_rpm: 1_000,
            tokens_per_minute: 10_000_000,
            burst_multiplier: 2.0,
        }
    }
}

/// Content policy
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentPolicy {
    /// Policy ID
    pub id: String,
    /// Check to perform
    pub check: ContentCheck,
    /// Action on violation
    pub action: PolicyAction,
    /// Whether policy is enabled
    pub enabled: bool,
}

/// Content check types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ContentCheck {
    /// Block specific patterns (regex)
    BlockPatterns(Vec<String>),
    /// Require patterns to be present
    RequirePatterns(Vec<String>),
    /// Maximum content size in bytes
    MaxSize(usize),
    /// Custom validation (placeholder for external hooks)
    Custom {
        /// Identifier of the external validator
        validator_id: String,
    },
}

/// Policy action on violation
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PolicyAction {
    /// Block the request
    Block,
    /// Warn but allow
    Warn,
    /// Log only
    Log,
    /// Redact matched content
    Redact,
}

/// Audit configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditConfig {
    /// Whether audit logging is enabled
    pub enabled: bool,
    /// Log successful requests
    pub log_success: bool,
    /// Log blocked requests
    pub log_blocked: bool,
    /// Log warnings
    pub log_warnings: bool,
    /// Maximum entries to keep in memory
    pub max_entries: usize,
    /// Flush interval in milliseconds
    pub flush_interval_ms: u64,
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            log_success: false,
            log_blocked: true,
            log_warnings: true,
            max_entries: 10_000,
            flush_interval_ms: 5_000,
        }
    }
}

/// Kill switch configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct KillSwitchConfig {
    /// Whether kill switch is currently active
    pub enabled: bool,
    /// Reason for activation
    pub reason: Option<String>,
    /// Auto-reset after seconds (None = manual reset required)
    pub auto_reset_secs: Option<u32>,
}

/// Enforcement mode
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum EnforcementMode {
    /// Enforce all limits
    #[default]
    Enforce,
    /// Log violations but don't block (audit mode)
    AuditOnly,
    /// Completely disabled
    Disabled,
}

// ============================================================================
// Safety Violations
// ============================================================================

/// Safety violation error
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafetyViolation {
    /// Kill switch is active
    KillSwitchActive {
        /// Reason the kill switch was activated
        reason: String,
    },
    /// Rate limit exceeded
    RateLimitExceeded {
        /// Type of rate limit that was exceeded
        limit_type: RateLimitType,
        /// Current usage count
        current: u64,
        /// Configured limit
        limit: u64,
    },
    /// Resource limit exceeded
    ResourceLimitExceeded {
        /// Type of resource that was exceeded
        resource: ResourceType,
        /// Amount of resource requested
        requested: u64,
        /// Amount of resource available
        available: u64,
    },
    /// Content policy violation
    ContentPolicyViolation {
        /// Identifier of the violated policy
        policy_id: String,
        /// Human-readable violation details
        details: String,
    },
    /// Request timeout
    Timeout {
        /// Elapsed time in milliseconds
        elapsed_ms: u64,
        /// Configured timeout limit in milliseconds
        limit_ms: u64,
    },
}

impl std::fmt::Display for SafetyViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::KillSwitchActive { reason } => {
                write!(f, "kill switch active: {}", reason)
            }
            Self::RateLimitExceeded {
                limit_type,
                current,
                limit,
            } => {
                write!(
                    f,
                    "rate limit exceeded: {:?} ({}/{})",
                    limit_type, current, limit
                )
            }
            Self::ResourceLimitExceeded {
                resource,
                requested,
                available,
            } => {
                write!(
                    f,
                    "resource limit exceeded: {:?} (requested {}, available {})",
                    resource, requested, available
                )
            }
            Self::ContentPolicyViolation { policy_id, details } => {
                write!(f, "content policy violation [{}]: {}", policy_id, details)
            }
            Self::Timeout {
                elapsed_ms,
                limit_ms,
            } => {
                write!(f, "timeout: {}ms elapsed, limit {}ms", elapsed_ms, limit_ms)
            }
        }
    }
}

impl std::error::Error for SafetyViolation {}

/// Rate limit type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimitType {
    /// Global requests per minute
    GlobalRpm,
    /// Per-source requests per minute
    PerSourceRpm,
    /// Tokens per minute
    TokensPerMinute,
}

/// Resource type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceType {
    /// Concurrent requests
    Concurrent,
    /// Tokens
    Tokens,
    /// Memory (MB)
    Memory,
    /// Time (ms)
    Time,
    /// Cost (cents)
    Cost,
}

// ============================================================================
// Resource Claim & Guard
// ============================================================================

/// Resource claim for a request
#[derive(Debug, Clone, Default)]
pub struct ResourceClaim {
    /// Number of concurrent slots
    pub concurrent_slots: u32,
    /// Estimated tokens
    pub tokens: u32,
    /// Estimated memory (MB)
    pub memory_mb: u32,
    /// Estimated time (ms)
    pub time_ms: u32,
    /// Estimated cost (cents)
    pub cost_cents: u32,
}

impl ResourceClaim {
    /// Create a new resource claim
    pub fn new() -> Self {
        Self::default()
    }

    /// Set concurrent slots
    pub fn with_concurrent(mut self, slots: u32) -> Self {
        self.concurrent_slots = slots;
        self
    }

    /// Set tokens
    pub fn with_tokens(mut self, tokens: u32) -> Self {
        self.tokens = tokens;
        self
    }

    /// Set memory
    pub fn with_memory_mb(mut self, mb: u32) -> Self {
        self.memory_mb = mb;
        self
    }

    /// Set time
    pub fn with_time_ms(mut self, ms: u32) -> Self {
        self.time_ms = ms;
        self
    }

    /// Set cost
    pub fn with_cost_cents(mut self, cents: u32) -> Self {
        self.cost_cents = cents;
        self
    }
}

/// RAII guard for acquired resources
pub struct ResourceGuard {
    enforcer: Arc<SafetyEnforcer>,
    claim: ResourceClaim,
    acquired_at: Instant,
}

impl ResourceGuard {
    /// Get elapsed time since acquisition
    pub fn elapsed(&self) -> Duration {
        self.acquired_at.elapsed()
    }

    /// Get the resource claim
    pub fn claim(&self) -> &ResourceClaim {
        &self.claim
    }

    /// Update the claim (e.g., after actual token count is known)
    pub fn update_tokens(&mut self, actual_tokens: u32) {
        let diff = actual_tokens as i64 - self.claim.tokens as i64;
        if diff > 0 {
            self.enforcer
                .usage
                .tokens
                .fetch_add(diff as u64, Ordering::Relaxed);
        } else if diff < 0 {
            // Use fetch_update with saturating subtraction to prevent
            // underflow wrapping the u64 counter to near-MAX, which
            // would permanently lock out all subsequent requests.
            let sub = (-diff) as u64;
            let _ = self.enforcer.usage.tokens.fetch_update(
                Ordering::Relaxed,
                Ordering::Relaxed,
                |current| Some(current.saturating_sub(sub)),
            );
        }
        self.claim.tokens = actual_tokens;
    }
}

impl Drop for ResourceGuard {
    fn drop(&mut self) {
        self.enforcer.release(&self.claim);
    }
}

// ============================================================================
// Rate Limiter
// ============================================================================

/// Token bucket rate limiter
struct RateLimiter {
    /// Global request count (resets each minute)
    global_requests: AtomicU64,
    /// Global token count (resets each minute)
    global_tokens: AtomicU64,
    /// Per-source request counts
    per_source: DashMap<NodeId, AtomicU64>,
    /// Last reset time
    last_reset: RwLock<Instant>,
    /// Reset interval
    reset_interval: Duration,
}

impl RateLimiter {
    fn new() -> Self {
        Self {
            global_requests: AtomicU64::new(0),
            global_tokens: AtomicU64::new(0),
            per_source: DashMap::new(),
            last_reset: RwLock::new(Instant::now()),
            reset_interval: Duration::from_secs(60),
        }
    }

    fn maybe_reset(&self) {
        let should_reset = {
            let last = self.last_reset.read().unwrap();
            last.elapsed() >= self.reset_interval
        };

        if should_reset {
            let mut last = self.last_reset.write().unwrap();
            if last.elapsed() >= self.reset_interval {
                self.global_requests.store(0, Ordering::Relaxed);
                self.global_tokens.store(0, Ordering::Relaxed);
                self.per_source.clear();
                *last = Instant::now();
            }
        }
    }

    fn check_global_rpm(&self, limit: u32, burst: f32) -> Result<(), SafetyViolation> {
        self.maybe_reset();
        let current = self.global_requests.load(Ordering::Relaxed);
        let effective_limit = (limit as f32 * burst) as u64;
        if current >= effective_limit {
            return Err(SafetyViolation::RateLimitExceeded {
                limit_type: RateLimitType::GlobalRpm,
                current,
                limit: effective_limit,
            });
        }
        Ok(())
    }

    fn check_source_rpm(
        &self,
        source: &NodeId,
        limit: u32,
        burst: f32,
    ) -> Result<(), SafetyViolation> {
        self.maybe_reset();
        let counter = self
            .per_source
            .entry(*source)
            .or_insert_with(|| AtomicU64::new(0));
        let current = counter.load(Ordering::Relaxed);
        let effective_limit = (limit as f32 * burst) as u64;
        if current >= effective_limit {
            return Err(SafetyViolation::RateLimitExceeded {
                limit_type: RateLimitType::PerSourceRpm,
                current,
                limit: effective_limit,
            });
        }
        Ok(())
    }

    fn check_tokens(&self, tokens: u64, limit: u64, burst: f32) -> Result<(), SafetyViolation> {
        self.maybe_reset();
        let current = self.global_tokens.load(Ordering::Relaxed);
        let effective_limit = (limit as f64 * burst as f64) as u64;
        // `checked_add` guards against attacker-influenced `tokens`
        // plus accumulated `current` wrapping `u64::MAX`. In debug
        // builds the raw `current + tokens` panics (DoS); in release
        // it wraps and silently bypasses the check. Treat overflow
        // as definitely over the limit.
        let would_be = match current.checked_add(tokens) {
            Some(sum) => sum,
            None => {
                return Err(SafetyViolation::RateLimitExceeded {
                    limit_type: RateLimitType::TokensPerMinute,
                    current: u64::MAX,
                    limit: effective_limit,
                });
            }
        };
        if would_be > effective_limit {
            return Err(SafetyViolation::RateLimitExceeded {
                limit_type: RateLimitType::TokensPerMinute,
                current: would_be,
                limit: effective_limit,
            });
        }
        Ok(())
    }

    #[allow(dead_code)] // retained for tests; production path uses try_acquire_*
    fn record_request(&self, source: Option<&NodeId>, tokens: u64) {
        self.global_requests.fetch_add(1, Ordering::Relaxed);
        self.global_tokens.fetch_add(tokens, Ordering::Relaxed);
        if let Some(src) = source {
            let counter = self
                .per_source
                .entry(*src)
                .or_insert_with(|| AtomicU64::new(0));
            counter.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// CAS-based "check and increment" for the global RPM cap. The
    /// request commits ONLY if the post-increment value still
    /// honors the cap; otherwise nothing is mutated and an Err is
    /// returned. Without this, the `check_global_rpm` + later
    /// `record_request` flow lets N concurrent acquirers all
    /// observe the pre-add value, all pass `check`, and all
    /// `record_request` past the cap.
    fn try_acquire_global_rpm(&self, limit: u32, burst: f32) -> Result<(), SafetyViolation> {
        self.maybe_reset();
        let effective_limit = (limit as f32 * burst) as u64;
        match self
            .global_requests
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                if current >= effective_limit {
                    None
                } else {
                    Some(current + 1)
                }
            }) {
            Ok(_) => Ok(()),
            Err(current) => Err(SafetyViolation::RateLimitExceeded {
                limit_type: RateLimitType::GlobalRpm,
                current,
                limit: effective_limit,
            }),
        }
    }

    /// CAS-based "check and increment" for per-source RPM. Same
    /// commit-or-rollback contract as `try_acquire_global_rpm`.
    fn try_acquire_source_rpm(
        &self,
        source: &NodeId,
        limit: u32,
        burst: f32,
    ) -> Result<(), SafetyViolation> {
        self.maybe_reset();
        let counter = self
            .per_source
            .entry(*source)
            .or_insert_with(|| AtomicU64::new(0));
        let effective_limit = (limit as f32 * burst) as u64;
        match counter.fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            if current >= effective_limit {
                None
            } else {
                Some(current + 1)
            }
        }) {
            Ok(_) => Ok(()),
            Err(current) => Err(SafetyViolation::RateLimitExceeded {
                limit_type: RateLimitType::PerSourceRpm,
                current,
                limit: effective_limit,
            }),
        }
    }

    /// CAS-based "check and add" for the tokens-per-minute counter.
    /// Treats `current + tokens` overflow as "definitely over limit"
    /// to avoid wrap-around DoS.
    fn try_acquire_tokens(
        &self,
        tokens: u64,
        limit: u64,
        burst: f32,
    ) -> Result<(), SafetyViolation> {
        self.maybe_reset();
        let effective_limit = (limit as f64 * burst as f64) as u64;
        match self
            .global_tokens
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                let next = current.checked_add(tokens)?;
                if next > effective_limit {
                    None
                } else {
                    Some(next)
                }
            }) {
            Ok(_) => Ok(()),
            Err(current) => Err(SafetyViolation::RateLimitExceeded {
                limit_type: RateLimitType::TokensPerMinute,
                current,
                limit: effective_limit,
            }),
        }
    }

    /// Roll back a previous successful `try_acquire_*` commit.
    /// Called from `acquire()` when a later step fails so the
    /// counter doesn't overcount.
    fn rollback_global_rpm(&self) {
        self.global_requests.fetch_sub(1, Ordering::Relaxed);
    }

    fn rollback_source_rpm(&self, source: &NodeId) {
        if let Some(counter) = self.per_source.get(source) {
            counter.fetch_sub(1, Ordering::Relaxed);
        }
    }

    #[allow(dead_code)] // symmetric with rollback_global_rpm / rollback_source_rpm
    fn rollback_tokens(&self, tokens: u64) {
        self.global_tokens.fetch_sub(tokens, Ordering::Relaxed);
    }
}

// ============================================================================
// Resource Usage Tracking
// ============================================================================

/// Atomic resource usage counters
struct AtomicResourceUsage {
    concurrent: AtomicU32,
    tokens: AtomicU64,
    memory_mb: AtomicU32,
    cost_cents_per_hour: AtomicU32,
    hour_start: RwLock<Instant>,
}

impl AtomicResourceUsage {
    fn new() -> Self {
        Self {
            concurrent: AtomicU32::new(0),
            tokens: AtomicU64::new(0),
            memory_mb: AtomicU32::new(0),
            cost_cents_per_hour: AtomicU32::new(0),
            hour_start: RwLock::new(Instant::now()),
        }
    }

    fn maybe_reset_hourly(&self) {
        let should_reset = {
            let start = self.hour_start.read().unwrap();
            start.elapsed() >= Duration::from_secs(3600)
        };

        if should_reset {
            let mut start = self.hour_start.write().unwrap();
            if start.elapsed() >= Duration::from_secs(3600) {
                self.cost_cents_per_hour.store(0, Ordering::Relaxed);
                *start = Instant::now();
            }
        }
    }
}

/// Usage statistics snapshot
#[derive(Debug, Clone, Default)]
pub struct UsageStats {
    /// Current concurrent requests
    pub concurrent: u32,
    /// Total tokens used (current window)
    pub tokens: u64,
    /// Current memory usage (MB)
    pub memory_mb: u32,
    /// Cost this hour (cents)
    pub cost_cents_per_hour: u32,
    /// Global requests this minute
    pub requests_per_minute: u64,
    /// Tokens this minute
    pub tokens_per_minute: u64,
}

// ============================================================================
// Audit Trail
// ============================================================================

/// Audit log entry
#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    /// Timestamp in nanoseconds
    pub timestamp_ns: u64,
    /// Event type
    pub event_type: AuditEventType,
    /// Source node (if applicable)
    pub source_node: Option<NodeId>,
    /// Request ID (if applicable)
    pub request_id: Option<u128>,
    /// Event details
    pub details: HashMap<String, String>,
    /// Outcome
    pub outcome: AuditOutcome,
}

/// Audit event type
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub enum AuditEventType {
    /// Request received
    RequestReceived,
    /// Request allowed
    RequestAllowed,
    /// Request blocked
    RequestBlocked,
    /// Rate limit hit
    RateLimitHit,
    /// Resource limit hit
    ResourceLimitHit,
    /// Content policy violation
    ContentPolicyViolation,
    /// Kill switch triggered
    KillSwitchTriggered,
    /// Kill switch reset
    KillSwitchReset,
    /// Envelope updated
    EnvelopeUpdated,
}

/// Audit outcome
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub enum AuditOutcome {
    /// Success
    Success,
    /// Blocked
    Blocked,
    /// Warning issued
    Warning,
    /// Error occurred
    Error,
}

/// Audit sink trait for external logging
pub trait AuditSink: Send + Sync {
    /// Write an audit entry
    fn write(&self, entry: &AuditEntry);
    /// Flush pending entries
    fn flush(&self);
}

/// In-memory audit log
struct AuditLog {
    entries: RwLock<VecDeque<AuditEntry>>,
    config: AuditConfig,
    sink: Option<Box<dyn AuditSink>>,
}

impl AuditLog {
    fn new(config: AuditConfig) -> Self {
        Self {
            entries: RwLock::new(VecDeque::with_capacity(config.max_entries)),
            config,
            sink: None,
        }
    }

    fn log(&self, entry: AuditEntry) {
        if !self.config.enabled {
            return;
        }

        // Check if we should log this event
        let should_log = match entry.outcome {
            AuditOutcome::Success => self.config.log_success,
            AuditOutcome::Blocked => self.config.log_blocked,
            AuditOutcome::Warning => self.config.log_warnings,
            AuditOutcome::Error => true,
        };

        if !should_log {
            return;
        }

        // Write to sink if available
        if let Some(ref sink) = self.sink {
            sink.write(&entry);
        }

        // Store in memory (O(1) eviction via VecDeque)
        let mut entries = self.entries.write().unwrap();
        if entries.len() >= self.config.max_entries {
            entries.pop_front();
        }
        entries.push_back(entry);
    }

    fn get_entries(&self, limit: usize) -> Vec<AuditEntry> {
        let entries = self.entries.read().unwrap();
        entries.iter().rev().take(limit).cloned().collect()
    }

    fn clear(&self) {
        self.entries.write().unwrap().clear();
    }
}

// ============================================================================
// Request Context
// ============================================================================

/// Request context for safety checks
#[derive(Debug, Clone, Default)]
pub struct SafetyRequest {
    /// Source node
    pub source_node: Option<NodeId>,
    /// Request ID
    pub request_id: Option<u128>,
    /// Content to check (optional)
    pub content: Option<String>,
    /// Content size in bytes
    pub content_size: usize,
    /// Estimated tokens
    pub estimated_tokens: u32,
    /// Custom metadata
    pub metadata: HashMap<String, String>,
}

impl SafetyRequest {
    /// Create a new safety request
    pub fn new() -> Self {
        Self::default()
    }

    /// Set source node
    pub fn with_source(mut self, node: NodeId) -> Self {
        self.source_node = Some(node);
        self
    }

    /// Set request ID
    pub fn with_request_id(mut self, id: u128) -> Self {
        self.request_id = Some(id);
        self
    }

    /// Set content
    pub fn with_content(mut self, content: impl Into<String>) -> Self {
        let content = content.into();
        self.content_size = content.len();
        self.content = Some(content);
        self
    }

    /// Set content size only (without content)
    pub fn with_content_size(mut self, size: usize) -> Self {
        self.content_size = size;
        self
    }

    /// Set estimated tokens
    pub fn with_tokens(mut self, tokens: u32) -> Self {
        self.estimated_tokens = tokens;
        self
    }
}

// ============================================================================
// Safety Enforcer
// ============================================================================

/// Safety enforcer (hot path optimized)
pub struct SafetyEnforcer {
    /// Current envelope
    envelope: RwLock<SafetyEnvelope>,
    /// Resource usage
    usage: AtomicResourceUsage,
    /// Rate limiter
    rate_limiter: RateLimiter,
    /// Audit log
    audit_log: AuditLog,
    /// Kill switch state
    kill_switch: AtomicBool,
    /// Kill switch timestamp
    kill_switch_at: RwLock<Option<Instant>>,
    /// Kill switch reason
    kill_switch_reason: RwLock<Option<String>>,
    /// Compiled content patterns (for hot path)
    #[cfg(feature = "regex")]
    #[allow(dead_code)]
    compiled_patterns: RwLock<Vec<(String, regex::Regex)>>,
}

impl SafetyEnforcer {
    /// Create a new safety enforcer with default envelope
    pub fn new() -> Self {
        Self::with_envelope(SafetyEnvelope::default())
    }

    /// Create with a specific envelope
    pub fn with_envelope(envelope: SafetyEnvelope) -> Self {
        let audit_log = AuditLog::new(envelope.audit.clone());
        let kill_switch = envelope.kill_switch.enabled;

        Self {
            envelope: RwLock::new(envelope),
            usage: AtomicResourceUsage::new(),
            rate_limiter: RateLimiter::new(),
            audit_log,
            kill_switch: AtomicBool::new(kill_switch),
            kill_switch_at: RwLock::new(None),
            kill_switch_reason: RwLock::new(None),
            #[cfg(feature = "regex")]
            compiled_patterns: RwLock::new(Vec::new()),
        }
    }

    /// Update the envelope
    pub fn update_envelope(&self, envelope: SafetyEnvelope) {
        *self.envelope.write().unwrap() = envelope;
        self.log_event(AuditEventType::EnvelopeUpdated, None, AuditOutcome::Success);
    }

    /// Check if a request is allowed (hot path)
    pub fn check(&self, req: &SafetyRequest) -> Result<(), SafetyViolation> {
        let envelope = self.envelope.read().unwrap();

        // Fast path: disabled mode
        if envelope.mode == EnforcementMode::Disabled {
            return Ok(());
        }

        // Check kill switch first
        self.check_kill_switch()?;

        // Check rate limits
        self.check_rate_limits(req, &envelope)?;

        // Check content policies
        self.check_content_policies(req, &envelope)?;

        // Log success if in audit-only mode
        if envelope.mode == EnforcementMode::AuditOnly {
            self.log_event(
                AuditEventType::RequestAllowed,
                req.source_node,
                AuditOutcome::Success,
            );
        }

        Ok(())
    }

    /// Acquire resources for a request
    ///
    /// Previously this did `load + compare` in
    /// `check_resource_limits`, then unconditionally `fetch_add`'d
    /// each counter. N concurrent acquirers all observed `current=0`
    /// and all proceeded past the cap — the kill-switch / safety
    /// envelope was breakable under load. The fix uses `fetch_update`
    /// (compare-and-swap loop) for each cumulative counter so the
    /// check + add is atomic per resource, and rolls back any partial
    /// successes if a later resource fails. `tokens` is per-request
    /// (not cumulative) so it stays as a straight load.
    pub fn acquire(
        self: &Arc<Self>,
        req: &SafetyRequest,
        claim: ResourceClaim,
    ) -> Result<ResourceGuard, SafetyViolation> {
        let envelope = self.envelope.read().unwrap();

        // Fast path: disabled mode
        if envelope.mode == EnforcementMode::Disabled {
            return Ok(ResourceGuard {
                enforcer: Arc::clone(self),
                claim,
                acquired_at: Instant::now(),
            });
        }

        // Check kill switch
        self.check_kill_switch()?;

        let limits = &envelope.resource_limits;
        let enforce = envelope.mode == EnforcementMode::Enforce;

        // tokens is per-request (not cumulative) so a plain
        // compare against the per-request cap is fine.
        if enforce && claim.tokens > limits.max_tokens_per_request {
            return Err(SafetyViolation::ResourceLimitExceeded {
                resource: ResourceType::Tokens,
                requested: claim.tokens as u64,
                available: limits.max_tokens_per_request as u64,
            });
        }

        // Reset the cost window before the cost CAS so a stale
        // accumulator doesn't reject a legitimate request right
        // after the hour rollover.
        self.usage.maybe_reset_hourly();

        // Helper: atomically `fetch_update` an `AtomicU32`
        // counter so that `add` only commits if `current + add
        // <= max`. Returns Err with the current value on cap
        // exceeded.
        fn try_fetch_add_capped_u32(
            counter: &std::sync::atomic::AtomicU32,
            add: u32,
            max: u32,
        ) -> Result<(), u32> {
            counter
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                    let next = current.saturating_add(add);
                    if next > max {
                        None
                    } else {
                        Some(next)
                    }
                })
                .map(|_| ())
        }

        // 1. Concurrent slots.
        if enforce {
            if let Err(cur) = try_fetch_add_capped_u32(
                &self.usage.concurrent,
                claim.concurrent_slots,
                limits.max_concurrent,
            ) {
                return Err(SafetyViolation::ResourceLimitExceeded {
                    resource: ResourceType::Concurrent,
                    requested: claim.concurrent_slots as u64,
                    available: limits.max_concurrent.saturating_sub(cur) as u64,
                });
            }
        } else {
            self.usage
                .concurrent
                .fetch_add(claim.concurrent_slots, Ordering::Relaxed);
        }

        // 2. Memory. On failure, roll back concurrent.
        if enforce {
            if let Err(cur) = try_fetch_add_capped_u32(
                &self.usage.memory_mb,
                claim.memory_mb,
                limits.max_memory_mb,
            ) {
                self.usage
                    .concurrent
                    .fetch_sub(claim.concurrent_slots, Ordering::Relaxed);
                return Err(SafetyViolation::ResourceLimitExceeded {
                    resource: ResourceType::Memory,
                    requested: claim.memory_mb as u64,
                    available: limits.max_memory_mb.saturating_sub(cur) as u64,
                });
            }
        } else {
            self.usage
                .memory_mb
                .fetch_add(claim.memory_mb, Ordering::Relaxed);
        }

        // 3. Hourly cost. On failure, roll back concurrent + memory.
        if enforce {
            if let Err(cur) = try_fetch_add_capped_u32(
                &self.usage.cost_cents_per_hour,
                claim.cost_cents,
                limits.max_cost_per_hour_cents,
            ) {
                self.usage
                    .concurrent
                    .fetch_sub(claim.concurrent_slots, Ordering::Relaxed);
                self.usage
                    .memory_mb
                    .fetch_sub(claim.memory_mb, Ordering::Relaxed);
                return Err(SafetyViolation::ResourceLimitExceeded {
                    resource: ResourceType::Cost,
                    requested: claim.cost_cents as u64,
                    available: limits.max_cost_per_hour_cents.saturating_sub(cur) as u64,
                });
            }
        } else {
            self.usage
                .cost_cents_per_hour
                .fetch_add(claim.cost_cents, Ordering::Relaxed);
        }

        // 4. Rate limits — global RPM, per-source RPM, tokens-per-
        //    minute. Previously these were checked only in `check()`
        //    (load + compare) with the increment happening separately
        //    via `record_request`. N concurrent acquirers could all
        //    pass `check()`, then all `record_request` past the cap
        //    — same TOCTOU as the resource limits. CAS-ifying the
        //    check + add per counter (with cross-counter rollback)
        //    closes the race.
        let rate = &envelope.rate_limits;
        let rate_burst = rate.burst_multiplier;

        if enforce {
            if let Err(e) = self
                .rate_limiter
                .try_acquire_global_rpm(rate.global_rpm, rate_burst)
            {
                self.usage
                    .concurrent
                    .fetch_sub(claim.concurrent_slots, Ordering::Relaxed);
                self.usage
                    .memory_mb
                    .fetch_sub(claim.memory_mb, Ordering::Relaxed);
                self.usage
                    .cost_cents_per_hour
                    .fetch_sub(claim.cost_cents, Ordering::Relaxed);
                self.log_event(
                    AuditEventType::RateLimitHit,
                    req.source_node,
                    AuditOutcome::Blocked,
                );
                return Err(e);
            }
        }

        if enforce {
            if let Some(ref source) = req.source_node {
                if let Err(e) = self.rate_limiter.try_acquire_source_rpm(
                    source,
                    rate.per_source_rpm,
                    rate_burst,
                ) {
                    self.rate_limiter.rollback_global_rpm();
                    self.usage
                        .concurrent
                        .fetch_sub(claim.concurrent_slots, Ordering::Relaxed);
                    self.usage
                        .memory_mb
                        .fetch_sub(claim.memory_mb, Ordering::Relaxed);
                    self.usage
                        .cost_cents_per_hour
                        .fetch_sub(claim.cost_cents, Ordering::Relaxed);
                    self.log_event(
                        AuditEventType::RateLimitHit,
                        req.source_node,
                        AuditOutcome::Blocked,
                    );
                    return Err(e);
                }
            }
        }

        if enforce {
            if let Err(e) = self.rate_limiter.try_acquire_tokens(
                claim.tokens as u64,
                rate.tokens_per_minute,
                rate_burst,
            ) {
                if let Some(ref source) = req.source_node {
                    self.rate_limiter.rollback_source_rpm(source);
                }
                self.rate_limiter.rollback_global_rpm();
                self.usage
                    .concurrent
                    .fetch_sub(claim.concurrent_slots, Ordering::Relaxed);
                self.usage
                    .memory_mb
                    .fetch_sub(claim.memory_mb, Ordering::Relaxed);
                self.usage
                    .cost_cents_per_hour
                    .fetch_sub(claim.cost_cents, Ordering::Relaxed);
                self.log_event(
                    AuditEventType::RateLimitHit,
                    req.source_node,
                    AuditOutcome::Blocked,
                );
                return Err(e);
            }
        } else {
            // Audit-only: still increment so observers see realistic
            // counters without any commit failing.
            //
            // Saturating CAS rather than `fetch_add` so a long-lived
            // process can't tip the audit counter into wrap (release)
            // or panic (debug). The audit-only path takes no commit
            // failure on overflow — by definition this counter only
            // drives observability dashboards — so wrap is silent
            // corruption (operators see the counter reset to ~0 mid-
            // window and conclude traffic dropped). `fetch_update`
            // with saturating_add inside is the standard pattern.
            let _ = self.rate_limiter.global_tokens.fetch_update(
                Ordering::Relaxed,
                Ordering::Relaxed,
                |v| Some(v.saturating_add(claim.tokens as u64)),
            );
        }

        // tokens (per-request `usage` counter) — free-running,
        // already-bounded by the per-request cap above.
        self.usage
            .tokens
            .fetch_add(claim.tokens as u64, Ordering::Relaxed);

        if !enforce {
            // Audit-only / Disabled: bump RPM counters too so observed
            // rates still reflect actual traffic.
            self.rate_limiter
                .global_requests
                .fetch_add(1, Ordering::Relaxed);
            if let Some(ref source) = req.source_node {
                let counter = self
                    .rate_limiter
                    .per_source
                    .entry(*source)
                    .or_insert_with(|| AtomicU64::new(0));
                counter.fetch_add(1, Ordering::Relaxed);
            }
        }

        // Log acquisition
        self.log_event(
            AuditEventType::RequestAllowed,
            req.source_node,
            AuditOutcome::Success,
        );

        Ok(ResourceGuard {
            enforcer: Arc::clone(self),
            claim,
            acquired_at: Instant::now(),
        })
    }

    /// Release resources (called by ResourceGuard on drop)
    fn release(&self, claim: &ResourceClaim) {
        // Use `fetch_update` + `saturating_sub` rather than raw
        // `fetch_sub` on `concurrent` and `memory_mb`. `acquire()`
        // short-circuits in `EnforcementMode::Disabled` and returns
        // a guard WITHOUT incrementing those counters; a raw
        // `fetch_sub` from a counter at 0 would wrap to ~`u32::MAX`,
        // and the next `Enforce`-mode `acquire` would see
        // `current.saturating_add(claim) > max_concurrent` and reject
        // every request forever (mode is hot-swappable via
        // `update_envelope`, so warm-up in `Disabled` then flip to
        // `Enforce` is the real-world trigger). The matching
        // tokens/cost paths already use `fetch_update` +
        // `saturating_sub` for exactly this reason.
        //
        // Use `AcqRel` (not `Relaxed`) to mirror the acquire path's
        // `try_fetch_add_capped_u32` ordering. Pre-fix the
        // asymmetric `Relaxed` release on weakly-ordered cores
        // (ARM / RISC-V) let a subsequent acquirer observe the
        // post-release counter while the release-side caller's
        // prior reads of the freed resource were still visible to
        // its CPU only — a window where the resource looked
        // available to the acquirer while the previous owner was
        // still touching it. The total counter eventually
        // converges, but the ordering mismatch produced
        // observable drift on metrics readers.
        let _ =
            self.usage
                .concurrent
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                    Some(current.saturating_sub(claim.concurrent_slots))
                });
        let _ = self
            .usage
            .memory_mb
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                Some(current.saturating_sub(claim.memory_mb))
            });
        // Release tokens and cost that were acquired — without this,
        // both counters grow monotonically, hitting limits prematurely.
        let _ = self
            .usage
            .tokens
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                Some(current.saturating_sub(claim.tokens as u64))
            });
        let _ = self.usage.cost_cents_per_hour.fetch_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |current| Some(current.saturating_sub(claim.cost_cents)),
        );
    }

    /// Trigger the kill switch
    pub fn kill(&self, reason: impl Into<String>) {
        let reason = reason.into();
        self.kill_switch.store(true, Ordering::SeqCst);
        *self.kill_switch_at.write().unwrap() = Some(Instant::now());
        *self.kill_switch_reason.write().unwrap() = Some(reason.clone());

        self.log_event_with_details(
            AuditEventType::KillSwitchTriggered,
            None,
            AuditOutcome::Success,
            [("reason".to_string(), reason)].into_iter().collect(),
        );
    }

    /// Reset the kill switch
    pub fn reset(&self) {
        self.kill_switch.store(false, Ordering::SeqCst);
        *self.kill_switch_at.write().unwrap() = None;
        *self.kill_switch_reason.write().unwrap() = None;

        self.log_event(AuditEventType::KillSwitchReset, None, AuditOutcome::Success);
    }

    /// Check if kill switch is active
    pub fn is_killed(&self) -> bool {
        self.kill_switch.load(Ordering::Relaxed)
    }

    /// Get current usage statistics
    pub fn usage(&self) -> UsageStats {
        UsageStats {
            concurrent: self.usage.concurrent.load(Ordering::Relaxed),
            tokens: self.usage.tokens.load(Ordering::Relaxed),
            memory_mb: self.usage.memory_mb.load(Ordering::Relaxed),
            cost_cents_per_hour: self.usage.cost_cents_per_hour.load(Ordering::Relaxed),
            requests_per_minute: self.rate_limiter.global_requests.load(Ordering::Relaxed),
            tokens_per_minute: self.rate_limiter.global_tokens.load(Ordering::Relaxed),
        }
    }

    /// Get recent audit entries
    pub fn audit_entries(&self, limit: usize) -> Vec<AuditEntry> {
        self.audit_log.get_entries(limit)
    }

    /// Clear audit log
    pub fn clear_audit(&self) {
        self.audit_log.clear();
    }

    /// Get the current envelope
    pub fn envelope(&self) -> SafetyEnvelope {
        self.envelope.read().unwrap().clone()
    }

    // Internal methods

    fn check_kill_switch(&self) -> Result<(), SafetyViolation> {
        if !self.kill_switch.load(Ordering::Relaxed) {
            return Ok(());
        }

        // Check auto-reset
        let envelope = self.envelope.read().unwrap();
        if let Some(auto_reset_secs) = envelope.kill_switch.auto_reset_secs {
            if let Some(killed_at) = *self.kill_switch_at.read().unwrap() {
                if killed_at.elapsed() >= Duration::from_secs(auto_reset_secs as u64) {
                    drop(envelope);
                    self.reset();
                    return Ok(());
                }
            }
        }

        let reason = self
            .kill_switch_reason
            .read()
            .unwrap()
            .clone()
            .unwrap_or_else(|| "kill switch active".to_string());

        Err(SafetyViolation::KillSwitchActive { reason })
    }

    fn check_rate_limits(
        &self,
        req: &SafetyRequest,
        envelope: &SafetyEnvelope,
    ) -> Result<(), SafetyViolation> {
        let limits = &envelope.rate_limits;
        let burst = limits.burst_multiplier;
        let outcome = match envelope.mode {
            EnforcementMode::Enforce => AuditOutcome::Blocked,
            EnforcementMode::AuditOnly => AuditOutcome::Warning,
            EnforcementMode::Disabled => AuditOutcome::Warning,
        };

        // Check global RPM
        if let Err(e) = self.rate_limiter.check_global_rpm(limits.global_rpm, burst) {
            self.log_event(AuditEventType::RateLimitHit, req.source_node, outcome);
            if envelope.mode == EnforcementMode::Enforce {
                return Err(e);
            }
        }

        // Check per-source RPM
        if let Some(ref source) = req.source_node {
            if let Err(e) = self
                .rate_limiter
                .check_source_rpm(source, limits.per_source_rpm, burst)
            {
                self.log_event(AuditEventType::RateLimitHit, req.source_node, outcome);
                if envelope.mode == EnforcementMode::Enforce {
                    return Err(e);
                }
            }
        }

        // Check tokens per minute
        if let Err(e) = self.rate_limiter.check_tokens(
            req.estimated_tokens as u64,
            limits.tokens_per_minute,
            burst,
        ) {
            self.log_event(AuditEventType::RateLimitHit, req.source_node, outcome);
            if envelope.mode == EnforcementMode::Enforce {
                return Err(e);
            }
        }

        Ok(())
    }

    #[allow(dead_code)]
    fn check_resource_limits(
        &self,
        claim: &ResourceClaim,
        envelope: &SafetyEnvelope,
    ) -> Result<(), SafetyViolation> {
        let limits = &envelope.resource_limits;

        // Check concurrent
        // Use saturating arithmetic to prevent underflow when current > max
        // (possible if limits were reduced while requests are in-flight).
        let current_concurrent = self.usage.concurrent.load(Ordering::Relaxed);
        if current_concurrent.saturating_add(claim.concurrent_slots) > limits.max_concurrent
            && envelope.mode == EnforcementMode::Enforce
        {
            return Err(SafetyViolation::ResourceLimitExceeded {
                resource: ResourceType::Concurrent,
                requested: claim.concurrent_slots as u64,
                available: limits.max_concurrent.saturating_sub(current_concurrent) as u64,
            });
        }

        // Check tokens
        if claim.tokens > limits.max_tokens_per_request && envelope.mode == EnforcementMode::Enforce
        {
            return Err(SafetyViolation::ResourceLimitExceeded {
                resource: ResourceType::Tokens,
                requested: claim.tokens as u64,
                available: limits.max_tokens_per_request as u64,
            });
        }

        // Check memory
        let current_memory = self.usage.memory_mb.load(Ordering::Relaxed);
        if current_memory.saturating_add(claim.memory_mb) > limits.max_memory_mb
            && envelope.mode == EnforcementMode::Enforce
        {
            return Err(SafetyViolation::ResourceLimitExceeded {
                resource: ResourceType::Memory,
                requested: claim.memory_mb as u64,
                available: limits.max_memory_mb.saturating_sub(current_memory) as u64,
            });
        }

        // Check hourly cost
        self.usage.maybe_reset_hourly();
        let current_cost = self.usage.cost_cents_per_hour.load(Ordering::Relaxed);
        if current_cost.saturating_add(claim.cost_cents) > limits.max_cost_per_hour_cents
            && envelope.mode == EnforcementMode::Enforce
        {
            return Err(SafetyViolation::ResourceLimitExceeded {
                resource: ResourceType::Cost,
                requested: claim.cost_cents as u64,
                available: limits.max_cost_per_hour_cents.saturating_sub(current_cost) as u64,
            });
        }

        Ok(())
    }

    fn check_content_policies(
        &self,
        req: &SafetyRequest,
        envelope: &SafetyEnvelope,
    ) -> Result<(), SafetyViolation> {
        for policy in &envelope.content_policies {
            if !policy.enabled {
                continue;
            }

            if let Err(violation) = self.check_policy(req, policy) {
                match policy.action {
                    PolicyAction::Block => {
                        if envelope.mode == EnforcementMode::Enforce {
                            self.log_event(
                                AuditEventType::ContentPolicyViolation,
                                req.source_node,
                                AuditOutcome::Blocked,
                            );
                            return Err(violation);
                        }
                    }
                    PolicyAction::Warn => {
                        self.log_event(
                            AuditEventType::ContentPolicyViolation,
                            req.source_node,
                            AuditOutcome::Warning,
                        );
                    }
                    PolicyAction::Log => {
                        self.log_event(
                            AuditEventType::ContentPolicyViolation,
                            req.source_node,
                            AuditOutcome::Warning,
                        );
                    }
                    PolicyAction::Redact => {
                        // Redaction would need mutable access to content
                        // For now, just log
                        self.log_event(
                            AuditEventType::ContentPolicyViolation,
                            req.source_node,
                            AuditOutcome::Warning,
                        );
                    }
                }
            }
        }

        Ok(())
    }

    fn check_policy(
        &self,
        req: &SafetyRequest,
        policy: &ContentPolicy,
    ) -> Result<(), SafetyViolation> {
        match &policy.check {
            ContentCheck::MaxSize(max_size) => {
                if req.content_size > *max_size {
                    return Err(SafetyViolation::ContentPolicyViolation {
                        policy_id: policy.id.clone(),
                        details: format!(
                            "content size {} exceeds max {}",
                            req.content_size, max_size
                        ),
                    });
                }
            }
            ContentCheck::BlockPatterns(patterns) => {
                if let Some(ref content) = req.content {
                    for pattern in patterns {
                        if content.contains(pattern) {
                            return Err(SafetyViolation::ContentPolicyViolation {
                                policy_id: policy.id.clone(),
                                details: format!("blocked pattern found: {}", pattern),
                            });
                        }
                    }
                }
            }
            ContentCheck::RequirePatterns(patterns) => {
                if let Some(ref content) = req.content {
                    for pattern in patterns {
                        if !content.contains(pattern) {
                            return Err(SafetyViolation::ContentPolicyViolation {
                                policy_id: policy.id.clone(),
                                details: format!("required pattern not found: {}", pattern),
                            });
                        }
                    }
                }
            }
            ContentCheck::Custom { validator_id } => {
                // Custom validators would be registered externally
                // For now, this is a placeholder
                let _ = validator_id;
            }
        }

        Ok(())
    }

    fn log_event(
        &self,
        event_type: AuditEventType,
        source_node: Option<NodeId>,
        outcome: AuditOutcome,
    ) {
        self.log_event_with_details(event_type, source_node, outcome, HashMap::new());
    }

    fn log_event_with_details(
        &self,
        event_type: AuditEventType,
        source_node: Option<NodeId>,
        outcome: AuditOutcome,
        details: HashMap<String, String>,
    ) {
        let entry = AuditEntry {
            timestamp_ns: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0),
            event_type,
            source_node,
            request_id: None,
            details,
            outcome,
        };
        self.audit_log.log(entry);
    }
}

impl Default for SafetyEnforcer {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_node_id(n: u8) -> NodeId {
        let mut id = [0u8; 32];
        id[0] = n;
        id
    }

    #[test]
    fn test_default_envelope() {
        let envelope = SafetyEnvelope::default();
        assert_eq!(envelope.mode, EnforcementMode::Enforce);
        assert_eq!(envelope.resource_limits.max_concurrent, 1000);
        assert_eq!(envelope.rate_limits.global_rpm, 10_000);
    }

    #[test]
    fn test_safety_enforcer_check_passes() {
        let enforcer = SafetyEnforcer::new();
        let req = SafetyRequest::new().with_tokens(100);

        let result = enforcer.check(&req);
        assert!(result.is_ok());
    }

    #[test]
    fn test_kill_switch() {
        let enforcer = SafetyEnforcer::new();
        let req = SafetyRequest::new();

        // Initially should pass
        assert!(enforcer.check(&req).is_ok());
        assert!(!enforcer.is_killed());

        // Trigger kill switch
        enforcer.kill("test kill");
        assert!(enforcer.is_killed());

        // Should now fail
        let result = enforcer.check(&req);
        assert!(matches!(
            result,
            Err(SafetyViolation::KillSwitchActive { .. })
        ));

        // Reset
        enforcer.reset();
        assert!(!enforcer.is_killed());
        assert!(enforcer.check(&req).is_ok());
    }

    #[test]
    fn test_resource_acquisition() {
        let enforcer = Arc::new(SafetyEnforcer::new());
        let req = SafetyRequest::new();
        let claim = ResourceClaim::new().with_concurrent(1).with_tokens(100);

        // Acquire resources
        let guard = enforcer.acquire(&req, claim).unwrap();
        assert_eq!(enforcer.usage().concurrent, 1);
        assert_eq!(enforcer.usage().tokens, 100);

        // Release resources (drop guard)
        drop(guard);
        assert_eq!(enforcer.usage().concurrent, 0);
    }

    #[test]
    fn test_concurrent_limit() {
        let envelope = SafetyEnvelope {
            resource_limits: ResourceEnvelope {
                max_concurrent: 2,
                ..Default::default()
            },
            ..Default::default()
        };
        let enforcer = Arc::new(SafetyEnforcer::with_envelope(envelope));
        let req = SafetyRequest::new();
        let claim = ResourceClaim::new().with_concurrent(1);

        // Acquire 2 slots
        let _guard1 = enforcer.acquire(&req, claim.clone()).unwrap();
        let _guard2 = enforcer.acquire(&req, claim.clone()).unwrap();

        // Third should fail
        let result = enforcer.acquire(&req, claim);
        assert!(matches!(
            result,
            Err(SafetyViolation::ResourceLimitExceeded {
                resource: ResourceType::Concurrent,
                ..
            })
        ));
    }

    #[test]
    fn test_content_policy_max_size() {
        let envelope = SafetyEnvelope {
            content_policies: vec![ContentPolicy {
                id: "max-size".to_string(),
                check: ContentCheck::MaxSize(100),
                action: PolicyAction::Block,
                enabled: true,
            }],
            ..Default::default()
        };
        let enforcer = SafetyEnforcer::with_envelope(envelope);

        // Small content should pass
        let req = SafetyRequest::new().with_content_size(50);
        assert!(enforcer.check(&req).is_ok());

        // Large content should fail
        let req = SafetyRequest::new().with_content_size(200);
        assert!(matches!(
            enforcer.check(&req),
            Err(SafetyViolation::ContentPolicyViolation { .. })
        ));
    }

    #[test]
    fn test_content_policy_block_patterns() {
        let envelope = SafetyEnvelope {
            content_policies: vec![ContentPolicy {
                id: "block-bad".to_string(),
                check: ContentCheck::BlockPatterns(vec!["bad_word".to_string()]),
                action: PolicyAction::Block,
                enabled: true,
            }],
            ..Default::default()
        };
        let enforcer = SafetyEnforcer::with_envelope(envelope);

        // Clean content should pass
        let req = SafetyRequest::new().with_content("hello world");
        assert!(enforcer.check(&req).is_ok());

        // Content with blocked pattern should fail
        let req = SafetyRequest::new().with_content("this has a bad_word in it");
        assert!(matches!(
            enforcer.check(&req),
            Err(SafetyViolation::ContentPolicyViolation { .. })
        ));
    }

    #[test]
    fn test_audit_only_mode() {
        let envelope = SafetyEnvelope {
            mode: EnforcementMode::AuditOnly,
            content_policies: vec![ContentPolicy {
                id: "max-size".to_string(),
                check: ContentCheck::MaxSize(100),
                action: PolicyAction::Block,
                enabled: true,
            }],
            ..Default::default()
        };
        let enforcer = SafetyEnforcer::with_envelope(envelope);

        // Should pass even with violation (audit only)
        let req = SafetyRequest::new().with_content_size(200);
        assert!(enforcer.check(&req).is_ok());
    }

    #[test]
    fn test_disabled_mode() {
        let envelope = SafetyEnvelope {
            mode: EnforcementMode::Disabled,
            ..Default::default()
        };
        let enforcer = SafetyEnforcer::with_envelope(envelope);

        // Should pass even with kill switch (disabled mode)
        enforcer.kill("test");
        let req = SafetyRequest::new();
        assert!(enforcer.check(&req).is_ok());
    }

    #[test]
    fn test_usage_stats() {
        let enforcer = Arc::new(SafetyEnforcer::new());
        let req = SafetyRequest::new();
        let claim = ResourceClaim::new()
            .with_concurrent(5)
            .with_tokens(1000)
            .with_memory_mb(100);

        let _guard = enforcer.acquire(&req, claim).unwrap();

        let stats = enforcer.usage();
        assert_eq!(stats.concurrent, 5);
        assert_eq!(stats.tokens, 1000);
        assert_eq!(stats.memory_mb, 100);
    }

    #[test]
    fn test_audit_entries() {
        let envelope = SafetyEnvelope {
            audit: AuditConfig {
                enabled: true,
                log_success: true,
                log_blocked: true,
                log_warnings: true,
                max_entries: 100,
                flush_interval_ms: 5000,
            },
            ..Default::default()
        };
        let enforcer = Arc::new(SafetyEnforcer::with_envelope(envelope));
        let req = SafetyRequest::new();
        let claim = ResourceClaim::new().with_concurrent(1);

        // Acquire and release
        let _guard = enforcer.acquire(&req, claim).unwrap();
        drop(_guard);

        // Check audit log
        let entries = enforcer.audit_entries(10);
        assert!(!entries.is_empty());
    }

    #[test]
    fn test_rate_limiting() {
        let envelope = SafetyEnvelope {
            rate_limits: RateEnvelope {
                global_rpm: 2,
                per_source_rpm: 1,
                tokens_per_minute: 1000,
                burst_multiplier: 1.0,
            },
            ..Default::default()
        };
        let enforcer = SafetyEnforcer::with_envelope(envelope);
        let source = make_node_id(1);

        // First request should pass
        let req = SafetyRequest::new().with_source(source).with_tokens(100);
        assert!(enforcer.check(&req).is_ok());
        enforcer.rate_limiter.record_request(Some(&source), 100);

        // Second request from same source should hit per-source limit
        let result = enforcer.check(&req);
        assert!(matches!(
            result,
            Err(SafetyViolation::RateLimitExceeded {
                limit_type: RateLimitType::PerSourceRpm,
                ..
            })
        ));
    }

    /// Pin: in `AuditOnly` mode every rate-limit violation must
    /// still produce a `RateLimitHit` audit entry (with
    /// `Warning` outcome) — pre-fix the violation was silently
    /// dropped because the `log_event` call was nested inside
    /// the `if mode == Enforce` branch, contradicting the
    /// envelope's documented "log violations but don't block"
    /// semantics.
    #[test]
    fn audit_only_mode_logs_rate_limit_violations_as_warnings() {
        let envelope = SafetyEnvelope {
            mode: EnforcementMode::AuditOnly,
            rate_limits: RateEnvelope {
                global_rpm: 1,
                per_source_rpm: 1,
                tokens_per_minute: 1000,
                burst_multiplier: 1.0,
            },
            audit: AuditConfig {
                enabled: true,
                log_success: false,
                log_blocked: true,
                log_warnings: true,
                max_entries: 100,
                flush_interval_ms: 5000,
            },
            ..Default::default()
        };
        let enforcer = SafetyEnforcer::with_envelope(envelope);
        let source = make_node_id(7);
        let req = SafetyRequest::new().with_source(source).with_tokens(100);

        // Burn the first request through the limiter so the
        // second exceeds it.
        assert!(enforcer.check(&req).is_ok());
        enforcer.rate_limiter.record_request(Some(&source), 100);

        // Second request: violation under AuditOnly. Must NOT
        // return Err (audit-only doesn't block) AND must log a
        // Warning-outcome RateLimitHit.
        assert!(
            enforcer.check(&req).is_ok(),
            "AuditOnly must not block the request"
        );

        let entries = enforcer.audit_entries(100);
        let hits: Vec<_> = entries
            .iter()
            .filter(|e| e.event_type == AuditEventType::RateLimitHit)
            .collect();
        assert!(
            !hits.is_empty(),
            "AuditOnly mode must emit a RateLimitHit audit entry on violation; \
             pre-fix the entry was suppressed because logging was gated on \
             Enforce mode. Entries: {:?}",
            entries,
        );
        assert!(
            hits.iter().all(|e| e.outcome == AuditOutcome::Warning),
            "AuditOnly violations must be logged with Warning outcome \
             (Blocked is reserved for the Enforce path that actually \
             returns Err). Outcomes: {:?}",
            hits.iter().map(|e| e.outcome).collect::<Vec<_>>(),
        );
    }

    /// Regression for BUG_AUDIT_2026_04_30_CORE.md #102: pre-fix
    /// `release()` used raw `fetch_sub` on `concurrent` and
    /// `memory_mb`. `acquire()` short-circuits in `Disabled`
    /// mode WITHOUT incrementing those counters; the matching
    /// release would then `fetch_sub` from 0, wrapping `u32` to
    /// ~4 billion. The next `Enforce`-mode `acquire` would see
    /// the wrapped value, decide the cap was exceeded, and reject
    /// every request forever (envelope is hot-swappable at
    /// runtime — operators warm-up in `Disabled` then flip).
    ///
    /// We pin the fix by:
    ///   1. Building an enforcer in `Disabled` mode.
    ///   2. Acquiring + dropping a guard with non-zero claim.
    ///   3. Asserting `concurrent` and `memory_mb` are still 0
    ///      (saturating_sub kept them clamped).
    ///   4. Switching to `Enforce` mode and acquiring again to
    ///      confirm the next acquire path doesn't see a wrapped
    ///      counter.
    #[test]
    fn release_does_not_underflow_concurrent_or_memory_in_disabled_mode() {
        let enforcer = Arc::new(SafetyEnforcer::with_envelope(SafetyEnvelope {
            mode: EnforcementMode::Disabled,
            ..Default::default()
        }));
        let req = SafetyRequest::new();
        let claim = ResourceClaim::new().with_concurrent(5).with_memory_mb(100);

        // Acquire (no-op in Disabled — counters stay at 0) +
        // drop (release runs, would have wrapped u32 to ~4B
        // pre-fix).
        let guard = enforcer.acquire(&req, claim).unwrap();
        drop(guard);

        let stats = enforcer.usage();
        assert_eq!(
            stats.concurrent, 0,
            "concurrent must stay clamped at 0 when releasing in \
             Disabled mode (pre-fix this wrapped to u32::MAX-4)"
        );
        assert_eq!(
            stats.memory_mb, 0,
            "memory_mb must stay clamped at 0 when releasing in \
             Disabled mode (pre-fix this wrapped to u32::MAX-99)"
        );

        // Hot-swap to Enforce. The next acquire must NOT see a
        // wrapped counter — it must see 0 and admit the request.
        let new_envelope = SafetyEnvelope {
            mode: EnforcementMode::Enforce,
            ..Default::default()
        };
        enforcer.update_envelope(new_envelope);

        let req2 = SafetyRequest::new();
        let claim2 = ResourceClaim::new().with_concurrent(1);
        // Pre-fix: this would error with `ResourceLimitExceeded`
        // because the wrapped counter exceeded `max_concurrent`.
        let guard2 = enforcer
            .acquire(&req2, claim2)
            .expect("Enforce-mode acquire after a Disabled-mode release must succeed");
        drop(guard2);
    }

    #[test]
    fn test_regression_release_decrements_tokens_and_cost() {
        // Regression: release() only decremented concurrent slots and
        // memory, but not tokens or cost_cents_per_hour. Both counters
        // grew monotonically, hitting limits prematurely.
        let enforcer = Arc::new(SafetyEnforcer::new());
        let source = make_node_id(1);
        let req = SafetyRequest::new().with_source(source).with_tokens(500);
        let claim = ResourceClaim {
            tokens: 500,
            concurrent_slots: 1,
            memory_mb: 100,
            time_ms: 0,
            cost_cents: 50,
        };

        let guard = enforcer.acquire(&req, claim).unwrap();

        // Tokens and cost should be nonzero after acquire
        assert!(enforcer.usage.tokens.load(Ordering::Relaxed) >= 500);
        assert!(enforcer.usage.cost_cents_per_hour.load(Ordering::Relaxed) >= 50);

        // Drop the guard (triggers release)
        drop(guard);

        // Tokens and cost should be decremented back
        assert_eq!(
            enforcer.usage.tokens.load(Ordering::Relaxed),
            0,
            "tokens should be released on drop"
        );
        assert_eq!(
            enforcer.usage.cost_cents_per_hour.load(Ordering::Relaxed),
            0,
            "cost should be released on drop"
        );
    }

    #[test]
    fn test_regression_update_tokens_no_underflow() {
        // Regression: update_tokens with a lower actual count used
        // fetch_sub on the global AtomicU64 counter, which wraps to
        // u64::MAX on underflow — permanently locking out all requests.
        let enforcer = Arc::new(SafetyEnforcer::new());
        let source = make_node_id(1);
        let req = SafetyRequest::new().with_source(source).with_tokens(100);
        let claim = ResourceClaim {
            tokens: 100,
            concurrent_slots: 1,
            memory_mb: 10,
            time_ms: 0,
            cost_cents: 0,
        };

        let mut guard = enforcer.acquire(&req, claim).unwrap();

        // Simulate actual usage being lower than estimated
        guard.update_tokens(30);

        // Counter should reflect the difference (subtracted 70)
        let tokens = enforcer.usage.tokens.load(Ordering::Relaxed);
        assert!(
            tokens < u64::MAX / 2,
            "token counter should not have underflowed (got {})",
            tokens
        );

        drop(guard);

        // After release, tokens should be 0 (saturating)
        let final_tokens = enforcer.usage.tokens.load(Ordering::Relaxed);
        assert_eq!(
            final_tokens, 0,
            "tokens should be 0 after release, not underflowed"
        );
    }

    #[test]
    fn test_regression_check_tokens_overflow_is_rejected() {
        // Regression (MEDIUM, BUGS.md): `check_tokens` computed
        // `current + tokens` on two u64 values without an overflow
        // guard. Under high accumulated `current` the addition
        // panicked in debug (DoS) or wrapped in release (bypass).
        //
        // Fix: use `checked_add` and treat overflow as "over limit".
        let limiter = RateLimiter::new();
        // Seed the counter to near-saturation so the next `tokens`
        // value would wrap.
        limiter
            .global_tokens
            .store(u64::MAX - 10, Ordering::Relaxed);

        // Asking to add 100 more tokens would overflow u64.
        let result = limiter.check_tokens(100, 1_000_000, 1.0);
        assert!(
            matches!(
                result,
                Err(SafetyViolation::RateLimitExceeded {
                    limit_type: RateLimitType::TokensPerMinute,
                    ..
                })
            ),
            "overflow must be rejected, got {:?}",
            result
        );
    }

    /// Regression: BUG_REPORT.md #8 — `acquire` previously did
    /// `load + compare` (`check_resource_limits`) then
    /// `fetch_add`. N concurrent acquirers all observed the same
    /// pre-add value and all proceeded past the cap. The fix
    /// uses `fetch_update` per cumulative resource so the check +
    /// add is atomic per counter.
    ///
    /// We pin this by spawning many threads that each try to
    /// acquire 1 concurrent slot against a cap of K. Pre-fix,
    /// the final `concurrent` counter could exceed K. Post-fix,
    /// the cap is honored exactly: we see at most K successful
    /// `acquire`s and the rest fail with `ResourceLimitExceeded`.
    #[test]
    fn acquire_concurrent_cap_is_atomic_under_contention() {
        use std::sync::Arc;
        use std::sync::Barrier;
        use std::thread;

        const CAP: u32 = 5;
        const ATTEMPTS: usize = 100;

        // Build an enforcer with `max_concurrent = CAP` and very
        // permissive other limits, so the race surfaces only on
        // `concurrent`.
        let limits = ResourceEnvelope {
            max_concurrent: CAP,
            max_tokens_per_request: 1_000_000,
            max_memory_mb: 1_000_000,
            max_time_ms: 1_000_000,
            max_cost_per_hour_cents: u32::MAX,
        };
        let envelope = SafetyEnvelope {
            mode: EnforcementMode::Enforce,
            resource_limits: limits,
            ..Default::default()
        };
        let enforcer = Arc::new(SafetyEnforcer::with_envelope(envelope));

        let barrier = Arc::new(Barrier::new(ATTEMPTS));
        let handles: Vec<_> = (0..ATTEMPTS)
            .map(|_| {
                let enf = Arc::clone(&enforcer);
                let b = Arc::clone(&barrier);
                thread::spawn(move || {
                    b.wait();
                    let req = SafetyRequest::new();
                    let claim = ResourceClaim {
                        concurrent_slots: 1,
                        tokens: 1,
                        memory_mb: 0,
                        time_ms: 0,
                        cost_cents: 0,
                    };
                    enf.acquire(&req, claim)
                })
            })
            .collect();

        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let successes: Vec<_> = results.into_iter().filter_map(|r| r.ok()).collect();

        // The crucial invariant: no more than CAP concurrent
        // claims actually committed. Pre-fix this would routinely
        // exceed CAP under high contention.
        assert!(
            successes.len() as u32 <= CAP,
            "TOCTOU regression (#8): {} concurrent acquires committed against \
             cap of {}",
            successes.len(),
            CAP
        );

        // And the counter itself reflects exactly that — never
        // higher than CAP.
        assert!(
            enforcer.usage.concurrent.load(Ordering::Relaxed) <= CAP,
            "concurrent counter exceeds cap"
        );
    }

    /// Regression: the rate-limit half of #8. Previously the
    /// global / per-source RPM and tokens-per-minute checks were
    /// load+compare in `check()` while the increment was a
    /// separate `record_request` in `acquire()`. Multiple
    /// concurrent acquirers could all pass the load+compare,
    /// then all increment past the cap. The fix CAS-ifies the
    /// check + add inside `acquire()` so the rate-limit cap is
    /// honored exactly under contention.
    #[test]
    fn acquire_global_rpm_cap_is_atomic_under_contention() {
        use std::sync::Arc;
        use std::sync::Barrier;
        use std::thread;

        const RPM_CAP: u32 = 5;
        const ATTEMPTS: usize = 100;

        let envelope = SafetyEnvelope {
            mode: EnforcementMode::Enforce,
            // Loose resource limits so concurrent / memory / cost
            // never trip; we want only the RPM cap to be the
            // contended counter.
            resource_limits: ResourceEnvelope {
                max_concurrent: u32::MAX,
                max_tokens_per_request: 1_000_000,
                max_memory_mb: u32::MAX,
                max_time_ms: u32::MAX,
                max_cost_per_hour_cents: u32::MAX,
            },
            rate_limits: RateEnvelope {
                global_rpm: RPM_CAP,
                per_source_rpm: u32::MAX,
                tokens_per_minute: u64::MAX,
                burst_multiplier: 1.0,
            },
            ..Default::default()
        };
        let enforcer = Arc::new(SafetyEnforcer::with_envelope(envelope));

        let barrier = Arc::new(Barrier::new(ATTEMPTS));
        let handles: Vec<_> = (0..ATTEMPTS)
            .map(|_| {
                let enf = Arc::clone(&enforcer);
                let b = Arc::clone(&barrier);
                thread::spawn(move || {
                    b.wait();
                    let req = SafetyRequest::new();
                    let claim = ResourceClaim {
                        concurrent_slots: 1,
                        tokens: 1,
                        memory_mb: 0,
                        time_ms: 0,
                        cost_cents: 0,
                    };
                    enf.acquire(&req, claim)
                })
            })
            .collect();

        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let successes: Vec<_> = results.into_iter().filter_map(|r| r.ok()).collect();

        assert!(
            successes.len() as u32 <= RPM_CAP,
            "RPM TOCTOU regression (#8): {} acquires committed against cap {}",
            successes.len(),
            RPM_CAP,
        );
        assert!(
            enforcer
                .rate_limiter
                .global_requests
                .load(Ordering::Relaxed)
                <= RPM_CAP as u64,
            "global_requests counter exceeds RPM cap",
        );
    }
}
