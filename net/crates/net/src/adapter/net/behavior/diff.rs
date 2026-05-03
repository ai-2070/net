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

        // Diff tags
        Self::diff_tags(&old.tags, &new.tags, &mut ops);

        // Diff models
        Self::diff_models(&old.models, &new.models, &mut ops);

        // Diff tools
        Self::diff_tools(&old.tools, &new.tools, &mut ops);

        // Diff hardware (only if changed)
        if old.hardware != new.hardware {
            // Check for partial updates
            if old.hardware.memory_mb != new.hardware.memory_mb
                && old.hardware.cpu_cores == new.hardware.cpu_cores
                && old.hardware.gpu == new.hardware.gpu
                && old.hardware.storage_mb == new.hardware.storage_mb
                && old.hardware.network_mbps == new.hardware.network_mbps
            {
                ops.push(DiffOp::UpdateMemory(new.hardware.memory_mb));
            } else if old.hardware.network_mbps != new.hardware.network_mbps
                && old.hardware.cpu_cores == new.hardware.cpu_cores
                && old.hardware.gpu == new.hardware.gpu
                && old.hardware.memory_mb == new.hardware.memory_mb
                && old.hardware.storage_mb == new.hardware.storage_mb
            {
                ops.push(DiffOp::UpdateNetwork(new.hardware.network_mbps));
            } else {
                ops.push(DiffOp::UpdateHardware(new.hardware.clone()));
            }
        }

        // Diff software
        if old.software != new.software {
            Self::diff_software(&old.software, &new.software, &mut ops);
        }

        // Diff limits (only if changed)
        if old.limits != new.limits {
            // Check for partial updates
            if old.limits.max_concurrent_requests != new.limits.max_concurrent_requests
                && old.limits.max_tokens_per_request == new.limits.max_tokens_per_request
                && old.limits.rate_limit_rpm == new.limits.rate_limit_rpm
                && old.limits.max_batch_size == new.limits.max_batch_size
            {
                ops.push(DiffOp::UpdateMaxConcurrent(
                    new.limits.max_concurrent_requests,
                ));
            } else if old.limits.rate_limit_rpm != new.limits.rate_limit_rpm
                && old.limits.max_concurrent_requests == new.limits.max_concurrent_requests
                && old.limits.max_tokens_per_request == new.limits.max_tokens_per_request
                && old.limits.max_batch_size == new.limits.max_batch_size
            {
                ops.push(DiffOp::UpdateRateLimit(new.limits.rate_limit_rpm));
            } else {
                ops.push(DiffOp::UpdateLimits(new.limits.clone()));
            }
        }

        ops
    }

    /// Diff tags between old and new
    fn diff_tags(old: &[String], new: &[String], ops: &mut Vec<DiffOp>) {
        let old_set: HashSet<&str> = old.iter().map(|s| s.as_str()).collect();
        let new_set: HashSet<&str> = new.iter().map(|s| s.as_str()).collect();

        // Removed tags
        for tag in old_set.difference(&new_set) {
            ops.push(DiffOp::RemoveTag((*tag).to_string()));
        }

        // Added tags
        for tag in new_set.difference(&old_set) {
            ops.push(DiffOp::AddTag((*tag).to_string()));
        }
    }

    /// Diff models between old and new
    fn diff_models(old: &[ModelCapability], new: &[ModelCapability], ops: &mut Vec<DiffOp>) {
        let old_map: std::collections::HashMap<&str, &ModelCapability> =
            old.iter().map(|m| (m.model_id.as_str(), m)).collect();
        let new_map: std::collections::HashMap<&str, &ModelCapability> =
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
        let old_map: std::collections::HashMap<&str, &ToolCapability> =
            old.iter().map(|t| (t.tool_id.as_str(), t)).collect();
        let new_map: std::collections::HashMap<&str, &ToolCapability> =
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

        // Diff runtimes
        let old_runtimes: std::collections::HashMap<&str, &str> = old
            .runtimes
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        let new_runtimes: std::collections::HashMap<&str, &str> = new
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

        // Diff frameworks
        let old_frameworks: std::collections::HashMap<&str, &str> = old
            .frameworks
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        let new_frameworks: std::collections::HashMap<&str, &str> = new
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
        since = "0.9.0",
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
        match op {
            DiffOp::AddTag(tag) => {
                if !caps.tags.contains(tag) {
                    caps.tags.push(tag.clone());
                }
            }
            DiffOp::RemoveTag(tag) => {
                if let Some(pos) = caps.tags.iter().position(|t| t == tag) {
                    caps.tags.remove(pos);
                } else if strict {
                    return Err(DiffError::TagNotFound(tag.clone()));
                }
            }
            DiffOp::AddModel(model) => {
                // Remove existing model with same ID if present
                caps.models.retain(|m| m.model_id != model.model_id);
                caps.models.push(model.clone());
            }
            DiffOp::RemoveModel(model_id) => {
                let before = caps.models.len();
                caps.models.retain(|m| m.model_id != *model_id);
                if strict && caps.models.len() == before {
                    return Err(DiffError::ModelNotFound(model_id.clone()));
                }
            }
            DiffOp::UpdateModel {
                model_id,
                tokens_per_sec,
                loaded,
            } => {
                if let Some(model) = caps.models.iter_mut().find(|m| m.model_id == *model_id) {
                    if let Some(tps) = tokens_per_sec {
                        model.tokens_per_sec = *tps;
                    }
                    if let Some(l) = loaded {
                        model.loaded = *l;
                    }
                } else if strict {
                    return Err(DiffError::ModelNotFound(model_id.clone()));
                }
            }
            DiffOp::AddTool(tool) => {
                // Remove existing tool with same ID if present
                caps.tools.retain(|t| t.tool_id != tool.tool_id);
                caps.tools.push(tool.clone());
            }
            DiffOp::RemoveTool(tool_id) => {
                let before = caps.tools.len();
                caps.tools.retain(|t| t.tool_id != *tool_id);
                if strict && caps.tools.len() == before {
                    return Err(DiffError::ToolNotFound(tool_id.clone()));
                }
            }
            DiffOp::UpdateHardware(hw) => {
                caps.hardware = hw.clone();
            }
            DiffOp::UpdateMemory(mem) => {
                caps.hardware.memory_mb = *mem;
            }
            DiffOp::UpdateNetwork(net) => {
                caps.hardware.network_mbps = *net;
            }
            DiffOp::UpdateSoftware(sw) => {
                caps.software = sw.clone();
            }
            DiffOp::AddRuntime { name, version } => {
                // Remove existing runtime with same name
                caps.software.runtimes.retain(|(n, _)| n != name);
                caps.software.runtimes.push((name.clone(), version.clone()));
            }
            DiffOp::RemoveRuntime(name) => {
                let before = caps.software.runtimes.len();
                caps.software.runtimes.retain(|(n, _)| n != name);
                if strict && caps.software.runtimes.len() == before {
                    return Err(DiffError::RuntimeNotFound(name.clone()));
                }
            }
            DiffOp::AddFramework { name, version } => {
                // Remove existing framework with same name
                caps.software.frameworks.retain(|(n, _)| n != name);
                caps.software
                    .frameworks
                    .push((name.clone(), version.clone()));
            }
            DiffOp::RemoveFramework(name) => {
                let before = caps.software.frameworks.len();
                caps.software.frameworks.retain(|(n, _)| n != name);
                if strict && caps.software.frameworks.len() == before {
                    return Err(DiffError::FrameworkNotFound(name.clone()));
                }
            }
            DiffOp::UpdateLimits(limits) => {
                caps.limits = limits.clone();
            }
            DiffOp::UpdateMaxConcurrent(max) => {
                caps.limits.max_concurrent_requests = *max;
            }
            DiffOp::UpdateRateLimit(rpm) => {
                caps.limits.rate_limit_rpm = *rpm;
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
        let gpu = GpuInfo::new(GpuVendor::Nvidia, "RTX 4090", 24576);
        let hardware = HardwareCapabilities::new()
            .with_cpu(16, 32)
            .with_memory(65536)
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
        new.tags.push("training".into());

        let ops = DiffEngine::diff(&old, &new);
        assert_eq!(ops.len(), 1);
        assert!(matches!(&ops[0], DiffOp::AddTag(t) if t == "training"));
    }

    #[test]
    fn test_diff_remove_tag() {
        let old = sample_capability_set();
        let mut new = old.clone();
        new.tags.retain(|t| t != "inference");

        let ops = DiffEngine::diff(&old, &new);
        assert_eq!(ops.len(), 1);
        assert!(matches!(&ops[0], DiffOp::RemoveTag(t) if t == "inference"));
    }

    #[test]
    fn test_diff_update_model_loaded() {
        let old = sample_capability_set();
        let mut new = old.clone();
        new.models[0].loaded = false;

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
        let mut new = old.clone();
        new.models.push(
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
        new.hardware.memory_mb = 131072;

        let ops = DiffEngine::diff(&old, &new);
        assert_eq!(ops.len(), 1);
        assert!(matches!(&ops[0], DiffOp::UpdateMemory(131072)));
    }

    #[test]
    fn test_apply_diff() {
        let old = sample_capability_set();

        let diff = CapabilityDiff::new(
            1,
            1,
            2,
            vec![
                DiffOp::AddTag("training".into()),
                DiffOp::UpdateMemory(131072),
            ],
        );

        let new = DiffEngine::apply(&old, &diff, true).unwrap();

        assert!(new.has_tag("training"));
        assert_eq!(new.hardware.memory_mb, 131072);
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
        let diff3 = CapabilityDiff::new(1, 3, 4, vec![DiffOp::UpdateMemory(131072)]);

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

        // Make several changes
        new.tags.push("training".into());
        new.tags.retain(|t| t != "inference");
        new.models[0].loaded = false;
        new.models[0].tokens_per_sec = 100;
        new.hardware.memory_mb = 131072;
        new.models.push(
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
        assert_eq!(applied.hardware.memory_mb, 131072);
        assert_eq!(applied.models.len(), 2);
        assert!(
            !applied
                .models
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
            vec![DiffOp::AddTag("test".into()), DiffOp::UpdateMemory(65536)],
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
        assert_eq!(result.hardware, caps.hardware);
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
            vec![
                DiffOp::AddTag("training".into()),
                DiffOp::UpdateMemory(65536),
            ],
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
}
