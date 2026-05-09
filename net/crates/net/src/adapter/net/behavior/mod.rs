//! Behavior Plane for Net (Phase 4)
//!
//! This module provides the semantic layer on top of theNet's transport:
//! - Capability announcements and indexing (CAP-ANN)
//! - Capability change diffs (CAP-DIFF)
//! - Node metadata surface (NODE-META)
//! - Node APIs and schemas (API-SCHEMA)
//! - Device autonomy rules (DEVICE-RULES)
//! - Context fabric (CTXT-FABRIC)
//! - Distributed load balancing (LOAD-BALANCE)
//! - Proximity graph integration (PINGWAVE++)
//! - Safety envelope enforcement

pub mod api;
pub mod broadcast;
pub mod capability;
pub mod context;
pub mod diff;
pub mod loadbalance;
pub mod metadata;
pub mod predicate;
pub mod proximity;
pub mod rules;
pub mod safety;
pub mod tag;

pub use broadcast::SUBPROTOCOL_CAPABILITY_ANN;

pub use capability::{
    AcceleratorInfo, AcceleratorType, CapabilityAnnouncement, CapabilityFilter, CapabilityIndex,
    CapabilityIndexStats, CapabilityRequirement, CapabilitySet, GpuInfo, GpuVendor,
    HardwareCapabilities, IndexedNode, Modality, ModelCapability, ResourceLimits, Signature64,
    SoftwareCapabilities, ToolCapability,
};

// Capability System Plan Phase A foundations — the new typed-tag
// taxonomy. Re-exported from the behavior plane root so downstream
// callers reach `Tag` / `TagKey` / `TaxonomyAxis` via the same path
// they already use for `CapabilitySet`.
pub use tag::{
    AxisSeparator, CapabilityTagError, RESERVED_PREFIXES, Tag, TagKey, TaxonomyAxis,
};

pub use predicate::{EvalContext, Predicate};

pub use diff::{CapabilityDiff, DiffEngine, DiffError, DiffOp};

pub use metadata::{
    LocationInfo, MetadataError, MetadataQuery, MetadataStore, MetadataStoreStats, NatType,
    NetworkTier, NodeId, NodeMetadata, NodeStatus, Region, TopologyHints,
};

pub use api::{
    ApiAnnouncement, ApiEndpoint, ApiMethod, ApiParameter, ApiQuery, ApiRegistry, ApiRegistryStats,
    ApiSchema, ApiValidationError, ApiVersion, IndexedApiNode, RegistryError, SchemaType,
    StringFormat, ValidationError,
};

pub use rules::{
    Action, AlertSeverity, CompareOp, Condition, ConditionExpr, LogLevel, LogicOp, Priority,
    RateLimit, Rule, RuleContext, RuleEngine, RuleEngineStats, RuleError, RuleResult, RuleSet,
    ScaleDirection,
};

pub use context::{
    AttributeValue, Baggage, BaggageItem, Context, ContextError, ContextScope, ContextStore,
    ContextStoreStats, PropagationContext, Sampler, SamplingStrategy, Span, SpanEvent, SpanId,
    SpanKind, SpanLink, SpanStatus, TraceFlags, TraceId,
};

pub use loadbalance::{
    Endpoint, HealthStatus, LoadBalancer, LoadBalancerConfig, LoadBalancerError, LoadBalancerStats,
    LoadMetrics, RequestContext as LbRequestContext, Selection, SelectionReason, Strategy,
};

pub use proximity::{
    CleanupStats, EnhancedPingwave, PrimaryCapabilities, ProximityConfig, ProximityEdge,
    ProximityGraph, ProximityNode, ProximityStats, ProximityStatsSnapshot,
};

pub use safety::{
    AuditConfig, AuditEntry, AuditEventType, AuditOutcome, AuditSink, ContentCheck, ContentPolicy,
    EnforcementMode, KillSwitchConfig, PolicyAction, RateEnvelope, RateLimitType, ResourceClaim,
    ResourceEnvelope, ResourceGuard, ResourceType, SafetyEnforcer, SafetyEnvelope, SafetyRequest,
    SafetyViolation, UsageStats,
};
