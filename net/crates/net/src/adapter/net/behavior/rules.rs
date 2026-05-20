//! Phase 4E: Device Autonomy Rules (DEVICE-RULES)
//!
//! This module provides a rule engine for autonomous device behavior:
//! - Declarative rules with conditions and actions
//! - Priority-based rule evaluation and conflict resolution
//! - Context-aware condition matching
//! - Action execution with rate limiting and cooldowns

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::metadata::NodeId;

/// Comparison operators for conditions
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CompareOp {
    /// Equal to
    Eq,
    /// Not equal to
    Ne,
    /// Less than
    Lt,
    /// Less than or equal
    Le,
    /// Greater than
    Gt,
    /// Greater than or equal
    Ge,
    /// Contains (for strings/arrays)
    Contains,
    /// Starts with (for strings)
    StartsWith,
    /// Ends with (for strings)
    EndsWith,
    /// Matches regex pattern
    Matches,
    /// Value is in set
    In,
    /// Value is not in set
    NotIn,
    /// Value exists (is not null)
    Exists,
    /// Value does not exist (is null)
    NotExists,
}

impl CompareOp {
    /// Evaluate comparison between two JSON values
    pub fn evaluate(&self, left: &serde_json::Value, right: &serde_json::Value) -> bool {
        match self {
            CompareOp::Eq => left == right,
            CompareOp::Ne => left != right,
            CompareOp::Lt => compare_values(left, right) == Some(std::cmp::Ordering::Less),
            CompareOp::Le => {
                matches!(
                    compare_values(left, right),
                    Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
                )
            }
            CompareOp::Gt => compare_values(left, right) == Some(std::cmp::Ordering::Greater),
            CompareOp::Ge => {
                matches!(
                    compare_values(left, right),
                    Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
                )
            }
            CompareOp::Contains => match (left, right) {
                (serde_json::Value::String(s), serde_json::Value::String(sub)) => {
                    s.contains(sub.as_str())
                }
                (serde_json::Value::Array(arr), val) => arr.contains(val),
                _ => false,
            },
            CompareOp::StartsWith => match (left, right) {
                (serde_json::Value::String(s), serde_json::Value::String(prefix)) => {
                    s.starts_with(prefix.as_str())
                }
                _ => false,
            },
            CompareOp::EndsWith => match (left, right) {
                (serde_json::Value::String(s), serde_json::Value::String(suffix)) => {
                    s.ends_with(suffix.as_str())
                }
                _ => false,
            },
            CompareOp::Matches => {
                // Simple pattern matching (not full regex for performance)
                match (left, right) {
                    (serde_json::Value::String(s), serde_json::Value::String(pattern)) => {
                        s.contains(pattern.as_str())
                    }
                    _ => false,
                }
            }
            CompareOp::In => match right {
                serde_json::Value::Array(arr) => arr.contains(left),
                _ => false,
            },
            CompareOp::NotIn => match right {
                serde_json::Value::Array(arr) => !arr.contains(left),
                _ => true,
            },
            CompareOp::Exists => !left.is_null(),
            CompareOp::NotExists => left.is_null(),
        }
    }
}

fn compare_values(
    left: &serde_json::Value,
    right: &serde_json::Value,
) -> Option<std::cmp::Ordering> {
    match (left, right) {
        (serde_json::Value::Number(a), serde_json::Value::Number(b)) => compare_numbers(a, b),
        (serde_json::Value::String(a), serde_json::Value::String(b)) => Some(a.cmp(b)),
        _ => None,
    }
}

/// Compare two `serde_json::Number` values without losing precision
/// when both fit in an integer type.
///
/// Pre-fix, `compare_values` always reduced both sides to
/// `f64` via `as_f64()`. For integer fields above `2^53` (e.g. byte
/// counts, ns timestamps, monotonic sequence numbers) the cast was
/// lossy: two adjacent values like `9_007_199_254_740_992` and
/// `9_007_199_254_740_993` compared `Equal`, so a `Gt` rule guarding
/// a quota silently failed to fire. NaN/Infinity (legal under
/// `arbitrary_precision`, though we don't enable it) yielded `None`,
/// silently masking the bug.
///
/// Post-fix:
///   1. Both i64 → exact i64 compare (handles negatives).
///   2. Both u64 → exact u64 compare (handles values > i64::MAX).
///   3. Mixed signed-i64 / large-u64 → resolved by sign:
///      a negative i64 is always less than any u64; a u64 above
///      i64::MAX is always greater than any negative i64.
///   4. At least one is a float → f64 fallback with the documented
///      lossy semantics.
fn compare_numbers(a: &serde_json::Number, b: &serde_json::Number) -> Option<std::cmp::Ordering> {
    // We don't enable serde_json's `arbitrary_precision` feature in
    // this tree, but a transitive dependency could turn it on
    // workspace-wide via Cargo's feature unification. With it, a
    // big-integer literal stops fitting in any of `as_i64` /
    // `as_u64` / `as_f64` (all three return `None`) and the rule
    // would silently fail to fire — quota gates, threshold checks,
    // and policy rules become no-ops with no diagnostic. The
    // `debug_assert!` makes the misuse loud during dev/test; in
    // release we still return `None` so the rule fails closed.
    debug_assert!(
        a.is_i64() || a.is_u64() || a.is_f64(),
        "compare_numbers: lhs is neither i64/u64/f64 — likely \
         `serde_json/arbitrary_precision` got enabled via feature \
         unification. Rule will silently fail closed in release."
    );
    debug_assert!(
        b.is_i64() || b.is_u64() || b.is_f64(),
        "compare_numbers: rhs is neither i64/u64/f64 — likely \
         `serde_json/arbitrary_precision` got enabled via feature \
         unification. Rule will silently fail closed in release."
    );

    // 1. Both fit in i64.
    if let (Some(ai), Some(bi)) = (a.as_i64(), b.as_i64()) {
        return Some(ai.cmp(&bi));
    }
    // 2. Both fit in u64. Order matters: i64 takes precedence above
    //    so non-negative integers that fit in both types use the
    //    signed compare; only this branch handles values > i64::MAX.
    if let (Some(au), Some(bu)) = (a.as_u64(), b.as_u64()) {
        return Some(au.cmp(&bu));
    }
    // 3. Mixed: one is a negative i64 (as_u64 returned None for it)
    //    and the other is a u64 above i64::MAX (as_i64 returned None
    //    for it). The negative side is always less than the
    //    non-negative side.
    if a.as_i64().is_some() && b.as_u64().is_some() {
        return Some(std::cmp::Ordering::Less);
    }
    if a.as_u64().is_some() && b.as_i64().is_some() {
        return Some(std::cmp::Ordering::Greater);
    }
    // 4. Float fallback. NaN → None, which collapses comparing-to-
    //    NaN rules to the existing `partial_cmp` semantics.
    a.as_f64()?.partial_cmp(&b.as_f64()?)
}

/// Logical operators for combining conditions
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LogicOp {
    /// All conditions must be true
    And,
    /// At least one condition must be true
    Or,
    /// No conditions must be true
    Not,
    /// Exactly one condition must be true
    Xor,
}

/// A single condition to evaluate
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Condition {
    /// Field path to evaluate (dot notation, e.g., "metrics.cpu_usage")
    pub field: String,
    /// Comparison operator
    pub op: CompareOp,
    /// Value to compare against
    pub value: serde_json::Value,
    /// Optional description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl Condition {
    /// Create a new condition
    pub fn new(field: impl Into<String>, op: CompareOp, value: serde_json::Value) -> Self {
        Self {
            field: field.into(),
            op,
            value,
            description: None,
        }
    }

    /// Add description
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    /// Evaluate condition against context
    pub fn evaluate(&self, context: &RuleContext) -> bool {
        let field_value = context.get_field(&self.field);
        self.op.evaluate(&field_value, &self.value)
    }

    // Convenience constructors

    /// Field equals value
    pub fn eq(field: impl Into<String>, value: impl Into<serde_json::Value>) -> Self {
        Self::new(field, CompareOp::Eq, value.into())
    }

    /// Field not equals value
    pub fn ne(field: impl Into<String>, value: impl Into<serde_json::Value>) -> Self {
        Self::new(field, CompareOp::Ne, value.into())
    }

    /// Field greater than value
    pub fn gt(field: impl Into<String>, value: impl Into<serde_json::Value>) -> Self {
        Self::new(field, CompareOp::Gt, value.into())
    }

    /// Field greater than or equal to value
    pub fn ge(field: impl Into<String>, value: impl Into<serde_json::Value>) -> Self {
        Self::new(field, CompareOp::Ge, value.into())
    }

    /// Field less than value
    pub fn lt(field: impl Into<String>, value: impl Into<serde_json::Value>) -> Self {
        Self::new(field, CompareOp::Lt, value.into())
    }

    /// Field less than or equal to value
    pub fn le(field: impl Into<String>, value: impl Into<serde_json::Value>) -> Self {
        Self::new(field, CompareOp::Le, value.into())
    }

    /// Field contains value
    pub fn contains(field: impl Into<String>, value: impl Into<serde_json::Value>) -> Self {
        Self::new(field, CompareOp::Contains, value.into())
    }

    /// Field is in set of values
    pub fn is_in(field: impl Into<String>, values: Vec<serde_json::Value>) -> Self {
        Self::new(field, CompareOp::In, serde_json::Value::Array(values))
    }

    /// Field exists
    pub fn exists(field: impl Into<String>) -> Self {
        Self::new(field, CompareOp::Exists, serde_json::Value::Null)
    }
}

/// A composite condition expression
#[derive(Debug, Clone, PartialEq)]
pub enum ConditionExpr {
    /// Single condition
    Single(Condition),
    /// Combine conditions with AND
    And(Vec<ConditionExpr>),
    /// Combine conditions with OR
    Or(Vec<ConditionExpr>),
    /// Negate a condition
    Not(Box<ConditionExpr>),
    /// Always true
    Always,
    /// Always false
    Never,
}

impl ConditionExpr {
    /// Create from single condition
    pub fn single(condition: Condition) -> Self {
        ConditionExpr::Single(condition)
    }

    /// Combine with AND
    #[expect(
        clippy::unwrap_used,
        reason = "len == 1 branch guarantees the iterator yields exactly one element"
    )]
    pub fn and(conditions: Vec<ConditionExpr>) -> Self {
        if conditions.is_empty() {
            ConditionExpr::Always
        } else if conditions.len() == 1 {
            conditions.into_iter().next().unwrap()
        } else {
            ConditionExpr::And(conditions)
        }
    }

    /// Combine with OR
    #[expect(
        clippy::unwrap_used,
        reason = "len == 1 branch guarantees the iterator yields exactly one element"
    )]
    pub fn or(conditions: Vec<ConditionExpr>) -> Self {
        if conditions.is_empty() {
            ConditionExpr::Never
        } else if conditions.len() == 1 {
            conditions.into_iter().next().unwrap()
        } else {
            ConditionExpr::Or(conditions)
        }
    }

    /// Negate condition
    pub fn negate(condition: ConditionExpr) -> Self {
        ConditionExpr::Not(Box::new(condition))
    }

    /// Evaluate expression against context
    pub fn evaluate(&self, context: &RuleContext) -> bool {
        match self {
            ConditionExpr::Single(c) => c.evaluate(context),
            ConditionExpr::And(conditions) => conditions.iter().all(|c| c.evaluate(context)),
            ConditionExpr::Or(conditions) => conditions.iter().any(|c| c.evaluate(context)),
            ConditionExpr::Not(c) => !c.evaluate(context),
            ConditionExpr::Always => true,
            ConditionExpr::Never => false,
        }
    }

    /// Count the number of conditions
    pub fn condition_count(&self) -> usize {
        match self {
            ConditionExpr::Single(_) => 1,
            ConditionExpr::And(conditions) | ConditionExpr::Or(conditions) => {
                conditions.iter().map(|c| c.condition_count()).sum()
            }
            ConditionExpr::Not(c) => c.condition_count(),
            ConditionExpr::Always | ConditionExpr::Never => 0,
        }
    }
}

/// Action types that can be executed when rules match
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Log a message
    Log {
        /// Severity level at which to log the message
        level: LogLevel,
        /// Text content of the log entry
        message: String,
    },
    /// Emit an event
    Emit {
        /// Identifier for the type of event being emitted
        event_type: String,
        /// Arbitrary JSON data attached to the event
        payload: serde_json::Value,
    },
    /// Set a context value
    SetContext {
        /// Context key to set
        key: String,
        /// JSON value to store under the key
        value: serde_json::Value,
    },
    /// Increment a counter
    IncrementCounter {
        /// Name of the counter to increment
        name: String,
        /// Amount by which to increment the counter
        amount: i64,
    },
    /// Send alert/notification
    Alert {
        /// Severity level of the alert
        severity: AlertSeverity,
        /// Short human-readable title for the alert
        title: String,
        /// Detailed description of the alert
        message: String,
    },
    /// Throttle/rate limit
    Throttle {
        /// Identifier key used to track the rate limit bucket
        key: String,
        /// Maximum number of requests allowed per second
        max_per_second: f64,
    },
    /// Reject request
    Reject {
        /// Human-readable explanation for the rejection
        reason: String,
        /// Numeric error code returned with the rejection
        code: u32,
    },
    /// Redirect to another node
    Redirect {
        /// Specific node to redirect to, if known
        target_node: Option<NodeId>,
        /// Tags used to select a target node when no specific node is given
        target_tags: Vec<String>,
    },
    /// Scale resources
    Scale {
        /// Name of the resource to scale
        resource: String,
        /// Whether to scale up or down
        direction: ScaleDirection,
        /// Magnitude of the scaling adjustment
        amount: u32,
    },
    /// Execute custom action
    Custom {
        /// Identifier for the custom action type
        action_type: String,
        /// Key-value parameters passed to the custom action handler
        params: HashMap<String, serde_json::Value>,
    },
    /// Chain multiple actions
    Chain(Vec<Action>),
    /// No action (useful for monitoring rules)
    Noop,
}

/// Log levels
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// Debug level
    Debug,
    /// Info level
    Info,
    /// Warning level
    Warn,
    /// Error level
    Error,
}

/// Alert severity levels
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertSeverity {
    /// Low priority
    Low,
    /// Medium priority
    Medium,
    /// High priority
    High,
    /// Critical priority
    Critical,
}

/// Scale direction
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScaleDirection {
    /// Scale up
    Up,
    /// Scale down
    Down,
}

impl Action {
    /// Create a log action
    pub fn log(level: LogLevel, message: impl Into<String>) -> Self {
        Action::Log {
            level,
            message: message.into(),
        }
    }

    /// Create an emit action
    pub fn emit(event_type: impl Into<String>, payload: serde_json::Value) -> Self {
        Action::Emit {
            event_type: event_type.into(),
            payload,
        }
    }

    /// Create a set context action
    pub fn set_context(key: impl Into<String>, value: serde_json::Value) -> Self {
        Action::SetContext {
            key: key.into(),
            value,
        }
    }

    /// Create an alert action
    pub fn alert(
        severity: AlertSeverity,
        title: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Action::Alert {
            severity,
            title: title.into(),
            message: message.into(),
        }
    }

    /// Create a reject action
    pub fn reject(reason: impl Into<String>, code: u32) -> Self {
        Action::Reject {
            reason: reason.into(),
            code,
        }
    }

    /// Create a redirect action
    pub fn redirect_to_tags(tags: Vec<String>) -> Self {
        Action::Redirect {
            target_node: None,
            target_tags: tags,
        }
    }

    /// Create a chain of actions
    pub fn chain(actions: Vec<Action>) -> Self {
        Action::Chain(actions)
    }

    /// Count total actions (including nested)
    pub fn action_count(&self) -> usize {
        match self {
            Action::Chain(actions) => actions.iter().map(|a| a.action_count()).sum(),
            _ => 1,
        }
    }
}

/// Rule priority levels
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Priority {
    /// Lowest priority (evaluated last)
    Lowest,
    /// Low priority
    Low,
    /// Normal priority (default)
    #[default]
    Normal,
    /// High priority
    High,
    /// Highest priority (evaluated first)
    Highest,
    /// Custom priority value
    Custom(u8),
}

impl PartialOrd for Priority {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Priority {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.value().cmp(&other.value())
    }
}

impl Priority {
    /// Get numeric priority value
    pub fn value(&self) -> u8 {
        match self {
            Priority::Lowest => 0,
            Priority::Low => 25,
            Priority::Normal => 50,
            Priority::High => 75,
            Priority::Highest => 100,
            Priority::Custom(v) => *v,
        }
    }
}

/// A complete rule definition
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    /// Unique rule ID
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// Description
    pub description: Option<String>,
    /// Rule priority
    pub priority: Priority,
    /// Condition expression
    pub condition: ConditionExpr,
    /// Action to execute when condition matches
    pub action: Action,
    /// Whether rule is enabled
    pub enabled: bool,
    /// Tags for categorization
    pub tags: Vec<String>,
    /// Cooldown between executions (milliseconds)
    pub cooldown_ms: Option<u64>,
    /// Maximum executions per time window
    pub rate_limit: Option<RateLimit>,
    /// Stop processing further rules if this one matches
    pub stop_on_match: bool,
    /// Created timestamp
    pub created_at: u64,
    /// Updated timestamp
    pub updated_at: u64,
}

/// Rate limit configuration
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateLimit {
    /// Maximum executions
    pub max_executions: u32,
    /// Time window in seconds
    pub window_secs: u32,
}

impl Rule {
    /// Create a new rule
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        // Pre-fix this used `.as_millis() as u64`, which
        // silently truncated the u128 millis on overflow. Realistic
        // dates (anything within u64::MAX milliseconds since UNIX
        // epoch — year ~584,554,051) are unaffected, but `as`
        // casts on durations are a footgun worth eliminating.
        // `try_from` saturates on overflow so a far-future clock
        // surfaces as a max-timestamp instead of wrapping to a
        // small value that would invert "newer-rule-wins"
        // comparisons.
        let now = u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        )
        .unwrap_or(u64::MAX);

        Self {
            id: id.into(),
            name: name.into(),
            description: None,
            priority: Priority::Normal,
            condition: ConditionExpr::Always,
            action: Action::Noop,
            enabled: true,
            tags: Vec::new(),
            cooldown_ms: None,
            rate_limit: None,
            stop_on_match: false,
            created_at: now,
            updated_at: now,
        }
    }

    /// Set description
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    /// Set priority
    pub fn with_priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }

    /// Set condition
    pub fn with_condition(mut self, condition: ConditionExpr) -> Self {
        self.condition = condition;
        self
    }

    /// Set action
    pub fn with_action(mut self, action: Action) -> Self {
        self.action = action;
        self
    }

    /// Add tag
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Set cooldown
    pub fn with_cooldown(mut self, cooldown_ms: u64) -> Self {
        self.cooldown_ms = Some(cooldown_ms);
        self
    }

    /// Set rate limit
    pub fn with_rate_limit(mut self, max_executions: u32, window_secs: u32) -> Self {
        self.rate_limit = Some(RateLimit {
            max_executions,
            window_secs,
        });
        self
    }

    /// Stop processing further rules on match
    pub fn stop_on_match(mut self) -> Self {
        self.stop_on_match = true;
        self
    }

    /// Disable rule
    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    /// Check if rule matches context
    pub fn matches(&self, context: &RuleContext) -> bool {
        self.enabled && self.condition.evaluate(context)
    }
}

/// Context for rule evaluation
#[derive(Debug, Clone, Default)]
pub struct RuleContext {
    /// Context data (nested JSON-like structure)
    data: HashMap<String, serde_json::Value>,
    /// Metadata about the evaluation
    metadata: HashMap<String, String>,
}

impl RuleContext {
    /// Create empty context
    pub fn new() -> Self {
        Self::default()
    }

    /// Create from JSON value
    pub fn from_value(value: serde_json::Value) -> Self {
        let mut ctx = Self::new();
        if let serde_json::Value::Object(map) = value {
            for (k, v) in map {
                ctx.data.insert(k, v);
            }
        }
        ctx
    }

    /// Set a value
    pub fn set(&mut self, key: impl Into<String>, value: serde_json::Value) {
        self.data.insert(key.into(), value);
    }

    /// Get a value by key
    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.data.get(key)
    }

    /// Get a field by dot-notation path
    pub fn get_field(&self, path: &str) -> serde_json::Value {
        let parts: Vec<&str> = path.split('.').collect();
        if parts.is_empty() {
            return serde_json::Value::Null;
        }

        let mut current = match self.data.get(parts[0]) {
            Some(v) => v.clone(),
            None => return serde_json::Value::Null,
        };

        for part in &parts[1..] {
            current = match current {
                serde_json::Value::Object(ref map) => {
                    map.get(*part).cloned().unwrap_or(serde_json::Value::Null)
                }
                serde_json::Value::Array(ref arr) => {
                    if let Ok(idx) = part.parse::<usize>() {
                        arr.get(idx).cloned().unwrap_or(serde_json::Value::Null)
                    } else {
                        serde_json::Value::Null
                    }
                }
                _ => serde_json::Value::Null,
            };
        }

        current
    }

    /// Set metadata
    pub fn set_metadata(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.metadata.insert(key.into(), value.into());
    }

    /// Get metadata
    pub fn get_metadata(&self, key: &str) -> Option<&str> {
        self.metadata.get(key).map(|s| s.as_str())
    }

    /// Merge another context into this one
    pub fn merge(&mut self, other: RuleContext) {
        for (k, v) in other.data {
            self.data.insert(k, v);
        }
        for (k, v) in other.metadata {
            self.metadata.insert(k, v);
        }
    }

    /// Convert to JSON value
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::Value::Object(
            self.data
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        )
    }
}

/// Result of evaluating a rule
#[derive(Debug, Clone)]
pub struct RuleResult {
    /// Rule that matched
    pub rule_id: String,
    /// Rule name
    pub rule_name: String,
    /// Whether the rule matched
    pub matched: bool,
    /// Action to execute (if matched)
    pub action: Option<Action>,
    /// Whether to stop processing more rules
    pub stop_processing: bool,
    /// Evaluation time in nanoseconds
    pub eval_time_ns: u64,
}

/// Execution state for a rule (tracks cooldowns and rate limits)
#[derive(Debug)]
struct RuleExecutionState {
    /// Last execution time
    last_execution: Option<Instant>,
    /// Execution count in current window
    execution_count: u32,
    /// Window start time
    window_start: Instant,
}

impl Default for RuleExecutionState {
    fn default() -> Self {
        Self {
            last_execution: None,
            execution_count: 0,
            window_start: Instant::now(),
        }
    }
}

/// Rule engine errors
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleError {
    /// Rule not found
    NotFound(String),
    /// Rule already exists
    AlreadyExists(String),
    /// Invalid rule
    Invalid(String),
    /// Rate limited
    RateLimited {
        /// Identifier of the rule that triggered the rate limit
        rule_id: String,
        /// Number of milliseconds to wait before retrying
        retry_after_ms: u64,
    },
    /// Cooldown active
    CooldownActive {
        /// Identifier of the rule currently in cooldown
        rule_id: String,
        /// Number of milliseconds remaining in the cooldown period
        remaining_ms: u64,
    },
}

impl std::fmt::Display for RuleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuleError::NotFound(id) => write!(f, "Rule not found: {}", id),
            RuleError::AlreadyExists(id) => write!(f, "Rule already exists: {}", id),
            RuleError::Invalid(msg) => write!(f, "Invalid rule: {}", msg),
            RuleError::RateLimited {
                rule_id,
                retry_after_ms,
            } => {
                write!(
                    f,
                    "Rule {} rate limited, retry after {}ms",
                    rule_id, retry_after_ms
                )
            }
            RuleError::CooldownActive {
                rule_id,
                remaining_ms,
            } => {
                write!(
                    f,
                    "Rule {} on cooldown, {}ms remaining",
                    rule_id, remaining_ms
                )
            }
        }
    }
}

impl std::error::Error for RuleError {}

/// Statistics for the rule engine
#[derive(Debug, Clone, Default)]
pub struct RuleEngineStats {
    /// Total rules
    pub total_rules: usize,
    /// Enabled rules
    pub enabled_rules: usize,
    /// Total evaluations
    pub evaluations: u64,
    /// Total matches
    pub matches: u64,
    /// Total actions executed
    pub actions_executed: u64,
    /// Rules by priority
    pub by_priority: HashMap<u8, usize>,
    /// Rules by tag
    pub by_tag: HashMap<String, usize>,
}

/// High-performance rule engine
pub struct RuleEngine {
    /// Rules sorted by priority (highest first)
    rules: Vec<Arc<Rule>>,
    /// Rule index by ID
    rules_by_id: HashMap<String, Arc<Rule>>,
    /// Rule index by tag
    rules_by_tag: HashMap<String, HashSet<String>>,
    /// Execution state for rate limiting and cooldowns
    execution_state: HashMap<String, RuleExecutionState>,
    /// Evaluation counter
    eval_count: AtomicU64,
    /// Match counter
    match_count: AtomicU64,
    /// Action counter
    action_count: AtomicU64,
}

impl RuleEngine {
    /// Create a new rule engine
    pub fn new() -> Self {
        Self {
            rules: Vec::new(),
            rules_by_id: HashMap::new(),
            rules_by_tag: HashMap::new(),
            execution_state: HashMap::new(),
            eval_count: AtomicU64::new(0),
            match_count: AtomicU64::new(0),
            action_count: AtomicU64::new(0),
        }
    }

    /// Add a rule
    pub fn add_rule(&mut self, rule: Rule) -> Result<(), RuleError> {
        if self.rules_by_id.contains_key(&rule.id) {
            return Err(RuleError::AlreadyExists(rule.id.clone()));
        }

        let rule_arc = Arc::new(rule);

        // Add to tag index
        for tag in &rule_arc.tags {
            self.rules_by_tag
                .entry(tag.clone())
                .or_default()
                .insert(rule_arc.id.clone());
        }

        // Add to ID index
        self.rules_by_id
            .insert(rule_arc.id.clone(), Arc::clone(&rule_arc));

        // Add to sorted list and re-sort by priority
        self.rules.push(rule_arc);
        self.rules
            .sort_by_key(|r| std::cmp::Reverse(r.priority.value()));

        Ok(())
    }

    /// Remove a rule
    pub fn remove_rule(&mut self, rule_id: &str) -> Option<Arc<Rule>> {
        let rule = self.rules_by_id.remove(rule_id)?;

        // Remove from tag index
        for tag in &rule.tags {
            if let Some(set) = self.rules_by_tag.get_mut(tag) {
                set.remove(rule_id);
            }
        }

        // Remove from sorted list
        self.rules.retain(|r| r.id != rule_id);

        // Remove execution state
        self.execution_state.remove(rule_id);

        Some(rule)
    }

    /// Get a rule by ID
    pub fn get_rule(&self, rule_id: &str) -> Option<Arc<Rule>> {
        self.rules_by_id.get(rule_id).cloned()
    }

    /// Get all rules
    pub fn rules(&self) -> &[Arc<Rule>] {
        &self.rules
    }

    /// Get rules by tag
    pub fn rules_by_tag(&self, tag: &str) -> Vec<Arc<Rule>> {
        self.rules_by_tag
            .get(tag)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.rules_by_id.get(id).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Evaluate all rules against context
    pub fn evaluate(&mut self, context: &RuleContext) -> Vec<RuleResult> {
        self.eval_count.fetch_add(1, Ordering::Relaxed);

        // First pass: evaluate rules and collect results
        let mut results = Vec::new();
        let mut rules_to_record = Vec::new();
        let mut should_stop = false;

        for rule in &self.rules {
            if should_stop {
                break;
            }

            let start = Instant::now();
            let matched = rule.matches(context);
            let eval_time = start.elapsed().as_nanos() as u64;

            if matched {
                self.match_count.fetch_add(1, Ordering::Relaxed);

                // Check cooldown and rate limit
                let can_execute = self.check_execution_allowed(&rule.id, rule.as_ref());

                let action = if can_execute {
                    rules_to_record.push(rule.id.clone());
                    self.action_count
                        .fetch_add(rule.action.action_count() as u64, Ordering::Relaxed);
                    Some(rule.action.clone())
                } else {
                    None
                };

                results.push(RuleResult {
                    rule_id: rule.id.clone(),
                    rule_name: rule.name.clone(),
                    matched: true,
                    action,
                    stop_processing: rule.stop_on_match,
                    eval_time_ns: eval_time,
                });

                if rule.stop_on_match {
                    should_stop = true;
                }
            } else {
                results.push(RuleResult {
                    rule_id: rule.id.clone(),
                    rule_name: rule.name.clone(),
                    matched: false,
                    action: None,
                    stop_processing: false,
                    eval_time_ns: eval_time,
                });
            }
        }

        // Second pass: record executions
        for rule_id in rules_to_record {
            self.record_execution(&rule_id);
        }

        results
    }

    /// Evaluate and return only matching rules
    pub fn evaluate_matching(&mut self, context: &RuleContext) -> Vec<RuleResult> {
        self.evaluate(context)
            .into_iter()
            .filter(|r| r.matched)
            .collect()
    }

    /// Evaluate until first match
    pub fn evaluate_first(&mut self, context: &RuleContext) -> Option<RuleResult> {
        self.eval_count.fetch_add(1, Ordering::Relaxed);

        let mut result = None;
        let mut rule_to_record = None;

        for rule in &self.rules {
            let start = Instant::now();
            let matched = rule.matches(context);
            let eval_time = start.elapsed().as_nanos() as u64;

            if matched {
                self.match_count.fetch_add(1, Ordering::Relaxed);

                let can_execute = self.check_execution_allowed(&rule.id, rule.as_ref());
                let action = if can_execute {
                    rule_to_record = Some(rule.id.clone());
                    self.action_count
                        .fetch_add(rule.action.action_count() as u64, Ordering::Relaxed);
                    Some(rule.action.clone())
                } else {
                    None
                };

                result = Some(RuleResult {
                    rule_id: rule.id.clone(),
                    rule_name: rule.name.clone(),
                    matched: true,
                    action,
                    stop_processing: rule.stop_on_match,
                    eval_time_ns: eval_time,
                });
                break;
            }
        }

        // Record execution after iteration is complete
        if let Some(rule_id) = rule_to_record {
            self.record_execution(&rule_id);
        }

        result
    }

    /// Check if a specific rule would match
    pub fn would_match(&self, rule_id: &str, context: &RuleContext) -> bool {
        self.rules_by_id
            .get(rule_id)
            .map(|r| r.matches(context))
            .unwrap_or(false)
    }

    /// Get statistics
    pub fn stats(&self) -> RuleEngineStats {
        let mut by_priority: HashMap<u8, usize> = HashMap::new();
        let mut by_tag: HashMap<String, usize> = HashMap::new();
        let mut enabled_count = 0;

        for rule in &self.rules {
            if rule.enabled {
                enabled_count += 1;
            }
            *by_priority.entry(rule.priority.value()).or_default() += 1;
            for tag in &rule.tags {
                *by_tag.entry(tag.clone()).or_default() += 1;
            }
        }

        RuleEngineStats {
            total_rules: self.rules.len(),
            enabled_rules: enabled_count,
            evaluations: self.eval_count.load(Ordering::Relaxed),
            matches: self.match_count.load(Ordering::Relaxed),
            actions_executed: self.action_count.load(Ordering::Relaxed),
            by_priority,
            by_tag,
        }
    }

    /// Number of rules
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Clear all rules
    pub fn clear(&mut self) {
        self.rules.clear();
        self.rules_by_id.clear();
        self.rules_by_tag.clear();
        self.execution_state.clear();
    }

    /// Reset execution state (cooldowns and rate limits)
    pub fn reset_execution_state(&mut self) {
        self.execution_state.clear();
    }

    // Check if execution is allowed (cooldown and rate limit)
    fn check_execution_allowed(&self, rule_id: &str, rule: &Rule) -> bool {
        let state = match self.execution_state.get(rule_id) {
            Some(s) => s,
            None => return true, // No state means no restrictions yet
        };

        let now = Instant::now();

        // Check cooldown
        if let Some(cooldown_ms) = rule.cooldown_ms {
            if let Some(last) = state.last_execution {
                let elapsed = now.duration_since(last).as_millis() as u64;
                if elapsed < cooldown_ms {
                    return false;
                }
            }
        }

        // Check rate limit
        if let Some(ref limit) = rule.rate_limit {
            let window_duration = Duration::from_secs(limit.window_secs as u64);
            if now.duration_since(state.window_start) < window_duration
                && state.execution_count >= limit.max_executions
            {
                return false;
            }
        }

        true
    }

    // Record an execution
    fn record_execution(&mut self, rule_id: &str) {
        let now = Instant::now();

        // Pre-fix this incremented `execution_count`
        // unconditionally, even for rules without a rate_limit.
        // A rule that toggled rate-limited → unlimited carried
        // its old count forever; on toggle BACK to rate-limited
        // with the same window, the count was already at-or-
        // above max and every execution was immediately blocked
        // — silent stuck-rule on hot reload.
        //
        // Fix: only touch rate-limit-specific state when the
        // current rule actually has a rate_limit. The
        // last_execution timestamp is independently used by
        // cooldown-based gating, so it advances regardless.
        let has_rate_limit = self
            .rules_by_id
            .get(rule_id)
            .and_then(|r| r.rate_limit.as_ref())
            .is_some();

        let state = self.execution_state.entry(rule_id.to_string()).or_default();
        state.last_execution = Some(now);

        if has_rate_limit {
            state.execution_count += 1;
            // Reset window if needed.
            if let Some(rule) = self.rules_by_id.get(rule_id) {
                if let Some(ref limit) = rule.rate_limit {
                    let window_duration = Duration::from_secs(limit.window_secs as u64);
                    if now.duration_since(state.window_start) >= window_duration {
                        state.window_start = now;
                        state.execution_count = 1;
                    }
                }
            }
        }
    }
}

impl Default for RuleEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Rule set for a node (collection of rules with metadata)
#[derive(Debug, Clone, PartialEq)]
pub struct RuleSet {
    /// Rule set ID
    pub id: String,
    /// Rule set name
    pub name: String,
    /// Description
    pub description: Option<String>,
    /// Rules in this set
    pub rules: Vec<Rule>,
    /// Version
    pub version: u64,
    /// Created timestamp
    pub created_at: u64,
    /// Updated timestamp
    pub updated_at: u64,
    /// Tags
    pub tags: Vec<String>,
}

impl RuleSet {
    /// Create a new rule set
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        // See Rule::new for rationale on saturating
        // u128 → u64 instead of `as u64`.
        let now = u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        )
        .unwrap_or(u64::MAX);

        Self {
            id: id.into(),
            name: name.into(),
            description: None,
            rules: Vec::new(),
            version: 1,
            created_at: now,
            updated_at: now,
            tags: Vec::new(),
        }
    }

    /// Add a rule
    pub fn add_rule(mut self, rule: Rule) -> Self {
        self.rules.push(rule);
        self
    }

    /// Set description
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    /// Add tag
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Load into a rule engine
    pub fn load_into(&self, engine: &mut RuleEngine) -> Result<(), RuleError> {
        for rule in &self.rules {
            engine.add_rule(rule.clone())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compare_op() {
        assert!(CompareOp::Eq.evaluate(&serde_json::json!(5), &serde_json::json!(5)));
        assert!(!CompareOp::Eq.evaluate(&serde_json::json!(5), &serde_json::json!(10)));

        assert!(CompareOp::Gt.evaluate(&serde_json::json!(10), &serde_json::json!(5)));
        assert!(!CompareOp::Gt.evaluate(&serde_json::json!(5), &serde_json::json!(10)));

        assert!(CompareOp::Contains.evaluate(
            &serde_json::json!("hello world"),
            &serde_json::json!("world")
        ));

        assert!(
            CompareOp::In.evaluate(&serde_json::json!("a"), &serde_json::json!(["a", "b", "c"]))
        );
    }

    /// Pre-fix, both sides were reduced to f64 via
    /// `as_f64()`. Two adjacent u64 values above 2^53 round to
    /// the same f64 — `9_007_199_254_740_992` and
    /// `9_007_199_254_740_993` both become `9007199254740992.0`,
    /// so `Gt` incorrectly returned false (they compared Equal).
    /// Real-world impact: rules guarding ns timestamps or byte
    /// counts silently failed to fire.
    #[test]
    fn gt_compares_large_u64_without_loss_of_precision() {
        // 2^53 + 1: just past the f64 mantissa boundary.
        let small = serde_json::json!(9_007_199_254_740_992u64);
        let big = serde_json::json!(9_007_199_254_740_993u64);

        // Sanity — pre-fix this would have failed (both round to
        // the same f64).
        assert!(
            CompareOp::Gt.evaluate(&big, &small),
            "Gt must distinguish u64 values one apart at the f64 boundary"
        );
        assert!(
            !CompareOp::Gt.evaluate(&small, &big),
            "Gt must NOT report the smaller value as greater"
        );
        assert!(
            CompareOp::Lt.evaluate(&small, &big),
            "Lt must distinguish at the f64 boundary"
        );
        assert!(
            !CompareOp::Eq.evaluate(&small, &big),
            "Eq must NOT collapse two distinct u64 values; \
             pre-fix these compared equal because both round to the same f64"
        );
    }

    /// Very large u64 values (> i64::MAX) must still
    /// compare correctly even though as_i64() returns None for
    /// them.
    #[test]
    fn gt_compares_u64_values_above_i64_max() {
        let a = serde_json::json!(u64::MAX);
        let b = serde_json::json!(u64::MAX - 1);
        assert!(CompareOp::Gt.evaluate(&a, &b));
        assert!(CompareOp::Lt.evaluate(&b, &a));
    }

    /// Comparing a negative i64 against a u64 above
    /// i64::MAX must always say "negative is less". Pre-fix the
    /// f64 fallback could happen to give a numerically correct
    /// answer here (negatives < positives in f64), but only by
    /// accident — the helper's contract is now explicit.
    #[test]
    fn compares_negative_i64_against_huge_u64_correctly() {
        let neg = serde_json::json!(-1i64);
        let huge = serde_json::json!(u64::MAX);
        assert!(CompareOp::Lt.evaluate(&neg, &huge));
        assert!(CompareOp::Gt.evaluate(&huge, &neg));
    }

    /// Floats still work via the f64 fallback.
    #[test]
    fn float_comparisons_still_work_via_fallback() {
        let a = serde_json::json!(1.5);
        let b = serde_json::json!(2.5);
        assert!(CompareOp::Lt.evaluate(&a, &b));
        assert!(CompareOp::Gt.evaluate(&b, &a));
    }

    /// Integer-vs-float comparison falls back
    /// to f64 (with the documented loss of precision for huge
    /// integers, which is unavoidable when one side is a float).
    #[test]
    fn integer_vs_float_uses_f64_fallback() {
        let i = serde_json::json!(5i64);
        let f = serde_json::json!(4.5);
        assert!(CompareOp::Gt.evaluate(&i, &f));
        assert!(CompareOp::Lt.evaluate(&f, &i));
    }

    #[test]
    fn test_condition() {
        let mut ctx = RuleContext::new();
        ctx.set("cpu_usage", serde_json::json!(85));
        ctx.set("status", serde_json::json!("running"));

        let cond1 = Condition::gt("cpu_usage", serde_json::json!(80));
        assert!(cond1.evaluate(&ctx));

        let cond2 = Condition::eq("status", serde_json::json!("running"));
        assert!(cond2.evaluate(&ctx));

        let cond3 = Condition::lt("cpu_usage", serde_json::json!(50));
        assert!(!cond3.evaluate(&ctx));
    }

    #[test]
    fn test_condition_expr() {
        let mut ctx = RuleContext::new();
        ctx.set("cpu", serde_json::json!(85));
        ctx.set("memory", serde_json::json!(70));

        // AND: both conditions must be true
        let expr_and = ConditionExpr::and(vec![
            ConditionExpr::single(Condition::gt("cpu", serde_json::json!(80))),
            ConditionExpr::single(Condition::gt("memory", serde_json::json!(60))),
        ]);
        assert!(expr_and.evaluate(&ctx));

        // OR: at least one must be true
        let expr_or = ConditionExpr::or(vec![
            ConditionExpr::single(Condition::gt("cpu", serde_json::json!(90))),
            ConditionExpr::single(Condition::gt("memory", serde_json::json!(60))),
        ]);
        assert!(expr_or.evaluate(&ctx));

        // NOT: negate condition
        let expr_not = ConditionExpr::negate(ConditionExpr::single(Condition::lt(
            "cpu",
            serde_json::json!(50),
        )));
        assert!(expr_not.evaluate(&ctx));
    }

    #[test]
    fn test_nested_field_access() {
        let mut ctx = RuleContext::new();
        ctx.set(
            "metrics",
            serde_json::json!({
                "cpu": {"usage": 85, "cores": 4},
                "memory": {"used": 8192, "total": 16384}
            }),
        );

        let cond = Condition::gt("metrics.cpu.usage", serde_json::json!(80));
        assert!(cond.evaluate(&ctx));

        let cond2 = Condition::eq("metrics.cpu.cores", serde_json::json!(4));
        assert!(cond2.evaluate(&ctx));
    }

    #[test]
    fn test_rule() {
        let rule = Rule::new("high-cpu", "High CPU Alert")
            .with_description("Alert when CPU usage is high")
            .with_priority(Priority::High)
            .with_condition(ConditionExpr::single(Condition::gt(
                "cpu",
                serde_json::json!(80),
            )))
            .with_action(Action::alert(
                AlertSeverity::High,
                "High CPU",
                "CPU usage exceeded 80%",
            ))
            .with_tag("monitoring")
            .with_cooldown(60000);

        let mut ctx = RuleContext::new();
        ctx.set("cpu", serde_json::json!(85));

        assert!(rule.matches(&ctx));

        ctx.set("cpu", serde_json::json!(50));
        assert!(!rule.matches(&ctx));
    }

    #[test]
    fn test_rule_engine() {
        let mut engine = RuleEngine::new();

        // Add rules with different priorities
        engine
            .add_rule(
                Rule::new("rule-low", "Low Priority")
                    .with_priority(Priority::Low)
                    .with_condition(ConditionExpr::Always)
                    .with_action(Action::log(LogLevel::Info, "Low priority")),
            )
            .unwrap();

        engine
            .add_rule(
                Rule::new("rule-high", "High Priority")
                    .with_priority(Priority::High)
                    .with_condition(ConditionExpr::Always)
                    .with_action(Action::log(LogLevel::Info, "High priority")),
            )
            .unwrap();

        engine
            .add_rule(
                Rule::new("rule-normal", "Normal Priority")
                    .with_priority(Priority::Normal)
                    .with_condition(ConditionExpr::Always)
                    .with_action(Action::log(LogLevel::Info, "Normal priority")),
            )
            .unwrap();

        // Rules should be sorted by priority
        let results = engine.evaluate(&RuleContext::new());
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].rule_id, "rule-high");
        assert_eq!(results[1].rule_id, "rule-normal");
        assert_eq!(results[2].rule_id, "rule-low");
    }

    #[test]
    fn test_stop_on_match() {
        let mut engine = RuleEngine::new();

        engine
            .add_rule(
                Rule::new("stopper", "Stopper")
                    .with_priority(Priority::High)
                    .with_condition(ConditionExpr::Always)
                    .with_action(Action::Noop)
                    .stop_on_match(),
            )
            .unwrap();

        engine
            .add_rule(
                Rule::new("after", "After")
                    .with_priority(Priority::Normal)
                    .with_condition(ConditionExpr::Always)
                    .with_action(Action::Noop),
            )
            .unwrap();

        let results = engine.evaluate(&RuleContext::new());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].rule_id, "stopper");
    }

    #[test]
    fn test_disabled_rule() {
        let mut engine = RuleEngine::new();

        engine
            .add_rule(
                Rule::new("disabled", "Disabled Rule")
                    .with_condition(ConditionExpr::Always)
                    .disabled(),
            )
            .unwrap();

        let results = engine.evaluate_matching(&RuleContext::new());
        assert!(results.is_empty());
    }

    #[test]
    fn test_rules_by_tag() {
        let mut engine = RuleEngine::new();

        engine
            .add_rule(
                Rule::new("r1", "Rule 1")
                    .with_tag("monitoring")
                    .with_tag("cpu"),
            )
            .unwrap();

        engine
            .add_rule(Rule::new("r2", "Rule 2").with_tag("monitoring"))
            .unwrap();

        engine
            .add_rule(Rule::new("r3", "Rule 3").with_tag("network"))
            .unwrap();

        let monitoring_rules = engine.rules_by_tag("monitoring");
        assert_eq!(monitoring_rules.len(), 2);

        let cpu_rules = engine.rules_by_tag("cpu");
        assert_eq!(cpu_rules.len(), 1);
    }

    #[test]
    fn test_rule_set() {
        let rule_set = RuleSet::new("default", "Default Rules")
            .with_description("Default monitoring rules")
            .with_tag("production")
            .add_rule(
                Rule::new("r1", "Rule 1")
                    .with_condition(ConditionExpr::Always)
                    .with_action(Action::Noop),
            )
            .add_rule(
                Rule::new("r2", "Rule 2")
                    .with_condition(ConditionExpr::Always)
                    .with_action(Action::Noop),
            );

        let mut engine = RuleEngine::new();
        rule_set.load_into(&mut engine).unwrap();

        assert_eq!(engine.len(), 2);
    }

    #[test]
    fn test_stats() {
        let mut engine = RuleEngine::new();

        engine
            .add_rule(
                Rule::new("r1", "Rule 1")
                    .with_priority(Priority::High)
                    .with_tag("tag1"),
            )
            .unwrap();

        engine
            .add_rule(
                Rule::new("r2", "Rule 2")
                    .with_priority(Priority::Normal)
                    .with_tag("tag1"),
            )
            .unwrap();

        engine
            .add_rule(
                Rule::new("r3", "Rule 3")
                    .with_priority(Priority::Normal)
                    .disabled(),
            )
            .unwrap();

        let stats = engine.stats();
        assert_eq!(stats.total_rules, 3);
        assert_eq!(stats.enabled_rules, 2);
        assert_eq!(stats.by_tag.get("tag1"), Some(&2));
    }

    #[test]
    fn test_action_chain() {
        let action = Action::chain(vec![
            Action::log(LogLevel::Info, "First"),
            Action::log(LogLevel::Info, "Second"),
            Action::emit("test", serde_json::json!({})),
        ]);

        assert_eq!(action.action_count(), 3);
    }

    // ---------- Cooldown and rate-limit gating ----------
    //
    // Existing tests cover the happy path: rule matches, action
    // fires. None exercise the gating that prevents action
    // execution when a rule is on cooldown or has spent its
    // rate-limit budget. These branches are load-bearing for any
    // production rule that throttles itself (alert fatigue, retry
    // storms) — a regression would silently bypass the throttle.

    #[test]
    fn cooldown_blocks_action_on_second_match_within_window() {
        let mut engine = RuleEngine::new();
        engine
            .add_rule(
                Rule::new("cd", "Cooldown rule")
                    .with_condition(ConditionExpr::Always)
                    .with_action(Action::Noop)
                    .with_cooldown(60_000), // 60s — won't elapse in-test
            )
            .unwrap();

        // First evaluation executes the action.
        let r1 = engine.evaluate(&RuleContext::new());
        assert_eq!(r1.len(), 1);
        assert!(r1[0].matched);
        assert!(r1[0].action.is_some());

        // Second evaluation matches but is on cooldown — action
        // must be None. Pre-fix any regression in
        // `check_execution_allowed` would let it fire again.
        let r2 = engine.evaluate(&RuleContext::new());
        assert_eq!(r2.len(), 1);
        assert!(r2[0].matched, "rule must still match while gated");
        assert!(
            r2[0].action.is_none(),
            "cooldown must suppress the action, got {:?}",
            r2[0].action,
        );
    }

    #[test]
    fn rate_limit_blocks_action_after_max_executions() {
        let mut engine = RuleEngine::new();
        engine
            .add_rule(
                Rule::new("rl", "Rate-limited rule")
                    .with_condition(ConditionExpr::Always)
                    .with_action(Action::Noop)
                    .with_rate_limit(2, 300), // 2 per 5min window
            )
            .unwrap();

        // First two evaluations consume the budget.
        assert!(engine.evaluate(&RuleContext::new())[0].action.is_some());
        assert!(engine.evaluate(&RuleContext::new())[0].action.is_some());

        // Third matches but rate limit fires.
        let r3 = engine.evaluate(&RuleContext::new());
        assert!(r3[0].matched);
        assert!(
            r3[0].action.is_none(),
            "rate limit must suppress the action, got {:?}",
            r3[0].action,
        );
    }

    /// Regression pin for the rate-limit-state hot-reload bug
    /// noted in `record_execution` (L1271-L1282): pre-fix the
    /// `execution_count` was incremented for every rule on every
    /// match, even rules without a rate_limit. Toggling a rule
    /// from rate-limited → unlimited → rate-limited (same window)
    /// would carry a stale count and silently block forever.
    ///
    /// We pin the fix by verifying that an unlimited rule's
    /// repeated matches never increment any rate-limit count
    /// (observable via continuing to fire actions).
    #[test]
    fn unlimited_rule_keeps_firing_across_many_evaluations() {
        let mut engine = RuleEngine::new();
        engine
            .add_rule(
                Rule::new("unl", "Unlimited rule")
                    .with_condition(ConditionExpr::Always)
                    .with_action(Action::Noop),
            )
            .unwrap();

        for i in 0..50 {
            let r = engine.evaluate(&RuleContext::new());
            assert!(
                r[0].action.is_some(),
                "unlimited rule must keep firing; blocked at iteration {i}",
            );
        }
    }

    // ---------- evaluate_first vs evaluate ----------

    #[test]
    fn evaluate_first_returns_first_matching_rule_only() {
        let mut engine = RuleEngine::new();
        engine
            .add_rule(
                Rule::new("a", "A")
                    .with_priority(Priority::High)
                    .with_condition(ConditionExpr::Always)
                    .with_action(Action::Noop),
            )
            .unwrap();
        engine
            .add_rule(
                Rule::new("b", "B")
                    .with_priority(Priority::Normal)
                    .with_condition(ConditionExpr::Always)
                    .with_action(Action::Noop),
            )
            .unwrap();

        // Both match but evaluate_first returns one result —
        // higher-priority rule wins under rules-sorted-by-priority
        // semantics.
        let r = engine.evaluate_first(&RuleContext::new());
        assert!(r.is_some());
        let r = r.unwrap();
        assert_eq!(r.rule_id, "a", "highest-priority rule must win");
        assert!(r.action.is_some());
    }

    #[test]
    fn evaluate_first_action_is_none_when_rule_is_rate_limited() {
        let mut engine = RuleEngine::new();
        engine
            .add_rule(
                Rule::new("rl", "Rate-limited")
                    .with_condition(ConditionExpr::Always)
                    .with_action(Action::Noop)
                    .with_rate_limit(1, 300),
            )
            .unwrap();

        assert!(engine.evaluate_first(&RuleContext::new()).unwrap().action.is_some());
        let r = engine.evaluate_first(&RuleContext::new()).unwrap();
        assert!(r.matched);
        assert!(
            r.action.is_none(),
            "evaluate_first must respect rate-limit gating just like evaluate",
        );
    }

    // ---------- Action constructor coverage ----------

    #[test]
    fn action_factory_methods_round_trip() {
        match Action::set_context("k", serde_json::json!(1)) {
            Action::SetContext { key, value } => {
                assert_eq!(key, "k");
                assert_eq!(value, serde_json::json!(1));
            }
            other => panic!("expected SetContext, got {:?}", other),
        }

        match Action::reject("policy denied", 403) {
            Action::Reject { reason, code } => {
                assert_eq!(reason, "policy denied");
                assert_eq!(code, 403);
            }
            other => panic!("expected Reject, got {:?}", other),
        }

        match Action::redirect_to_tags(vec!["gpu".into(), "fast".into()]) {
            Action::Redirect {
                target_node,
                target_tags,
            } => {
                assert!(target_node.is_none());
                assert_eq!(target_tags, vec!["gpu".to_string(), "fast".to_string()]);
            }
            other => panic!("expected Redirect, got {:?}", other),
        }
    }

    // ---------- RuleContext::from_value ----------

    #[test]
    fn rule_context_from_value_loads_object_keys() {
        let ctx = RuleContext::from_value(serde_json::json!({
            "user": "alice",
            "count": 42,
        }));
        assert_eq!(ctx.get_field("user"), serde_json::json!("alice"));
        assert_eq!(ctx.get_field("count"), serde_json::json!(42));

        // Non-object inputs produce an empty context (no panic).
        let ctx = RuleContext::from_value(serde_json::json!("not-an-object"));
        assert_eq!(ctx.get_field("anything"), serde_json::json!(null));
    }
}
