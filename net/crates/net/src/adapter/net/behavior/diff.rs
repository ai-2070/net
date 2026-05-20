//! Capability Change Diffs (CAP-DIFF) for Phase 4B.
//!
//! This module provides:
//! - `DiffOp` - Individual diff operations (add/remove tags, models, tools, etc.)
//! - `CapabilityDiff` - Versioned diff message with operations
//! - `DiffEngine` - Generate and apply diffs between capability sets
//!
//! # Performance Targets
//! - Diff generation: < 1µs for typical changes
//! - Diff application: < 500ns
//! - Diff size (1 op): < 50 bytes
//! - Bandwidth savings: > 90% vs full CAP-ANN

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use super::capability::{
    CapabilitySet, HardwareCapabilities, ModelCapability, ResourceLimits, SoftwareCapabilities,
    ToolCapability,
};

// ============================================================================
// Diff Operations
// ============================================================================

/// Individual diff operation
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DiffOp {
    // Tag operations
    /// Add a tag
    AddTag(String),
    /// Remove a tag
    RemoveTag(String),

    // Model operations
    /// Add a model capability
    AddModel(ModelCapability),
    /// Remove a model by ID
    RemoveModel(String),
    /// Update model fields (partial update)
    UpdateModel {
        /// Model ID to update
        model_id: String,
        /// New tokens per second (if changed)
        tokens_per_sec: Option<u32>,
        /// New loaded status (if changed)
        loaded: Option<bool>,
    },

    // Tool operations
    /// Add a tool capability
    AddTool(ToolCapability),
    /// Remove a tool by ID
    RemoveTool(String),

    // Hardware operations
    /// Update hardware capabilities (full replacement)
    UpdateHardware(HardwareCapabilities),
    /// Update memory only
    UpdateMemory(u32),
    /// Update network bandwidth only
    UpdateNetwork(u32),

    // Software operations
    /// Update software capabilities (full replacement)
    UpdateSoftware(SoftwareCapabilities),
    /// Add a runtime
    AddRuntime {
        /// Runtime name
        name: String,
        /// Runtime version
        version: String,
    },
    /// Remove a runtime
    RemoveRuntime(String),
    /// Add a framework
    AddFramework {
        /// Framework name
        name: String,
        /// Framework version
        version: String,
    },
    /// Remove a framework
    RemoveFramework(String),

    // Resource limits
    /// Update resource limits (full replacement)
    UpdateLimits(ResourceLimits),
    /// Update max concurrent requests only
    UpdateMaxConcurrent(u32),
    /// Update rate limit only
    UpdateRateLimit(u32),

    // Custom field operations (for extensibility)
    /// Set a custom JSON field by path
    SetField {
        /// JSON path (e.g., "custom.foo.bar")
        path: String,
        /// JSON value
        value: serde_json::Value,
    },
    /// Unset a custom field
    UnsetField {
        /// JSON path
        path: String,
    },
}

impl DiffOp {
    /// Estimate serialized size of this operation in bytes
    pub fn estimated_size(&self) -> usize {
        match self {
            DiffOp::AddTag(s) | DiffOp::RemoveTag(s) => 8 + s.len(),
            DiffOp::AddModel(m) => 50 + m.model_id.len() + m.family.len(),
            DiffOp::RemoveModel(s) => 8 + s.len(),
            DiffOp::UpdateModel { model_id, .. } => 16 + model_id.len(),
            DiffOp::AddTool(t) => 50 + t.tool_id.len() + t.name.len(),
            DiffOp::RemoveTool(s) => 8 + s.len(),
            DiffOp::UpdateHardware(_) => 64,
            DiffOp::UpdateMemory(_) => 8,
            DiffOp::UpdateNetwork(_) => 8,
            DiffOp::UpdateSoftware(_) => 128,
            DiffOp::AddRuntime { name, version } => 12 + name.len() + version.len(),
            DiffOp::RemoveRuntime(s) => 8 + s.len(),
            DiffOp::AddFramework { name, version } => 12 + name.len() + version.len(),
            DiffOp::RemoveFramework(s) => 8 + s.len(),
            DiffOp::UpdateLimits(_) => 32,
            DiffOp::UpdateMaxConcurrent(_) => 8,
            DiffOp::UpdateRateLimit(_) => 8,
            DiffOp::SetField { path, value } => 16 + path.len() + value.to_string().len(),
            DiffOp::UnsetField { path } => 8 + path.len(),
        }
    }

    /// Check if this is a tag operation
    pub fn is_tag_op(&self) -> bool {
        matches!(self, DiffOp::AddTag(_) | DiffOp::RemoveTag(_))
    }

    /// Check if this is a model operation
    pub fn is_model_op(&self) -> bool {
        matches!(
            self,
            DiffOp::AddModel(_) | DiffOp::RemoveModel(_) | DiffOp::UpdateModel { .. }
        )
    }

    /// Check if this is a tool operation
    pub fn is_tool_op(&self) -> bool {
        matches!(self, DiffOp::AddTool(_) | DiffOp::RemoveTool(_))
    }
}

// ============================================================================
// Capability Diff
// ============================================================================

/// Maximum byte length for a wire-format `CapabilityDiff`. Chosen at
/// 64 KiB — generous against real diffs (`estimated_size()` for a
/// busy capability set with several model and tag changes is well
/// under 4 KiB) while blocking a balloon-DoS vector where a peer
/// ships a 100 MB JSON to make `from_slice` allocate that much
/// heap.
pub const MAX_DIFF_BYTES: usize = 64 * 1024;

/// Maximum number of operations per `CapabilityDiff`. A real
/// announcement-driven diff has O(changed-fields) ops (typically
/// well under 50); 1024 is far past that and still bounds the cost
/// of `apply` to a constant rather than O(peer-controlled).
/// Chosen so a fully-packed op-flood payload (each op is the
/// smallest JSON encoding, e.g. `{"AddTag":"t0"}`) stays well
/// under `MAX_DIFF_BYTES`, keeping both caps coherent: byte cap
/// guards heap during parsing, op cap guards CPU during `apply`.
pub const MAX_DIFF_OPS: usize = 1024;

/// Capability diff message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityDiff {
    /// Source node ID
    pub node_id: u64,
    /// Base version this diff applies to
    pub base_version: u64,
    /// New version after applying diff
    pub new_version: u64,
    /// Operations to apply (in order)
    pub ops: Vec<DiffOp>,
    /// Timestamp (nanoseconds since epoch)
    pub timestamp_ns: u64,
}

impl CapabilityDiff {
    /// Create a new capability diff
    pub fn new(node_id: u64, base_version: u64, new_version: u64, ops: Vec<DiffOp>) -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};
        let timestamp_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);

        Self {
            node_id,
            base_version,
            new_version,
            ops,
            timestamp_ns,
        }
    }

    /// Check if this diff is empty (no operations)
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Get number of operations
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// Estimate total serialized size in bytes
    pub fn estimated_size(&self) -> usize {
        // Header: node_id(8) + base_version(8) + new_version(8) + timestamp(8) + ops_len(4)
        let header_size = 36;
        let ops_size: usize = self.ops.iter().map(|op| op.estimated_size()).sum();
        header_size + ops_size
    }

    /// Serialize to bytes (legacy — silent empty-on-failure path).
    ///
    /// Returns `Vec::new()` on any encoding error or cap violation,
    /// indistinguishable from a legitimate empty diff. A sender
    /// that hit the cap silently transmits zero bytes; the receiver
    /// drops the empty payload and the two sides diverge with no
    /// diagnostic. New callers MUST use [`Self::try_to_bytes`],
    /// which surfaces the cap violation as a typed error.
    #[deprecated(note = "use `try_to_bytes` — `to_bytes` swallows cap-violations as an empty Vec")]
    pub fn to_bytes(&self) -> Vec<u8> {
        self.try_to_bytes().unwrap_or_default()
    }

    /// Serialize to bytes with explicit size-cap enforcement.
    ///
    /// Returns `Err(DiffSizeError::TooManyOps)` when `ops.len()`
    /// exceeds [`MAX_DIFF_OPS`], and `Err(DiffSizeError::Encoded
    /// { … })` when the serialized form exceeds
    /// [`MAX_DIFF_BYTES`]. Both checks MUST mirror what
    /// [`Self::from_bytes`] enforces — otherwise the sender
    /// would produce bytes the receiver silently discards.
    /// Production senders building diffs from peer-supplied or
    /// large-cardinality input MUST use this entry point.
    pub fn try_to_bytes(&self) -> Result<Vec<u8>, DiffSizeError> {
        if self.ops.len() > MAX_DIFF_OPS {
            return Err(DiffSizeError::TooManyOps {
                got: self.ops.len(),
                cap: MAX_DIFF_OPS,
            });
        }
        let encoded = serde_json::to_vec(self).map_err(|_| DiffSizeError::Encoded {
            got: self.estimated_size(),
            cap: MAX_DIFF_BYTES,
        })?;
        if encoded.len() > MAX_DIFF_BYTES {
            return Err(DiffSizeError::Encoded {
                got: encoded.len(),
                cap: MAX_DIFF_BYTES,
            });
        }
        Ok(encoded)
    }

    /// Deserialize from bytes.
    ///
    /// Rejects inputs over `MAX_DIFF_BYTES` before parsing, and
    /// rejects diffs over `MAX_DIFF_OPS` after parsing. Both
    /// failure modes return `None` — the existing
    /// "malformed-or-too-large" outcome callers already handle.
    /// Without these caps a peer-supplied 100 MB JSON would expand
    /// into a `Vec<DiffOp>` of arbitrary length, and `apply` would
    /// iterate every op (each currently a no-op for
    /// `SetField`/`UnsetField`) — peer-controlled CPU/RAM burn
    /// for no useful effect.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() > MAX_DIFF_BYTES {
            return None;
        }
        let parsed: Self = serde_json::from_slice(data).ok()?;
        if parsed.ops.len() > MAX_DIFF_OPS {
            return None;
        }
        Some(parsed)
    }
}

// ============================================================================
// Diff Error
// ============================================================================

/// Error during diff application
#[derive(Debug, Clone, PartialEq)]
pub enum DiffError {
    /// Version mismatch (expected base version doesn't match)
    VersionMismatch {
        /// Expected base version
        expected: u64,
        /// Actual current version
        actual: u64,
    },
    /// Model not found for update/remove
    ModelNotFound(String),
    /// Tool not found for remove
    ToolNotFound(String),
    /// Tag not found for remove
    TagNotFound(String),
    /// Runtime not found for remove
    RuntimeNotFound(String),
    /// Framework not found for remove
    FrameworkNotFound(String),
    /// Invalid field path
    InvalidFieldPath(String),
    /// Operation not applicable
    NotApplicable(String),
}

impl std::fmt::Display for DiffError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DiffError::VersionMismatch { expected, actual } => {
                write!(f, "version mismatch: expected {}, got {}", expected, actual)
            }
            DiffError::ModelNotFound(id) => write!(f, "model not found: {}", id),
            DiffError::ToolNotFound(id) => write!(f, "tool not found: {}", id),
            DiffError::TagNotFound(tag) => write!(f, "tag not found: {}", tag),
            DiffError::RuntimeNotFound(name) => write!(f, "runtime not found: {}", name),
            DiffError::FrameworkNotFound(name) => write!(f, "framework not found: {}", name),
            DiffError::InvalidFieldPath(path) => write!(f, "invalid field path: {}", path),
            DiffError::NotApplicable(msg) => write!(f, "operation not applicable: {}", msg),
        }
    }
}

impl std::error::Error for DiffError {}

/// Error returned by [`CapabilityDiff::try_to_bytes`] when the diff
/// would exceed the wire-format caps the receiver enforces.
///
/// Senders that build diffs from peer-supplied or
/// large-cardinality input MUST surface this error rather than
/// swallow it. Without a sender-side cap, `to_bytes` would
/// silently emit bytes that every peer's `from_bytes` discarded
/// — silent state divergence indistinguishable from a network
/// drop.
#[derive(Debug, Clone, PartialEq)]
pub enum DiffSizeError {
    /// `ops.len()` exceeds [`MAX_DIFF_OPS`]. Detected before
    /// serialization so the sender pays no heap cost on rejection.
    TooManyOps {
        /// Actual op count.
        got: usize,
        /// The cap, [`MAX_DIFF_OPS`].
        cap: usize,
    },
    /// Encoded byte length exceeds [`MAX_DIFF_BYTES`]. Surfaces
    /// either the actual encoded length (post-serialize) or a
    /// best-estimate from `estimated_size()` if the encoder
    /// itself failed.
    Encoded {
        /// Actual or estimated encoded length in bytes.
        got: usize,
        /// The cap, [`MAX_DIFF_BYTES`].
        cap: usize,
    },
}

impl std::fmt::Display for DiffSizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DiffSizeError::TooManyOps { got, cap } => write!(
                f,
                "capability diff has {} ops; cap is {} (MAX_DIFF_OPS)",
                got, cap
            ),
            DiffSizeError::Encoded { got, cap } => write!(
                f,
                "encoded capability diff is {} bytes; cap is {} (MAX_DIFF_BYTES)",
                got, cap
            ),
        }
    }
}

impl std::error::Error for DiffSizeError {}

// ============================================================================
// Diff Engine
// ============================================================================

/// Engine for generating and applying capability diffs
pub struct DiffEngine;

impl DiffEngine {
    /// Generate diff operations between old and new capability sets
    pub fn diff(old: &CapabilitySet, new: &CapabilitySet) -> Vec<DiffOp> {
        let mut ops = Vec::new();

        // Phase A.5.4: read both sides through views() once. Post
        // Phase A.5.N (when the typed-struct fields are removed),
        // this is the only seam this function will need to touch.
        let old_views = old.views();
        let new_views = new.views();

        // Diff tags
        Self::diff_tags(&old.tags, &new.tags, &mut ops);

        // Diff models
        Self::diff_models(old_views.models(), new_views.models(), &mut ops);

        // Diff tools
        Self::diff_tools(old_views.tools(), new_views.tools(), &mut ops);

        // Diff hardware (only if changed). Cache the projections
        // since we read them repeatedly in the partial-update sentinels.
        let old_hw = old_views.hardware();
        let new_hw = new_views.hardware();
        if old_hw != new_hw {
            // Check for partial updates
            if old_hw.memory_gb != new_hw.memory_gb
                && old_hw.cpu_cores == new_hw.cpu_cores
                && old_hw.gpu == new_hw.gpu
                && old_hw.storage_gb == new_hw.storage_gb
                && old_hw.network_gbps == new_hw.network_gbps
            {
                ops.push(DiffOp::UpdateMemory(new_hw.memory_gb));
            } else if old_hw.network_gbps != new_hw.network_gbps
                && old_hw.cpu_cores == new_hw.cpu_cores
                && old_hw.gpu == new_hw.gpu
                && old_hw.memory_gb == new_hw.memory_gb
                && old_hw.storage_gb == new_hw.storage_gb
            {
                ops.push(DiffOp::UpdateNetwork(new_hw.network_gbps));
            } else {
                ops.push(DiffOp::UpdateHardware(new_hw.clone()));
            }
        }

        // Diff software
        let old_sw = old_views.software();
        let new_sw = new_views.software();
        if old_sw != new_sw {
            Self::diff_software(old_sw, new_sw, &mut ops);
        }

        // Diff limits (only if changed)
        let old_limits = old_views.resource_limits();
        let new_limits = new_views.resource_limits();
        if old_limits != new_limits {
            // Check for partial updates
            if old_limits.max_concurrent_requests != new_limits.max_concurrent_requests
                && old_limits.max_tokens_per_request == new_limits.max_tokens_per_request
                && old_limits.rate_limit_rpm == new_limits.rate_limit_rpm
                && old_limits.max_batch_size == new_limits.max_batch_size
            {
                ops.push(DiffOp::UpdateMaxConcurrent(
                    new_limits.max_concurrent_requests,
                ));
            } else if old_limits.rate_limit_rpm != new_limits.rate_limit_rpm
                && old_limits.max_concurrent_requests == new_limits.max_concurrent_requests
                && old_limits.max_tokens_per_request == new_limits.max_tokens_per_request
                && old_limits.max_batch_size == new_limits.max_batch_size
            {
                ops.push(DiffOp::UpdateRateLimit(new_limits.rate_limit_rpm));
            } else {
                ops.push(DiffOp::UpdateLimits(new_limits.clone()));
            }
        }

        ops
    }

    /// Diff tags between old and new.
    ///
    /// Phase A.5.N.3: diffs only the *residual* tag set — tags not
    /// claimed by the per-struct decoders. Axis-owned tags
    /// (`hardware.*`, `software.*`, `software.model.*`,
    /// `software.tool.*`, `hardware.limits.*`) are diffed via the
    /// typed `UpdateHardware` / `UpdateSoftware` / `UpdateModel` /
    /// `UpdateMemory` / etc. ops; emitting per-tag AddTag/RemoveTag
    /// for them as well would double-count every change.
    ///
    /// Reserved-prefix tags (`scope:*`, `causal:*`) and legacy
    /// untyped tags pass through here as AddTag/RemoveTag, since
    /// no typed op carries them.
    fn diff_tags(
        old: &HashSet<crate::adapter::net::behavior::tag::Tag>,
        new: &HashSet<crate::adapter::net::behavior::tag::Tag>,
        ops: &mut Vec<DiffOp>,
    ) {
        use crate::adapter::net::behavior::tag::Tag;
        use crate::adapter::net::behavior::tag_codec::{
            hardware_from_tags, hardware_to_tags, models_from_tags, models_to_tags,
            resource_limits_from_tags, resource_limits_to_tags, software_from_tags,
            software_to_tags, tools_from_tags, tools_to_tags,
        };

        // The set of tags the typed encoders ACTUALLY produce for
        // a given input. Filter on this rather than the broader
        // `is_*_owned_tag` predicates: those return true for every
        // `hardware.*` / `software.*` tag including forward-compat
        // unknowns (e.g. a peer-emitted `hardware.future_field` or
        // `software.experimental_runtime=v1`) that the typed
        // decoders silently drop. Without round-tripping, those
        // unknowns get filtered out of the residual set yet aren't
        // captured by any typed `Update*` op either — real changes
        // disappear.
        //
        // The round-trip closure mirrors `set_*` mutators: a tag
        // is "owned" iff re-encoding the decoded struct emits it
        // again. Forward-compat tags (which the decoder ignores)
        // therefore fall through to the residual diff and surface
        // as `AddTag` / `RemoveTag` ops.
        let owned = |tags: &HashSet<Tag>| -> HashSet<Tag> {
            let v: Vec<Tag> = tags.iter().cloned().collect();
            let mut out: HashSet<Tag> = HashSet::new();
            out.extend(hardware_to_tags(&hardware_from_tags(&v)));
            out.extend(software_to_tags(&software_from_tags(&v)));
            out.extend(models_to_tags(&models_from_tags(&v)));
            out.extend(tools_to_tags(&tools_from_tags(&v)));
            out.extend(resource_limits_to_tags(&resource_limits_from_tags(&v)));
            out
        };
        let old_owned = owned(old);
        let new_owned = owned(new);

        // Membership-by-axis-key, not exact `Tag` equality.
        // `Tag::AxisValue` carries its `=` / `:` separator in the
        // struct, and `Eq` compares it. The typed encoders emit
        // canonical separators, so an input tag of
        // `software.os:linux` (Colon) re-encodes to
        // `software.os=linux` (Eq) — exact `HashSet::contains`
        // misses, the colon form lands in the residual, and a
        // `RemoveTag` ships without a compensating
        // `UpdateSoftware` (the typed projections compare equal,
        // since both decode to `os = "linux"`). Apply on the
        // receiver then drops the tag entirely.
        //
        // Reduce to `(axis, key)` for the consumed check so the
        // separator becomes irrelevant. Forward-compat tags keep
        // working: their axis_key isn't in `*_owned_keys` (the
        // encoders never emitted it), so they fall through to the
        // residual diff exactly as before.
        let consumed_axis_keys =
            |owned: &HashSet<Tag>| -> HashSet<crate::adapter::net::behavior::tag::TagKey> {
                owned.iter().filter_map(|t| t.axis_key()).collect()
            };
        let old_consumed = consumed_axis_keys(&old_owned);
        let new_consumed = consumed_axis_keys(&new_owned);
        let is_residual =
            |t: &Tag, consumed: &HashSet<crate::adapter::net::behavior::tag::TagKey>| -> bool {
                // Reserved/Legacy tags never have an axis_key; always
                // residual. Axis-prefixed tags are residual iff their
                // (axis, key) wasn't claimed by any typed encoder.
                t.axis_key().is_none_or(|k| !consumed.contains(&k))
            };
        let old_residual: HashSet<&Tag> = old
            .iter()
            .filter(|t| is_residual(t, &old_consumed))
            .collect();
        let new_residual: HashSet<&Tag> = new
            .iter()
            .filter(|t| is_residual(t, &new_consumed))
            .collect();

        // Deterministic op order: HashSet iteration is randomized,
        // so two senders with identical inputs would otherwise emit
        // ops in different orders, breaking signed-envelope hashing
        // (same class as 3291b2c2 fixed for tag emission). Sort the
        // residual diffs by canonical wire string. See CR-4 in
        // `CODE_REVIEW_2026_05_10_CAPABILITY_SYSTEM_2.md`.
        let mut removed: Vec<String> = old_residual
            .difference(&new_residual)
            .map(|t| t.to_string())
            .collect();
        removed.sort();
        let mut added: Vec<String> = new_residual
            .difference(&old_residual)
            .map(|t| t.to_string())
            .collect();
        added.sort();
        for s in removed {
            ops.push(DiffOp::RemoveTag(s));
        }
        for s in added {
            ops.push(DiffOp::AddTag(s));
        }
    }

    /// Diff models between old and new
    fn diff_models(old: &[ModelCapability], new: &[ModelCapability], ops: &mut Vec<DiffOp>) {
        // BTreeMap (not HashMap) for deterministic op order: the
        // emitted `Vec<DiffOp>` is hashed/signed by downstream
        // consumers, and a HashMap iteration order would give
        // identical inputs different ops sequences across runs.
        // See CR-4 in `CODE_REVIEW_2026_05_10_CAPABILITY_SYSTEM_2.md`.
        let old_map: std::collections::BTreeMap<&str, &ModelCapability> =
            old.iter().map(|m| (m.model_id.as_str(), m)).collect();
        let new_map: std::collections::BTreeMap<&str, &ModelCapability> =
            new.iter().map(|m| (m.model_id.as_str(), m)).collect();

        // Removed models
        for (id, _) in old_map.iter() {
            if !new_map.contains_key(id) {
                ops.push(DiffOp::RemoveModel((*id).to_string()));
            }
        }

        // Added or updated models
        for (id, new_model) in new_map.iter() {
            if let Some(old_model) = old_map.get(id) {
                // Check for updates
                if *old_model != *new_model {
                    // Check if only tokens_per_sec or loaded changed
                    if old_model.family == new_model.family
                        && old_model.parameters_b_x10 == new_model.parameters_b_x10
                        && old_model.context_length == new_model.context_length
                        && old_model.quantization == new_model.quantization
                        && old_model.modalities == new_model.modalities
                    {
                        // Partial update
                        let tokens_per_sec = if old_model.tokens_per_sec != new_model.tokens_per_sec
                        {
                            Some(new_model.tokens_per_sec)
                        } else {
                            None
                        };
                        let loaded = if old_model.loaded != new_model.loaded {
                            Some(new_model.loaded)
                        } else {
                            None
                        };
                        if tokens_per_sec.is_some() || loaded.is_some() {
                            ops.push(DiffOp::UpdateModel {
                                model_id: (*id).to_string(),
                                tokens_per_sec,
                                loaded,
                            });
                        }
                    } else {
                        // Full replacement
                        ops.push(DiffOp::RemoveModel((*id).to_string()));
                        ops.push(DiffOp::AddModel((*new_model).clone()));
                    }
                }
            } else {
                // New model
                ops.push(DiffOp::AddModel((*new_model).clone()));
            }
        }
    }

    /// Diff tools between old and new
    fn diff_tools(old: &[ToolCapability], new: &[ToolCapability], ops: &mut Vec<DiffOp>) {
        // BTreeMap for deterministic op order — see CR-4 / diff_models.
        let old_map: std::collections::BTreeMap<&str, &ToolCapability> =
            old.iter().map(|t| (t.tool_id.as_str(), t)).collect();
        let new_map: std::collections::BTreeMap<&str, &ToolCapability> =
            new.iter().map(|t| (t.tool_id.as_str(), t)).collect();

        // Removed tools
        for (id, _) in old_map.iter() {
            if !new_map.contains_key(id) {
                ops.push(DiffOp::RemoveTool((*id).to_string()));
            }
        }

        // Added or replaced tools
        for (id, new_tool) in new_map.iter() {
            if let Some(old_tool) = old_map.get(id) {
                if *old_tool != *new_tool {
                    // Tools don't have partial updates, full replacement
                    ops.push(DiffOp::RemoveTool((*id).to_string()));
                    ops.push(DiffOp::AddTool((*new_tool).clone()));
                }
            } else {
                ops.push(DiffOp::AddTool((*new_tool).clone()));
            }
        }
    }

    /// Diff software capabilities
    fn diff_software(
        old: &SoftwareCapabilities,
        new: &SoftwareCapabilities,
        ops: &mut Vec<DiffOp>,
    ) {
        // Check if we can do partial updates
        let os_changed = old.os != new.os || old.os_version != new.os_version;
        let cuda_changed = old.cuda_version != new.cuda_version;
        let drivers_changed = old.drivers != new.drivers;

        if os_changed || cuda_changed || drivers_changed {
            // Full software update needed
            ops.push(DiffOp::UpdateSoftware(new.clone()));
            return;
        }

        // Diff runtimes — BTreeMap for deterministic op order
        // (see CR-4 / diff_models).
        let old_runtimes: std::collections::BTreeMap<&str, &str> = old
            .runtimes
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        let new_runtimes: std::collections::BTreeMap<&str, &str> = new
            .runtimes
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();

        for (name, _) in old_runtimes.iter() {
            if !new_runtimes.contains_key(name) {
                ops.push(DiffOp::RemoveRuntime((*name).to_string()));
            }
        }
        for (name, version) in new_runtimes.iter() {
            if old_runtimes.get(name) != Some(version) {
                ops.push(DiffOp::AddRuntime {
                    name: (*name).to_string(),
                    version: (*version).to_string(),
                });
            }
        }

        // Diff frameworks — BTreeMap for deterministic op order.
        let old_frameworks: std::collections::BTreeMap<&str, &str> = old
            .frameworks
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        let new_frameworks: std::collections::BTreeMap<&str, &str> = new
            .frameworks
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();

        for (name, _) in old_frameworks.iter() {
            if !new_frameworks.contains_key(name) {
                ops.push(DiffOp::RemoveFramework((*name).to_string()));
            }
        }
        for (name, version) in new_frameworks.iter() {
            if old_frameworks.get(name) != Some(version) {
                ops.push(DiffOp::AddFramework {
                    name: (*name).to_string(),
                    version: (*version).to_string(),
                });
            }
        }
    }

    /// Apply diff operations to a capability set, ignoring
    /// `diff.base_version`.
    ///
    /// **Deprecated:** production callers MUST use
    /// [`Self::apply_with_version`] so a stale `base_version →
    /// new_version` diff cannot silently roll receiver state
    /// backward. The version-naive entry point is preserved only
    /// for hand-built diffs in unit tests where no live version is
    /// tracked. New non-test callsites should reach for
    /// `apply_with_version`.
    ///
    /// Returns the updated capability set or an error if
    /// application fails. The `strict` parameter controls whether
    /// missing items cause errors.
    ///
    /// This function ignores `diff.base_version` entirely. A
    /// receiver at v5 will happily accept an old `base_version=2 →
    /// new_version=3` diff and silently roll state back, so
    /// callers MUST gate via `apply_with_version` outside tests.
    #[deprecated(
        since = "0.10.0",
        note = "version-naive — use apply_with_version to enforce the \
                base_version check; this entry point is retained only for \
                hand-built diffs in unit tests"
    )]
    pub fn apply(
        base: &CapabilitySet,
        diff: &CapabilityDiff,
        strict: bool,
    ) -> Result<CapabilitySet, DiffError> {
        Self::apply_unchecked(base, diff, strict)
    }

    /// Internal, version-naive apply. Same body the deprecated public
    /// `apply` carried — split out so test code can reach it without
    /// triggering the deprecation warning. Production callers must
    /// use [`Self::apply_with_version`].
    pub(crate) fn apply_unchecked(
        base: &CapabilitySet,
        diff: &CapabilityDiff,
        strict: bool,
    ) -> Result<CapabilitySet, DiffError> {
        let mut result = base.clone();

        for op in &diff.ops {
            Self::apply_op(&mut result, op, strict)?;
        }

        Ok(result)
    }

    /// Apply diff operations to a capability set, asserting that
    /// the diff was generated against `current_version` (the version
    /// the caller has authoritative state for).
    ///
    /// Returns [`DiffError::VersionMismatch`] when
    /// `current_version != diff.base_version` — i.e. the diff is
    /// stale (older than what we hold) or out-of-order (newer than
    /// what we have a base for). This is the correct contract for
    /// any production caller; [`Self::apply`] is reserved for
    /// version-naive contexts (tests, hand-built diffs, etc.).
    ///
    /// The version-checked counterpart to `apply`, which is
    /// version-naive and silently accepts stale diffs.
    pub fn apply_with_version(
        base: &CapabilitySet,
        current_version: u64,
        diff: &CapabilityDiff,
        strict: bool,
    ) -> Result<CapabilitySet, DiffError> {
        if current_version != diff.base_version {
            return Err(DiffError::VersionMismatch {
                expected: diff.base_version,
                actual: current_version,
            });
        }
        // Reject diffs that don't advance the version forward. A
        // diff with `new_version <= base_version` (legitimate
        // base, but regressing or stationary new_version) would
        // change the receiver's state while the tracked version
        // went backward (or stayed put). `validate_chain` catches
        // this when chains are validated in bulk, but without this
        // check a single `apply_with_version` call has no
        // protection. The check is consistent with
        // `validate_chain`'s strict-forward-progress invariant and
        // prevents single-diff rollback even in the absence of
        // chain validation.
        if diff.new_version <= diff.base_version {
            return Err(DiffError::NotApplicable(format!(
                "non-forward diff version: {} -> {}",
                diff.base_version, diff.new_version
            )));
        }
        // Internal: the version checks just succeeded, so we
        // delegate to the unchecked apply rather than the
        // deprecated public wrapper (avoids the deprecation
        // warning at this site).
        Self::apply_unchecked(base, diff, strict)
    }

    /// Apply a single diff operation
    fn apply_op(caps: &mut CapabilitySet, op: &DiffOp, strict: bool) -> Result<(), DiffError> {
        // Phase A.5.6: every write goes through CapabilitySet's
        // typed setters (`set_hardware` / `set_software` / `set_limits`
        // / `set_models` / `set_tools`) instead of touching the typed
        // struct fields directly. Partial-field updates use the
        // read-modify-write idiom: pull the projection via `views()`,
        // mutate the owned clone, then hand the whole thing back via
        // the matching setter. Post-Phase-A.5.N (when typed fields
        // are gone), the setter bodies re-encode into the underlying
        // tag set; this function does not need to change again.
        //
        // `caps.tags` is a `HashSet<Tag>` (post Phase A.5.N.2);
        // AddTag/RemoveTag wire payloads are still String, so we
        // parse each one through `Tag::parse` (permissive — wire
        // payloads come from peer nodes that are authoritative for
        // the value). A malformed tag string is treated as "no-op"
        // for AddTag and "not found" for RemoveTag.
        match op {
            DiffOp::AddTag(tag) => {
                if let Ok(parsed) = crate::adapter::net::behavior::tag::Tag::parse(tag) {
                    caps.tags.insert(parsed);
                }
            }
            DiffOp::RemoveTag(tag) => {
                let parsed = match crate::adapter::net::behavior::tag::Tag::parse(tag) {
                    Ok(t) => t,
                    Err(_) => {
                        if strict {
                            return Err(DiffError::TagNotFound(tag.clone()));
                        }
                        return Ok(());
                    }
                };
                if !caps.tags.remove(&parsed) && strict {
                    return Err(DiffError::TagNotFound(tag.clone()));
                }
            }
            DiffOp::AddModel(model) => {
                // Remove existing model with same ID if present
                let mut models = caps.views().models().clone();
                models.retain(|m| m.model_id != model.model_id);
                models.push(model.clone());
                caps.set_models(models);
            }
            DiffOp::RemoveModel(model_id) => {
                let mut models = caps.views().models().clone();
                let before = models.len();
                models.retain(|m| m.model_id != *model_id);
                if models.len() == before {
                    if strict {
                        return Err(DiffError::ModelNotFound(model_id.clone()));
                    }
                    // No-op; skip the redundant set_models clone.
                } else {
                    caps.set_models(models);
                }
            }
            DiffOp::UpdateModel {
                model_id,
                tokens_per_sec,
                loaded,
            } => {
                let mut models = caps.views().models().clone();
                if let Some(model) = models.iter_mut().find(|m| m.model_id == *model_id) {
                    if let Some(tps) = tokens_per_sec {
                        model.tokens_per_sec = *tps;
                    }
                    if let Some(l) = loaded {
                        model.loaded = *l;
                    }
                    caps.set_models(models);
                } else if strict {
                    return Err(DiffError::ModelNotFound(model_id.clone()));
                }
            }
            DiffOp::AddTool(tool) => {
                let mut tools = caps.views().tools().clone();
                tools.retain(|t| t.tool_id != tool.tool_id);
                tools.push(tool.clone());
                caps.set_tools(tools);
            }
            DiffOp::RemoveTool(tool_id) => {
                let mut tools = caps.views().tools().clone();
                let before = tools.len();
                tools.retain(|t| t.tool_id != *tool_id);
                if tools.len() == before {
                    if strict {
                        return Err(DiffError::ToolNotFound(tool_id.clone()));
                    }
                } else {
                    caps.set_tools(tools);
                }
            }
            DiffOp::UpdateHardware(hw) => {
                caps.set_hardware(hw.clone());
            }
            DiffOp::UpdateMemory(mem) => {
                let mut hw = caps.views().hardware().clone();
                hw.memory_gb = *mem;
                caps.set_hardware(hw);
            }
            DiffOp::UpdateNetwork(net) => {
                let mut hw = caps.views().hardware().clone();
                hw.network_gbps = *net;
                caps.set_hardware(hw);
            }
            DiffOp::UpdateSoftware(sw) => {
                caps.set_software(sw.clone());
            }
            DiffOp::AddRuntime { name, version } => {
                let mut sw = caps.views().software().clone();
                sw.runtimes.retain(|(n, _)| n != name);
                sw.runtimes.push((name.clone(), version.clone()));
                caps.set_software(sw);
            }
            DiffOp::RemoveRuntime(name) => {
                let mut sw = caps.views().software().clone();
                let before = sw.runtimes.len();
                sw.runtimes.retain(|(n, _)| n != name);
                if sw.runtimes.len() == before {
                    if strict {
                        return Err(DiffError::RuntimeNotFound(name.clone()));
                    }
                } else {
                    caps.set_software(sw);
                }
            }
            DiffOp::AddFramework { name, version } => {
                let mut sw = caps.views().software().clone();
                sw.frameworks.retain(|(n, _)| n != name);
                sw.frameworks.push((name.clone(), version.clone()));
                caps.set_software(sw);
            }
            DiffOp::RemoveFramework(name) => {
                let mut sw = caps.views().software().clone();
                let before = sw.frameworks.len();
                sw.frameworks.retain(|(n, _)| n != name);
                if sw.frameworks.len() == before {
                    if strict {
                        return Err(DiffError::FrameworkNotFound(name.clone()));
                    }
                } else {
                    caps.set_software(sw);
                }
            }
            DiffOp::UpdateLimits(limits) => {
                caps.set_limits(limits.clone());
            }
            DiffOp::UpdateMaxConcurrent(max) => {
                let mut limits = caps.views().resource_limits().clone();
                limits.max_concurrent_requests = *max;
                caps.set_limits(limits);
            }
            DiffOp::UpdateRateLimit(rpm) => {
                let mut limits = caps.views().resource_limits().clone();
                limits.rate_limit_rpm = *rpm;
                caps.set_limits(limits);
            }
            DiffOp::SetField { path, .. } | DiffOp::UnsetField { path } => {
                // SetField/UnsetField are unimplemented — the
                // JSON-path primitive that would back them hasn't
                // been built. `strict=true` surfaces the gap as
                // `DiffError::NotApplicable(path)`; non-strict
                // continues to no-op for best-effort callers.
                // Returning `Ok(())` even under `strict=true` would
                // let a peer shipping `SetField{path: "tags",
                // value: [...]}` get `Ok` from `apply` while
                // nothing changed — sender's view would diverge
                // from receiver's silently and `validate_chain`
                // couldn't catch it. When the JSON-path primitive
                // lands, this arm should switch to a real mutation
                // and drop the strict-mode error.
                if strict {
                    return Err(DiffError::NotApplicable(format!(
                        "SetField/UnsetField unimplemented (path: {})",
                        path
                    )));
                }
            }
        }
        Ok(())
    }

    /// Validate that a chain of diffs is consistent
    ///
    /// Checks that version numbers are sequential and base versions match.
    ///
    /// Each diff is required to satisfy `curr.new_version >
    /// curr.base_version` AND
    /// `prev.new_version == curr.base_version` between adjacent
    /// diffs. The within-diff `new_version > base_version` invariant
    /// is load-bearing — without it a peer could ship
    /// `base_version=5, new_version=3` (a "rollback while applying
    /// ops") and validation would accept it. Combined with the
    /// version-naive [`Self::apply`], a receiver could advance
    /// state forward while its tracked version went backward.
    pub fn validate_chain(diffs: &[CapabilityDiff]) -> bool {
        if diffs.is_empty() {
            return true;
        }

        // Within-diff check: every diff must move the version
        // forward (strictly). A diff with
        // `new_version <= base_version` is incoherent — there's
        // nothing to "apply forward" if the version is going
        // backward or staying put.
        for diff in diffs {
            if diff.new_version <= diff.base_version {
                return false;
            }
        }

        for i in 1..diffs.len() {
            let prev = &diffs[i - 1];
            let curr = &diffs[i];

            // Same node
            if prev.node_id != curr.node_id {
                return false;
            }

            // Version chain
            if prev.new_version != curr.base_version {
                return false;
            }

            // Monotonic timestamps
            if prev.timestamp_ns > curr.timestamp_ns {
                return false;
            }
        }

        true
    }

    /// Compact a chain of diffs into a single diff
    ///
    /// This is useful for reducing storage/bandwidth when many small diffs accumulate.
    pub fn compact(base: &CapabilitySet, diffs: &[CapabilityDiff]) -> Option<CapabilityDiff> {
        if diffs.is_empty() {
            return None;
        }

        // Apply all diffs to get final state. This is the internal
        // chain-compaction path — the caller has already validated
        // the chain via `validate_chain` before reaching here, so
        // the per-diff version check is redundant.
        // `apply_unchecked` bypasses it cleanly without triggering
        // the public `apply`'s deprecation warning.
        let mut current = base.clone();
        for diff in diffs {
            current = Self::apply_unchecked(&current, diff, false).ok()?;
        }

        // Generate new diff from base to final
        let ops = Self::diff(base, &current);

        let first = diffs.first()?;
        let last = diffs.last()?;

        Some(CapabilityDiff {
            node_id: first.node_id,
            base_version: first.base_version,
            new_version: last.new_version,
            ops,
            timestamp_ns: last.timestamp_ns,
        })
    }

    /// Estimate bandwidth savings of using diff vs full announcement
    ///
    /// Returns (diff_size, full_size, savings_percent)
    pub fn bandwidth_savings(diff: &CapabilityDiff, full: &CapabilitySet) -> (usize, usize, f64) {
        let diff_size = diff.estimated_size();
        let full_size = full.to_bytes().len();

        let savings = if full_size > 0 {
            100.0 * (1.0 - (diff_size as f64 / full_size as f64))
        } else {
            0.0
        };

        (diff_size, full_size, savings)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
// This module's tests intentionally exercise the deprecated
// `DiffEngine::apply` to verify version-naive semantics. The
// deprecation warning is the right signal for production callers but
// only noise for the unit tests that pin the version-naive contract.
#[allow(deprecated)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::capability::{GpuInfo, GpuVendor, Modality};

    fn sample_capability_set() -> CapabilitySet {
        let gpu = GpuInfo::new(GpuVendor::Nvidia, "RTX 4090", 24);
        let hardware = HardwareCapabilities::new()
            .with_cpu(16, 32)
            .with_memory(64)
            .with_gpu(gpu);

        let software = SoftwareCapabilities::new()
            .with_os("linux", "6.1")
            .add_runtime("python", "3.11")
            .add_framework("pytorch", "2.1");

        let model = ModelCapability::new("llama-3.1-70b", "llama")
            .with_parameters(70.0)
            .with_context_length(128000)
            .add_modality(Modality::Text)
            .with_tokens_per_sec(50)
            .with_loaded(true);

        let tool = ToolCapability::new("python_repl", "Python REPL");

        CapabilitySet::new()
            .with_hardware(hardware)
            .with_software(software)
            .add_model(model)
            .add_tool(tool)
            .add_tag("inference")
            .add_tag("gpu")
            .with_limits(ResourceLimits::new().with_max_concurrent(10))
    }

    /// Regression for CR-4: diff op order must be deterministic
    /// across runs. Previously `diff_tags` iterated `HashSet`
    /// difference and `diff_models` / `diff_tools` / `diff_software`
    /// iterated `HashMap`s — both have randomized order, so two
    /// senders with identical inputs (or one sender retrying) would
    /// emit ops in different orders, breaking signed-envelope
    /// hashing. Run the diff multiple times and pin equality.
    #[test]
    fn diff_op_order_is_stable_across_runs() {
        use crate::adapter::net::behavior::capability::CapabilitySet;
        use crate::adapter::net::behavior::tag::Tag;

        // Many residual tags so any HashSet iteration randomness
        // is overwhelmingly likely to surface across 64 runs.
        let mut old = CapabilitySet::new();
        let mut new = CapabilitySet::new();
        for i in 0..16 {
            old.tags
                .insert(Tag::parse(&format!("legacy-old-{i}")).unwrap());
            new.tags
                .insert(Tag::parse(&format!("legacy-new-{i}")).unwrap());
        }

        // Multiple models in both sides exercise diff_models.
        for i in 0..8 {
            let m = ModelCapability::new(format!("model-{i}"), "llama").with_tokens_per_sec(50);
            old = old.add_model(m.clone());
            // Skip every other model in `new` so removed-models is
            // exercised; modify the rest so updates are exercised.
            if i % 2 == 0 {
                new = new.add_model(m.with_tokens_per_sec(60));
            }
        }

        // Multiple tools.
        for i in 0..8 {
            let t = ToolCapability::new(format!("tool-{i}"), format!("Tool {i}"));
            old = old.add_tool(t.clone());
            if i % 3 != 0 {
                new = new.add_tool(t);
            }
        }

        let baseline = DiffEngine::diff(&old, &new);
        for run in 0..64 {
            let next = DiffEngine::diff(&old, &new);
            assert_eq!(
                baseline, next,
                "run {run}: diff op order differed across runs"
            );
        }
    }

    #[test]
    fn test_diff_no_changes() {
        let caps = sample_capability_set();
        let ops = DiffEngine::diff(&caps, &caps);
        assert!(ops.is_empty());
    }

    #[test]
    fn test_diff_add_tag() {
        let old = sample_capability_set();
        let mut new = old.clone();
        new = new.add_tag("training");

        let ops = DiffEngine::diff(&old, &new);
        assert_eq!(ops.len(), 1);
        assert!(matches!(&ops[0], DiffOp::AddTag(t) if t == "training"));
    }

    #[test]
    fn test_diff_remove_tag() {
        let old = sample_capability_set();
        let mut new = old.clone();
        // Phase A.5.N.2: tags is HashSet<Tag>. Remove the legacy
        // "inference" tag by re-parsing it and calling HashSet::remove.
        let inference =
            crate::adapter::net::behavior::tag::Tag::parse("inference").expect("legacy tag parse");
        new.tags.remove(&inference);

        let ops = DiffEngine::diff(&old, &new);
        assert_eq!(ops.len(), 1);
        assert!(matches!(&ops[0], DiffOp::RemoveTag(t) if t == "inference"));
    }

    /// Regression: forward-compatible axis-prefixed tags that the
    /// typed decoders silently ignore (e.g. a peer-emitted
    /// `hardware.future_field=v2`) used to be filtered as
    /// "axis-owned" by `is_*_owned_tag`, removed from the residual
    /// diff, AND ignored by every typed `Update*` op — real
    /// changes disappeared. The fix uses the round-trip closure
    /// (`*_to_tags(&*_from_tags(...))`) to identify what's
    /// actually consumed by the typed encoders, so unknowns fall
    /// through to AddTag / RemoveTag.
    #[test]
    fn diff_emits_addtag_removetag_for_forward_compat_axis_tags() {
        use crate::adapter::net::behavior::capability::CapabilitySet;
        use crate::adapter::net::behavior::tag::Tag;

        let make = |raw: &str| Tag::parse(raw).expect("tag parses");

        // Forward-compat axis tag added across the diff. Neither
        // side carries any other axis-relevant content, so the
        // typed encoders produce empty closures and the residual
        // diff sees the tag exactly once.
        let old = CapabilitySet::new();
        let mut new = CapabilitySet::new();
        new.tags.insert(make("hardware.future_field=v2"));

        let ops = DiffEngine::diff(&old, &new);
        assert!(
            ops.iter()
                .any(|op| matches!(op, DiffOp::AddTag(t) if t == "hardware.future_field=v2")),
            "expected AddTag(hardware.future_field=v2) for forward-compat axis tag, got {:?}",
            ops,
        );

        // Symmetric: forward-compat tag removed.
        let mut old = CapabilitySet::new();
        old.tags.insert(make("software.experimental_runtime=v1"));
        let new = CapabilitySet::new();

        let ops = DiffEngine::diff(&old, &new);
        assert!(
            ops.iter().any(
                |op| matches!(op, DiffOp::RemoveTag(t) if t == "software.experimental_runtime=v1")
            ),
            "expected RemoveTag(software.experimental_runtime=v1), got {:?}",
            ops,
        );

        // Negative control: a known hardware key (encoded by
        // `hardware_to_tags`) still routes through `UpdateHardware`,
        // not through AddTag. The fix doesn't double-emit.
        let old = CapabilitySet::new();
        let new = CapabilitySet::new().with_hardware(
            crate::adapter::net::behavior::capability::HardwareCapabilities::new().with_memory(64),
        );
        let ops = DiffEngine::diff(&old, &new);
        assert!(
            !ops.iter()
                .any(|op| matches!(op, DiffOp::AddTag(t) if t.starts_with("hardware.memory_gb"))),
            "known hardware keys must not double-emit as AddTag — got {:?}",
            ops,
        );
    }

    /// Regression: an axis tag whose separator differs from the
    /// canonical form emitted by the typed encoder (e.g. an input
    /// `software.os:linux` with `:` separator vs. the encoder's
    /// `software.os=linux` with `=`) used to be missed by exact
    /// `HashSet::contains` membership. Result: the colon-form
    /// landed in the residual and shipped as `RemoveTag` even
    /// though the typed projections (`SoftwareCapabilities`)
    /// compared equal — so no compensating `UpdateSoftware` op
    /// rode along. Apply on the receiver dropped the tag entirely.
    /// The fix tests membership by `(axis, key)` so the separator
    /// is irrelevant.
    #[test]
    fn diff_does_not_remove_axis_tag_when_only_separator_differs() {
        use crate::adapter::net::behavior::capability::CapabilitySet;
        use crate::adapter::net::behavior::tag::Tag;

        // Both sides decode to `SoftwareCapabilities { os: "linux", … }`.
        // The only on-wire difference is the separator. No typed
        // change → no `UpdateSoftware`; no residual change should
        // ship either.
        let mut old = CapabilitySet::new();
        old.tags
            .insert(Tag::parse("software.os:linux").expect("parse colon form"));
        let mut new = CapabilitySet::new();
        new.tags
            .insert(Tag::parse("software.os=linux").expect("parse eq form"));

        let ops = DiffEngine::diff(&old, &new);
        assert!(
            !ops.iter().any(|op| matches!(op, DiffOp::RemoveTag(_))),
            "no `RemoveTag` should be emitted for canonical-separator-only differences — got {:?}",
            ops,
        );
        assert!(
            !ops.iter().any(|op| matches!(op, DiffOp::AddTag(_))),
            "no `AddTag` either — the typed `UpdateSoftware` path owns the change — got {:?}",
            ops,
        );

        // End-to-end safety: applying the diff to `old` must NOT
        // strip the `os=linux` data. Round-trip through the typed
        // projection.
        let mut applied = old.clone();
        for op in &ops {
            DiffEngine::apply_op(&mut applied, op, false).expect("apply succeeds");
        }
        let applied_sw =
            crate::adapter::net::behavior::capability::SoftwareCapabilities::from(&applied);
        assert_eq!(
            applied_sw.os, "linux",
            "`os=linux` data must survive a separator-only diff round-trip — applied caps = {applied:?}",
        );
    }

    #[test]
    fn test_diff_update_model_loaded() {
        let old = sample_capability_set();
        // Phase A.5.N.3: read models via views(), mutate, set_models.
        let mut models = old.views().models().clone();
        models[0].loaded = false;
        let mut new = old.clone();
        new.set_models(models);

        let ops = DiffEngine::diff(&old, &new);
        assert_eq!(ops.len(), 1);
        assert!(matches!(
            &ops[0],
            DiffOp::UpdateModel { model_id, loaded: Some(false), .. } if model_id == "llama-3.1-70b"
        ));
    }

    #[test]
    fn test_diff_add_model() {
        let old = sample_capability_set();
        let new = old.clone().add_model(
            ModelCapability::new("mistral-7b", "mistral")
                .with_parameters(7.0)
                .add_modality(Modality::Text),
        );

        let ops = DiffEngine::diff(&old, &new);
        assert_eq!(ops.len(), 1);
        assert!(matches!(&ops[0], DiffOp::AddModel(m) if m.model_id == "mistral-7b"));
    }

    #[test]
    fn test_diff_update_memory() {
        let old = sample_capability_set();
        let mut new = old.clone();
        let mut hw = new.views().hardware().clone();
        hw.memory_gb = 128;
        new.set_hardware(hw);

        let ops = DiffEngine::diff(&old, &new);
        assert_eq!(ops.len(), 1);
        assert!(matches!(&ops[0], DiffOp::UpdateMemory(128)));
    }

    #[test]
    fn test_apply_diff() {
        let old = sample_capability_set();

        let diff = CapabilityDiff::new(
            1,
            1,
            2,
            vec![DiffOp::AddTag("training".into()), DiffOp::UpdateMemory(128)],
        );

        let new = DiffEngine::apply(&old, &diff, true).unwrap();

        assert!(new.has_tag("training"));
        assert_eq!(new.views().hardware().memory_gb, 128);
    }

    #[test]
    fn test_apply_strict_error() {
        let caps = sample_capability_set();

        let diff = CapabilityDiff::new(1, 1, 2, vec![DiffOp::RemoveTag("nonexistent".into())]);

        let result = DiffEngine::apply(&caps, &diff, true);
        assert!(matches!(result, Err(DiffError::TagNotFound(_))));

        // Non-strict should succeed
        let result = DiffEngine::apply(&caps, &diff, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_chain() {
        let diff1 = CapabilityDiff::new(1, 1, 2, vec![DiffOp::AddTag("a".into())]);
        let mut diff2 = CapabilityDiff::new(1, 2, 3, vec![DiffOp::AddTag("b".into())]);
        diff2.timestamp_ns = diff1.timestamp_ns + 1000;

        assert!(DiffEngine::validate_chain(&[diff1.clone(), diff2.clone()]));

        // Wrong base version
        let diff3 = CapabilityDiff::new(1, 1, 3, vec![DiffOp::AddTag("c".into())]);
        assert!(!DiffEngine::validate_chain(&[diff1.clone(), diff3]));
    }

    #[test]
    fn test_compact_diffs() {
        let base = sample_capability_set();

        let diff1 = CapabilityDiff::new(1, 1, 2, vec![DiffOp::AddTag("training".into())]);
        let diff2 = CapabilityDiff::new(1, 2, 3, vec![DiffOp::AddTag("distributed".into())]);
        let diff3 = CapabilityDiff::new(1, 3, 4, vec![DiffOp::UpdateMemory(128)]);

        let compacted = DiffEngine::compact(&base, &[diff1, diff2, diff3]).unwrap();

        assert_eq!(compacted.base_version, 1);
        assert_eq!(compacted.new_version, 4);
        assert_eq!(compacted.ops.len(), 3); // 2 AddTag + 1 UpdateMemory
    }

    #[test]
    fn test_bandwidth_savings() {
        let caps = sample_capability_set();

        // Small diff
        let diff = CapabilityDiff::new(1, 1, 2, vec![DiffOp::AddTag("test".into())]);

        let (diff_size, full_size, savings) = DiffEngine::bandwidth_savings(&diff, &caps);

        assert!(diff_size < full_size);
        assert!(savings > 50.0); // Should save significant bandwidth
    }

    #[test]
    fn test_roundtrip_diff() {
        let old = sample_capability_set();
        let mut new = old.clone();

        // Make several changes. Phase A.5.N.3: every write goes
        // through the typed setters / add_*; reads through views().
        new = new.add_tag("training");
        let inference =
            crate::adapter::net::behavior::tag::Tag::parse("inference").expect("legacy tag parse");
        new.tags.remove(&inference);

        // Tweak the first existing model: loaded=false + new throughput.
        let mut models = new.views().models().clone();
        models[0].loaded = false;
        models[0].tokens_per_sec = 100;
        new.set_models(models);

        // Bump memory.
        let mut hw = new.views().hardware().clone();
        hw.memory_gb = 128;
        new.set_hardware(hw);

        // Add a second model.
        new = new.add_model(
            ModelCapability::new("mistral-7b", "mistral")
                .with_parameters(7.0)
                .add_modality(Modality::Text),
        );

        // Generate diff
        let ops = DiffEngine::diff(&old, &new);
        let diff = CapabilityDiff::new(1, 1, 2, ops);

        // Apply diff
        let applied = DiffEngine::apply(&old, &diff, true).unwrap();

        // Verify
        assert!(applied.has_tag("training"));
        assert!(!applied.has_tag("inference"));
        let v = applied.views();
        assert_eq!(v.hardware().memory_gb, 128);
        assert_eq!(v.models().len(), 2);
        assert!(
            !v.models()
                .iter()
                .find(|m| m.model_id == "llama-3.1-70b")
                .unwrap()
                .loaded
        );
    }

    #[test]
    fn test_diff_serialization() {
        let diff = CapabilityDiff::new(
            1,
            1,
            2,
            vec![DiffOp::AddTag("test".into()), DiffOp::UpdateMemory(64)],
        );

        let bytes = diff.try_to_bytes().expect("normal-size diff must encode");
        let parsed = CapabilityDiff::from_bytes(&bytes).unwrap();

        assert_eq!(diff.node_id, parsed.node_id);
        assert_eq!(diff.base_version, parsed.base_version);
        assert_eq!(diff.new_version, parsed.new_version);
        assert_eq!(diff.ops.len(), parsed.ops.len());
    }

    // ========================================================================
    // from_bytes must reject oversized / op-flooded payloads
    // ========================================================================

    /// A wire-format diff larger than `MAX_DIFF_BYTES` is rejected
    /// before `serde_json::from_slice` is called, so a peer-shipped
    /// balloon JSON cannot pre-allocate arbitrary heap during
    /// parsing. Pre-fix `from_bytes` had no length cap and would
    /// faithfully expand the payload into a `Vec<DiffOp>` regardless
    /// of size.
    #[test]
    fn from_bytes_rejects_payload_over_max_diff_bytes() {
        // Vec of bytes longer than the cap. Content doesn't matter
        // — the size guard runs before any parse work.
        let oversized = vec![b'x'; MAX_DIFF_BYTES + 1];
        assert!(
            CapabilityDiff::from_bytes(&oversized).is_none(),
            "from_bytes must reject inputs larger than MAX_DIFF_BYTES",
        );
    }

    /// A diff parseable but containing more than `MAX_DIFF_OPS` ops
    /// is rejected post-parse. Pre-fix `apply` iterated every op
    /// (each currently a no-op for SetField/UnsetField); a peer
    /// could exhaust CPU by shipping a small JSON expanding to a
    /// large `Vec`. Now `from_bytes` returns `None` and `apply` is
    /// never called.
    #[test]
    fn from_bytes_rejects_diff_with_too_many_ops() {
        // Build a diff with MAX_DIFF_OPS + 1 trivial ops. Use
        // very short tag strings so the JSON encoding stays well
        // under MAX_DIFF_BYTES (otherwise the byte-size guard
        // would fire first and we wouldn't be testing the op-count
        // guard specifically).
        let ops: Vec<DiffOp> = (0..(MAX_DIFF_OPS + 1))
            .map(|i| DiffOp::AddTag(format!("t{}", i % 10)))
            .collect();
        let diff = CapabilityDiff::new(1, 1, 2, ops);
        // Bypass the cap-aware encoders (`to_bytes` / `try_to_bytes`)
        // so we can construct a wire payload that is well-formed JSON
        // but exceeds `MAX_DIFF_OPS`. Going through `to_bytes` here
        // would yield `Vec::new()` (cap-violation suppression) and
        // the test would falsely pass against an empty input.
        let bytes = serde_json::to_vec(&diff).unwrap();

        // Sanity-check: the wire payload fits under the byte cap,
        // so the op-count guard is the discriminator.
        assert!(
            bytes.len() <= MAX_DIFF_BYTES,
            "test setup: encoded diff must stay within byte cap to test op-count guard \
             (got {} bytes, cap is {})",
            bytes.len(),
            MAX_DIFF_BYTES,
        );

        assert!(
            CapabilityDiff::from_bytes(&bytes).is_none(),
            "from_bytes must reject diffs with more than MAX_DIFF_OPS ops",
        );
    }

    // ========================================================================
    // apply_with_version must reject diffs against stale state
    // ========================================================================

    /// `apply_with_version` rejects when the live version doesn't
    /// match `diff.base_version`. Pre-fix `apply` silently accepted
    /// any diff regardless of version, allowing a stale diff (e.g.
    /// `base_version=2` arriving at a v5 receiver) to roll state
    /// back. The new entry point surfaces
    /// [`DiffError::VersionMismatch`].
    #[test]
    fn apply_with_version_rejects_stale_diff() {
        let caps = sample_capability_set();
        // Live state is at v5; the diff is generated against v2.
        let diff = CapabilityDiff::new(1, 2, 3, vec![DiffOp::AddTag("training".into())]);

        let err = DiffEngine::apply_with_version(&caps, 5, &diff, false)
            .expect_err("must reject stale diff");
        assert!(
            matches!(
                err,
                DiffError::VersionMismatch {
                    expected: 2,
                    actual: 5
                }
            ),
            "expected VersionMismatch {{ expected: 2, actual: 5 }}, got {:?}",
            err,
        );
    }

    /// Cubic P1: `apply_with_version` must also reject a diff
    /// whose `new_version` does not advance forward of
    /// `base_version`. A regressing diff (`base=5 → new=3`) or a
    /// stationary diff (`base=5 → new=5`) would otherwise apply
    /// the ops while leaving the receiver's tracked version at
    /// or behind where it started — silent state divergence
    /// across replicas, identical in shape to the silent-rollback
    /// hazard `apply_with_version` was originally introduced to
    /// fix. `validate_chain` catches this in bulk; the single-
    /// call apply path now catches it too.
    #[test]
    fn cubic_p1_apply_with_version_rejects_non_forward_new_version() {
        let caps = sample_capability_set();

        // Regression: base=5, new=3 (rolls forward in apply, but
        // tracked version goes backward).
        let regressing = CapabilityDiff::new(1, 5, 3, vec![DiffOp::AddTag("a".into())]);
        let err = DiffEngine::apply_with_version(&caps, 5, &regressing, false)
            .expect_err("regressing diff must be rejected");
        assert!(
            matches!(err, DiffError::NotApplicable(ref msg) if msg.contains("non-forward")),
            "expected NotApplicable about non-forward diff version, got {:?}",
            err
        );

        // Stationary: base=5, new=5. Same hazard at zero rate.
        let stationary = CapabilityDiff::new(1, 5, 5, vec![DiffOp::AddTag("b".into())]);
        let err = DiffEngine::apply_with_version(&caps, 5, &stationary, false)
            .expect_err("stationary diff must be rejected");
        assert!(
            matches!(err, DiffError::NotApplicable(ref msg) if msg.contains("non-forward")),
            "expected NotApplicable about non-forward diff version, got {:?}",
            err
        );
    }

    /// `apply_with_version` accepts when the live version matches.
    /// Pins the success path so a future overzealous tightening
    /// can't lock out legitimately-aligned diffs.
    #[test]
    fn apply_with_version_accepts_aligned_diff() {
        let caps = sample_capability_set();
        let diff = CapabilityDiff::new(1, 7, 8, vec![DiffOp::AddTag("training".into())]);

        let applied = DiffEngine::apply_with_version(&caps, 7, &diff, false)
            .expect("must accept aligned diff");
        assert!(applied.has_tag("training"));
    }

    /// Future-dated diffs (`diff.base_version` ahead of
    /// `current_version`) must also be rejected — the caller doesn't
    /// have the intermediate state needed for the diff to make
    /// sense, and silent acceptance would leave them with a forked
    /// view of the capability set.
    #[test]
    fn apply_with_version_rejects_future_dated_diff() {
        let caps = sample_capability_set();
        let diff = CapabilityDiff::new(1, 10, 11, vec![DiffOp::AddTag("training".into())]);

        let err = DiffEngine::apply_with_version(&caps, 5, &diff, false)
            .expect_err("must reject future-dated diff");
        assert!(
            matches!(
                err,
                DiffError::VersionMismatch {
                    expected: 10,
                    actual: 5
                }
            ),
            "expected VersionMismatch {{ expected: 10, actual: 5 }}, got {:?}",
            err,
        );
    }

    // ========================================================================
    // SetField/UnsetField are silent no-ops despite documentation
    // ========================================================================

    /// Strict-mode `apply` of a `SetField` diff returns
    /// `DiffError::NotApplicable` rather than silently returning
    /// `Ok(())`. Pre-fix the silent-success behavior let a sender
    /// believe the receiver mutated state (it didn't), so views
    /// diverged silently and `validate_chain` couldn't catch it.
    /// Non-strict mode preserves the historic best-effort no-op
    /// for callers that intentionally tolerate unimplemented
    /// variants.
    #[test]
    fn apply_strict_surfaces_not_applicable_for_set_field() {
        let caps = sample_capability_set();
        let diff = CapabilityDiff::new(
            1,
            1,
            2,
            vec![DiffOp::SetField {
                path: "custom.foo".into(),
                value: serde_json::json!("bar"),
            }],
        );
        let err = DiffEngine::apply(&caps, &diff, true)
            .expect_err("strict apply must surface SetField as NotApplicable");
        assert!(
            matches!(err, DiffError::NotApplicable(_)),
            "expected NotApplicable, got {:?}",
            err,
        );
    }

    #[test]
    fn apply_strict_surfaces_not_applicable_for_unset_field() {
        let caps = sample_capability_set();
        let diff = CapabilityDiff::new(
            1,
            1,
            2,
            vec![DiffOp::UnsetField {
                path: "custom.foo".into(),
            }],
        );
        let err = DiffEngine::apply(&caps, &diff, true)
            .expect_err("strict apply must surface UnsetField as NotApplicable");
        assert!(matches!(err, DiffError::NotApplicable(_)));
    }

    #[test]
    fn apply_non_strict_still_no_ops_set_field() {
        let caps = sample_capability_set();
        let diff = CapabilityDiff::new(
            1,
            1,
            2,
            vec![DiffOp::SetField {
                path: "custom.foo".into(),
                value: serde_json::json!("bar"),
            }],
        );
        let result = DiffEngine::apply(&caps, &diff, false)
            .expect("non-strict apply preserves the historic no-op");
        // No mutation expected — caps unchanged.
        assert_eq!(result.tags, caps.tags);
        assert_eq!(result.views().hardware(), caps.views().hardware());
    }

    // ========================================================================
    // validate_chain must reject diffs with new_version <= base_version
    // ========================================================================

    /// A diff with `base_version=5, new_version=3` is rejected by
    /// `validate_chain`. Pre-fix it was accepted because the chain
    /// loop only checked adjacency (`prev.new_version ==
    /// curr.base_version`), not the within-diff progression.
    #[test]
    fn validate_chain_rejects_within_diff_version_regression() {
        let mut backwards = CapabilityDiff::new(1, 5, 3, vec![DiffOp::AddTag("x".into())]);
        backwards.timestamp_ns = 1_000_000;
        assert!(
            !DiffEngine::validate_chain(&[backwards]),
            "validate_chain must reject new_version < base_version",
        );
    }

    /// Equal versions (`base_version == new_version`) are also
    /// rejected — there's nothing to apply forward.
    #[test]
    fn validate_chain_rejects_equal_base_and_new_version() {
        let stalled = CapabilityDiff::new(1, 5, 5, vec![DiffOp::AddTag("x".into())]);
        assert!(
            !DiffEngine::validate_chain(&[stalled]),
            "validate_chain must reject new_version == base_version",
        );
    }

    /// A multi-diff chain where the last diff individually
    /// regresses must be rejected even when the inter-diff chain
    /// would otherwise check out.
    #[test]
    fn validate_chain_rejects_chain_with_one_regressing_diff() {
        let d1 = CapabilityDiff::new(1, 1, 2, vec![DiffOp::AddTag("a".into())]);
        let mut d2 = CapabilityDiff::new(1, 2, 1, vec![DiffOp::AddTag("b".into())]); // regresses
        d2.timestamp_ns = d1.timestamp_ns.saturating_add(1000);
        assert!(
            !DiffEngine::validate_chain(&[d1, d2]),
            "chain containing a regressing diff must be rejected",
        );
    }

    /// A diff with exactly `MAX_DIFF_OPS` ops is accepted — pins
    /// the boundary so a future tightening that flips `>` to `>=`
    /// can't silently break legitimate large-but-bounded diffs.
    #[test]
    fn from_bytes_accepts_diff_at_exact_max_diff_ops() {
        let ops: Vec<DiffOp> = (0..MAX_DIFF_OPS)
            .map(|i| DiffOp::AddTag(format!("t{}", i % 10)))
            .collect();
        let diff = CapabilityDiff::new(1, 1, 2, ops);
        let bytes = diff
            .try_to_bytes()
            .expect("diff at the exact MAX_DIFF_OPS boundary must encode");
        // Defence-in-depth: if MAX_DIFF_OPS is later raised past
        // what fits in MAX_DIFF_BYTES, this `assert!` fires and the
        // test author can adjust either cap rather than chase a
        // surprising byte-cap failure here.
        assert!(bytes.len() <= MAX_DIFF_BYTES);
        let parsed = CapabilityDiff::from_bytes(&bytes)
            .expect("diff at the exact MAX_DIFF_OPS boundary must be accepted");
        assert_eq!(parsed.ops.len(), MAX_DIFF_OPS);
    }

    // ========================================================================
    // CR-10: try_to_bytes must enforce the same caps from_bytes does
    // ========================================================================

    /// CR-10: a diff with too many ops MUST surface a typed error
    /// from `try_to_bytes`, not silently emit bytes the receiver
    /// will reject. Pre-CR-10 the sender had no cap check —
    /// every receiver's `from_bytes` rejected the diff and the
    /// sender saw "encode succeeded" → silent state divergence.
    #[test]
    fn try_to_bytes_rejects_diff_with_too_many_ops() {
        let ops: Vec<DiffOp> = (0..(MAX_DIFF_OPS + 1))
            .map(|i| DiffOp::AddTag(format!("t{}", i % 10)))
            .collect();
        let diff = CapabilityDiff::new(1, 1, 2, ops);
        let err = diff
            .try_to_bytes()
            .expect_err("over-cap op count must surface DiffSizeError::TooManyOps");
        match err {
            DiffSizeError::TooManyOps { got, cap } => {
                assert_eq!(got, MAX_DIFF_OPS + 1);
                assert_eq!(cap, MAX_DIFF_OPS);
            }
            other => panic!("expected TooManyOps, got {:?}", other),
        }
    }

    /// CR-10: a diff whose serialized form exceeds MAX_DIFF_BYTES
    /// (large per-op payloads, fewer-than-cap op count) must
    /// surface `DiffSizeError::Encoded`. We pack the bytes by
    /// using long tag strings — each op stays under MAX_DIFF_OPS
    /// but the encoded total exceeds MAX_DIFF_BYTES.
    #[test]
    fn try_to_bytes_rejects_diff_over_max_diff_bytes() {
        // Each tag is ~80 bytes when JSON-encoded;
        // `MAX_DIFF_BYTES / 80 + 100` ops definitely overshoots.
        let big_tag = "x".repeat(80);
        let ops_count = (MAX_DIFF_BYTES / 80) + 100;
        // Make sure we don't trip the op-count cap first (the
        // `TooManyOps` check fires before encoding); pin against
        // a configuration that puts us over bytes but under ops.
        let ops_count = ops_count.min(MAX_DIFF_OPS);
        let ops: Vec<DiffOp> = (0..ops_count)
            .map(|_| DiffOp::AddTag(big_tag.clone()))
            .collect();
        let diff = CapabilityDiff::new(1, 1, 2, ops);
        let err = diff
            .try_to_bytes()
            .expect_err("over-cap encoded bytes must surface DiffSizeError::Encoded");
        match err {
            DiffSizeError::Encoded { got, cap } => {
                assert!(got > MAX_DIFF_BYTES, "got {} must exceed cap {}", got, cap);
                assert_eq!(cap, MAX_DIFF_BYTES);
            }
            DiffSizeError::TooManyOps { .. } => {
                // Acceptable — if MAX_DIFF_OPS happens to be the
                // tighter cap for this fixture, the op-count
                // check fires first. Either rejection prevents
                // the silent-success failure mode CR-10 targets.
            }
        }
    }

    /// CR-10: a diff at the exact byte boundary must succeed.
    /// Pin the boundary so a future tightening doesn't silently
    /// break legitimate large-but-bounded diffs.
    #[test]
    fn try_to_bytes_accepts_normal_diff() {
        let diff = CapabilityDiff::new(
            1,
            1,
            2,
            vec![DiffOp::AddTag("training".into()), DiffOp::UpdateMemory(64)],
        );
        let bytes = diff.try_to_bytes().expect("normal-size diff must succeed");
        assert!(bytes.len() <= MAX_DIFF_BYTES);
        // Round-trip — receiver-side `from_bytes` must accept.
        let parsed = CapabilityDiff::from_bytes(&bytes).expect("normal-size diff must round-trip");
        assert_eq!(parsed.ops.len(), 2);
    }

    /// The legacy `to_bytes` returns Vec::new() on cap violation —
    /// indistinguishable from an empty diff. New callers must use
    /// `try_to_bytes`; this test pins the legacy path's
    /// silent-empty behavior so any future change that switches
    /// the failure mode (e.g. to a panic) doesn't go unnoticed.
    #[test]
    #[allow(deprecated)]
    fn to_bytes_returns_empty_when_cap_exceeded() {
        let ops: Vec<DiffOp> = (0..(MAX_DIFF_OPS + 5))
            .map(|i| DiffOp::AddTag(format!("t{}", i)))
            .collect();
        let diff = CapabilityDiff::new(1, 1, 2, ops);
        let bytes = diff.to_bytes();
        assert!(
            bytes.is_empty(),
            "to_bytes must surface Vec::new() on cap violation, got {} bytes",
            bytes.len()
        );
    }

    /// CR-15: pin that `DiffEngine::apply` carries the
    /// `#[deprecated]` attribute. Pre-CR-15 the version-naive
    /// `apply` was the same shape as the post-#125 fix's
    /// `apply_with_version` — only convention separated the two.
    /// Marking `apply` deprecated turns a future caller's misuse
    /// into a compile-time warning instead of a silent rollback
    /// hazard. This source-level tripwire fires loudly the moment
    /// the marker is removed.
    #[test]
    fn cr15_diff_engine_apply_must_be_deprecated() {
        let src = include_str!("diff.rs");
        // Find the `pub fn apply(` definition and assert that the
        // preceding non-blank/non-comment lines include
        // `#[deprecated`.
        let needle = format!("pub fn {}({}", "apply", "");
        let lines: Vec<&str> = src.lines().collect();
        let apply_lineno = lines
            .iter()
            .position(|l| l.contains(&needle))
            .expect("DiffEngine::apply definition must exist (CR-15 guards its deprecation)");

        // Walk backward over attribute / doc-comment lines; the
        // `#[deprecated` token must appear before we hit a blank
        // line (which terminates the attribute block).
        let mut found = false;
        for i in (0..apply_lineno).rev() {
            let line = lines[i].trim();
            if line.is_empty() {
                break;
            }
            if line.starts_with("#[deprecated") {
                found = true;
                break;
            }
        }
        assert!(
            found,
            "CR-15 regression: DiffEngine::apply must carry #[deprecated]; \
             the version-naive entry point should warn callers off the \
             silent-rollback hazard. Definition at diff.rs:{}",
            apply_lineno + 1
        );
    }

    /// `CapabilityDiff::to_bytes` must carry `#[deprecated]` so a
    /// future caller doesn't accidentally emit `Vec::new()` on a
    /// cap violation and assume the encode succeeded. The
    /// preferred entry point is `try_to_bytes`, which surfaces the
    /// failure mode as a typed error.
    #[test]
    fn capability_diff_to_bytes_must_be_deprecated() {
        let src = include_str!("diff.rs");
        let needle = "pub fn to_bytes(";
        let lines: Vec<&str> = src.lines().collect();
        let to_bytes_lineno = lines
            .iter()
            .position(|l| l.contains(needle))
            .expect("CapabilityDiff::to_bytes definition must exist");

        let mut found = false;
        for i in (0..to_bytes_lineno).rev() {
            let line = lines[i].trim();
            if line.is_empty() {
                break;
            }
            if line.starts_with("#[deprecated") {
                found = true;
                break;
            }
        }
        assert!(
            found,
            "CapabilityDiff::to_bytes must carry #[deprecated]: \
             the silent-empty-on-cap-violation behavior is a footgun. \
             Definition at diff.rs:{}",
            to_bytes_lineno + 1
        );
    }

    // ---------- DiffOp::estimated_size per-variant coverage ----------

    #[test]
    fn estimated_size_covers_every_variant() {
        // Each arm has its own arithmetic; a regression in any one
        // would be hard to spot without per-variant pinning. The
        // values here aren't load-bearing — we only require that
        // estimated_size produces a sensible non-zero number that
        // scales with the variant's payload.
        let add_tag = DiffOp::AddTag("abc".into());
        let rm_tag = DiffOp::RemoveTag("abc".into());
        assert_eq!(add_tag.estimated_size(), 8 + 3);
        assert_eq!(rm_tag.estimated_size(), 8 + 3);

        let model = ModelCapability::new("m1", "llama");
        let add_model = DiffOp::AddModel(model.clone());
        assert!(add_model.estimated_size() >= 50 + "m1".len() + "llama".len());
        assert_eq!(DiffOp::RemoveModel("m1".into()).estimated_size(), 8 + 2);
        assert_eq!(
            DiffOp::UpdateModel {
                model_id: "m1".into(),
                tokens_per_sec: Some(50),
                loaded: None,
            }
            .estimated_size(),
            16 + 2,
        );

        let tool = ToolCapability::new("t1", "T1");
        assert!(DiffOp::AddTool(tool).estimated_size() >= 50 + "t1".len() + "T1".len());
        assert_eq!(DiffOp::RemoveTool("t1".into()).estimated_size(), 8 + 2);

        assert_eq!(
            DiffOp::UpdateHardware(HardwareCapabilities::new()).estimated_size(),
            64,
        );
        assert_eq!(DiffOp::UpdateMemory(32).estimated_size(), 8);
        assert_eq!(DiffOp::UpdateNetwork(10).estimated_size(), 8);
        assert_eq!(
            DiffOp::UpdateSoftware(SoftwareCapabilities::new()).estimated_size(),
            128,
        );

        assert_eq!(
            DiffOp::AddRuntime {
                name: "py".into(),
                version: "3.11".into(),
            }
            .estimated_size(),
            12 + 2 + 4,
        );
        assert_eq!(DiffOp::RemoveRuntime("py".into()).estimated_size(), 8 + 2);
        assert_eq!(
            DiffOp::AddFramework {
                name: "pt".into(),
                version: "2.1".into(),
            }
            .estimated_size(),
            12 + 2 + 3,
        );
        assert_eq!(DiffOp::RemoveFramework("pt".into()).estimated_size(), 8 + 2);

        assert_eq!(
            DiffOp::UpdateLimits(ResourceLimits::new()).estimated_size(),
            32,
        );
        assert_eq!(DiffOp::UpdateMaxConcurrent(10).estimated_size(), 8);
        assert_eq!(DiffOp::UpdateRateLimit(60).estimated_size(), 8);

        let set_field = DiffOp::SetField {
            path: "custom.x".into(),
            value: serde_json::json!(42),
        };
        assert!(set_field.estimated_size() >= 16 + "custom.x".len());
        assert_eq!(
            DiffOp::UnsetField {
                path: "custom.x".into(),
            }
            .estimated_size(),
            8 + "custom.x".len(),
        );
    }

    // ---------- diff_limits partial-update detection ----------
    //
    // When only one of `max_concurrent` / `rate_limit_rpm` changes
    // and the rest of `ResourceLimits` is unchanged, `diff_limits`
    // emits the targeted `UpdateMaxConcurrent` / `UpdateRateLimit`
    // shortcut instead of the heavier `UpdateLimits` (full
    // replacement). Existing tests only exercise the fallthrough
    // path; these pin the shortcut branches.

    fn caps_with_limits(l: ResourceLimits) -> CapabilitySet {
        CapabilitySet::new().with_limits(l)
    }

    #[test]
    fn diff_limits_emits_max_concurrent_shortcut_when_only_concurrent_changes() {
        let old = caps_with_limits(ResourceLimits::new().with_max_concurrent(10));
        let new = caps_with_limits(ResourceLimits::new().with_max_concurrent(20));
        let ops = DiffEngine::diff(&old, &new);
        assert!(
            ops.iter()
                .any(|op| matches!(op, DiffOp::UpdateMaxConcurrent(20))),
            "expected UpdateMaxConcurrent(20), got {:?}",
            ops,
        );
        assert!(
            !ops.iter().any(|op| matches!(op, DiffOp::UpdateLimits(_))),
            "shortcut path must not also emit UpdateLimits",
        );
    }

    #[test]
    fn diff_limits_emits_rate_limit_shortcut_when_only_rpm_changes() {
        let old = caps_with_limits(ResourceLimits::new().with_rate_limit(60));
        let new = caps_with_limits(ResourceLimits::new().with_rate_limit(120));
        let ops = DiffEngine::diff(&old, &new);
        assert!(
            ops.iter()
                .any(|op| matches!(op, DiffOp::UpdateRateLimit(120))),
            "expected UpdateRateLimit(120), got {:?}",
            ops,
        );
    }

    #[test]
    fn diff_limits_falls_through_to_full_replacement_when_multiple_fields_change() {
        let old = caps_with_limits(
            ResourceLimits::new()
                .with_max_concurrent(10)
                .with_rate_limit(60),
        );
        let new = caps_with_limits(
            ResourceLimits::new()
                .with_max_concurrent(20)
                .with_rate_limit(120),
        );
        let ops = DiffEngine::diff(&old, &new);
        assert!(
            ops.iter().any(|op| matches!(op, DiffOp::UpdateLimits(_))),
            "multi-field change must take the UpdateLimits fallthrough",
        );
    }

    // ---------- apply branches for non-Add/non-tag operations ----------

    #[test]
    fn apply_update_hardware_memory_network() {
        let base = sample_capability_set();
        let mut hw = base.views().hardware().clone();
        hw.memory_gb = 128;
        let diff = CapabilityDiff::new(1, 1, 2, vec![DiffOp::UpdateHardware(hw.clone())]);
        let after = DiffEngine::apply(&base, &diff, true).unwrap();
        assert_eq!(after.views().hardware().memory_gb, 128);

        let diff = CapabilityDiff::new(1, 1, 2, vec![DiffOp::UpdateMemory(256)]);
        let after = DiffEngine::apply(&base, &diff, true).unwrap();
        assert_eq!(after.views().hardware().memory_gb, 256);

        let diff = CapabilityDiff::new(1, 1, 2, vec![DiffOp::UpdateNetwork(25)]);
        let after = DiffEngine::apply(&base, &diff, true).unwrap();
        assert_eq!(after.views().hardware().network_gbps, 25);
    }

    #[test]
    fn apply_update_software_and_runtime_framework() {
        let base = sample_capability_set();
        let new_sw = SoftwareCapabilities::new().with_os("linux", "6.5");
        let diff = CapabilityDiff::new(1, 1, 2, vec![DiffOp::UpdateSoftware(new_sw)]);
        let after = DiffEngine::apply(&base, &diff, true).unwrap();
        assert_eq!(after.views().software().os_version, "6.5");

        let diff = CapabilityDiff::new(
            1,
            1,
            2,
            vec![DiffOp::AddRuntime {
                name: "node".into(),
                version: "20".into(),
            }],
        );
        let after = DiffEngine::apply(&base, &diff, true).unwrap();
        assert!(after
            .views()
            .software()
            .runtimes
            .iter()
            .any(|(n, _)| n == "node"));

        let diff = CapabilityDiff::new(
            1,
            1,
            2,
            vec![DiffOp::AddFramework {
                name: "jax".into(),
                version: "0.4".into(),
            }],
        );
        let after = DiffEngine::apply(&base, &diff, true).unwrap();
        assert!(after
            .views()
            .software()
            .frameworks
            .iter()
            .any(|(n, _)| n == "jax"));
    }

    #[test]
    fn apply_update_limits_and_shortcuts() {
        let base = sample_capability_set();

        let new_limits = ResourceLimits::new()
            .with_max_concurrent(99)
            .with_rate_limit(50);
        let diff = CapabilityDiff::new(1, 1, 2, vec![DiffOp::UpdateLimits(new_limits)]);
        let after = DiffEngine::apply(&base, &diff, true).unwrap();
        assert_eq!(after.views().resource_limits().max_concurrent_requests, 99);

        let diff = CapabilityDiff::new(1, 1, 2, vec![DiffOp::UpdateMaxConcurrent(42)]);
        let after = DiffEngine::apply(&base, &diff, true).unwrap();
        assert_eq!(after.views().resource_limits().max_concurrent_requests, 42);

        let diff = CapabilityDiff::new(1, 1, 2, vec![DiffOp::UpdateRateLimit(999)]);
        let after = DiffEngine::apply(&base, &diff, true).unwrap();
        assert_eq!(after.views().resource_limits().rate_limit_rpm, 999);
    }

    // ---------- Strict vs non-strict for Remove ops on missing items ----------

    #[test]
    fn remove_tool_missing_errors_in_strict_mode_noops_otherwise() {
        let base = sample_capability_set();
        let diff = CapabilityDiff::new(1, 1, 2, vec![DiffOp::RemoveTool("nonexistent".into())]);
        // strict=true: surface NotFound rather than silently ignoring.
        assert!(matches!(
            DiffEngine::apply(&base, &diff, true),
            Err(DiffError::ToolNotFound(_))
        ));
        // strict=false: best-effort no-op, returns the unchanged set.
        let after = DiffEngine::apply(&base, &diff, false).unwrap();
        assert_eq!(after.views().tools().len(), base.views().tools().len());
    }

    #[test]
    fn remove_runtime_missing_errors_in_strict_mode_noops_otherwise() {
        let base = sample_capability_set();
        let diff = CapabilityDiff::new(1, 1, 2, vec![DiffOp::RemoveRuntime("nonexistent".into())]);
        assert!(matches!(
            DiffEngine::apply(&base, &diff, true),
            Err(DiffError::RuntimeNotFound(_))
        ));
        assert!(DiffEngine::apply(&base, &diff, false).is_ok());
    }

    #[test]
    fn remove_framework_missing_errors_in_strict_mode_noops_otherwise() {
        let base = sample_capability_set();
        let diff =
            CapabilityDiff::new(1, 1, 2, vec![DiffOp::RemoveFramework("nonexistent".into())]);
        assert!(matches!(
            DiffEngine::apply(&base, &diff, true),
            Err(DiffError::FrameworkNotFound(_))
        ));
        assert!(DiffEngine::apply(&base, &diff, false).is_ok());
    }
}
