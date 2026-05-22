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
pub mod bloom;
pub mod bounded_ring;
pub mod broadcast;
pub mod capability;
pub mod context;
pub mod dataforts_capabilities;
#[cfg(feature = "meshos")]
pub mod deck;
pub mod diff;
pub mod fold;
pub mod group;
pub mod loadbalance;
#[cfg(feature = "meshdb")]
pub mod meshdb;
#[cfg(feature = "meshos")]
pub mod meshos;
pub mod metadata;
pub mod placement;
pub mod placement_registry;
pub mod predicate;
pub mod proximity;
pub mod query;
pub mod required_capability;
pub mod rules;
pub mod safety;
pub mod schema;
pub mod subnet;
pub mod tag;
pub mod tag_codec;

pub use bloom::BloomFilter;

pub use query::{
    Aggregator, CapabilityQuery, Count, Distance, EdgeKind, MaxNumericMetadata, UniqueAxisValues,
};

pub use broadcast::SUBPROTOCOL_CAPABILITY_ANN;

pub use capability::{
    AcceleratorInfo, AcceleratorType, CapabilityAnnouncement, CapabilityFilter, CapabilityIndex,
    CapabilityIndexStats, CapabilityRequirement, CapabilitySet, CapabilitySetDiff, CapabilityViews,
    CardinalityCache, CardinalityProvider, GpuInfo, GpuVendor, HardwareCapabilities, IndexedNode,
    MetadataChange, Modality, ModelCapability, ResourceLimits, Signature64, SoftwareCapabilities,
    ToolCapability,
};

pub use dataforts_capabilities::{
    is_blob_storage_unhealthy, BlobCapability, GravityCapability, GreedyCapability, TopologyScope,
    BLOB_STORAGE_UNHEALTHY_TAG,
};

// Capability System Plan Phase A foundations — the new typed-tag
// taxonomy. Re-exported from the behavior plane root so downstream
// callers reach `Tag` / `TagKey` / `TaxonomyAxis` via the same path
// they already use for `CapabilitySet`.
pub use tag::{AxisSeparator, CapabilityTagError, Tag, TagKey, TaxonomyAxis, RESERVED_PREFIXES};

pub use predicate::{
    filter_by_predicate, predicate_from_rpc_headers, predicate_to_rpc_header, AsRpcHeader,
    ClauseStats, ClauseTrace, EvalContext, Predicate, PredicateDebugReport, PredicateNodeWire,
    PredicateRpcDecodeError, PredicateRpcEncodeError, PredicateWire, PredicateWireError,
    RpcPredicateContext, MAX_PREDICATE_RPC_HEADER_VALUE_LEN, RPC_WHERE_HEADER,
};

pub use placement::{
    compose_axis_scores, tie_break_compare, AntiAffinityConfig, Artifact, ColocationPolicy,
    IntentMatchPolicy, IntentRegistry, LeadershipStatsLookup, LegacyPlacement,
    NodeId as PlacementNodeId, PlacementFilter, PlacementMetadataKeys, ResourceAxis, RttLookup,
    ScopeLabel, StandardPlacement, TieBreakContext,
};

pub use placement_registry::{global_placement_filter_registry, PlacementFilterRegistry};

pub use required_capability::{RequireParseError, RequiredCapability};

pub use tag_codec::{
    capability_set_from_tag_set, capability_set_to_tag_set, hardware_from_tags, hardware_to_tags,
    is_hardware_owned_tag, is_models_owned_tag, is_resource_limits_owned_tag,
    is_software_owned_tag, is_tools_owned_tag, models_from_tags, models_to_tags,
    resource_limits_from_tags, resource_limits_to_tags, software_from_tags, software_to_tags,
    tools_from_tags, tools_to_tags,
};

pub use diff::{CapabilityDiff, DiffEngine, DiffError, DiffOp};

pub use schema::{
    validate_capabilities, validate_capabilities_against, AxisEntry, AxisSchema, KeyEntry,
    KeyShape, KeyShapeKind, SchemaError, ValidationReport, ValidationWarning, ValueType,
    AXIS_SCHEMA, METADATA_SOFT_CAP_BYTES,
};

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
