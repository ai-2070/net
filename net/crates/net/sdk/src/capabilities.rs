//! Capability declarations — what a node can do.
//!
//! `CapabilitySet` describes hardware (CPU / GPU / memory), software
//! (OS / runtimes / frameworks), loaded models, available tools, and
//! free-form tags. `CapabilityFilter` lets peers query for nodes that
//! match a requirement; `CapabilityAnnouncement` is the signed
//! on-wire form.
//!
//! # Example
//!
//! ```
//! use net_sdk::capabilities::{
//!     CapabilityFilter, CapabilitySet, GpuInfo, GpuVendor, HardwareCapabilities,
//! };
//!
//! // Declare: "this node has one RTX 4090 + 64 GB RAM, tagged `prod`".
//! let hw = HardwareCapabilities::new()
//!     .with_cpu(16, 32)
//!     .with_memory(65_536)
//!     .with_gpu(GpuInfo::new(GpuVendor::Nvidia, "RTX 4090", 24_576));
//! let caps = CapabilitySet::new().with_hardware(hw).add_tag("prod");
//!
//! // Match-ability: does this node satisfy "needs GPU ≥ 16 GB VRAM"?
//! let filter = CapabilityFilter::new().require_gpu().with_min_vram(16_384);
//! assert!(filter.matches(&caps));
//! ```
//!
//! # Cross-node (direct-peer, one-hop)
//!
//! With `--features net`, `Mesh` has
//! [`announce_capabilities`](crate::mesh::Mesh::announce_capabilities)
//! and [`find_nodes`](crate::mesh::Mesh::find_nodes). Announce-side
//! self-indexes, so a single-node test is round-trippable:
//!
//! ```
//! # #[cfg(feature = "net")]
//! # async fn doc() -> net_sdk::error::Result<()> {
//! use net_sdk::capabilities::{CapabilityFilter, CapabilitySet};
//! use net_sdk::mesh::MeshBuilder;
//!
//! let node = MeshBuilder::new("127.0.0.1:0", &[0x42u8; 32])?
//!     .build()
//!     .await?;
//!
//! // Announce; also self-indexes.
//! node.announce_capabilities(CapabilitySet::new().add_tag("gpu"))
//!     .await?;
//!
//! // Self-match hits.
//! let hits = node.find_nodes(&CapabilityFilter::new().require_tag("gpu"));
//! assert!(hits.contains(&node.node_id()));
//!
//! node.shutdown().await?;
//! # Ok(())
//! # }
//! ```
//!
//! Multi-hop propagation is deferred — peers more than one hop away
//! will not see the announcement. TTL + GC eviction match the core
//! defaults (`capability_gc_interval` on [`net::adapter::net::MeshNodeConfig`]).
//!
//! # Phase A — typed taxonomy + lazy view projections
//!
//! `CapabilitySet`'s wire format is two opaque fields: `tags:
//! HashSet<Tag>` and `metadata: BTreeMap<String, String>`. The
//! per-axis typed shapes (`HardwareCapabilities`,
//! `SoftwareCapabilities`, …) are *projections* — derived on demand
//! via [`CapabilitySet::views`] and cached lazily per access:
//!
//! ```
//! use net_sdk::capabilities::{CapabilitySet, HardwareCapabilities};
//!
//! let caps = CapabilitySet::new()
//!     .with_hardware(HardwareCapabilities::new().with_memory(65_536))
//!     .with_metadata("intent", "ml-training");
//!
//! let v = caps.views();
//! assert_eq!(v.hardware().memory_mb, 65_536);  // first call: decodes hardware tags
//! assert_eq!(v.hardware().memory_mb, 65_536);  // cached; pointer load only
//! ```
//!
//! Mutations to `caps` invalidate the handle (compiler-enforced via
//! the `&caps` borrow held by `views()`).
//!
//! # `CapabilitySet::diff` — change detection
//!
//! Two-line diff between successive announcements; powers
//! event-driven placement, dashboards, and delta propagation.
//!
//! ```
//! use net_sdk::capabilities::{CapabilitySet, MetadataChange};
//!
//! let prev = CapabilitySet::new().with_metadata("intent", "embedding-cache");
//! let curr = CapabilitySet::new().with_metadata("intent", "ml-training");
//!
//! let diff = curr.diff(&prev);
//! assert!(diff.added_tags.is_empty());
//! assert_eq!(diff.changed_metadata.len(), 1);
//! assert!(matches!(
//!     &diff.changed_metadata[0],
//!     MetadataChange::Updated { key, new_value, .. }
//!         if key == "intent" && new_value == "ml-training",
//! ));
//! ```
//!
//! # Chain composition helpers
//!
//! Sugar over the `causal:` / `fork-of:` / `heat:` reserved-prefix
//! tags from the substrate's discovery primitive:
//!
//! ```
//! use net_sdk::capabilities::CapabilitySet;
//!
//! let caps = CapabilitySet::new()
//!     .require_chain("origin-hash-abc")
//!     .require_chain_range("range-chain", 100, 500)
//!     .from_fork("parent-hash")
//!     .heat_level("origin-hash-abc", 0.85);
//!
//! assert_eq!(caps.tags.len(), 4);
//! ```
//!
//! See [`docs/CAPABILITY_ENHANCEMENTS_USAGE.md`](https://github.com/ai-2070/net/blob/main/net/crates/net/docs/CAPABILITY_ENHANCEMENTS_USAGE.md)
//! for the full set of patterns.

// =============================================================================
// Substrate-layer types — Phase A foundation.
// =============================================================================

pub use net::adapter::net::behavior::capability::{
    AcceleratorInfo, AcceleratorType, CapabilityAnnouncement, CapabilityFilter, CapabilityIndex,
    CapabilityIndexStats, CapabilityRequirement, CapabilitySet, GpuInfo, GpuVendor,
    HardwareCapabilities, IndexedNode, Modality, ModelCapability, ResourceLimits, ScopeFilter,
    Signature64, SoftwareCapabilities, ToolCapability,
};

// =============================================================================
// Typed taxonomy — Phase A.1 of `CAPABILITY_SYSTEM_PLAN.md`.
//
// `Tag` is the parsed representation of a capability tag; `TagKey`
// is the `(axis, key)` half used by predicate variants that match
// on the key without the value. The four-axis ontology is fixed:
// `Hardware` / `Software` / `Devices` / `Dataforts`.
// =============================================================================

pub use net::adapter::net::behavior::tag::{
    AxisSeparator, CapabilityTagError, RESERVED_PREFIXES, Tag, TagKey, TaxonomyAxis,
};

pub use net::adapter::net::behavior::tag_codec::{
    capability_set_from_tag_set, capability_set_to_tag_set, hardware_from_tags, hardware_to_tags,
    is_hardware_owned_tag, is_models_owned_tag, is_resource_limits_owned_tag,
    is_software_owned_tag, is_tools_owned_tag, models_from_tags, models_to_tags,
    resource_limits_from_tags, resource_limits_to_tags, software_from_tags, software_to_tags,
    tools_from_tags, tools_to_tags,
};

// =============================================================================
// Lazy view projections — Phase 1 of `CAPABILITY_ENHANCEMENTS_PLAN.md`.
//
// `caps.views()` returns a borrowing handle whose per-axis fields
// decode on first access and cache for the handle's lifetime.
// Reading only one axis no longer pays for the others.
// =============================================================================

pub use net::adapter::net::behavior::capability::CapabilityViews;

// =============================================================================
// `CapabilitySet::diff` + `MetadataChange` — Phase 1 of
// `CAPABILITY_ENHANCEMENTS_PLAN.md`. Cheap before/after change
// detection over tag sets + metadata maps.
// =============================================================================

pub use net::adapter::net::behavior::capability::{CapabilitySetDiff, MetadataChange};

// =============================================================================
// Cardinality — Phase 4 follow-on of `CAPABILITY_ENHANCEMENTS_PLAN.md`.
//
// `CapabilityIndex` exposes O(1) cardinality lookups via the
// `CardinalityProvider` trait. `CardinalityCache<'a>` snapshots
// the lookups with a TTL to drop DashMap shard contention on
// hot loops; passes through the same trait so the planner uses
// either as a drop-in.
// =============================================================================

pub use net::adapter::net::behavior::capability::{CardinalityCache, CardinalityProvider};

// =============================================================================
// Placement filter — Phase F slice 1 of `CAPABILITY_SYSTEM_PLAN.md`.
//
// `PlacementFilter::placement_score(target, artifact) -> Option<f32>`
// is the substrate's "score a candidate for placing an artifact"
// primitive. `LegacyPlacement` preserves today's
// `find_migration_targets` behavior (any node matching the
// `CapabilityFilter` is eligible at score 1.0) so the scheduler
// migration window swaps cleanly. `StandardPlacement` (the multi-
// axis reference impl) lands in slice 2 alongside `IntentRegistry`
// + the tie-breaking comparator.
// =============================================================================

pub use net::adapter::net::behavior::placement::{
    AntiAffinityConfig, Artifact, ColocationPolicy, IntentMatchPolicy, IntentRegistry,
    LegacyPlacement, NodeId as PlacementNodeId, PlacementFilter, PlacementMetadataKeys,
    ResourceAxis, RttLookup, ScopeLabel, StandardPlacement, TieBreakContext,
    compose_axis_scores, tie_break_compare,
};

// =============================================================================
// Required capability + macros — Phase A.3 of
// `CAPABILITY_SYSTEM_PLAN.md`. Used by `IntentRegistry` (Phase F)
// to declare per-intent placement requirements.
// =============================================================================

pub use net::adapter::net::behavior::required_capability::{
    RequireParseError, RequiredCapability,
};

// Re-export the require! / require_axis! / require_axis_value! macros
// so callers can build `RequiredCapability` values without naming the
// underlying `TagKey` constructor.
pub use net::{require, require_axis, require_axis_value};

// =============================================================================
// Predicate AST + planner + wire format — Phases 4 / 5 / 6 of
// `CAPABILITY_ENHANCEMENTS_PLAN.md`.
// =============================================================================

/// Predicate AST + evaluation, debug session, wire format, and
/// nRPC envelope helpers.
///
/// Phases 4 (planner), 5 (wire format + nRPC envelope), and 6
/// (debug session) of `CAPABILITY_ENHANCEMENTS_PLAN.md`.
///
/// ```
/// use net_sdk::capabilities::predicate::{
///     EvalContext, Predicate, RPC_WHERE_HEADER, predicate_to_rpc_header,
/// };
/// use net_sdk::capabilities::{Tag, TagKey, TaxonomyAxis};
/// use std::collections::BTreeMap;
///
/// // Build a predicate: GPU + memory ≥ 32 GB + intent = ml-training.
/// let pred = Predicate::And(vec![
///     Predicate::Exists {
///         key: TagKey::new(TaxonomyAxis::Hardware, "gpu"),
///     },
///     Predicate::NumericAtLeast {
///         key: TagKey::new(TaxonomyAxis::Hardware, "memory_mb"),
///         threshold: 32_768.0,
///     },
///     Predicate::MetadataEquals {
///         key: "intent".into(),
///         value: "ml-training".into(),
///     },
/// ]);
///
/// // Encode for an nRPC request header.
/// let header = predicate_to_rpc_header(&pred).expect("encode");
/// assert_eq!(header.0, RPC_WHERE_HEADER);
///
/// // Evaluate against a candidate.
/// let tags: Vec<Tag> = vec![Tag::AxisPresent {
///     axis: TaxonomyAxis::Hardware,
///     key: "gpu".into(),
/// }];
/// let mut metadata = BTreeMap::new();
/// metadata.insert("intent".into(), "ml-training".into());
/// let _ctx = EvalContext::new(&tags, &metadata);
/// // pred.evaluate(&_ctx) → true (gpu present, intent matches; memory clause fails since not in tags)
/// ```
pub mod predicate {
    pub use net::adapter::net::behavior::predicate::{
        AsRpcHeader, ClauseStats, ClauseTrace, EvalContext, MAX_PREDICATE_RPC_HEADER_VALUE_LEN,
        Predicate, PredicateDebugReport, PredicateNodeWire, PredicateRpcDecodeError,
        PredicateRpcEncodeError, PredicateWire, PredicateWireError, RPC_WHERE_HEADER,
        RpcPredicateContext, filter_by_predicate, predicate_from_rpc_headers,
        predicate_to_rpc_header,
    };

    /// Re-export of the substrate's `pred!` macro.
    ///
    /// Builds a [`Predicate`] AST from a parse-time DSL. Each
    /// invocation produces one clause; compose with `and [..]` /
    /// `or [..]` / `not ..` for boolean structure.
    ///
    /// ```
    /// use net_sdk::capabilities::pred;
    /// let p = pred!(and [
    ///     pred!(exists "hardware.gpu"),
    ///     pred!(num_at_least "hardware.memory_mb", 65536.0),
    ///     pred!(metadata_equals "intent", "ml-training"),
    /// ]);
    /// # let _ = p;
    /// ```
    pub use net::pred;
}

// Promote the `pred!` macro to the top-level `capabilities` module
// for ergonomics — most call sites read like `pred!(...)` rather
// than `predicate::pred!(...)`.
pub use net::pred;

// =============================================================================
// Schema + validation — Phase 2 of `CAPABILITY_ENHANCEMENTS_PLAN.md`.
//
// Per-binding canonical-doc-mirroring schema with a lightweight
// validator. Drives auto-completion, runtime validation, and CI
// drift detection (binding regenerators check against
// `CAPABILITIES_SCHEMA.md`).
// =============================================================================

/// Axis schemas + capability-set validation.
///
/// Phase 2 of `CAPABILITY_ENHANCEMENTS_PLAN.md`. `AXIS_SCHEMA`
/// mirrors the canonical
/// [`CAPABILITIES_SCHEMA.md`](https://github.com/ai-2070/net/blob/main/net/crates/net/docs/CAPABILITIES_SCHEMA.md);
/// `validate_capabilities(&caps)` returns a structured
/// [`ValidationReport`].
///
/// ```
/// use net_sdk::capabilities::CapabilitySet;
/// use net_sdk::capabilities::schema::validate_capabilities;
///
/// let caps = CapabilitySet::new().add_tag("nat:full-cone");
/// let report = validate_capabilities(&caps);
/// assert!(report.is_valid()); // legacy tag is a warning, not error
/// assert!(!report.warnings.is_empty());
/// ```
pub mod schema {
    pub use net::adapter::net::behavior::schema::{
        AXIS_SCHEMA, AxisEntry, AxisSchema, KeyEntry, KeyShape, KeyShapeKind,
        METADATA_SOFT_CAP_BYTES, SchemaError, ValidationReport, ValidationWarning, ValueType,
        validate_capabilities, validate_capabilities_against,
    };
}

// =============================================================================
// Diff engine — `behavior::diff::DiffEngine` is exposed alongside
// the simpler `CapabilitySet::diff` so callers picking the
// structural-ops shape (for propagation paths) reach for it via
// the same `capabilities::` namespace.
// =============================================================================

pub use net::adapter::net::behavior::diff::{CapabilityDiff, DiffEngine, DiffError, DiffOp};
