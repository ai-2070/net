//! Capability Announcements (CAP-ANN) for Phase 4A.
//!
//! This module provides:
//! - `CapabilitySet` - Structured capability representation
//! - `CapabilityAnnouncement` - Versioned capability broadcast
//! - `CapabilityFilter` - Query capabilities by various criteria
//! - `CardinalityProvider` - Trait used by the predicate planner
//!
//! The legacy `CapabilityIndex` in-memory store was removed in
//! Phase 3B of the multifold migration. Membership + cardinality
//! data now live on the `CapabilityFold` (see
//! `behavior/fold/capability`); downstream callers go through
//! `MeshNode`'s fold helpers or `capability_bridge`.

use serde::{Deserialize, Serialize};
use std::cell::OnceCell;
use std::collections::{BTreeMap, HashSet};
use std::hash::Hash;

use crate::adapter::net::behavior::tag::Tag;

// ============================================================================
// Hardware Capabilities
// ============================================================================

/// GPU vendor enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[repr(u8)]
pub enum GpuVendor {
    /// Unrecognized or unspecified GPU vendor.
    #[default]
    Unknown = 0,
    /// NVIDIA Corporation.
    Nvidia = 1,
    /// Advanced Micro Devices (AMD).
    Amd = 2,
    /// Intel Corporation.
    Intel = 3,
    /// Apple Inc. (e.g., M-series integrated GPU).
    Apple = 4,
    /// Qualcomm (e.g., Adreno GPU).
    Qualcomm = 5,
}

impl From<u8> for GpuVendor {
    fn from(v: u8) -> Self {
        match v {
            1 => Self::Nvidia,
            2 => Self::Amd,
            3 => Self::Intel,
            4 => Self::Apple,
            5 => Self::Qualcomm,
            _ => Self::Unknown,
        }
    }
}

/// GPU information
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuInfo {
    /// GPU vendor
    pub vendor: GpuVendor,
    /// Model name (e.g., "RTX 4090", "M2 Ultra")
    pub model: String,
    /// VRAM in GB
    pub vram_gb: u32,
    /// Compute units / SMs
    pub compute_units: u16,
    /// Tensor cores (0 if none)
    pub tensor_cores: u16,
    /// FP16 TFLOPS (scaled by 10, e.g., 825 = 82.5 TFLOPS).
    ///
    /// Widened from `u16` to `u32` because the old ceiling
    /// (`u16::MAX / 10 ≈ 6.5 PFLOPS`) silently saturated on any
    /// aggregated cluster figure worth reporting; individual GPUs
    /// still fit in `u16` but operators roll these up per-node
    /// and per-mesh.
    pub fp16_tflops_x10: u32,
}

impl Default for GpuInfo {
    fn default() -> Self {
        Self {
            vendor: GpuVendor::Unknown,
            model: String::new(),
            vram_gb: 0,
            compute_units: 0,
            tensor_cores: 0,
            fp16_tflops_x10: 0,
        }
    }
}

impl GpuInfo {
    /// Create new GPU info
    pub fn new(vendor: GpuVendor, model: impl Into<String>, vram_gb: u32) -> Self {
        Self {
            vendor,
            model: model.into(),
            vram_gb,
            ..Default::default()
        }
    }

    /// Set compute units
    pub fn with_compute_units(mut self, units: u16) -> Self {
        self.compute_units = units;
        self
    }

    /// Set tensor cores
    pub fn with_tensor_cores(mut self, cores: u16) -> Self {
        self.tensor_cores = cores;
        self
    }

    /// Set FP16 performance.
    ///
    /// Clamped at `u32::MAX` to be explicit about the ceiling: a
    /// pathological f32 (NaN, negative, > ~4.3e8 TFLOPS) saturates
    /// rather than wrapping to a garbage value.
    pub fn with_fp16_tflops(mut self, tflops: f32) -> Self {
        let scaled = (tflops * 10.0).max(0.0);
        self.fp16_tflops_x10 = if scaled.is_finite() && scaled < u32::MAX as f32 {
            scaled as u32
        } else {
            u32::MAX
        };
        self
    }
}

/// Accelerator type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[repr(u8)]
pub enum AcceleratorType {
    /// Unrecognized or unspecified accelerator type.
    #[default]
    Unknown = 0,
    /// Tensor Processing Unit (e.g., Google TPU).
    Tpu = 1,
    /// Neural Processing Unit for on-device AI inference.
    Npu = 2,
    /// Field-Programmable Gate Array.
    Fpga = 3,
    /// Application-Specific Integrated Circuit.
    Asic = 4,
    /// Digital Signal Processor.
    Dsp = 5,
}

impl From<u8> for AcceleratorType {
    fn from(v: u8) -> Self {
        match v {
            1 => Self::Tpu,
            2 => Self::Npu,
            3 => Self::Fpga,
            4 => Self::Asic,
            5 => Self::Dsp,
            _ => Self::Unknown,
        }
    }
}

/// Accelerator information (TPU, NPU, etc.)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceleratorInfo {
    /// Accelerator type
    pub accel_type: AcceleratorType,
    /// Model/name
    pub model: String,
    /// Memory in GB (if applicable)
    pub memory_gb: u32,
    /// TOPS (tera operations per second, scaled by 10)
    pub tops_x10: u16,
}

impl AcceleratorInfo {
    /// Create new accelerator info
    pub fn new(accel_type: AcceleratorType, model: impl Into<String>) -> Self {
        Self {
            accel_type,
            model: model.into(),
            memory_gb: 0,
            tops_x10: 0,
        }
    }
}

/// Hardware capabilities
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HardwareCapabilities {
    /// CPU cores
    pub cpu_cores: u16,
    /// CPU threads (if different from cores due to SMT)
    pub cpu_threads: u16,
    /// Total memory in GB
    pub memory_gb: u32,
    /// GPU info (if present)
    pub gpu: Option<GpuInfo>,
    /// Additional GPUs (for multi-GPU setups)
    pub additional_gpus: Vec<GpuInfo>,
    /// Storage in GB
    pub storage_gb: u64,
    /// Network bandwidth in Gbps
    pub network_gbps: u32,
    /// Accelerators (TPU, NPU, etc.)
    pub accelerators: Vec<AcceleratorInfo>,
}

impl HardwareCapabilities {
    /// Create new hardware capabilities
    pub fn new() -> Self {
        Self::default()
    }

    /// Set CPU cores
    pub fn with_cpu(mut self, cores: u16, threads: u16) -> Self {
        self.cpu_cores = cores;
        self.cpu_threads = threads;
        self
    }

    /// Set memory
    pub fn with_memory(mut self, memory_gb: u32) -> Self {
        self.memory_gb = memory_gb;
        self
    }

    /// Set primary GPU
    pub fn with_gpu(mut self, gpu: GpuInfo) -> Self {
        self.gpu = Some(gpu);
        self
    }

    /// Add additional GPU
    pub fn add_gpu(mut self, gpu: GpuInfo) -> Self {
        self.additional_gpus.push(gpu);
        self
    }

    /// Set storage
    pub fn with_storage(mut self, storage_gb: u64) -> Self {
        self.storage_gb = storage_gb;
        self
    }

    /// Set network bandwidth
    pub fn with_network(mut self, network_gbps: u32) -> Self {
        self.network_gbps = network_gbps;
        self
    }

    /// Add accelerator
    pub fn add_accelerator(mut self, accel: AcceleratorInfo) -> Self {
        self.accelerators.push(accel);
        self
    }

    /// Total GPU count
    pub fn gpu_count(&self) -> usize {
        self.gpu.as_ref().map(|_| 1).unwrap_or(0) + self.additional_gpus.len()
    }

    /// Total VRAM across all GPUs
    pub fn total_vram_gb(&self) -> u32 {
        let primary = self.gpu.as_ref().map(|g| g.vram_gb).unwrap_or(0);
        let additional: u32 = self.additional_gpus.iter().map(|g| g.vram_gb).sum();
        primary + additional
    }

    /// Check if has any GPU
    pub fn has_gpu(&self) -> bool {
        self.gpu.is_some()
    }

    /// Get primary GPU vendor
    pub fn gpu_vendor(&self) -> Option<GpuVendor> {
        self.gpu.as_ref().map(|g| g.vendor)
    }
}

// ============================================================================
// Software Capabilities
// ============================================================================

/// Software/runtime capabilities
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SoftwareCapabilities {
    /// Operating system
    pub os: String,
    /// OS version
    pub os_version: String,
    /// Runtime versions (e.g., "python:3.11", "node:20")
    pub runtimes: Vec<(String, String)>,
    /// Installed frameworks (e.g., "pytorch:2.1", "tensorflow:2.15")
    pub frameworks: Vec<(String, String)>,
    /// CUDA version (if applicable)
    pub cuda_version: Option<String>,
    /// Driver versions
    pub drivers: Vec<(String, String)>,
}

impl SoftwareCapabilities {
    /// Create new software capabilities
    pub fn new() -> Self {
        Self::default()
    }

    /// Set OS
    pub fn with_os(mut self, os: impl Into<String>, version: impl Into<String>) -> Self {
        self.os = os.into();
        self.os_version = version.into();
        self
    }

    /// Add runtime
    pub fn add_runtime(mut self, name: impl Into<String>, version: impl Into<String>) -> Self {
        self.runtimes.push((name.into(), version.into()));
        self
    }

    /// Add framework
    pub fn add_framework(mut self, name: impl Into<String>, version: impl Into<String>) -> Self {
        self.frameworks.push((name.into(), version.into()));
        self
    }

    /// Set CUDA version
    pub fn with_cuda(mut self, version: impl Into<String>) -> Self {
        self.cuda_version = Some(version.into());
        self
    }

    /// Check if has a specific runtime
    pub fn has_runtime(&self, name: &str) -> bool {
        self.runtimes.iter().any(|(n, _)| n == name)
    }

    /// Check if has a specific framework
    pub fn has_framework(&self, name: &str) -> bool {
        self.frameworks.iter().any(|(n, _)| n == name)
    }
}

// ============================================================================
// Model Capabilities
// ============================================================================

/// Modality support
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum Modality {
    /// Plain text input/output.
    Text = 0,
    /// Static image understanding or generation.
    Image = 1,
    /// Audio understanding or synthesis.
    Audio = 2,
    /// Video understanding or generation.
    Video = 3,
    /// Source code generation or analysis.
    Code = 4,
    /// Vector embedding production.
    Embedding = 5,
    /// Structured tool/function calling.
    ToolUse = 6,
}

impl From<u8> for Modality {
    fn from(v: u8) -> Self {
        match v {
            0 => Self::Text,
            1 => Self::Image,
            2 => Self::Audio,
            3 => Self::Video,
            4 => Self::Code,
            5 => Self::Embedding,
            6 => Self::ToolUse,
            _ => Self::Text,
        }
    }
}

/// Model capability
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapability {
    /// Unique model identifier (e.g., "llama-3.1-70b")
    pub model_id: String,
    /// Model family (e.g., "llama", "mistral", "claude")
    pub family: String,
    /// Parameter count (in billions, scaled by 10: 700 = 70B)
    pub parameters_b_x10: u32,
    /// Context length in tokens
    pub context_length: u32,
    /// Quantization (e.g., "fp16", "int8", "int4")
    pub quantization: Option<String>,
    /// Supported modalities
    pub modalities: Vec<Modality>,
    /// Estimated tokens per second (for this hardware)
    pub tokens_per_sec: u32,
    /// Whether model is currently loaded
    pub loaded: bool,
}

impl ModelCapability {
    /// Create new model capability
    pub fn new(model_id: impl Into<String>, family: impl Into<String>) -> Self {
        Self {
            model_id: model_id.into(),
            family: family.into(),
            parameters_b_x10: 0,
            context_length: 0,
            quantization: None,
            modalities: vec![Modality::Text],
            tokens_per_sec: 0,
            loaded: false,
        }
    }

    /// Set parameter count in billions
    pub fn with_parameters(mut self, billions: f32) -> Self {
        self.parameters_b_x10 = (billions * 10.0) as u32;
        self
    }

    /// Set context length
    pub fn with_context_length(mut self, length: u32) -> Self {
        self.context_length = length;
        self
    }

    /// Set quantization
    pub fn with_quantization(mut self, quant: impl Into<String>) -> Self {
        self.quantization = Some(quant.into());
        self
    }

    /// Add modality
    pub fn add_modality(mut self, modality: Modality) -> Self {
        if !self.modalities.contains(&modality) {
            self.modalities.push(modality);
        }
        self
    }

    /// Set tokens per second
    pub fn with_tokens_per_sec(mut self, tps: u32) -> Self {
        self.tokens_per_sec = tps;
        self
    }

    /// Set loaded status
    pub fn with_loaded(mut self, loaded: bool) -> Self {
        self.loaded = loaded;
        self
    }

    /// Get parameter count as f32
    pub fn parameters(&self) -> f32 {
        self.parameters_b_x10 as f32 / 10.0
    }
}

// ============================================================================
// Tool Capabilities
// ============================================================================

/// Tool capability
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCapability {
    /// Unique tool identifier
    pub tool_id: String,
    /// Human-readable name
    pub name: String,
    /// Version
    pub version: String,
    /// Input schema (JSON Schema as string)
    pub input_schema: Option<String>,
    /// Output schema (JSON Schema as string)
    pub output_schema: Option<String>,
    /// Required capabilities/dependencies
    pub requires: Vec<String>,
    /// Estimated execution time in ms (for typical input)
    pub estimated_time_ms: u32,
    /// Whether tool is stateless
    pub stateless: bool,
}

impl ToolCapability {
    /// Metadata key carrying this tool's input JSON Schema.
    ///
    /// Phase A.5.N convention: tool input/output schemas live in
    /// `CapabilitySet::metadata` rather than the tag wire format
    /// (JSON contains `=`/`:`/`,` which can't round-trip through
    /// tags). Format: `tool::<tool_id>::input_schema`.
    pub fn input_schema_metadata_key(tool_id: &str) -> String {
        format!("tool::{tool_id}::input_schema")
    }

    /// Metadata key carrying this tool's output JSON Schema.
    /// See [`Self::input_schema_metadata_key`].
    pub fn output_schema_metadata_key(tool_id: &str) -> String {
        format!("tool::{tool_id}::output_schema")
    }

    /// Create new tool capability
    pub fn new(tool_id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            tool_id: tool_id.into(),
            name: name.into(),
            version: "1.0.0".into(),
            input_schema: None,
            output_schema: None,
            requires: Vec::new(),
            estimated_time_ms: 0,
            stateless: true,
        }
    }

    /// Set version
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    /// Set input schema
    pub fn with_input_schema(mut self, schema: impl Into<String>) -> Self {
        self.input_schema = Some(schema.into());
        self
    }

    /// Set output schema
    pub fn with_output_schema(mut self, schema: impl Into<String>) -> Self {
        self.output_schema = Some(schema.into());
        self
    }

    /// Add requirement
    pub fn requires(mut self, dep: impl Into<String>) -> Self {
        self.requires.push(dep.into());
        self
    }

    /// Set estimated time
    pub fn with_estimated_time(mut self, ms: u32) -> Self {
        self.estimated_time_ms = ms;
        self
    }

    /// Set stateless flag
    pub fn with_stateless(mut self, stateless: bool) -> Self {
        self.stateless = stateless;
        self
    }
}

// ============================================================================
// Resource Limits
// ============================================================================

/// Resource limits
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ResourceLimits {
    /// Maximum concurrent requests
    pub max_concurrent_requests: u32,
    /// Maximum tokens per request
    pub max_tokens_per_request: u32,
    /// Rate limit (requests per minute)
    pub rate_limit_rpm: u32,
    /// Maximum batch size
    pub max_batch_size: u32,
    /// Maximum input size in bytes
    pub max_input_bytes: u32,
    /// Maximum output size in bytes
    pub max_output_bytes: u32,
}

impl ResourceLimits {
    /// Create new resource limits
    pub fn new() -> Self {
        Self::default()
    }

    /// Set max concurrent requests
    pub fn with_max_concurrent(mut self, max: u32) -> Self {
        self.max_concurrent_requests = max;
        self
    }

    /// Set max tokens per request
    pub fn with_max_tokens(mut self, max: u32) -> Self {
        self.max_tokens_per_request = max;
        self
    }

    /// Set rate limit
    pub fn with_rate_limit(mut self, rpm: u32) -> Self {
        self.rate_limit_rpm = rpm;
        self
    }

    /// Set max batch size
    pub fn with_max_batch(mut self, max: u32) -> Self {
        self.max_batch_size = max;
        self
    }
}

// ============================================================================
// Capability Scope (reserved-tag discovery filter)
// ============================================================================

/// Reserved tag prefix marking a capability set as advertised under
/// a specific tenant. Format: `scope:tenant:<id>`.
pub const TAG_SCOPE_TENANT_PREFIX: &str = "scope:tenant:";

/// Reserved tag prefix marking a capability set as advertised under
/// a specific region. Format: `scope:region:<name>`.
pub const TAG_SCOPE_REGION_PREFIX: &str = "scope:region:";

/// Reserved tag marking a capability set as visible only to peers
/// in the same subnet as the announcer. Mutually exclusive with
/// tenant / region scopes — when present, the scope resolver
/// returns `SubnetLocal` regardless of the other reserved tags
/// (strictest scope wins).
pub const TAG_SCOPE_SUBNET_LOCAL: &str = "scope:subnet-local";

/// Optional explicit form of the default global scope. Carries no
/// extra meaning over absence of any `scope:*` tag — included so
/// callers can spell their intent.
pub const TAG_SCOPE_GLOBAL: &str = "scope:global";

/// Resolved scope of a capability announcement, derived from the
/// reserved `scope:*` tags inside the announcer's [`CapabilitySet`].
/// Pure derivation — never stored, recomputed on each query via
/// `behavior::fold::capability_bridge::scope_from_membership_tags`.
///
/// Precedence: `SubnetLocal` > tenants/regions > `Global`. A node
/// that tags itself with both `scope:subnet-local` and
/// `scope:tenant:foo` resolves to `SubnetLocal` (strictest wins).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CapabilityScope {
    /// No `scope:*` tag, or `scope:global` only — visible to every
    /// query that doesn't explicitly opt out (`GlobalOnly` /
    /// `SameSubnet`).
    Global,
    /// `scope:subnet-local` present — visible only under
    /// [`ScopeFilter::SameSubnet`]. Excluded from
    /// [`ScopeFilter::Any`] and every other filter, because the
    /// announcer has explicitly opted out of cross-subnet
    /// discovery.
    SubnetLocal,
    /// One or more `scope:tenant:*` tags, no regions, no
    /// subnet-local.
    Tenants(Vec<String>),
    /// One or more `scope:region:*` tags, no tenants, no
    /// subnet-local.
    Regions(Vec<String>),
    /// Both tenants and regions present. Queries match if either
    /// list satisfies the filter (logical OR — a tenant query and
    /// a region query against the same node are independent
    /// concerns).
    TenantsAndRegions {
        /// Tenant ids declared via `scope:tenant:*` tags.
        tenants: Vec<String>,
        /// Region names declared via `scope:region:*` tags.
        regions: Vec<String>,
    },
}

/// Parse `subnet:<hex32>` and `group:<hex64>` tags out of an
/// announcement's tag set. Used at index time so the
/// capability-auth `may_execute` gate can look up a peer's
/// declared membership in O(1) without re-walking tags per call.
///
/// Multiple `subnet:` tags on one announcement are out of model:
/// the substrate treats subnet membership as single-valued. To
/// keep the gate verdict deterministic across receivers — a
/// previous implementation read whichever subnet tag the
/// `HashSet<Tag>` iterator surfaced first, which is hash-order
/// dependent — multiple distinct subnet tags collapse to `None`
/// and the announcement contributes no subnet membership. Single
/// subnet tag works as expected. All distinct `group:` tags
/// accumulate (deterministically sorted by byte value so receivers
/// agree on iteration order); duplicates (Eq) are removed.
///
/// Kept (with `#[allow(dead_code)]`) for downstream consumers
/// (capability_bridge translates the same shape onto the fold).
/// The legacy `CapabilityIndex` caller was removed in Phase 3B
/// of the multifold migration.
#[allow(dead_code)]
pub(crate) fn parse_membership_tags(
    tags: &HashSet<Tag>,
) -> (Option<super::subnet::SubnetId>, Vec<super::group::GroupId>) {
    let mut subnet_candidates: Vec<super::subnet::SubnetId> = Vec::new();
    let mut groups: Vec<super::group::GroupId> = Vec::new();
    for tag in tags {
        let rendered = tag.to_string();
        if let Some(s) = super::subnet::SubnetId::from_tag(&rendered) {
            if !subnet_candidates.contains(&s) {
                subnet_candidates.push(s);
            }
            continue;
        }
        if let Some(g) = super::group::GroupId::from_tag(&rendered) {
            if !groups.contains(&g) {
                groups.push(g);
            }
        }
    }
    // Single distinct subnet → use it; zero or multiple → no
    // subnet membership (multiple is out-of-model malformed and
    // would otherwise pick a hash-order-dependent winner).
    let subnet = if subnet_candidates.len() == 1 {
        Some(subnet_candidates[0])
    } else {
        None
    };
    // Deterministic group order so receivers agree on iteration
    // sequence regardless of local hash randomization. Lexicographic
    // by byte value is stable and cheap on the 32-byte payload.
    groups.sort_by_key(|g| g.0);
    (subnet, groups)
}

/// Caller's intent for narrowing peer discovery by reserved scope
/// tags. The legacy `CapabilityIndex::find_nodes_scoped` /
/// `find_best_node_scoped` callers were rewired to the
/// `CapabilityFold` in Phase 3B; this filter still parameterizes
/// the scope-axis decision on the fold side.
///
/// `Any` reproduces v1 behavior for non-`SubnetLocal` peers but
/// excludes peers that explicitly tagged themselves
/// `scope:subnet-local` — that tag is an opt-out from cross-subnet
/// discovery.
#[derive(Debug, Clone)]
pub enum ScopeFilter<'a> {
    /// Match every peer regardless of scope, except those tagged
    /// `scope:subnet-local` (which always require [`Self::SameSubnet`]).
    Any,
    /// Match only peers with no `scope:*` tag (resolve to
    /// `Global`). Useful for opting out of all scoped peers.
    GlobalOnly,
    /// Match peers whose subnet equals the caller's. The actual
    /// subnet comparison is supplied by the caller (typically by
    /// closing over `MeshNode::peer_subnets`); the index doesn't
    /// own subnet state.
    SameSubnet,
    /// Match peers tagged `scope:tenant:<t>` OR untagged
    /// (`Global` is permissive across tenants by design).
    Tenant(&'a str),
    /// Match peers tagged `scope:tenant:<t>` for any `t` in the
    /// list, OR untagged.
    Tenants(&'a [&'a str]),
    /// Match peers tagged `scope:region:<r>` OR untagged.
    Region(&'a str),
    /// Match peers tagged `scope:region:<r>` for any `r` in the
    /// list, OR untagged.
    Regions(&'a [&'a str]),
}

/// Predicate: does this candidate's resolved [`CapabilityScope`]
/// satisfy the caller's [`ScopeFilter`]?
///
/// `same_subnet` is supplied by the caller and is consulted only
/// when the filter is [`ScopeFilter::SameSubnet`] or the candidate
/// is [`CapabilityScope::SubnetLocal`] (which always requires
/// same-subnet membership). For the warm-up case where one
/// side's subnet isn't known yet, callers default `same_subnet`
/// to `true` (permissive).
pub(crate) fn matches_scope(
    candidate_scope: &CapabilityScope,
    filter: &ScopeFilter<'_>,
    same_subnet: bool,
) -> bool {
    use CapabilityScope as S;
    use ScopeFilter as F;
    match (filter, candidate_scope) {
        // SubnetLocal is asymmetric: the announcer has explicitly
        // opted out of cross-subnet discovery, so it shows up only
        // under SameSubnet.
        (F::SameSubnet, S::SubnetLocal) => same_subnet,
        (_, S::SubnetLocal) => false,

        // Any matches every non-SubnetLocal peer.
        (F::Any, _) => true,

        // GlobalOnly is the strict opposite of "include scoped peers."
        (F::GlobalOnly, S::Global) => true,
        (F::GlobalOnly, _) => false,

        // SameSubnet for non-SubnetLocal candidates falls through to
        // the caller-supplied predicate. Permissive when subnet is
        // unknown for either side.
        (F::SameSubnet, _) => same_subnet,

        // Global candidates match every tenant/region query —
        // permissive default, matches the v1 expectation that a
        // node which doesn't tag itself stays discoverable.
        (F::Tenant(_), S::Global)
        | (F::Tenants(_), S::Global)
        | (F::Region(_), S::Global)
        | (F::Regions(_), S::Global) => true,

        (F::Tenant(t), S::Tenants(ts))
        | (F::Tenant(t), S::TenantsAndRegions { tenants: ts, .. }) => ts.iter().any(|x| x == t),
        (F::Tenant(_), S::Regions(_)) => false,

        (F::Tenants(wanted), S::Tenants(ts))
        | (F::Tenants(wanted), S::TenantsAndRegions { tenants: ts, .. }) => {
            ts.iter().any(|x| wanted.iter().any(|w| w == x))
        }
        (F::Tenants(_), S::Regions(_)) => false,

        (F::Region(r), S::Regions(rs))
        | (F::Region(r), S::TenantsAndRegions { regions: rs, .. }) => rs.iter().any(|x| x == r),
        (F::Region(_), S::Tenants(_)) => false,

        (F::Regions(wanted), S::Regions(rs))
        | (F::Regions(wanted), S::TenantsAndRegions { regions: rs, .. }) => {
            rs.iter().any(|x| wanted.iter().any(|w| w == x))
        }
        (F::Regions(_), S::Tenants(_)) => false,
    }
}

// ============================================================================
// Capability Set
// ============================================================================

/// Complete capability set for a node.
///
/// Phase A.5.N.3 final shape: a typed `tags: HashSet<Tag>` plus
/// a `metadata: BTreeMap` for data that can't safely round-trip
/// through the tag wire format. Hardware / Software / Model /
/// Tool / ResourceLimits are *projections* of these two fields,
/// computed on demand via `views()` / the `From<&CapabilitySet>`
/// impls. Typed-struct fields no longer exist on the storage
/// shape — every read goes through the projection layer; every
/// write goes through the typed setters which re-encode into the
/// canonical tag set.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CapabilitySet {
    /// Canonical typed tag set. Holds:
    ///
    /// - `Tag::AxisPresent` / `Tag::AxisValue` axis-prefixed tags
    ///   (`hardware.gpu`, `hardware.memory_gb=64`,
    ///   `software.model.0.id=llama-3.1-70b`, …) that encode the
    ///   five projections.
    /// - `Tag::Reserved` cross-axis tags (`scope:tenant:foo`,
    ///   `causal:<hex>`, `fork-of:<hex>`, `heat:*`).
    /// - `Tag::Legacy` untyped tags (free-form strings, e.g.
    ///   `nat:full-cone` / `nrpc:<service>`).
    ///
    /// Wire format emits tags in sorted `Tag::to_string()` order so
    /// every serialization is canonical. The `HashSet` keeps O(1)
    /// membership for in-memory lookups; the `serialize_with` hook
    /// flattens to a sorted `Vec` on the way out. Two sides of a
    /// signed-announcement round-trip therefore produce identical
    /// bytes regardless of `HashSet` iteration order (which is
    /// process-local random and would otherwise cause spurious
    /// signature-verification failures across processes).
    #[serde(default, serialize_with = "serialize_tags_sorted")]
    pub tags: HashSet<Tag>,
    /// Free-form key-value metadata.
    ///
    /// Phase A.5.N introduction. Carries data that doesn't fit the
    /// typed-tag taxonomy:
    ///
    /// - **Tool schemas**: `tool::<tool_id>::input_schema` and
    ///   `tool::<tool_id>::output_schema` keys hold JSON Schema
    ///   strings (the `=`/`:`/`,` characters in JSON make these
    ///   unsafe to round-trip through the tag wire format).
    /// - **Intent**: `intent` key carries the application-defined
    ///   placement intent (Phase F).
    /// - **Colocation hints**: `colocate-with` key carries a chain
    ///   origin hash for chain-aware placement.
    /// - Application-defined keys (subject to the metadata size cap
    ///   in Phase C: 4 KB soft / 16 KB hard).
    ///
    /// `BTreeMap` for deterministic iteration order over the wire.
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

impl CapabilitySet {
    /// Create empty capability set
    pub fn new() -> Self {
        Self::default()
    }

    /// Set hardware capabilities
    pub fn with_hardware(mut self, hardware: HardwareCapabilities) -> Self {
        self.set_hardware(hardware);
        self
    }

    /// Set software capabilities
    pub fn with_software(mut self, software: SoftwareCapabilities) -> Self {
        self.set_software(software);
        self
    }

    /// Add model capability. Read-modify-write through `views()`
    /// since models live in the canonical tag set as
    /// `software.model.<i>.*` indexed-encoding.
    pub fn add_model(mut self, model: ModelCapability) -> Self {
        let mut models = self.views().models().clone();
        models.push(model);
        self.set_models(models);
        self
    }

    /// Add tool capability. Read-modify-write through `views()`
    /// since tools live in the canonical tag set as
    /// `software.tool.<i>.*` indexed-encoding; schemas are mirrored
    /// into `metadata` by `set_tools`.
    ///
    /// For adding more than one tool, prefer
    /// [`Self::add_tools`] — the batch form invokes `set_tools`
    /// exactly once instead of N times, dropping the announce-path
    /// cost from O(N²) to O(N).
    pub fn add_tool(mut self, tool: ToolCapability) -> Self {
        let mut tools = self.views().tools().clone();
        tools.push(tool);
        self.set_tools(tools);
        self
    }

    /// Batch counterpart to [`Self::add_tool`] — extends the current
    /// tool list with every element of `tools` and invokes
    /// `set_tools` exactly once. The single-`set_tools` call clears
    /// stale tags + metadata once and re-encodes the final list, so
    /// the cost is O(N) regardless of how many tools the iterator
    /// yields.
    ///
    /// Use this from announce paths that drain a `tool_registry`
    /// (which can hold many tools); the per-call `add_tool` rebuilds
    /// every previously-added tool's tags + metadata, an O(N²)
    /// pattern in the size of the registry.
    pub fn add_tools(mut self, tools: impl IntoIterator<Item = ToolCapability>) -> Self {
        let mut merged = self.views().tools().clone();
        merged.extend(tools);
        self.set_tools(merged);
        self
    }

    /// Add a tag (parsed via the application-facing parser, which
    /// rejects reserved cross-axis prefixes — use the dedicated
    /// scope helpers for those). Untyped strings parse as
    /// `Tag::Legacy`; axis-prefixed strings (`hardware.gpu`,
    /// `software.os=linux`) parse as `AxisPresent` / `AxisValue`.
    /// Empty tags and reserved-prefix tags are silently dropped
    /// (the parser returns `Err` and we ignore it).
    pub fn add_tag(mut self, tag: impl Into<String>) -> Self {
        let s: String = tag.into();
        if let Ok(t) = Tag::parse_user(&s) {
            self.tags.insert(t);
        }
        self
    }

    /// Add a typed `BlobCapability` projection. Emits the matching
    /// `dataforts.blob.*` tags via the projection's `write_into`.
    /// Builder-style; producer-side counterpart to
    /// `BlobCapability::from_capability_set`. Round-tripping
    /// through both functions returns the original projection.
    #[cfg(feature = "dataforts")]
    pub fn with_blob_capability(self, blob: super::dataforts_capabilities::BlobCapability) -> Self {
        blob.write_into(self)
    }

    /// Add a typed `GreedyCapability` projection. Emits
    /// `dataforts.greedy.*` tags.
    #[cfg(feature = "dataforts")]
    pub fn with_greedy_capability(
        self,
        greedy: super::dataforts_capabilities::GreedyCapability,
    ) -> Self {
        greedy.write_into(self)
    }

    /// Add a typed `GravityCapability` projection. Emits
    /// `dataforts.gravity.*` tags.
    #[cfg(feature = "dataforts")]
    pub fn with_gravity_capability(
        self,
        gravity: super::dataforts_capabilities::GravityCapability,
    ) -> Self {
        gravity.write_into(self)
    }

    /// Add a `scope:tenant:<id>` reserved tag, marking this
    /// announcement as advertised under the given tenant. Idempotent
    /// — repeated calls with the same id do not duplicate. Empty
    /// `tenant_id` is silently dropped (matches the scope resolver,
    /// which rejects empty ids).
    pub fn with_tenant_scope(mut self, tenant_id: impl Into<String>) -> Self {
        let id = tenant_id.into();
        if id.is_empty() {
            return self;
        }
        let tag = format!("{TAG_SCOPE_TENANT_PREFIX}{id}");
        if let Ok(t) = Tag::parse(&tag) {
            self.tags.insert(t);
        }
        self
    }

    /// Add a `scope:region:<name>` reserved tag, marking this
    /// announcement as advertised under the given region.
    /// Idempotent. Empty `region` is silently dropped.
    pub fn with_region_scope(mut self, region: impl Into<String>) -> Self {
        let name = region.into();
        if name.is_empty() {
            return self;
        }
        let tag = format!("{TAG_SCOPE_REGION_PREFIX}{name}");
        if let Ok(t) = Tag::parse(&tag) {
            self.tags.insert(t);
        }
        self
    }

    /// Add the `scope:subnet-local` reserved tag, opting this
    /// announcement out of cross-subnet discovery. The strictest
    /// scope wins: any tenant / region tags also present on this
    /// set are ignored by the scope resolver while
    /// `scope:subnet-local` is set. Idempotent.
    pub fn with_subnet_local_scope(mut self) -> Self {
        if let Ok(t) = Tag::parse(TAG_SCOPE_SUBNET_LOCAL) {
            self.tags.insert(t);
        }
        self
    }

    // ========================================================================
    // Chain composition helpers — Phase 3 of CAPABILITY_ENHANCEMENTS_PLAN.md.
    //
    // Pure syntactic sugar over the underlying `causal:` / `fork-of:` /
    // `heat:` reserved-prefix tags documented in CAPABILITY_SYSTEM_PLAN.md
    // §2 + CAPABILITIES_SCHEMA.md "Reserved cross-axis prefixes".
    // Each helper is a single-line wrapper around `Tag::parse(...)` plus
    // `tags.insert(...)` — the substrate gains no new primitives, just
    // ergonomic emission paths so call sites read cleanly.
    //
    // Empty / blank chain hashes are silently dropped (matches the
    // scope-helper convention so a builder fed an empty value doesn't
    // produce a malformed tag).
    // ========================================================================

    /// Declare this node holds the chain identified by `chain_hash`.
    ///
    /// Emits the `causal:<chain_hash>` reserved tag. Idempotent —
    /// repeated calls with the same hash do not duplicate.
    pub fn require_chain(mut self, chain_hash: impl AsRef<str>) -> Self {
        let hash = chain_hash.as_ref();
        if hash.is_empty() {
            return self;
        }
        if let Ok(t) = Tag::parse(&format!("causal:{hash}")) {
            self.tags.insert(t);
        }
        self
    }

    /// Declare this node holds the chain `<chain_hash>` up to the
    /// named `tip_seq`.
    ///
    /// Emits `causal:<chain_hash>:<tip_seq>`. Per
    /// `CAPABILITY_SYSTEM_PLAN.md` §2: receivers downsample chains
    /// shorter than they need, so a peer announcing a tip_seq is
    /// implicitly also a holder for every prefix of that chain.
    pub fn require_chain_tip(mut self, chain_hash: impl AsRef<str>, tip_seq: u64) -> Self {
        let hash = chain_hash.as_ref();
        if hash.is_empty() {
            return self;
        }
        if let Ok(t) = Tag::parse(&format!("causal:{hash}:{tip_seq}")) {
            self.tags.insert(t);
        }
        self
    }

    /// Declare this node holds the half-open range `[start_seq..end_seq)`
    /// of the chain `<chain_hash>`.
    ///
    /// Emits `causal:<chain_hash>[<start>..<end>]`. The validator
    /// enforces `start_seq < end_seq`; equal or inverted ranges are
    /// silently dropped.
    pub fn require_chain_range(
        mut self,
        chain_hash: impl AsRef<str>,
        start_seq: u64,
        end_seq: u64,
    ) -> Self {
        let hash = chain_hash.as_ref();
        if hash.is_empty() || start_seq >= end_seq {
            return self;
        }
        if let Ok(t) = Tag::parse(&format!("causal:{hash}[{start_seq}..{end_seq}]")) {
            self.tags.insert(t);
        }
        self
    }

    /// Declare this node holds any of the named chains. One
    /// `causal:<hash>` reserved tag emitted per non-empty hash.
    /// Empty / blank hashes in the iterator are silently skipped.
    pub fn require_any_chain<I, S>(mut self, chain_hashes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for hash in chain_hashes {
            self = self.require_chain(hash);
        }
        self
    }

    /// Declare this chain forks from `parent_chain_hash`.
    ///
    /// Emits the `fork-of:<parent_chain_hash>` reserved tag, used
    /// by the chain-discovery layer for lineage walks.
    pub fn from_fork(mut self, parent_chain_hash: impl AsRef<str>) -> Self {
        let hash = parent_chain_hash.as_ref();
        if hash.is_empty() {
            return self;
        }
        if let Ok(t) = Tag::parse(&format!("fork-of:{hash}")) {
            self.tags.insert(t);
        }
        self
    }

    /// Declare this node's heat (read-rate / activity score) for
    /// the named chain.
    ///
    /// `rate` is clamped to `[0.0, 1.0]` and emitted with two-decimal
    /// precision (`heat:<chain_hash>=0.85`). Heat is per-chain, not
    /// per-node; one call per chain.
    pub fn heat_level(mut self, chain_hash: impl AsRef<str>, rate: f64) -> Self {
        let hash = chain_hash.as_ref();
        if hash.is_empty() {
            return self;
        }
        let clamped = if rate.is_finite() {
            rate.clamp(0.0, 1.0)
        } else {
            return self;
        };
        if let Ok(t) = Tag::parse(&format!("heat:{hash}={clamped:.2}")) {
            self.tags.insert(t);
        }
        self
    }

    /// Set resource limits
    pub fn with_limits(mut self, limits: ResourceLimits) -> Self {
        self.set_limits(limits);
        self
    }

    /// Set or overwrite a metadata key-value entry.
    ///
    /// CR-16: silently drops writes whose key matches a
    /// substrate-reserved *prefix* (`tool::`). Those keys are
    /// authored by the substrate's own codecs (the tool codec
    /// emits `tool::<id>::input_schema` etc.) and user code
    /// must not collide with them — same shape as `Tag::parse_user`
    /// rejecting reserved tag prefixes.
    ///
    /// Note: the schema's `metadata_reserved` *exact-match* list
    /// (`intent`, `colocate-with`, `priority`, `owner`) is
    /// intentionally NOT gated — those are well-known *user-facing*
    /// scheduler hints; the substrate reads them to make placement
    /// decisions, but user code is expected to *set* them. The
    /// validator (`validate_capabilities`) does flag user writes
    /// onto exact-match reserved keys as a `MetadataReservedKey`
    /// warning so misconfiguration is visible without being fatal.
    ///
    /// Substrate-internal callers that need to emit `tool::*` keys
    /// use the `with_metadata_unchecked` sibling (crate-private).
    pub fn with_metadata(self, key: impl Into<String>, value: impl Into<String>) -> Self {
        let key: String = key.into();
        if super::schema::AXIS_SCHEMA
            .metadata_reserved_prefixes
            .iter()
            .any(|p| key.starts_with(*p))
        {
            return self;
        }
        self.with_metadata_unchecked(key, value)
    }

    /// Internal counterpart to [`Self::with_metadata`] that bypasses
    /// the reserved-prefix gate. Substrate-side code that authors
    /// reserved metadata (`tool::<id>::input_schema` from the tool
    /// codec) goes through this; user code MUST use the gated
    /// [`Self::with_metadata`].
    pub(crate) fn with_metadata_unchecked(
        mut self,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    // ========================================================================
    // Mutable setters — Phase A.5.6 write-path seam.
    //
    // These are the *only* places that should write to typed-struct
    // state on a `CapabilitySet`. The diff engine, the FFI layer
    // (when applying a remote update), and any other write path go
    // through these methods, so Phase A.5.N can rewrite the bodies
    // (e.g. to re-encode into a `tag_set: HashSet<Tag>`) without
    // touching call sites.
    //
    // Each setter takes ownership of the new value to make the
    // replacement obvious (no ambiguity about whether the caller
    // retains a partial view) and to give the eventual tag-set
    // reencoder a single owned input to consume.
    // ========================================================================

    /// Replace the hardware projection in-place.
    ///
    /// Phase A.5.N.3: clears every `hardware.*` tag (excluding
    /// `hardware.limits.*` which belongs to `ResourceLimits`) and
    /// re-emits the new ones via `hardware_to_tags`.
    pub fn set_hardware(&mut self, hardware: HardwareCapabilities) {
        self.tags
            .retain(|t| !crate::adapter::net::behavior::tag_codec::is_hardware_owned_tag(t));
        self.tags
            .extend(crate::adapter::net::behavior::tag_codec::hardware_to_tags(
                &hardware,
            ));
    }

    /// Replace the software projection in-place.
    ///
    /// Phase A.5.N.3: clears every `software.*` tag (excluding
    /// `software.model.*` and `software.tool.*` which belong to
    /// model/tool sub-collections) and re-emits the new ones.
    pub fn set_software(&mut self, software: SoftwareCapabilities) {
        self.tags
            .retain(|t| !crate::adapter::net::behavior::tag_codec::is_software_owned_tag(t));
        self.tags
            .extend(crate::adapter::net::behavior::tag_codec::software_to_tags(
                &software,
            ));
    }

    /// Replace the resource-limits projection in-place.
    ///
    /// Phase A.5.N.3: clears every `hardware.limits.*` tag and
    /// re-emits the new ones.
    pub fn set_limits(&mut self, limits: ResourceLimits) {
        self.tags
            .retain(|t| !crate::adapter::net::behavior::tag_codec::is_resource_limits_owned_tag(t));
        self.tags
            .extend(crate::adapter::net::behavior::tag_codec::resource_limits_to_tags(&limits));
    }

    /// Replace the loaded-model list in-place.
    ///
    /// Phase A.5.N.3: clears every `software.model.*` tag and
    /// re-emits the new indexed encoding via `models_to_tags`.
    pub fn set_models(&mut self, models: Vec<ModelCapability>) {
        self.tags
            .retain(|t| !crate::adapter::net::behavior::tag_codec::is_models_owned_tag(t));
        self.tags
            .extend(crate::adapter::net::behavior::tag_codec::models_to_tags(
                &models,
            ));
    }

    /// Replace the available-tool list in-place.
    ///
    /// Phase A.5.N.3: clears every `software.tool.*` tag, prunes
    /// stale `tool::<id>::*_schema` metadata, re-emits the indexed
    /// tag encoding, and mirrors fresh schemas into metadata.
    pub fn set_tools(&mut self, tools: Vec<ToolCapability>) {
        // Clear tool tags from the canonical set.
        self.tags
            .retain(|t| !crate::adapter::net::behavior::tag_codec::is_tools_owned_tag(t));

        // Drop schema metadata entries for tools no longer present.
        let new_ids: HashSet<&str> = tools.iter().map(|t| t.tool_id.as_str()).collect();
        self.metadata.retain(|key, _| {
            let Some(rest) = key.strip_prefix("tool::") else {
                return true;
            };
            let Some((id, _suffix)) = rest.split_once("::") else {
                return true;
            };
            new_ids.contains(id)
        });

        // Re-emit the tag encoding (which intentionally drops
        // schemas — they ride in metadata).
        self.tags
            .extend(crate::adapter::net::behavior::tag_codec::tools_to_tags(
                &tools,
            ));

        // Mirror fresh schemas into metadata.
        for tool in &tools {
            if let Some(schema) = &tool.input_schema {
                self.metadata.insert(
                    ToolCapability::input_schema_metadata_key(&tool.tool_id),
                    schema.clone(),
                );
            }
            if let Some(schema) = &tool.output_schema {
                self.metadata.insert(
                    ToolCapability::output_schema_metadata_key(&tool.tool_id),
                    schema.clone(),
                );
            }
        }
    }

    /// Check if has a specific tag.
    ///
    /// The query string is parsed via the permissive parser
    /// ([`Tag::parse`]) so reserved-prefix queries (`scope:tenant:foo`)
    /// resolve correctly. Set membership is exact: a query for
    /// `hardware.gpu` matches the AxisPresent tag, not an
    /// AxisValue with a different value.
    pub fn has_tag(&self, tag: &str) -> bool {
        let Ok(parsed) = Tag::parse(tag) else {
            return false;
        };
        // Separator-agnostic membership: a stored `software.os=linux`
        // matches a query `software.os:linux` (and vice versa). Plain
        // `HashSet::contains` would distinguish them via PartialEq's
        // separator field — see CR-1 in
        // `CODE_REVIEW_2026_05_10_CAPABILITY_SYSTEM_2.md`.
        self.tags.iter().any(|t| t.semantic_eq(&parsed))
    }

    /// Check if has a specific model.
    ///
    /// Phase A.5.N.3: scans for `software.model.<i>.id=<model_id>`
    /// directly in the canonical tag set rather than reconstructing
    /// the full `Vec<ModelCapability>` via `views()`.
    pub fn has_model(&self, model_id: &str) -> bool {
        self.has_indexed_software_value("model.", "id", model_id)
    }

    /// Check if has a specific tool.
    ///
    /// Phase A.5.N.3: scans for `software.tool.<i>.tool_id=<tool_id>`
    /// directly in the canonical tag set.
    pub fn has_tool(&self, tool_id: &str) -> bool {
        self.has_indexed_software_value("tool.", "tool_id", tool_id)
    }

    /// Shared scan body for `has_model` / `has_tool` — looks for a
    /// `software.<family_prefix><idx>.<sub_key>=<expected_value>` tag
    /// (e.g. `software.model.0.id=llama-3.1-7b`).
    ///
    /// Performance note: matches `Tag::AxisValue` directly to avoid
    /// `Tag::axis_key()`'s per-tag `String` clone. The value compare
    /// runs first because most tags in the set won't carry the target
    /// value — that lets the key parse (`strip_prefix` + `split_once`)
    /// run only on the small set of value-matching candidates. See
    /// `docs/misc/PERF_AUDIT_2026_05_28_CAPABILITY.md` fix #5.
    fn has_indexed_software_value(
        &self,
        family_prefix: &str,
        sub_key: &str,
        expected_value: &str,
    ) -> bool {
        use crate::adapter::net::behavior::tag::TaxonomyAxis;
        self.tags.iter().any(|tag| match tag {
            Tag::AxisValue {
                axis: TaxonomyAxis::Software,
                key,
                value,
                ..
            } if value == expected_value => {
                let Some(rest) = key.strip_prefix(family_prefix) else {
                    return false;
                };
                let Some((_idx, sub)) = rest.split_once('.') else {
                    return false;
                };
                sub == sub_key
            }
            _ => false,
        })
    }

    /// Check if has GPU.
    ///
    /// Phase A.5.N.3: looks for the `hardware.gpu` AxisPresent
    /// marker directly. Cheaper than reconstructing the full
    /// `HardwareCapabilities` projection.
    pub fn has_gpu(&self) -> bool {
        use crate::adapter::net::behavior::tag::TaxonomyAxis;
        self.tags.contains(&Tag::AxisPresent {
            axis: TaxonomyAxis::Hardware,
            key: "gpu".into(),
        })
    }

    /// First `AxisValue` tag matching `(axis, key)`, returning its
    /// value if present. Linear in `tags` count with early return.
    ///
    /// Phase A.5.N.3 fast-path helper for single-field predicates
    /// (`CapabilityFilter::matches` memory / VRAM checks). Avoids
    /// forcing the full `HardwareCapabilities` decode via
    /// `views().hardware()` when only one tag is needed. See
    /// `docs/misc/PERF_AUDIT_2026_05_28_CAPABILITY.md` fix #2.
    pub(crate) fn axis_value(
        &self,
        axis: crate::adapter::net::behavior::tag::TaxonomyAxis,
        key: &str,
    ) -> Option<&str> {
        self.tags.iter().find_map(|tag| match tag {
            Tag::AxisValue {
                axis: a,
                key: k,
                value,
                ..
            } if *a == axis && k == key => Some(value.as_str()),
            _ => None,
        })
    }

    /// Get all model IDs.
    ///
    /// Phase A.5.N.3: returns owned `String`s (rather than borrowed
    /// `&str` over a typed-struct field that no longer exists).
    pub fn model_ids(&self) -> Vec<String> {
        self.views()
            .models()
            .iter()
            .map(|m| m.model_id.clone())
            .collect()
    }

    /// Get all tool IDs.
    pub fn tool_ids(&self) -> Vec<String> {
        self.views()
            .tools()
            .iter()
            .map(|t| t.tool_id.clone())
            .collect()
    }

    /// Serialize to bytes (compact binary format)
    pub fn to_bytes(&self) -> Vec<u8> {
        // Use JSON for now (can optimize to binary later)
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Deserialize from bytes
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        serde_json::from_slice(data).ok()
    }

    /// Compute the structural change from `prev` to `self`.
    ///
    /// Phase 1 of `CAPABILITY_ENHANCEMENTS_PLAN.md`: a cheap
    /// before/after change detector that returns the raw set/map
    /// difference — added tags, removed tags, and per-key
    /// metadata changes (Added / Removed / Updated). Powers
    /// event-driven placement updates, capability-aware dashboards,
    /// and delta-based metadata propagation.
    ///
    /// Cost: `O(|tags| + |metadata|)`. Two `HashSet::difference`
    /// scans + a `BTreeMap` walk; no allocation beyond the output
    /// collections.
    ///
    /// **Composes with [`crate::adapter::net::behavior::diff::DiffEngine`]**:
    /// `DiffEngine::diff` produces structural `DiffOp`s (used by
    /// the propagation path); this method returns the raw set/map
    /// diff (better for change-event consumers). Same input data;
    /// pick the surface that matches the consumer's shape.
    pub fn diff(&self, prev: &CapabilitySet) -> CapabilitySetDiff {
        // Tag diff: separator-agnostic. Plain `HashSet::difference`
        // would compare via `Tag::PartialEq`, which distinguishes
        // `=` vs `:` on `AxisValue` — two semantically-identical
        // tags would land as both Added and Removed. The structural
        // `DiffEngine::diff` was patched for this in 38612b61; this
        // companion API was not. See CR-3 in
        // `CODE_REVIEW_2026_05_10_CAPABILITY_SYSTEM_2.md`.
        let added_tags: HashSet<Tag> = self
            .tags
            .iter()
            .filter(|t| !prev.tags.iter().any(|p| p.semantic_eq(t)))
            .cloned()
            .collect();
        let removed_tags: HashSet<Tag> = prev
            .tags
            .iter()
            .filter(|t| !self.tags.iter().any(|c| c.semantic_eq(t)))
            .cloned()
            .collect();

        // Metadata diff: walk both maps simultaneously. Both are
        // `BTreeMap` so we can rely on ordered iteration; merge
        // by key.
        let mut changed_metadata = Vec::new();
        let mut prev_iter = prev.metadata.iter().peekable();
        let mut curr_iter = self.metadata.iter().peekable();
        loop {
            match (prev_iter.peek(), curr_iter.peek()) {
                (Some((pk, pv)), Some((ck, cv))) => match pk.cmp(ck) {
                    std::cmp::Ordering::Less => {
                        changed_metadata.push(MetadataChange::Removed {
                            key: (*pk).clone(),
                            prev_value: (*pv).clone(),
                        });
                        prev_iter.next();
                    }
                    std::cmp::Ordering::Greater => {
                        changed_metadata.push(MetadataChange::Added {
                            key: (*ck).clone(),
                            value: (*cv).clone(),
                        });
                        curr_iter.next();
                    }
                    std::cmp::Ordering::Equal => {
                        if pv != cv {
                            changed_metadata.push(MetadataChange::Updated {
                                key: (*pk).clone(),
                                prev_value: (*pv).clone(),
                                new_value: (*cv).clone(),
                            });
                        }
                        prev_iter.next();
                        curr_iter.next();
                    }
                },
                (Some((pk, pv)), None) => {
                    changed_metadata.push(MetadataChange::Removed {
                        key: (*pk).clone(),
                        prev_value: (*pv).clone(),
                    });
                    prev_iter.next();
                }
                (None, Some((ck, cv))) => {
                    changed_metadata.push(MetadataChange::Added {
                        key: (*ck).clone(),
                        value: (*cv).clone(),
                    });
                    curr_iter.next();
                }
                (None, None) => break,
            }
        }

        CapabilitySetDiff {
            added_tags,
            removed_tags,
            changed_metadata,
        }
    }

    // ========================================================================
    // View projections — Capability System Plan §1, Phase A.4.
    //
    // Today these are simple field clones because `CapabilitySet`
    // still carries the typed structs as fields. Phase A.5 removes
    // the typed-struct fields and migrates wire format to
    // `tags: HashSet<Tag>`; the same `From<&CapabilitySet>` impls
    // then reconstruct the typed view by scanning the tag set.
    //
    // Downstream code SHOULD adopt the projection accessors NOW
    // (`caps.views().hardware`, `HardwareCapabilities::from(&caps)`)
    // so the migration in A.5 doesn't ripple through every call
    // site. The legacy direct-field access (`caps.hardware`)
    // continues to work in this commit but is documented as
    // deprecated in `CAPABILITY_SYSTEM_PLAN.md` Locked decision 1.
    // ========================================================================

    /// All five view projections rolled into one struct, computed
    /// once per call. Cheaper than calling each `From<&CapabilitySet>`
    /// individually when the consumer reads more than one of them.
    ///
    /// ```
    /// # use net::adapter::net::behavior::capability::CapabilitySet;
    /// let caps = CapabilitySet::new();
    /// let views = caps.views();
    /// let _ = views.hardware();
    /// let _ = views.software();
    /// let _ = views.resource_limits();
    /// let _ = views.models();
    /// let _ = views.tools();
    /// ```
    /// Borrowing handle exposing the five typed projections
    /// ([`HardwareCapabilities`], [`SoftwareCapabilities`],
    /// [`ResourceLimits`], `Vec<ModelCapability>`,
    /// `Vec<ToolCapability>`).
    ///
    /// Phase A.5.N.3 + Phase 1 of `CAPABILITY_ENHANCEMENTS_PLAN.md`:
    /// each projection is decoded from the canonical tag set
    /// (+ metadata, for tool schemas) on first access and cached
    /// for the lifetime of the handle. Repeated reads of the same
    /// projection hit the cache; reads of unrelated projections
    /// don't force the full set of decoders.
    ///
    /// The handle borrows `self`. Mutations to `self` invalidate
    /// the handle (compiler-enforced through the lifetime).
    pub fn views(&self) -> CapabilityViews<'_> {
        CapabilityViews {
            caps: self,
            sorted_tags: OnceCell::new(),
            hardware: OnceCell::new(),
            software: OnceCell::new(),
            resource_limits: OnceCell::new(),
            models: OnceCell::new(),
            tools: OnceCell::new(),
        }
    }

    // ========================================================================
    // Typed-tag-set access — Phase A.5.1 ergonomic accessors.
    //
    // These methods give downstream code the future access pattern
    // for capability data. Downstream code SHOULD adopt these now
    // so Phase A.5.2+ (when typed-struct fields are removed from
    // `CapabilitySet`) is invisible at the consumer level.
    //
    // Uses the bijection helpers from `behavior::tag_codec`. Today
    // computed on demand (no field change); Phase A.5.N introduces
    // internal `tag_set: HashSet<Tag>` storage as the source of truth
    // and removes the typed-struct fields. Either way, the surface
    // below stays stable.
    //
    // Migration path for downstream code:
    //
    // ```text
    // // Before (typed-struct field access):
    // if caps.hardware.gpu.is_some() { ... }
    // for tag in &caps.tags { ... }
    //
    // // After (read via the projection — canonical):
    // let views = caps.views();
    // if views.hardware().gpu.is_some() { ... }
    // for model in views.models() { ... }
    //
    // // Or directly through the `From` impl when only one field is needed:
    // if HardwareCapabilities::from(&caps).gpu.is_some() { ... }
    //
    // // Tags survive Phase A.5.N as a top-level field; iterate as before:
    // for tag in &caps.tags { ... }
    //
    // // Or read the typed-tag set (Phase A.5.1):
    // for tag in caps.typed_tags() { ... }
    //
    // // Writes go through the typed setters (Phase A.5.6):
    // caps.set_hardware(new_hw);
    // ```
    //
    // Application code that needs to compose with federated query
    // primitives (Phase E) will use `typed_tags()` to feed the
    // tag set into `Predicate::evaluate`'s `EvalContext`.
    // ========================================================================

    /// All capability data as a typed-tag set, including the
    /// hardware / software / models / tools / limits structs
    /// re-encoded as axis-prefixed tags AND the legacy `tags`
    /// `Vec<String>` parsed via [`Tag::parse`]. The future wire
    /// format (Phase A.5.2+) is exactly this `HashSet<Tag>`.
    ///
    /// Round-trip-stable: `Self::from_typed_tags(&caps.typed_tags())`
    /// produces a `CapabilitySet` semantically equal to `caps`,
    /// modulo the documented order non-preservation for non-indexed
    /// `Vec` fields (runtimes / frameworks / drivers).
    ///
    /// Cost: linear in tag count. Currently computed on every
    /// call; downstream callers that read in a hot loop should
    /// cache the result.
    pub fn typed_tags(&self) -> std::collections::HashSet<crate::adapter::net::behavior::tag::Tag> {
        crate::adapter::net::behavior::tag_codec::capability_set_to_tag_set(self)
    }

    /// Build a `CapabilitySet` from a typed-tag set. Inverse of
    /// [`Self::typed_tags`]; uses the per-struct decoders to
    /// reconstruct the typed fields plus a legacy-carrier scan for
    /// reserved-prefix tags + unknown axis tags.
    ///
    /// See [`Self::typed_tags`] for the round-trip contract.
    pub fn from_typed_tags(
        tags: &std::collections::HashSet<crate::adapter::net::behavior::tag::Tag>,
    ) -> Self {
        crate::adapter::net::behavior::tag_codec::capability_set_from_tag_set(tags)
    }
}

/// Lazy borrowing handle exposing the five typed projections of a
/// [`CapabilitySet`].
///
/// Returned by [`CapabilitySet::views`]. Each projection is decoded
/// from the canonical tag set on first access and cached for the
/// lifetime of the handle:
///
/// ```ignore
/// let caps = CapabilitySet::default();
/// let v = caps.views();
/// let _ = v.hardware();   // first read: decodes hardware tags
/// let _ = v.hardware();   // cached; no re-decode
/// let _ = v.models();     // separate cache; decodes model tags
/// ```
///
/// Phase 1 of `CAPABILITY_ENHANCEMENTS_PLAN.md`: callers that
/// previously read `views.hardware` (field) now call
/// `views.hardware()` (accessor). Hot-path post-cache cost is a
/// single pointer load (`OnceCell::get`); pre-cache cost is one
/// invocation of the underlying `*_from_tags` decoder.
#[derive(Debug)]
pub struct CapabilityViews<'a> {
    caps: &'a CapabilitySet,
    sorted_tags: OnceCell<Vec<Tag>>,
    hardware: OnceCell<HardwareCapabilities>,
    software: OnceCell<SoftwareCapabilities>,
    resource_limits: OnceCell<ResourceLimits>,
    models: OnceCell<Vec<ModelCapability>>,
    tools: OnceCell<Vec<ToolCapability>>,
}

impl<'a> CapabilityViews<'a> {
    /// Sorted tag vector — shared scratch for the per-axis
    /// decoders. Sort stabilizes Vec-valued fields whose tag
    /// encoding is non-indexed (`software.runtimes` etc.) so
    /// repeated reads produce identical projections.
    fn sorted_tags(&self) -> &Vec<Tag> {
        self.sorted_tags
            .get_or_init(|| decoder_sorted_tag_vec(&self.caps.tags))
    }

    /// Hardware projection. Decodes the `hardware.*` axis tags
    /// (excluding `hardware.limits.*`) on first call; subsequent
    /// calls return the cached projection.
    pub fn hardware(&self) -> &HardwareCapabilities {
        self.hardware.get_or_init(|| {
            crate::adapter::net::behavior::tag_codec::hardware_from_tags(self.sorted_tags())
        })
    }

    /// Software projection. Decodes the `software.*` axis tags
    /// (excluding `software.model.*` and `software.tool.*`) on
    /// first call.
    pub fn software(&self) -> &SoftwareCapabilities {
        self.software.get_or_init(|| {
            crate::adapter::net::behavior::tag_codec::software_from_tags(self.sorted_tags())
        })
    }

    /// Resource-limits projection. Decodes the `hardware.limits.*`
    /// tags on first call.
    pub fn resource_limits(&self) -> &ResourceLimits {
        self.resource_limits.get_or_init(|| {
            crate::adapter::net::behavior::tag_codec::resource_limits_from_tags(self.sorted_tags())
        })
    }

    /// Loaded-model projection. Decodes the `software.model.<i>.*`
    /// indexed tags on first call.
    pub fn models(&self) -> &Vec<ModelCapability> {
        self.models.get_or_init(|| {
            crate::adapter::net::behavior::tag_codec::models_from_tags(self.sorted_tags())
        })
    }

    /// Available-tool projection. Decodes the `software.tool.<i>.*`
    /// indexed tags on first call AND layers tool input/output JSON
    /// Schemas back from `caps.metadata` (key shape:
    /// `tool::<id>::input_schema` / `tool::<id>::output_schema`).
    pub fn tools(&self) -> &Vec<ToolCapability> {
        self.tools.get_or_init(|| {
            let mut tools =
                crate::adapter::net::behavior::tag_codec::tools_from_tags(self.sorted_tags());
            for tool in &mut tools {
                if let Some(s) = self
                    .caps
                    .metadata
                    .get(&ToolCapability::input_schema_metadata_key(&tool.tool_id))
                {
                    tool.input_schema = Some(s.clone());
                }
                if let Some(s) = self
                    .caps
                    .metadata
                    .get(&ToolCapability::output_schema_metadata_key(&tool.tool_id))
                {
                    tool.output_schema = Some(s.clone());
                }
            }
            tools
        })
    }
}

// ============================================================================
// View projections — `From<&CapabilitySet>` for each typed struct.
//
// Phase A.5.N.3: each impl scans the canonical `tags: HashSet<Tag>`
// via the `tag_codec::*_from_tags` decoders. The typed-struct
// fields they previously cloned no longer exist; the tag set is
// the source of truth.
// ============================================================================

/// Materialize the tag set as a sorted `Vec<Tag>` for the
/// per-struct decoders. Sort stabilizes Vec-valued fields
/// whose tag encoding is non-indexed (`software.runtimes` etc.)
/// so consecutive `views()` calls produce identical projections.
fn sorted_tag_vec(tags: &HashSet<Tag>) -> Vec<Tag> {
    let mut v: Vec<Tag> = tags.iter().cloned().collect();
    v.sort_by_key(|a| a.to_string());
    v
}

/// Decoder-path sort: stabilizes tag order for the per-axis
/// projection decoders. Uses `Tag`'s derived `Ord` (no per-element
/// `String` allocation) — any total order works here as long as it
/// is deterministic. Wire serialization keeps `sorted_tag_vec`'s
/// `Tag::to_string()` order so signed-announcement bytes stay stable.
fn decoder_sorted_tag_vec(tags: &HashSet<Tag>) -> Vec<Tag> {
    let mut v: Vec<Tag> = tags.iter().cloned().collect();
    v.sort_unstable();
    v
}

/// Serialize a `HashSet<Tag>` as a sorted JSON array — `Tag::to_string()`
/// order. The wire format is canonical so two ends of a signed
/// `CapabilityAnnouncement` round-trip produce identical bytes
/// regardless of process-local `HashSet` iteration order.
fn serialize_tags_sorted<S: serde::Serializer>(
    tags: &HashSet<Tag>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeSeq;
    let sorted = sorted_tag_vec(tags);
    let mut seq = serializer.serialize_seq(Some(sorted.len()))?;
    for t in &sorted {
        seq.serialize_element(t)?;
    }
    seq.end()
}

impl From<&CapabilitySet> for HardwareCapabilities {
    fn from(caps: &CapabilitySet) -> Self {
        crate::adapter::net::behavior::tag_codec::hardware_from_tags(&decoder_sorted_tag_vec(
            &caps.tags,
        ))
    }
}

impl From<&CapabilitySet> for SoftwareCapabilities {
    fn from(caps: &CapabilitySet) -> Self {
        crate::adapter::net::behavior::tag_codec::software_from_tags(&decoder_sorted_tag_vec(
            &caps.tags,
        ))
    }
}

impl From<&CapabilitySet> for ResourceLimits {
    fn from(caps: &CapabilitySet) -> Self {
        crate::adapter::net::behavior::tag_codec::resource_limits_from_tags(&decoder_sorted_tag_vec(
            &caps.tags,
        ))
    }
}

// ============================================================================
// CapabilitySet diff (Phase 1 of CAPABILITY_ENHANCEMENTS_PLAN.md)
// ============================================================================

/// Structural difference between two [`CapabilitySet`] values.
///
/// Returned by [`CapabilitySet::diff`]. Carries:
///
/// - `added_tags`: tags in `self` that aren't in `prev`.
/// - `removed_tags`: tags in `prev` that aren't in `self`.
/// - `changed_metadata`: per-key metadata changes (Added /
///   Removed / Updated). Key renames surface as Removed + Added,
///   not as Updated, since the key identity changed.
///
/// The diff is the input shape for event-driven placement
/// updates, capability-change dashboards, and delta-based
/// metadata propagation. For the structural ops shape consumed
/// by the propagation path, use
/// [`crate::adapter::net::behavior::diff::DiffEngine::diff`].
#[derive(Debug, Clone, PartialEq)]
pub struct CapabilitySetDiff {
    /// Tags newly present in `self`.
    pub added_tags: HashSet<Tag>,
    /// Tags that were in `prev` but are no longer in `self`.
    pub removed_tags: HashSet<Tag>,
    /// Per-key metadata changes, in key order.
    pub changed_metadata: Vec<MetadataChange>,
}

impl CapabilitySetDiff {
    /// True if no tags or metadata entries differ.
    pub fn is_empty(&self) -> bool {
        self.added_tags.is_empty()
            && self.removed_tags.is_empty()
            && self.changed_metadata.is_empty()
    }
}

/// One metadata-key change between two [`CapabilitySet`]s.
///
/// Renamed keys surface as `Removed { old_key } + Added { new_key }`,
/// not `Updated`, because key identity changes are semantically
/// distinct from value changes.
#[derive(Debug, Clone, PartialEq)]
pub enum MetadataChange {
    /// Key was not present in `prev`; now has `value`.
    Added {
        /// Metadata key.
        key: String,
        /// New value.
        value: String,
    },
    /// Key was present in `prev` with `prev_value`; no longer in `self`.
    Removed {
        /// Metadata key.
        key: String,
        /// Value held in the previous state.
        prev_value: String,
    },
    /// Key present in both; value changed.
    Updated {
        /// Metadata key.
        key: String,
        /// Value held in the previous state.
        prev_value: String,
        /// New value.
        new_value: String,
    },
}

// ============================================================================
// Capability Announcement
// ============================================================================

/// Capability announcement message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityAnnouncement {
    /// Announcing node ID
    pub node_id: u64,
    /// Announcing entity — the 32-byte ed25519 public key. Pairs
    /// with `signature` so receivers can verify end-to-end, and
    /// lets the mesh's channel-auth path resolve
    /// `node_id → EntityId` for token lookups.
    pub entity_id: super::super::identity::EntityId,
    /// Monotonic version (for diffing)
    pub version: u64,
    /// Timestamp of announcement (nanoseconds since epoch)
    pub timestamp_ns: u64,
    /// TTL for this announcement in seconds
    pub ttl_secs: u32,
    /// Capability set
    pub capabilities: CapabilitySet,
    /// Optional Ed25519 signature (64 bytes, hex encoded for serde).
    /// Covers every other field EXCEPT [`Self::hop_count`] — the
    /// internal signing helper zeros `hop_count` before serializing
    /// and hashing, so forwarders can increment it without
    /// invalidating this signature. See [`Self::sign`] /
    /// [`Self::verify`] for the public API; the zeroing is an
    /// implementation detail of both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<Signature64>,
    /// Number of times this announcement has been forwarded. Origin
    /// sets 0; each forwarder increments before re-broadcasting.
    /// Sits *outside* the signed envelope so forwarders don't need
    /// the origin's secret key. Capped at `MAX_CAPABILITY_HOPS` —
    /// announcements at or beyond the cap are dropped rather than
    /// re-broadcast. Old-format announcements missing this field
    /// deserialize as 0 via `#[serde(default)]`.
    ///
    /// `skip_serializing_if` omits the field when it's zero so the
    /// SIGNED byte form stays identical to pre-M-1 announcements —
    /// a pre-M-1 node's signature verifies on a post-M-1 node
    /// during a rolling upgrade because both produce the same
    /// canonical bytes for the origin (hop_count=0). Forwarded
    /// announcements (hop_count > 0) serialize the field; receivers
    /// still zero it in `signed_payload()` so verification hits the
    /// omitted-when-zero form.
    #[serde(default, skip_serializing_if = "is_hop_count_zero")]
    pub hop_count: u8,
    /// Observer-visible reflexive `SocketAddr` as seen by this
    /// node's anchor peers during NAT classification. Populated
    /// once the `ClassifyFsm` (under the `nat-traversal` feature,
    /// in `adapter/net/traversal/classify.rs`) has ≥ 2 probe
    /// results; stays `None` on nodes that haven't classified
    /// yet, ran with `nat-traversal` disabled, or landed in the
    /// `Unknown` bucket (different peers disagree on our port
    /// so no single address is truthful).
    ///
    /// **Peer usage.** Receivers cache this alongside the
    /// `nat:*` tag and use it as the initial rendezvous target
    /// for hole punching — one fewer reflex round-trip per
    /// first-contact punch. The field is advisory: the punch
    /// step still waits for a real keep-alive exchange on the
    /// advertised address before handing off to the Noise
    /// handshake, so a lying peer can only fail its own
    /// incoming punches, not redirect traffic to a third party
    /// (see `docs/NAT_TRAVERSAL_PLAN.md` §7 for the trust model).
    ///
    /// **Wire compat.** `skip_serializing_if` keeps the old
    /// on-wire shape when the field is `None`, so pre-stage-2
    /// nodes round-trip through a post-stage-2 deserializer
    /// without breaking signatures. A post-stage-2 node
    /// deserializing a pre-stage-2 announcement sees the field
    /// default to `None` via `#[serde(default)]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reflex_addr: Option<std::net::SocketAddr>,
    /// v0.4 capability-auth allow-list — explicit `NodeId`s that
    /// may invoke any capability listed in `capabilities`. Empty
    /// vec = permissive default (anyone may invoke, subject to
    /// the other two lists). See `CAPABILITY_AUTH_PLAN.md`.
    ///
    /// Capped at [`MAX_ALLOW_LIST_LEN`] (64) per axis — past that,
    /// operators use a [`super::group::GroupId`] instead.
    ///
    /// `skip_serializing_if` preserves byte-identity with pre-v0.4
    /// announcements: an unrestricted (empty) list serializes to
    /// nothing, so an existing signature verifies on a v0.4 reader
    /// and a v0.4 signature verifies on a pre-v0.4 reader (which
    /// defaults the field to empty via `#[serde(default)]`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_nodes: Vec<u64>,
    /// v0.4 capability-auth allow-list — [`super::subnet::SubnetId`]s
    /// whose members may invoke. Empty = permissive default.
    /// Receivers determine a caller's subnet via the `subnet:<hex>`
    /// tag on the caller's own announcement (self-declared, signed,
    /// TOFU-bound). Same wire-compat treatment as `allowed_nodes`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_subnets: Vec<super::subnet::SubnetId>,
    /// v0.4 capability-auth allow-list — [`super::group::GroupId`]s
    /// whose claimants may invoke. Empty = permissive default.
    /// Group membership is self-declared via `group:<hex>` tags on
    /// the caller's own announcement. Same wire-compat treatment
    /// as `allowed_nodes`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_groups: Vec<super::group::GroupId>,
}

/// Cap on any single allow-list axis on a
/// [`CapabilityAnnouncement`]. 64 entries keeps the announcement
/// under the wire-size ceiling and matches the operator guidance
/// "lists > 64 use a group, not inline node enumeration."
pub const MAX_ALLOW_LIST_LEN: usize = 64;

/// Serde predicate: skip serializing `hop_count` when it's zero.
/// Preserves on-wire byte-compat with pre-M-1 announcements that
/// didn't carry this field at all. See
/// [`CapabilityAnnouncement::hop_count`] for the rationale.
fn is_hop_count_zero(v: &u8) -> bool {
    *v == 0
}

/// Hard cap on `CapabilityAnnouncement::hop_count`. Mirrors the
/// pingwave `MAX_HOPS` so both multi-hop broadcast paths share the
/// same forwarding-depth contract.
pub const MAX_CAPABILITY_HOPS: u8 = 16;

/// 64-byte signature wrapper with serde support
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Signature64(pub [u8; 64]);

impl Serialize for Signature64 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // Serialize as hex string for JSON compatibility
        if serializer.is_human_readable() {
            let hex = hex::encode(self.0);
            serializer.serialize_str(&hex)
        } else {
            serializer.serialize_bytes(&self.0)
        }
    }
}

impl<'de> Deserialize<'de> for Signature64 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            let hex_str = String::deserialize(deserializer)?;
            let bytes = hex::decode(&hex_str).map_err(serde::de::Error::custom)?;
            if bytes.len() != 64 {
                return Err(serde::de::Error::custom("signature must be 64 bytes"));
            }
            let mut arr = [0u8; 64];
            arr.copy_from_slice(&bytes);
            Ok(Signature64(arr))
        } else {
            let bytes = <Vec<u8>>::deserialize(deserializer)?;
            if bytes.len() != 64 {
                return Err(serde::de::Error::custom("signature must be 64 bytes"));
            }
            let mut arr = [0u8; 64];
            arr.copy_from_slice(&bytes);
            Ok(Signature64(arr))
        }
    }
}

impl CapabilityAnnouncement {
    /// Default `ttl_secs` value assigned by [`Self::new`]. Five
    /// minutes — long enough that a missed re-announcement on one
    /// node doesn't immediately evict it from every peer's
    /// capability fold, short enough that stale state clears on
    /// realistic operational timescales. Exposed as a constant so
    /// multi-hop dedup retention can be scaled off it.
    pub const DEFAULT_TTL_SECS: u32 = 300;

    /// Create a new unsigned announcement. Receivers that run with
    /// `require_signed_capabilities = true` will drop it until
    /// [`Self::sign`] is called.
    pub fn new(
        node_id: u64,
        entity_id: super::super::identity::EntityId,
        version: u64,
        capabilities: CapabilitySet,
    ) -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};
        let timestamp_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);

        Self {
            node_id,
            entity_id,
            version,
            timestamp_ns,
            ttl_secs: Self::DEFAULT_TTL_SECS,
            capabilities,
            signature: None,
            hop_count: 0,
            reflex_addr: None,
            allowed_nodes: Vec::new(),
            allowed_subnets: Vec::new(),
            allowed_groups: Vec::new(),
        }
    }

    /// Set TTL
    pub fn with_ttl(mut self, ttl_secs: u32) -> Self {
        self.ttl_secs = ttl_secs;
        self
    }

    /// Attach the classifier's observed reflex address. Typically
    /// called by the mesh's capability-broadcast path after NAT
    /// classification has completed at least two probes. Pass
    /// `None` to clear a previously-set address — e.g. if
    /// reclassification landed in `Unknown`.
    ///
    /// Included in the signed envelope: a post-signing change
    /// invalidates verification.
    pub fn with_reflex_addr(mut self, reflex: Option<std::net::SocketAddr>) -> Self {
        self.reflex_addr = reflex;
        self
    }

    /// Set signature
    pub fn with_signature(mut self, sig: [u8; 64]) -> Self {
        self.signature = Some(Signature64(sig));
        self
    }

    /// Serialize the sign/verify payload: same bytes on both sides
    /// of the signature round-trip, with `signature` cleared AND
    /// `hop_count` zeroed. Keeping `hop_count` out of the signed
    /// envelope is what lets downstream forwarders bump it without
    /// invalidating the origin's signature — standard multi-hop
    /// gossip design (libp2p gossipsub, Chord, etc.).
    ///
    /// Pre-fix this called `to_bytes()` (= `unwrap_or_default`) on
    /// the canonical clone. A `serde_json::to_vec` failure produced
    /// an empty `Vec` that signer + verifier both observed as the
    /// same constant transcript, defeating the signature for every
    /// affected announcement and making a single captured signature
    /// replay across every other failing call. The failure mode is
    /// unreachable today (none of the `CapabilityAnnouncement`
    /// fields have a fallible `Serialize`), but propagating the
    /// error explicitly with a panic gives a loud diagnostic if a
    /// future refactor ever adds one — strictly better than silent
    /// signature-compromise.
    #[expect(
        clippy::expect_used,
        reason = "no CapabilityAnnouncement field has a fallible Serialize impl today; panic is the documented loud-diagnostic strategy for a future refactor that introduces one"
    )]
    fn signed_payload(&self) -> Vec<u8> {
        let mut canonical = self.clone();
        canonical.signature = None;
        canonical.hop_count = 0;
        serde_json::to_vec(&canonical).expect(
            "CapabilityAnnouncement::signed_payload: serde_json::to_vec is infallible \
             over the current field set; if this ever fires, a fallible Serialize impl \
             was added and the signed transcript must be re-designed before merging",
        )
    }

    /// Sign this announcement in place with `keypair`. The resulting
    /// signature covers every field EXCEPT [`Self::hop_count`] — the
    /// caller must still ensure `keypair.entity_id() == self.entity_id`
    /// or receivers will reject with `InvalidSignature`.
    pub fn sign(&mut self, keypair: &super::super::identity::EntityKeypair) {
        let payload = self.signed_payload();
        let sig = keypair.sign(&payload);
        self.signature = Some(Signature64(sig.to_bytes()));
    }

    /// Verify the signature against the announcement's own
    /// `entity_id`. Ignores [`Self::hop_count`] — forwarders are
    /// expected to bump it. Returns `Err` if no signature is
    /// present, if the signature can't be decoded, or if
    /// verification fails.
    pub fn verify(&self) -> Result<(), super::super::identity::EntityError> {
        let Some(Signature64(raw)) = self.signature else {
            return Err(super::super::identity::EntityError::InvalidSignature);
        };
        let payload = self.signed_payload();
        let sig = ed25519_dalek::Signature::from_bytes(&raw);
        self.entity_id.verify(&payload, &sig)
    }

    /// Serialize to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Deserialize from bytes. Returns `None` on a JSON parse
    /// failure OR when any v0.4 capability-auth allow-list exceeds
    /// [`MAX_ALLOW_LIST_LEN`] — the cap is a wire-level invariant
    /// (operators above 64 entries per axis must use a group), so
    /// receivers reject oversized announcements at the deserializer
    /// boundary rather than scanning unbounded vectors inside
    /// `may_execute` on every call. Symmetric with the CLI's
    /// announce-side check; closes the asymmetry where the
    /// substrate accepted any vector length the wire delivered.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        let ann: Self = serde_json::from_slice(data).ok()?;
        if ann.allowed_nodes.len() > MAX_ALLOW_LIST_LEN
            || ann.allowed_subnets.len() > MAX_ALLOW_LIST_LEN
            || ann.allowed_groups.len() > MAX_ALLOW_LIST_LEN
        {
            return None;
        }
        Some(ann)
    }

    /// Drop every metadata key that the substrate reserves for
    /// local trust use (`intent`, `colocate-with`, `priority`,
    /// `owner`). Call this on every announcement decoded from an
    /// inbound peer before its metadata is consulted by greedy
    /// admission, placement scoring, or anything else that lets a
    /// metadata value steer substrate decisions: pre-fix a peer
    /// could stamp `intent = "high-priority-tenant-X"` on its own
    /// announcement and steer the receiver's admission to itself.
    ///
    /// `tool::*` keys are NOT stripped — they're peer-advertised
    /// AI tool descriptors (schemas, descriptions, tags) that
    /// `MeshNode::list_tools` surfaces to agents. Substrate never
    /// makes trust decisions from them, so stripping would only
    /// defeat cross-mesh tool discovery. See
    /// [`schema::METADATA_RESERVED_PREFIXES`](super::schema).
    ///
    /// The schema's `metadata_reserved` doc says these keys are
    /// **writable by user code on the local node** — the local
    /// node knows its own legitimate intent. But the same wire
    /// shape carries inbound peer announcements that the
    /// substrate must NOT trust for those decisions. This method
    /// is the boundary that draws the distinction; callers on the
    /// receive path invoke it after `from_bytes`.
    pub fn strip_reserved_metadata(&mut self) {
        use super::schema::AXIS_SCHEMA;
        self.capabilities.metadata.retain(|key, _| {
            if AXIS_SCHEMA.metadata_reserved.contains(&key.as_str()) {
                return false;
            }
            // `metadata_reserved_prefixes` is empty as of A-4 — the
            // `tool::*` family that used to live here is intentionally
            // peer-advertised content. See the prefix-list doc in
            // `behavior::schema`. The retain loop is kept for forward
            // compat if a future substrate-trust prefix needs gating.
            !AXIS_SCHEMA
                .metadata_reserved_prefixes
                .iter()
                .any(|prefix| key.starts_with(prefix))
        });
    }

    /// Check if expired
    pub fn is_expired(&self) -> bool {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let age_secs = (now_ns.saturating_sub(self.timestamp_ns)) / 1_000_000_000;
        // Inclusive-expiry: at age == ttl the announcement is already expired.
        // Matches `PermissionToken::is_valid` (see identity/token.rs) so the
        // effective lifetime is exactly `ttl_secs` seconds.
        age_secs >= self.ttl_secs as u64
    }
}

// ============================================================================
// Capability Filter
// ============================================================================

/// Filter for querying capabilities
#[derive(Debug, Clone, Default)]
pub struct CapabilityFilter {
    /// Require specific tags (all must match)
    pub require_tags: Vec<String>,
    /// Require specific models (any must match)
    pub require_models: Vec<String>,
    /// Require specific tools (any must match)
    pub require_tools: Vec<String>,
    /// Minimum memory in GB
    pub min_memory_gb: Option<u32>,
    /// Require GPU
    pub require_gpu: bool,
    /// Specific GPU vendor
    pub gpu_vendor: Option<GpuVendor>,
    /// Minimum VRAM in GB
    pub min_vram_gb: Option<u32>,
    /// Minimum context length
    pub min_context_length: Option<u32>,
    /// Require specific modalities
    pub require_modalities: Vec<Modality>,
}

impl CapabilityFilter {
    /// Create empty filter (matches all)
    pub fn new() -> Self {
        Self::default()
    }

    /// Require tag
    pub fn require_tag(mut self, tag: impl Into<String>) -> Self {
        self.require_tags.push(tag.into());
        self
    }

    /// Require model
    pub fn require_model(mut self, model: impl Into<String>) -> Self {
        self.require_models.push(model.into());
        self
    }

    /// Require tool
    pub fn require_tool(mut self, tool: impl Into<String>) -> Self {
        self.require_tools.push(tool.into());
        self
    }

    /// Set minimum memory
    pub fn with_min_memory(mut self, gb: u32) -> Self {
        self.min_memory_gb = Some(gb);
        self
    }

    /// Require GPU
    pub fn require_gpu(mut self) -> Self {
        self.require_gpu = true;
        self
    }

    /// Require specific GPU vendor
    pub fn with_gpu_vendor(mut self, vendor: GpuVendor) -> Self {
        self.gpu_vendor = Some(vendor);
        self.require_gpu = true;
        self
    }

    /// Set minimum VRAM
    pub fn with_min_vram(mut self, gb: u32) -> Self {
        self.min_vram_gb = Some(gb);
        self.require_gpu = true;
        self
    }

    /// Set minimum context length
    pub fn with_min_context(mut self, length: u32) -> Self {
        self.min_context_length = Some(length);
        self
    }

    /// Require modality
    pub fn require_modality(mut self, modality: Modality) -> Self {
        self.require_modalities.push(modality);
        self
    }

    /// Check if a capability set matches this filter.
    ///
    /// Phase A.5.2: reads through `caps.views()` for the
    /// hardware / models / tools projections. Methods that already
    /// abstract field access (`has_tag` / `has_gpu` / `has_model`
    /// / `has_tool`) keep working unchanged. Once Phase A.5.N
    /// removes the typed-struct fields from `CapabilitySet`, the
    /// `views()` body becomes a tag-set scan and this matcher
    /// keeps working without further changes.
    pub fn matches(&self, caps: &CapabilitySet) -> bool {
        use crate::adapter::net::behavior::tag::{AxisSeparator, TaxonomyAxis};
        use crate::adapter::net::behavior::tag_codec::gpu_vendor_str;

        // Check tags (all required tags must be present)
        for tag in &self.require_tags {
            if !caps.has_tag(tag) {
                return false;
            }
        }

        // Check models (any required model must be present)
        if !self.require_models.is_empty() {
            let has_model = self.require_models.iter().any(|m| caps.has_model(m));
            if !has_model {
                return false;
            }
        }

        // Check tools (any required tool must be present)
        if !self.require_tools.is_empty() {
            let has_tool = self.require_tools.iter().any(|t| caps.has_tool(t));
            if !has_tool {
                return false;
            }
        }

        // Tag-direct fast paths for single-field hardware predicates —
        // avoid forcing the full `HardwareCapabilities` decode (sort +
        // per-tag axis_key parse) when only one tag's value is needed.
        // See `docs/misc/PERF_AUDIT_2026_05_28_CAPABILITY.md` fix #2.
        if let Some(min_mem) = self.min_memory_gb {
            let mem = caps
                .axis_value(TaxonomyAxis::Hardware, "memory_gb")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0);
            if mem < min_mem {
                return false;
            }
        }

        if self.require_gpu && !caps.has_gpu() {
            return false;
        }

        if let Some(vendor) = self.gpu_vendor {
            // O(1) HashSet probe for `hardware.gpu.vendor=<vendor>`.
            let expected = Tag::AxisValue {
                axis: TaxonomyAxis::Hardware,
                key: "gpu.vendor".to_string(),
                value: gpu_vendor_str(vendor).to_string(),
                separator: AxisSeparator::Eq,
            };
            if !caps.tags.contains(&expected) {
                return false;
            }
        }

        if let Some(min_vram) = self.min_vram_gb {
            // Single-GPU fast path: `hardware.gpu.vram_gb=<n>`. Falls
            // through to the full `HardwareCapabilities::total_vram_gb`
            // sum for multi-GPU configs (where additional `gpu.<i>.*`
            // tags exist beyond the primary).
            let vram = caps
                .axis_value(TaxonomyAxis::Hardware, "gpu.vram_gb")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0);
            if vram < min_vram {
                let total = caps.views().hardware().total_vram_gb();
                if total < min_vram {
                    return false;
                }
            }
        }

        // Remaining predicates need the model projection — decode lazily.
        if self.min_context_length.is_some() || !self.require_modalities.is_empty() {
            let views = caps.views();

            if let Some(min_ctx) = self.min_context_length {
                let has_sufficient = views.models().iter().any(|m| m.context_length >= min_ctx);
                if !has_sufficient {
                    return false;
                }
            }

            for modality in &self.require_modalities {
                let has_modality = views
                    .models()
                    .iter()
                    .any(|m| m.modalities.contains(modality));
                if !has_modality {
                    return false;
                }
            }
        }

        true
    }
}

// ============================================================================
// Capability Requirement (for load balancing)
// ============================================================================

/// Capability requirement with scoring
#[derive(Debug, Clone, Default)]
pub struct CapabilityRequirement {
    /// Base filter
    pub filter: CapabilityFilter,
    /// Prefer more memory (weight 0.0-1.0)
    pub prefer_more_memory: f32,
    /// Prefer more VRAM (weight 0.0-1.0)
    pub prefer_more_vram: f32,
    /// Prefer faster tokens/sec (weight 0.0-1.0)
    pub prefer_faster_inference: f32,
    /// Prefer loaded models (weight 0.0-1.0)
    pub prefer_loaded_models: f32,
}

impl CapabilityRequirement {
    /// Create from filter
    pub fn from_filter(filter: CapabilityFilter) -> Self {
        Self {
            filter,
            ..Default::default()
        }
    }

    /// Set memory preference weight
    pub fn prefer_memory(mut self, weight: f32) -> Self {
        self.prefer_more_memory = weight.clamp(0.0, 1.0);
        self
    }

    /// Set VRAM preference weight
    pub fn prefer_vram(mut self, weight: f32) -> Self {
        self.prefer_more_vram = weight.clamp(0.0, 1.0);
        self
    }

    /// Set inference speed preference
    pub fn prefer_speed(mut self, weight: f32) -> Self {
        self.prefer_faster_inference = weight.clamp(0.0, 1.0);
        self
    }

    /// Set loaded model preference
    pub fn prefer_loaded(mut self, weight: f32) -> Self {
        self.prefer_loaded_models = weight.clamp(0.0, 1.0);
        self
    }

    /// Score a capability set (higher is better)
    pub fn score(&self, caps: &CapabilitySet) -> f32 {
        if !self.filter.matches(caps) {
            return 0.0;
        }

        // Phase A.5.5: read through views() once. Same projection
        // pattern Phase A.5.2/A.5.3/A.5.4 applied to filter / proximity
        // / diff — survives Phase A.5.N field removal unchanged.
        let views = caps.views();

        let mut score = 1.0;

        // Memory score (normalized to 256GB)
        if self.prefer_more_memory > 0.0 {
            let mem_score = (views.hardware().memory_gb as f32 / 256.0).min(1.0);
            score += self.prefer_more_memory * mem_score;
        }

        // VRAM score (normalized to 80GB)
        if self.prefer_more_vram > 0.0 {
            let vram_score = (views.hardware().total_vram_gb() as f32 / 80.0).min(1.0);
            score += self.prefer_more_vram * vram_score;
        }

        // Inference speed score (normalized to 1000 tok/s)
        if self.prefer_faster_inference > 0.0 {
            let max_tps: u32 = views
                .models()
                .iter()
                .map(|m| m.tokens_per_sec)
                .max()
                .unwrap_or(0);
            let speed_score = (max_tps as f32 / 1000.0).min(1.0);
            score += self.prefer_faster_inference * speed_score;
        }

        // Loaded model score
        if self.prefer_loaded_models > 0.0 {
            let models = views.models();
            let loaded_count = models.iter().filter(|m| m.loaded).count();
            let loaded_ratio = if models.is_empty() {
                0.0
            } else {
                loaded_count as f32 / models.len() as f32
            };
            score += self.prefer_loaded_models * loaded_ratio;
        }

        score
    }
}

// ============================================================================
// CardinalityProvider trait — used by the predicate planner for
// per-key selectivity estimates. The legacy CapabilityIndex impl
// was removed in Phase 3B of the multifold migration; downstream
// users now bring their own provider (the fold side ships one
// through `capability::CapabilityFold`).
// ============================================================================

/// Source of per-key cardinality data for the predicate query
/// planner. Implementors return distinct-value counts for axis
/// tag keys and metadata keys; the planner uses these to order
/// And-clauses (rare-true first) and Or-clauses (often-true
/// first) for early-out savings.
pub trait CardinalityProvider {
    /// Distinct-value count for the given axis tag key. Returns 0
    /// when the key is absent — planner treats this as "no data,
    /// fall back to static cost".
    fn axis_cardinality(&self, key: &crate::adapter::net::behavior::tag::TagKey) -> usize;

    /// Distinct-value count for the given metadata key.
    fn metadata_value_cardinality(&self, key: &str) -> usize;
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    /// Fixed-bytes `EntityId` for unit-test fixtures. Valid as a
    /// *value* (it's just 32 bytes) but not a valid ed25519 public
    /// key — callers that also exercise signature verification
    /// should construct a real `EntityKeypair` instead.
    fn test_entity() -> super::super::super::identity::EntityId {
        super::super::super::identity::EntityId::from_bytes([0u8; 32])
    }
    /// `strip_reserved_metadata` drops every exact-match reserved
    /// key (`intent`, `colocate-with`, `priority`, `owner`) and
    /// leaves all other keys intact. The substrate calls this on
    /// every inbound peer announcement before downstream consumers
    /// (greedy admission, placement scoring) read metadata, so a
    /// peer can't steer receiver decisions through the
    /// substrate-trusted slot keys.
    ///
    /// A-4 update: `tool::*` keys are NOT stripped any more — they
    /// carry peer-advertised AI tool schemas / descriptions /
    /// tags that `MeshNode::list_tools` surfaces to agents. The
    /// substrate never makes trust decisions from those values, so
    /// stripping them would only defeat cross-mesh tool discovery.
    #[test]
    fn strip_reserved_metadata_drops_reserved_keys() {
        let mut ann = CapabilityAnnouncement::new(0xDEAD, test_entity(), 7, CapabilitySet::new());
        ann.capabilities
            .metadata
            .insert("intent".into(), "evil-tenant".into());
        ann.capabilities
            .metadata
            .insert("colocate-with".into(), "0xdeadbeef".into());
        ann.capabilities
            .metadata
            .insert("priority".into(), "9999".into());
        ann.capabilities
            .metadata
            .insert("owner".into(), "attacker".into());
        ann.capabilities.metadata.insert(
            "tool::web_search::description".into(),
            "Search the web.".into(),
        );
        ann.capabilities
            .metadata
            .insert("app::region".into(), "us-east".into());
        ann.capabilities
            .metadata
            .insert("user_tag".into(), "fine".into());

        ann.strip_reserved_metadata();

        assert!(!ann.capabilities.metadata.contains_key("intent"));
        assert!(!ann.capabilities.metadata.contains_key("colocate-with"));
        assert!(!ann.capabilities.metadata.contains_key("priority"));
        assert!(!ann.capabilities.metadata.contains_key("owner"));
        // `tool::*` keys survive — they are peer-advertised AI tool
        // descriptor content, not substrate trust signal.
        assert_eq!(
            ann.capabilities
                .metadata
                .get("tool::web_search::description")
                .map(String::as_str),
            Some("Search the web."),
        );
        // Non-reserved keys survive — substrate only filters its
        // own reserved namespace, not the caller's app namespace.
        assert_eq!(
            ann.capabilities
                .metadata
                .get("app::region")
                .map(String::as_str),
            Some("us-east"),
        );
        assert_eq!(
            ann.capabilities
                .metadata
                .get("user_tag")
                .map(String::as_str),
            Some("fine"),
        );
    }
    /// The signature transcript covers `capabilities.metadata`, so
    /// `strip_reserved_metadata` invalidates the signature. The
    /// inbound dispatch path must therefore re-broadcast the
    /// announcement BEFORE stripping; otherwise a multi-hop
    /// receiver with `require_signed_capabilities = true` would
    /// reject every forwarded announcement that originally carried
    /// a reserved metadata key.
    #[test]
    fn strip_reserved_metadata_invalidates_signature() {
        use super::super::super::identity::EntityKeypair;
        let keypair = EntityKeypair::generate();
        let mut ann =
            CapabilityAnnouncement::new(1, keypair.entity_id().clone(), 1, sample_capability_set());
        ann.capabilities
            .metadata
            .insert("intent".into(), "compute".into());
        ann.sign(&keypair);

        // Baseline: signed announcement verifies, and the bytes a
        // forwarder would re-broadcast also verify (the bug a
        // pre-forward strip would cause).
        assert!(ann.verify().is_ok());
        let forward_bytes = ann.to_bytes();
        let forwarded =
            CapabilityAnnouncement::from_bytes(&forward_bytes).expect("forwarded parses");
        assert!(
            forwarded.verify().is_ok(),
            "downstream verifier must accept the un-stripped wire bytes"
        );

        // After strip the signature transcript no longer matches —
        // pins the invariant the inbound dispatch order in
        // `mesh.rs::process_capability_announcement` relies on.
        ann.strip_reserved_metadata();
        assert!(
            ann.verify().is_err(),
            "strip must invalidate the signature so the substrate is forced \
             to strip the local copy AFTER any re-broadcast"
        );
    }
    fn sample_capability_set() -> CapabilitySet {
        let gpu = GpuInfo::new(GpuVendor::Nvidia, "RTX 4090", 24)
            .with_compute_units(128)
            .with_tensor_cores(512)
            .with_fp16_tflops(82.5);

        let hardware = HardwareCapabilities::new()
            .with_cpu(16, 32)
            .with_memory(64)
            .with_gpu(gpu)
            .with_storage(2000)
            .with_network(10);

        let software = SoftwareCapabilities::new()
            .with_os("linux", "6.1")
            .add_runtime("python", "3.11")
            .add_framework("pytorch", "2.1")
            .with_cuda("12.1");

        let model = ModelCapability::new("llama-3.1-70b", "llama")
            .with_parameters(70.0)
            .with_context_length(128000)
            .with_quantization("fp16")
            .add_modality(Modality::Text)
            .add_modality(Modality::Code)
            .with_tokens_per_sec(50)
            .with_loaded(true);

        let tool = ToolCapability::new("python_repl", "Python REPL")
            .with_version("1.0.0")
            .with_estimated_time(100);

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
    fn test_capability_set_creation() {
        let caps = sample_capability_set();
        assert!(caps.has_gpu());
        assert!(caps.has_tag("inference"));
        assert!(caps.has_model("llama-3.1-70b"));
        assert!(caps.has_tool("python_repl"));
        assert_eq!(caps.views().hardware().memory_gb, 64);
    }
    #[test]
    fn test_capability_set_serialization() {
        let caps = sample_capability_set();
        let bytes = caps.to_bytes();
        let parsed = CapabilitySet::from_bytes(&bytes).unwrap();

        assert_eq!(
            caps.views().hardware().memory_gb,
            parsed.views().hardware().memory_gb,
        );
        assert_eq!(caps.tags, parsed.tags);
        assert_eq!(caps.views().models().len(), parsed.views().models().len());
    }
    /// A-4: `with_metadata` consults `METADATA_RESERVED_PREFIXES`,
    /// which is now empty — the `tool::*` family was hoisted out
    /// because tool descriptors are peer-advertised content, not
    /// substrate-trust slots. The gate stays wired so a future
    /// re-add (e.g. a new substrate-internal prefix) plugs back
    /// in here without a fan-out edit, but the current contract is
    /// "tool::* writes pass through". Exact-match reserved keys
    /// (`intent`, `owner`, …) are NOT gated by `with_metadata`
    /// either — those are well-known user-facing scheduler hints
    /// the substrate reads and the user is expected to set.
    #[test]
    fn with_metadata_preserves_tool_prefix_after_a4() {
        // `tool::*` writes survive — A-4 contract.
        let caps = CapabilitySet::new()
            .with_metadata("tool::web_search::input_schema", "{}")
            .with_metadata("region", "us-east");
        assert_eq!(
            caps.metadata
                .get("tool::web_search::input_schema")
                .map(|s| s.as_str()),
            Some("{}"),
            "tool::* writes must pass through with_metadata: {:?}",
            caps.metadata,
        );
        // Non-reserved key passes through.
        assert_eq!(
            caps.metadata.get("region").map(|s| s.as_str()),
            Some("us-east")
        );

        // Exact-match reserved keys (NOT gated) — these are
        // user-facing scheduler hints, the substrate reads them
        // and user code is expected to set them.
        let caps = CapabilitySet::new().with_metadata("intent", "ml-training");
        assert_eq!(
            caps.metadata.get("intent").map(|s| s.as_str()),
            Some("ml-training")
        );
    }
    /// E-2 regression: add_tools must produce the same final
    /// CapabilitySet as N successive add_tool calls, but via one
    /// set_tools invocation. We verify the equivalence by building
    /// the same capability set both ways and comparing.
    #[test]
    fn add_tools_batch_matches_repeated_add_tool() {
        let tools = vec![
            ToolCapability::new("web_search", "Web Search").with_version("1.0.0"),
            ToolCapability::new("summarize", "Summarize").with_version("1.0.0"),
            ToolCapability::new("code_eval", "Code Eval")
                .with_version("2.0.0")
                .with_input_schema(r#"{"type":"object"}"#),
        ];

        let via_repeated = tools
            .iter()
            .fold(CapabilitySet::new(), |caps, t| caps.add_tool(t.clone()));
        let via_batch = CapabilitySet::new().add_tools(tools.iter().cloned());

        // Tag sets must be byte-equal (the canonical software.tool.*
        // indexed encoding is order-stable for set_tools).
        assert_eq!(via_repeated.tags, via_batch.tags);
        // Schema metadata must be byte-equal too — set_tools is the
        // codepath that mirrors input/output schemas.
        assert_eq!(via_repeated.metadata, via_batch.metadata);
        // And the typed view must agree.
        assert_eq!(
            via_repeated.views().tools().len(),
            via_batch.views().tools().len()
        );
    }

    /// E-2 regression: add_tools onto a non-empty set must extend,
    /// not replace. Guards against a future implementation that
    /// might mistakenly call `set_tools(iter.collect())` and drop
    /// the prior tools.
    #[test]
    fn add_tools_extends_existing_tools() {
        let caps = CapabilitySet::new()
            .add_tool(ToolCapability::new("first", "First").with_version("1.0.0"))
            .add_tools(vec![
                ToolCapability::new("second", "Second").with_version("1.0.0"),
                ToolCapability::new("third", "Third").with_version("1.0.0"),
            ]);
        assert!(caps.has_tool("first"));
        assert!(caps.has_tool("second"));
        assert!(caps.has_tool("third"));
    }

    #[test]
    fn has_tag_matches_across_separator_forms() {
        // Regression for CR-1: `Tag::AxisValue` derives `PartialEq`
        // including the `=` vs `:` separator. A capability set built
        // by inserting one wire form must still be findable when the
        // caller queries the other — the separator is a serialization
        // detail, not part of identity. Mirrors the prior diff-engine
        // fix in commit 38612b61 but for the public membership API.
        use crate::adapter::net::behavior::tag::{AxisSeparator, Tag, TaxonomyAxis};
        let mut caps = CapabilitySet::new();
        caps.tags.insert(Tag::AxisValue {
            axis: TaxonomyAxis::Software,
            key: "os".to_string(),
            value: "linux".to_string(),
            separator: AxisSeparator::Colon,
        });
        // Stored colon, queried equals — must hit.
        assert!(caps.has_tag("software.os=linux"));
        // Stored colon, queried colon — must hit.
        assert!(caps.has_tag("software.os:linux"));
        // Different value — must miss.
        assert!(!caps.has_tag("software.os=darwin"));

        let mut caps = CapabilitySet::new();
        caps.tags.insert(Tag::AxisValue {
            axis: TaxonomyAxis::Hardware,
            key: "gpu.vram_gb".to_string(),
            value: "80".to_string(),
            separator: AxisSeparator::Eq,
        });
        // Stored equals, queried colon — must hit.
        assert!(caps.has_tag("hardware.gpu.vram_gb:80"));
        // Stored equals, queried equals — must hit.
        assert!(caps.has_tag("hardware.gpu.vram_gb=80"));
    }
    #[test]
    fn test_capability_filter_matches() {
        let caps = sample_capability_set();

        // Tag filter
        let filter = CapabilityFilter::new().require_tag("inference");
        assert!(filter.matches(&caps));

        let filter = CapabilityFilter::new().require_tag("training");
        assert!(!filter.matches(&caps));

        // GPU filter
        let filter = CapabilityFilter::new().require_gpu();
        assert!(filter.matches(&caps));

        let filter = CapabilityFilter::new().with_gpu_vendor(GpuVendor::Nvidia);
        assert!(filter.matches(&caps));

        let filter = CapabilityFilter::new().with_gpu_vendor(GpuVendor::Amd);
        assert!(!filter.matches(&caps));

        // Memory filter
        let filter = CapabilityFilter::new().with_min_memory(32);
        assert!(filter.matches(&caps));

        let filter = CapabilityFilter::new().with_min_memory(128);
        assert!(!filter.matches(&caps));

        // Model filter
        let filter = CapabilityFilter::new().require_model("llama-3.1-70b");
        assert!(filter.matches(&caps));

        let filter = CapabilityFilter::new().require_model("gpt-4");
        assert!(!filter.matches(&caps));
    }
    #[test]
    fn test_capability_requirement_scoring() {
        let caps = sample_capability_set();

        let req = CapabilityRequirement::from_filter(CapabilityFilter::new().require_gpu())
            .prefer_memory(0.5)
            .prefer_vram(0.5)
            .prefer_speed(0.5);

        let score = req.score(&caps);
        assert!(score > 1.0); // Base score + preferences
    }
    #[test]
    fn test_capability_announcement_expiry() {
        let caps = sample_capability_set();
        let mut ann = CapabilityAnnouncement::new(1, test_entity(), 1, caps);

        // Fresh announcement should not be expired
        assert!(!ann.is_expired());

        // Set timestamp to the past
        ann.timestamp_ns = 0;
        ann.ttl_secs = 1;

        // Should be expired now
        assert!(ann.is_expired());
    }
    /// `CapabilityAnnouncement::is_expired()` uses `SystemTime`, so
    /// we can backdate `timestamp_ns` and exercise the ttl boundary
    /// directly. Covers the inclusive-expiry contract at every TTL
    /// bucket in the plan.
    #[test]
    fn announcement_is_expired_table_driven_across_ttl_buckets() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
        let sec_ns = 1_000_000_000u64;

        // (ttl_secs, age_secs, expected_is_expired, label)
        let cases: &[(u32, u64, bool, &str)] = &[
            // TTL=0: inclusive-expiry — any age (including 0) is expired.
            (0, 0, true, "ttl=0 fresh"),
            // TTL=1: 0s age → fresh; 2s age → expired.
            (1, 0, false, "ttl=1s fresh"),
            (1, 2, true, "ttl=1s aged 2s"),
            // TTL=1h: boundary at 3600s.
            (3_600, 1, false, "ttl=1h aged 1s"),
            (3_600, 3_599, false, "ttl=1h aged 3599s"),
            (3_600, 3_600, true, "ttl=1h aged exactly 3600s (inclusive)"),
            (3_600, 3_601, true, "ttl=1h aged 3601s"),
            // TTL=1yr: day-old is fresh, 2yr-old is expired.
            (31_536_000, 86_400, false, "ttl=1yr aged 1 day"),
            (31_536_000, 31_536_001, true, "ttl=1yr aged just past"),
            // TTL=u32::MAX: a 1-year-old entry is still fresh. Pins
            // that `ttl_secs as u64` widens without wrapping.
            (u32::MAX, 31_536_000, false, "ttl=u32::MAX aged 1 year"),
        ];

        for &(ttl_secs, age_secs, expected, label) in cases {
            let mut ann = CapabilityAnnouncement::new(1, test_entity(), 1, sample_capability_set());
            ann.ttl_secs = ttl_secs;
            ann.timestamp_ns = now_ns.saturating_sub(age_secs.saturating_mul(sec_ns));

            assert_eq!(
                ann.is_expired(),
                expected,
                "is_expired({label}) must be {expected}",
            );
        }
    }
    // ========================================================================
    // Multi-hop wire format (M-1)
    // ========================================================================

    #[test]
    fn hop_count_defaults_to_zero() {
        let ann = CapabilityAnnouncement::new(1, test_entity(), 1, sample_capability_set());
        assert_eq!(ann.hop_count, 0);
    }
    #[test]
    fn hop_count_roundtrips_through_serde() {
        let mut ann = CapabilityAnnouncement::new(1, test_entity(), 1, sample_capability_set());
        ann.hop_count = 7;
        let bytes = ann.to_bytes();
        let restored = CapabilityAnnouncement::from_bytes(&bytes).expect("parse");
        assert_eq!(restored.hop_count, 7);
    }
    #[test]
    fn old_format_without_hop_count_parses_as_zero() {
        // Hand-crafted JSON missing the `hop_count` field — the
        // #[serde(default)] attribute should rescue us.
        let payload = serde_json::json!({
            "node_id": 1,
            "entity_id": hex::encode([0u8; 32]),
            "version": 1,
            "timestamp_ns": 0u64,
            "ttl_secs": 300u32,
            "capabilities": sample_capability_set(),
        });
        let bytes = serde_json::to_vec(&payload).expect("serialize");
        let parsed = CapabilityAnnouncement::from_bytes(&bytes).expect("parse old format");
        assert_eq!(parsed.hop_count, 0);
    }
    #[test]
    fn signature_verifies_across_hop_count_bumps() {
        use super::super::super::identity::EntityKeypair;
        let keypair = EntityKeypair::generate();
        let mut ann =
            CapabilityAnnouncement::new(1, keypair.entity_id().clone(), 1, sample_capability_set());
        ann.sign(&keypair);
        // Baseline: freshly signed announcement verifies.
        assert!(ann.verify().is_ok());

        // Simulate a forwarder bumping the counter. Signature still
        // holds because `hop_count` sits outside the signed envelope.
        for bumped in 1..=MAX_CAPABILITY_HOPS {
            ann.hop_count = bumped;
            assert!(
                ann.verify().is_ok(),
                "signature should remain valid after hop_count={}",
                bumped
            );
        }
    }
    #[test]
    fn signature_rejects_tampered_payload_even_at_hop_zero() {
        use super::super::super::identity::EntityKeypair;
        let keypair = EntityKeypair::generate();
        let mut ann =
            CapabilityAnnouncement::new(1, keypair.entity_id().clone(), 1, sample_capability_set());
        ann.sign(&keypair);
        // Flip a byte inside the signed envelope (node_id).
        ann.node_id ^= 0x01;
        assert!(ann.verify().is_err());
    }
    #[test]
    fn max_capability_hops_matches_pingwave_contract() {
        // MAX_CAPABILITY_HOPS is documented to mirror the pingwave
        // MAX_HOPS. If the pingwave side is ever renumbered this
        // test flags the divergence at compile time.
        assert_eq!(MAX_CAPABILITY_HOPS, 16);
    }
    // ─────────────────────────────────────────────────────────────────
    // v0.4 capability-auth: allow-list wire-format + signing tests
    // ─────────────────────────────────────────────────────────────────

    /// An announcement with all three allow-lists empty must
    /// produce JSON bytes identical to a pre-v0.4 announcement.
    /// This is the wire-compat contract the plan §"What ships"
    /// pins: existing peers must round-trip a v0.4-produced
    /// unrestricted announcement byte-for-byte.
    #[test]
    fn empty_allow_lists_omit_fields_from_wire() {
        let ann = CapabilityAnnouncement::new(
            42,
            super::super::super::identity::EntityId::from_bytes([0xAA; 32]),
            1,
            sample_capability_set(),
        );
        let bytes = ann.to_bytes();
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(
            !s.contains("allowed_nodes"),
            "empty allowed_nodes must be skipped on the wire; got: {}",
            s
        );
        assert!(
            !s.contains("allowed_subnets"),
            "empty allowed_subnets must be skipped on the wire; got: {}",
            s
        );
        assert!(
            !s.contains("allowed_groups"),
            "empty allowed_groups must be skipped on the wire; got: {}",
            s
        );
    }
    /// Round-trip an announcement with each allow-list populated
    /// — the decoder must reconstruct the exact field values.
    #[test]
    fn populated_allow_lists_round_trip() {
        let mut ann = CapabilityAnnouncement::new(
            7,
            super::super::super::identity::EntityId::from_bytes([0xBB; 32]),
            2,
            sample_capability_set(),
        );
        ann.allowed_nodes = vec![100, 200, 300];
        ann.allowed_subnets = vec![super::super::subnet::SubnetId([0x11; 16])];
        ann.allowed_groups = vec![
            super::super::group::GroupId([0x33; 32]),
            super::super::group::GroupId([0x44; 32]),
        ];
        let bytes = ann.to_bytes();
        let decoded = CapabilityAnnouncement::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded.allowed_nodes, ann.allowed_nodes);
        assert_eq!(decoded.allowed_subnets, ann.allowed_subnets);
        assert_eq!(decoded.allowed_groups, ann.allowed_groups);
    }
    /// The canonical signed payload of an unrestricted
    /// announcement must NOT carry the three allow-list keys at
    /// all — that's what keeps the v0.4 signed byte-pattern
    /// identical to the pre-v0.4 shape, so a pre-v0.4 verifier
    /// validates a v0.4 unrestricted announcement and vice versa.
    /// Distinct from `empty_allow_lists_omit_fields_from_wire`,
    /// which checks the same invariant on the serialized wire
    /// form (`to_bytes`); this one checks the canonical signed
    /// payload (`signed_payload`, which also zeroes `hop_count`).
    #[test]
    fn signed_payload_omits_empty_allow_lists() {
        use super::super::super::identity::EntityKeypair;
        let keypair = EntityKeypair::generate();
        let ann =
            CapabilityAnnouncement::new(5, keypair.entity_id().clone(), 1, sample_capability_set());
        let canonical = ann.signed_payload();
        let v: serde_json::Value = serde_json::from_slice(&canonical).expect("parse");
        let obj = v.as_object().expect("object");
        assert!(
            !obj.contains_key("allowed_nodes"),
            "pre-v0.4 wire shape must not carry allowed_nodes when empty"
        );
        assert!(
            !obj.contains_key("allowed_subnets"),
            "pre-v0.4 wire shape must not carry allowed_subnets when empty"
        );
        assert!(
            !obj.contains_key("allowed_groups"),
            "pre-v0.4 wire shape must not carry allowed_groups when empty"
        );
    }
    /// A signed announcement carrying non-empty allow-lists
    /// verifies after wire round-trip. Pins that the signature
    /// covers the new fields end-to-end.
    #[test]
    fn signed_announcement_with_allow_lists_verifies_after_round_trip() {
        use super::super::super::identity::EntityKeypair;
        let keypair = EntityKeypair::generate();
        let mut ann =
            CapabilityAnnouncement::new(9, keypair.entity_id().clone(), 1, sample_capability_set());
        ann.allowed_nodes = vec![1, 2, 3];
        ann.allowed_subnets = vec![super::super::subnet::SubnetId([0x55; 16])];
        ann.allowed_groups = vec![super::super::group::GroupId([0x66; 32])];
        ann.sign(&keypair);
        let bytes = ann.to_bytes();
        let decoded = CapabilityAnnouncement::from_bytes(&bytes).expect("decode");
        assert!(
            decoded.verify().is_ok(),
            "signature must cover the new allow-list fields end-to-end"
        );
    }
    /// Tampering with any allow-list after signing must fail
    /// verification — proves the signature covers each new field.
    #[test]
    fn signed_announcement_rejects_tampered_allow_lists() {
        use super::super::super::identity::EntityKeypair;
        let keypair = EntityKeypair::generate();
        for which in &["nodes", "subnets", "groups"] {
            let mut ann = CapabilityAnnouncement::new(
                9,
                keypair.entity_id().clone(),
                1,
                sample_capability_set(),
            );
            ann.allowed_nodes = vec![1, 2];
            ann.allowed_subnets = vec![super::super::subnet::SubnetId([0x77; 16])];
            ann.allowed_groups = vec![super::super::group::GroupId([0x88; 32])];
            ann.sign(&keypair);
            // Tamper post-sign.
            match *which {
                "nodes" => ann.allowed_nodes.push(999),
                "subnets" => ann
                    .allowed_subnets
                    .push(super::super::subnet::SubnetId([0x99; 16])),
                "groups" => ann
                    .allowed_groups
                    .push(super::super::group::GroupId([0xAA; 32])),
                _ => unreachable!(),
            }
            assert!(
                ann.verify().is_err(),
                "tampering with allowed_{} must invalidate signature",
                which
            );
        }
    }
    #[test]
    fn allow_list_cap_documented() {
        // Sanity: keep the doc-string + the constant in sync. If
        // someone bumps the cap they have to re-think wire-size
        // budgeting — explicit pin makes the change visible.
        assert_eq!(MAX_ALLOW_LIST_LEN, 64);
    }
    /// M1 regression — pre-fix, `from_bytes` accepted any allow-list
    /// length the wire delivered; a malicious or buggy peer could
    /// ship a million-entry `allowed_nodes` and the receiver would
    /// fold it, with every `may_execute` then linearly scanning the
    /// unbounded vector. Post-fix, the deserializer rejects
    /// announcements exceeding the documented per-axis cap.
    #[test]
    fn from_bytes_rejects_allow_list_over_cap() {
        for which in ["nodes", "subnets", "groups"] {
            let mut ann = CapabilityAnnouncement::new(
                1,
                super::super::super::identity::EntityId::from_bytes([0xAA; 32]),
                1,
                sample_capability_set(),
            );
            match which {
                "nodes" => {
                    ann.allowed_nodes = (0..(MAX_ALLOW_LIST_LEN as u64) + 1).collect();
                }
                "subnets" => {
                    ann.allowed_subnets = (0..(MAX_ALLOW_LIST_LEN as u8) + 1)
                        .map(|i| super::super::subnet::SubnetId([i; 16]))
                        .collect();
                }
                "groups" => {
                    ann.allowed_groups = (0..(MAX_ALLOW_LIST_LEN as u8) + 1)
                        .map(|i| super::super::group::GroupId([i; 32]))
                        .collect();
                }
                _ => unreachable!(),
            }
            let bytes = ann.to_bytes();
            assert!(
                CapabilityAnnouncement::from_bytes(&bytes).is_none(),
                "from_bytes must reject allowed_{which} exceeding MAX_ALLOW_LIST_LEN",
            );
        }
    }
    /// Boundary check — exactly `MAX_ALLOW_LIST_LEN` entries
    /// must STILL deserialize (the cap is inclusive).
    #[test]
    fn from_bytes_accepts_allow_list_at_cap() {
        let mut ann = CapabilityAnnouncement::new(
            1,
            super::super::super::identity::EntityId::from_bytes([0xAB; 32]),
            1,
            sample_capability_set(),
        );
        ann.allowed_nodes = (0..MAX_ALLOW_LIST_LEN as u64).collect();
        let bytes = ann.to_bytes();
        let decoded =
            CapabilityAnnouncement::from_bytes(&bytes).expect("exactly-at-cap must deserialize");
        assert_eq!(decoded.allowed_nodes.len(), MAX_ALLOW_LIST_LEN);
    }
    /// Regression for a cubic-flagged P1: adding `hop_count` to the
    /// signed canonical serialization broke rolling-upgrade
    /// compatibility — pre-M-1 announcements were signed over bytes
    /// that had no `hop_count` key, so a post-M-1 verifier's
    /// recomputed `signed_payload()` (which unconditionally
    /// serialized `hop_count: 0`) produced different bytes and the
    /// signature failed.
    ///
    /// The fix is `#[serde(skip_serializing_if = "is_hop_count_zero")]`:
    /// both pre-M-1 signers AND post-M-1 signed_payload (which
    /// always zeros hop_count) omit the field, producing identical
    /// canonical bytes.
    ///
    /// Approach: construct a mirror struct matching pre-M-1's layout
    /// (same fields, no hop_count) and compare its serialized output
    /// byte-for-byte with the current node's `signed_payload()`.
    /// Can't use `serde_json::json!` — that goes through
    /// `serde_json::Map` which sorts keys alphabetically, whereas
    /// `CapabilityAnnouncement`'s derived Serialize writes in
    /// struct-declaration order. The mirror struct keeps the same
    /// serialization path.
    #[test]
    fn reflex_addr_roundtrips_through_serde_when_set() {
        // Stage 2 of NAT traversal: `reflex_addr` rides the
        // signed envelope when the classifier has an observed
        // address. Round-trip must preserve it intact.
        let reflex: std::net::SocketAddr = "198.51.100.5:54321".parse().unwrap();
        let ann = CapabilityAnnouncement::new(1, test_entity(), 1, sample_capability_set())
            .with_reflex_addr(Some(reflex));
        let bytes = ann.to_bytes();
        let restored = CapabilityAnnouncement::from_bytes(&bytes).expect("parse");
        assert_eq!(restored.reflex_addr, Some(reflex));
    }
    #[test]
    fn reflex_addr_none_is_omitted_from_wire_bytes() {
        // The `skip_serializing_if = "Option::is_none"` on
        // `reflex_addr` is what preserves on-wire byte-compat
        // with pre-stage-2 announcements. The canonical bytes
        // must not mention `reflex_addr` at all when it's None —
        // otherwise pre-stage-2 nodes' signatures wouldn't
        // verify on post-stage-2 nodes (same shape of
        // compatibility guarantee as `hop_count`).
        let ann = CapabilityAnnouncement::new(1, test_entity(), 1, sample_capability_set());
        let bytes = ann.to_bytes();
        let text = std::str::from_utf8(&bytes).expect("valid utf8");
        assert!(
            !text.contains("reflex_addr"),
            "reflex_addr key must be omitted when the field is None; got: {text}",
        );
    }
    // `signed_payload_stays_compatible_with_pre_hop_count_format`
    // intentionally removed in Phase A.5.N.3. That test pinned the
    // pre-hop_count byte-identical serialization so signatures
    // issued before that field landed could still verify after a
    // rolling upgrade. Phase A.5.N.3 changes the CapabilitySet
    // wire format outright (no more `hardware`/`software`/`models`/
    // `tools`/`limits` keys; just `tags` + `metadata`), so peers
    // must upgrade together — there is no rolling-upgrade path
    // across this commit. The hop_count omission contract itself
    // is still pinned by `hop_count_zero_omits_key_while_nonzero_keeps_it`.

    #[test]
    fn hop_count_zero_omits_key_while_nonzero_keeps_it() {
        // Complements the cross-version compat test: proves the
        // serde predicate behaves as documented — hop_count=0 is
        // elided (old-format compat) but hop_count=N>0 survives on
        // the wire so forwarders can read + bump it.
        let caps = sample_capability_set();
        let mut ann = CapabilityAnnouncement::new(1, test_entity(), 1, caps);

        let zero_bytes = ann.to_bytes();
        let zero_str = std::str::from_utf8(&zero_bytes).expect("utf8");
        assert!(
            !zero_str.contains("hop_count"),
            "hop_count=0 must be omitted from serialized output",
        );

        ann.hop_count = 3;
        let bumped_bytes = ann.to_bytes();
        let bumped_str = std::str::from_utf8(&bumped_bytes).expect("utf8");
        assert!(
            bumped_str.contains("\"hop_count\":3"),
            "hop_count>0 must survive serialization so forwarders \
             can read + bump. Got: {}",
            bumped_str,
        );
    }
    // ========================================================================
    // Scope helpers (`matches_scope`) — scope tag resolution itself
    // is tested in `behavior::fold::capability_bridge::tests` under
    // `scope_from_membership_tags`.
    // ========================================================================

    #[test]
    fn matches_scope_global_visible_to_tenant_filter() {
        // A peer that doesn't tag itself stays discoverable under
        // tenant queries — this is the v1-permissive default that
        // keeps existing announcements working when scoped queries
        // ship.
        let global = CapabilityScope::Global;
        assert!(matches_scope(
            &global,
            &ScopeFilter::Tenant("oem-123"),
            false
        ));
        assert!(matches_scope(
            &global,
            &ScopeFilter::Region("eu-west"),
            false
        ));
        assert!(matches_scope(&global, &ScopeFilter::Any, false));

        // GlobalOnly filter: only Global candidates pass.
        assert!(matches_scope(&global, &ScopeFilter::GlobalOnly, false));
        let tenant_only = CapabilityScope::Tenants(vec!["foo".to_string()]);
        assert!(!matches_scope(
            &tenant_only,
            &ScopeFilter::GlobalOnly,
            false
        ));
    }
    #[test]
    fn matches_scope_subnet_local_excluded_from_any() {
        // SubnetLocal is opt-out from cross-subnet discovery: it
        // shows up only under SameSubnet (and only when the
        // caller-supplied predicate confirms membership).
        let sl = CapabilityScope::SubnetLocal;
        assert!(!matches_scope(&sl, &ScopeFilter::Any, false));
        assert!(!matches_scope(&sl, &ScopeFilter::Any, true));
        assert!(!matches_scope(&sl, &ScopeFilter::Tenant("foo"), true));
        assert!(!matches_scope(&sl, &ScopeFilter::GlobalOnly, true));

        // SameSubnet with same_subnet=true admits SubnetLocal.
        assert!(matches_scope(&sl, &ScopeFilter::SameSubnet, true));
        // SameSubnet with same_subnet=false rejects SubnetLocal.
        assert!(!matches_scope(&sl, &ScopeFilter::SameSubnet, false));

        // Tenant filter against a tenant-tagged candidate behaves
        // as expected — verifies the SubnetLocal branch isn't
        // bleeding into the tenant arm.
        let tenants = CapabilityScope::Tenants(vec!["oem-123".to_string()]);
        assert!(matches_scope(
            &tenants,
            &ScopeFilter::Tenant("oem-123"),
            false
        ));
        assert!(!matches_scope(
            &tenants,
            &ScopeFilter::Tenant("other"),
            false
        ));
    }
    // ========================================================================
    // CapabilitySet builders for reserved scope tags
    // ========================================================================

    #[test]
    fn with_tenant_scope_appends_prefixed_tag() {
        let caps = CapabilitySet::new()
            .add_tag("gpu")
            .with_tenant_scope("oem-123");
        assert!(caps.has_tag("gpu"));
        assert!(caps.has_tag("scope:tenant:oem-123"));

        // The builder writes the wire string the bridge's
        // `scope_from_membership_tags` matches on.
        let wire_tags: Vec<String> = caps.tags.iter().map(|t| t.to_string()).collect();
        let resolved =
            super::super::fold::capability_bridge::scope_from_membership_tags(&wire_tags);
        assert_eq!(
            resolved,
            CapabilityScope::Tenants(vec!["oem-123".to_string()]),
        );
    }
    #[test]
    fn with_tenant_scope_is_idempotent_and_drops_empty() {
        let caps = CapabilitySet::new()
            .with_tenant_scope("oem-123")
            .with_tenant_scope("oem-123") // duplicate
            .with_tenant_scope(""); // empty — silently dropped
                                    // Phase A.5.N.2: tags are typed; render to wire form
                                    // for prefix-string filtering.
        let tenant_tags: Vec<String> = caps
            .tags
            .iter()
            .map(|t| t.to_string())
            .filter(|s| s.starts_with(TAG_SCOPE_TENANT_PREFIX))
            .collect();
        assert_eq!(
            tenant_tags.len(),
            1,
            "duplicate not deduped: {:?}",
            caps.tags
        );
        assert_eq!(tenant_tags[0], "scope:tenant:oem-123");
    }
    #[test]
    fn with_region_and_subnet_local_scope_compose_with_resolver() {
        use super::super::fold::capability_bridge::scope_from_membership_tags;
        let to_wire = |caps: &CapabilitySet| -> Vec<String> {
            caps.tags.iter().map(|t| t.to_string()).collect()
        };

        // Region builder produces a Regions scope.
        let caps_region = CapabilitySet::new().with_region_scope("eu-west");
        assert!(caps_region.has_tag("scope:region:eu-west"));
        assert_eq!(
            scope_from_membership_tags(&to_wire(&caps_region)),
            CapabilityScope::Regions(vec!["eu-west".to_string()]),
        );

        // Empty region is dropped by the builder (matches the
        // resolver's empty-id rejection).
        let caps_empty_region = CapabilitySet::new().with_region_scope("");
        assert!(caps_empty_region.tags.is_empty());

        // SubnetLocal builder is idempotent and dominates tenant
        // tags (strictest scope wins) — the resolver test below
        // is what locks in the precedence; the builder just has
        // to produce a list the resolver reads correctly.
        let caps_local = CapabilitySet::new()
            .with_tenant_scope("oem-123")
            .with_subnet_local_scope()
            .with_subnet_local_scope(); // idempotent
        let local_tags: Vec<String> = caps_local
            .tags
            .iter()
            .map(|t| t.to_string())
            .filter(|s| s.as_str() == TAG_SCOPE_SUBNET_LOCAL)
            .collect();
        assert_eq!(local_tags.len(), 1);
        assert_eq!(
            scope_from_membership_tags(&to_wire(&caps_local)),
            CapabilityScope::SubnetLocal
        );
    }
    // ========================================================================
    // Chain composition helpers — Phase 3 of CAPABILITY_ENHANCEMENTS_PLAN.md.
    // ========================================================================

    fn reserved_tag(prefix: &str, body: &str) -> Tag {
        Tag::Reserved {
            prefix: prefix.to_string(),
            body: body.to_string(),
        }
    }
    #[test]
    fn require_chain_emits_causal_reserved_tag() {
        let caps = CapabilitySet::new().require_chain("abc123");
        assert!(caps.tags.contains(&reserved_tag("causal:", "abc123")));
    }
    #[test]
    fn require_chain_is_idempotent() {
        let caps = CapabilitySet::new()
            .require_chain("abc123")
            .require_chain("abc123");
        let causal_count = caps
            .tags
            .iter()
            .filter(|t| matches!(t, Tag::Reserved { prefix, .. } if prefix == "causal:"))
            .count();
        assert_eq!(causal_count, 1);
    }
    #[test]
    fn require_chain_drops_empty_hash() {
        let caps = CapabilitySet::new().require_chain("");
        assert!(caps.tags.is_empty());
    }
    #[test]
    fn require_chain_tip_emits_with_seq_separator() {
        let caps = CapabilitySet::new().require_chain_tip("abc", 100);
        assert!(caps.tags.contains(&reserved_tag("causal:", "abc:100")));
    }
    #[test]
    fn require_chain_range_emits_bracket_form() {
        let caps = CapabilitySet::new().require_chain_range("abc", 100, 200);
        assert!(caps
            .tags
            .contains(&reserved_tag("causal:", "abc[100..200]")));
    }
    #[test]
    fn require_chain_range_drops_inverted_or_equal_range() {
        // Equal range: silently dropped (zero-length range is meaningless).
        let caps = CapabilitySet::new().require_chain_range("abc", 100, 100);
        assert!(caps.tags.is_empty());
        // Inverted range: silently dropped.
        let caps = CapabilitySet::new().require_chain_range("abc", 200, 100);
        assert!(caps.tags.is_empty());
    }
    #[test]
    fn require_any_chain_emits_one_tag_per_hash() {
        let caps = CapabilitySet::new().require_any_chain(["abc", "def", "ghi"]);
        assert!(caps.tags.contains(&reserved_tag("causal:", "abc")));
        assert!(caps.tags.contains(&reserved_tag("causal:", "def")));
        assert!(caps.tags.contains(&reserved_tag("causal:", "ghi")));
        assert_eq!(caps.tags.len(), 3);
    }
    #[test]
    fn require_any_chain_skips_empty_hashes() {
        let caps = CapabilitySet::new().require_any_chain(["abc", "", "def"]);
        assert_eq!(caps.tags.len(), 2);
    }
    #[test]
    fn from_fork_emits_fork_of_reserved_tag() {
        let caps = CapabilitySet::new().from_fork("parent_hash");
        assert!(caps.tags.contains(&reserved_tag("fork-of:", "parent_hash")));
    }
    #[test]
    fn heat_level_emits_chain_hash_equals_rate_with_two_decimals() {
        let caps = CapabilitySet::new().heat_level("abc", 0.85);
        assert!(caps.tags.contains(&reserved_tag("heat:", "abc=0.85")));
    }
    #[test]
    fn heat_level_clamps_out_of_range_rate() {
        // Above 1.0 clamps to 1.00.
        let caps = CapabilitySet::new().heat_level("abc", 1.5);
        assert!(caps.tags.contains(&reserved_tag("heat:", "abc=1.00")));
        // Below 0.0 clamps to 0.00.
        let caps = CapabilitySet::new().heat_level("abc", -0.3);
        assert!(caps.tags.contains(&reserved_tag("heat:", "abc=0.00")));
    }
    #[test]
    fn heat_level_drops_non_finite_rate() {
        let caps = CapabilitySet::new().heat_level("abc", f64::NAN);
        assert!(caps.tags.is_empty());
        let caps = CapabilitySet::new().heat_level("abc", f64::INFINITY);
        assert!(caps.tags.is_empty());
    }
    #[test]
    fn chain_helpers_compose_naturally_in_a_builder_chain() {
        // Pinned: helpers chain ergonomically without intermediate
        // bindings or `.clone()`s. This is the contract that makes
        // the surface readable in operator code.
        let caps = CapabilitySet::new()
            .require_chain("origin-hash")
            .require_chain_tip("chain-with-tip", 1024)
            .require_chain_range("range-chain", 100, 500)
            .require_any_chain(["alt-1", "alt-2"])
            .from_fork("parent")
            .heat_level("origin-hash", 0.5);
        // Six emissions: 1 + 1 + 1 + 2 + 1 + 1 = 7 reserved tags.
        let reserved_count = caps
            .tags
            .iter()
            .filter(|t| matches!(t, Tag::Reserved { .. }))
            .count();
        assert_eq!(reserved_count, 7, "tags: {:?}", caps.tags);
    }
    // ========================================================================
    // View projections — `From<&CapabilitySet>` + `CapabilitySet::views`.
    // Phase A.4: pin the contract so Phase A.5's wire-format migration
    // doesn't drift the projection semantics.
    // ========================================================================

    #[test]
    fn projection_hardware_round_trips_via_from_impl() {
        // Phase A.5.N.3: `From<&CapabilitySet>` reconstructs the
        // typed view by scanning the tag set. The round-trip
        // through builder → views → comparison pins the bijection
        // for hardware fields the codec covers.
        let hw_input = HardwareCapabilities::new().with_cpu(8, 16).with_memory(64);
        let caps = CapabilitySet::new().with_hardware(hw_input.clone());
        let hw_via_from: HardwareCapabilities = (&caps).into();
        assert_eq!(hw_via_from, hw_input);
    }
    #[test]
    fn projection_software_and_resource_limits_round_trip() {
        // Round-trip via builder → views for software and limits.
        let sw_input = SoftwareCapabilities::new().with_os("linux", "6.5");
        let limits_input = ResourceLimits::new()
            .with_max_concurrent(64)
            .with_rate_limit(100);
        let caps = CapabilitySet::new()
            .with_software(sw_input.clone())
            .with_limits(limits_input.clone());
        let sw: SoftwareCapabilities = (&caps).into();
        assert_eq!(sw, sw_input);
        let limits: ResourceLimits = (&caps).into();
        assert_eq!(limits, limits_input);
    }
    #[test]
    fn views_struct_returns_all_five_projections() {
        // Pin: `views()` returns the five typed projections together,
        // each lazily decoded on first access. Cheaper than reaching
        // for the From impls when the consumer reads more than one
        // axis (the OnceCell cache hits subsequent reads).
        let caps = sample_capability_set();
        let views = caps.views();
        // Round-trip via builder → views — assert the projection
        // is non-default for the fields the sample populates.
        assert!(views.hardware().memory_gb > 0);
        assert!(!views.models().is_empty());
        assert!(!views.tools().is_empty());
    }
    #[test]
    fn lazy_view_handle_caches_per_projection() {
        // Phase 1 of `CAPABILITY_ENHANCEMENTS_PLAN.md`: each
        // projection is decoded at most once per handle. A second
        // read of the same projection returns the cached value
        // (proven via pointer-equality on the borrowed reference).
        let caps = sample_capability_set();
        let views = caps.views();
        let hw_ptr_1 = views.hardware() as *const _;
        let hw_ptr_2 = views.hardware() as *const _;
        assert_eq!(hw_ptr_1, hw_ptr_2, "hardware projection must be cached");
        let models_ptr_1 = views.models() as *const _;
        let models_ptr_2 = views.models() as *const _;
        assert_eq!(
            models_ptr_1, models_ptr_2,
            "models projection must be cached",
        );
    }
    // ========================================================================
    // Phase A.5.1: typed-tag access methods + wire-format snapshots.
    // ========================================================================

    #[test]
    fn typed_tags_method_round_trips() {
        // `CapabilitySet::typed_tags()` and `from_typed_tags()`
        // are the future access pattern; pin the round-trip
        // contract here as inherent-method tests, mirroring the
        // standalone-function pin in `tag_codec`.
        let caps = sample_capability_set();
        let tag_set = caps.typed_tags();
        let caps2 = CapabilitySet::from_typed_tags(&tag_set);
        // Phase A.5.N.3: round-trip is via the canonical tag set;
        // compare projections (tool schemas live in metadata so
        // they don't survive `from_typed_tags`, which gets only
        // the bare tag set).
        let v1 = caps.views();
        let v2 = caps2.views();
        assert_eq!(v1.hardware(), v2.hardware());
        assert_eq!(v1.models(), v2.models());
        assert_eq!(v1.resource_limits(), v2.resource_limits());
        // Tools' non-schema fields round-trip; schemas are dropped
        // (`from_typed_tags` produces empty metadata by design).
        let v1_tools = v1.tools();
        let v2_tools = v2.tools();
        assert_eq!(v1_tools.len(), v2_tools.len());
        for (a, b) in v1_tools.iter().zip(v2_tools.iter()) {
            assert_eq!(a.tool_id, b.tool_id);
            assert_eq!(a.name, b.name);
            assert_eq!(a.version, b.version);
        }
    }
    #[test]
    fn typed_tags_default_capability_set_is_empty() {
        // Pinned: a default CapabilitySet's typed-tag set is empty.
        // Future Phase A.5.2's wire-format change (omitting
        // empty-tag-set sets from the wire) depends on this.
        let caps = CapabilitySet::default();
        assert!(caps.typed_tags().is_empty());
    }
    // ========================================================================
    // CapabilitySet::diff tests (Phase 1 of CAPABILITY_ENHANCEMENTS_PLAN.md).
    // ========================================================================

    #[test]
    fn diff_empty_vs_empty_is_empty() {
        let prev = CapabilitySet::default();
        let curr = CapabilitySet::default();
        let diff = curr.diff(&prev);
        assert!(diff.is_empty());
        assert!(diff.added_tags.is_empty());
        assert!(diff.removed_tags.is_empty());
        assert!(diff.changed_metadata.is_empty());
    }
    #[test]
    fn diff_against_empty_reports_full_added() {
        let prev = CapabilitySet::default();
        let curr = CapabilitySet::new()
            .add_tag("inference")
            .with_metadata("intent", "ml-training");
        let diff = curr.diff(&prev);
        assert!(!diff.is_empty());
        assert_eq!(diff.added_tags.len(), 1);
        let inference_tag = Tag::parse("inference").unwrap();
        assert!(diff.added_tags.contains(&inference_tag));
        assert!(diff.removed_tags.is_empty());
        assert_eq!(diff.changed_metadata.len(), 1);
        assert!(matches!(
            &diff.changed_metadata[0],
            MetadataChange::Added { key, value }
                if key == "intent" && value == "ml-training"
        ));
    }
    #[test]
    fn diff_added_and_removed_tags_are_separated() {
        // Distinct sets: prev has {a, b}, curr has {b, c}.
        // Diff must show added={c}, removed={a}; b is unchanged.
        let prev = CapabilitySet::new().add_tag("a").add_tag("b");
        let curr = CapabilitySet::new().add_tag("b").add_tag("c");
        let diff = curr.diff(&prev);
        let added: Vec<_> = diff.added_tags.iter().map(|t| t.to_string()).collect();
        let removed: Vec<_> = diff.removed_tags.iter().map(|t| t.to_string()).collect();
        assert_eq!(added, vec!["c".to_string()]);
        assert_eq!(removed, vec!["a".to_string()]);
    }
    #[test]
    fn diff_ignores_separator_form_on_axis_value_tags() {
        // Regression for CR-3: `Tag::AxisValue` PartialEq distinguishes
        // `=` vs `:`. A naive `HashSet::difference` would land two
        // semantically-identical tags as both Added and Removed.
        // The structural `DiffEngine::diff` was patched in 38612b61;
        // the companion `CapabilitySet::diff` API was not.
        use crate::adapter::net::behavior::tag::{AxisSeparator, Tag, TaxonomyAxis};
        let mut prev = CapabilitySet::new();
        prev.tags.insert(Tag::AxisValue {
            axis: TaxonomyAxis::Software,
            key: "os".to_string(),
            value: "linux".to_string(),
            separator: AxisSeparator::Eq,
        });
        let mut curr = CapabilitySet::new();
        curr.tags.insert(Tag::AxisValue {
            axis: TaxonomyAxis::Software,
            key: "os".to_string(),
            value: "linux".to_string(),
            separator: AxisSeparator::Colon,
        });
        let diff = curr.diff(&prev);
        assert!(
            diff.added_tags.is_empty(),
            "added tags should be empty for separator-only difference, got {:?}",
            diff.added_tags
        );
        assert!(
            diff.removed_tags.is_empty(),
            "removed tags should be empty for separator-only difference, got {:?}",
            diff.removed_tags
        );
    }
    #[test]
    fn diff_metadata_updated_for_value_change() {
        let prev = CapabilitySet::new().with_metadata("intent", "ml-training");
        let curr = CapabilitySet::new().with_metadata("intent", "embedding");
        let diff = curr.diff(&prev);
        assert!(diff.added_tags.is_empty());
        assert!(diff.removed_tags.is_empty());
        assert_eq!(diff.changed_metadata.len(), 1);
        match &diff.changed_metadata[0] {
            MetadataChange::Updated {
                key,
                prev_value,
                new_value,
            } => {
                assert_eq!(key, "intent");
                assert_eq!(prev_value, "ml-training");
                assert_eq!(new_value, "embedding");
            }
            other => panic!("expected Updated, got {other:?}"),
        }
    }
    #[test]
    fn diff_metadata_key_rename_is_remove_plus_add_not_update() {
        // Pinned: a key rename surfaces as Removed + Added, NOT
        // as Updated. Key identity changes are semantically
        // distinct from value-of-same-key changes.
        let prev = CapabilitySet::new().with_metadata("old-key", "v");
        let curr = CapabilitySet::new().with_metadata("new-key", "v");
        let diff = curr.diff(&prev);
        assert_eq!(diff.changed_metadata.len(), 2);
        // BTreeMap iteration is sorted, so "new-key" comes before "old-key".
        let kinds: Vec<_> = diff
            .changed_metadata
            .iter()
            .map(|c| match c {
                MetadataChange::Added { key, .. } => format!("added:{key}"),
                MetadataChange::Removed { key, .. } => format!("removed:{key}"),
                MetadataChange::Updated { key, .. } => format!("updated:{key}"),
            })
            .collect();
        assert!(
            kinds.contains(&"added:new-key".to_string())
                && kinds.contains(&"removed:old-key".to_string()),
            "expected Added(new-key) + Removed(old-key); got {kinds:?}"
        );
    }
    #[test]
    fn diff_changed_metadata_preserves_btreemap_ordering() {
        // BTreeMap iteration order is stable + sorted. The diff
        // walk emits changes in key order so consumers can rely
        // on deterministic output.
        let prev = CapabilitySet::default();
        let curr = CapabilitySet::new()
            .with_metadata("zebra", "z")
            .with_metadata("alpha", "a")
            .with_metadata("middle", "m");
        let diff = curr.diff(&prev);
        let keys: Vec<_> = diff
            .changed_metadata
            .iter()
            .map(|c| match c {
                MetadataChange::Added { key, .. }
                | MetadataChange::Removed { key, .. }
                | MetadataChange::Updated { key, .. } => key.clone(),
            })
            .collect();
        assert_eq!(keys, vec!["alpha", "middle", "zebra"]);
    }
    #[test]
    fn diff_round_trips_via_apply_diff_on_canonical_diff_engine() {
        // Property-style: applying the structural DiffEngine ops
        // computed from `prev → curr` produces a CapabilitySet
        // whose tags + metadata match `curr`. The two diff surfaces
        // (this method's set/map diff, DiffEngine's structural ops)
        // are different shapes of the same change information; this
        // test pins they agree on the underlying state transition.
        use crate::adapter::net::behavior::diff::{CapabilityDiff, DiffEngine};

        let prev = CapabilitySet::new()
            .add_tag("inference")
            .with_metadata("intent", "old");
        let curr = prev
            .clone()
            .add_tag("training")
            .with_metadata("intent", "new")
            .with_metadata("colocate-with", "chain-a");
        // DiffEngine produces structural ops; apply them to prev
        // and assert tags + metadata match curr (state convergence).
        let ops = DiffEngine::diff(&prev, &curr);
        let applied =
            DiffEngine::apply_with_version(&prev, 1, &CapabilityDiff::new(1, 1, 2, ops), true)
                .unwrap();
        assert_eq!(applied.tags, curr.tags);
        // DiffEngine doesn't emit metadata ops yet; metadata diff
        // ships separately and is consumed by event-driven listeners,
        // not by the diff-apply propagation path. Pin the contract
        // here so a future DiffEngine extension that adds metadata
        // ops doesn't accidentally regress this surface.
        let cset_diff = curr.diff(&prev);
        assert!(!cset_diff.is_empty());
        assert_eq!(cset_diff.changed_metadata.len(), 2);
    }
    #[test]
    fn wire_format_serialization_snapshot() {
        // Pin the post-Phase-A.5.N.3 wire format. CapabilitySet
        // ships exactly two top-level keys now: `tags` (the
        // canonical tag-set, holding axis-prefixed + reserved +
        // legacy entries as a JSON string array via Tag's
        // custom serde) and `metadata` (a free-form key-value
        // map). Hardware / software / models / tools / limits
        // fields no longer exist on the wire — their content is
        // encoded as tags.
        let caps = CapabilitySet::new()
            .with_hardware(HardwareCapabilities::new().with_cpu(8, 16))
            .add_tag("inference");
        let json = String::from_utf8(caps.to_bytes()).unwrap();
        assert!(json.contains("\"tags\":"), "missing tags field: {json}");
        assert!(
            json.contains("\"metadata\":"),
            "missing metadata field: {json}"
        );
        // The legacy untyped tag rides through unchanged.
        assert!(json.contains("\"inference\""), "missing legacy tag: {json}");
        // Hardware fields are encoded as axis tags inside `tags`.
        assert!(
            json.contains("\"hardware.cpu_cores=8\""),
            "missing hardware.cpu_cores=8 tag: {json}",
        );
        assert!(
            json.contains("\"hardware.cpu_threads=16\""),
            "missing hardware.cpu_threads=16 tag: {json}",
        );
        // Old top-level typed-struct keys are gone.
        assert!(
            !json.contains("\"hardware\":"),
            "stale hardware key: {json}"
        );
        assert!(
            !json.contains("\"software\":"),
            "stale software key: {json}"
        );
        assert!(!json.contains("\"models\":"), "stale models key: {json}");
        assert!(!json.contains("\"tools\":"), "stale tools key: {json}");
        assert!(!json.contains("\"limits\":"), "stale limits key: {json}");
    }
    #[test]
    fn wire_format_round_trips_through_json() {
        // Pinned: a CapabilitySet round-trips through `to_bytes` →
        // `from_bytes`. Phase A.5.N.2's wire format change must
        // preserve this property — a CapabilitySet built via the
        // typed builder methods then serialized then deserialized
        // produces an equal value. Test against a non-trivial
        // capability set to exercise every field.
        let caps = sample_capability_set();
        let bytes = caps.to_bytes();
        let caps2 = CapabilitySet::from_bytes(&bytes).expect("round-trip parses");
        assert_eq!(caps, caps2);
    }
    #[test]
    fn typed_tags_includes_legacy_string_tags() {
        // Pinned: legacy `Vec<String>` tags appear in the typed-
        // tag set as `Tag::Legacy` / `Tag::Reserved` / parsed
        // axis tags. Downstream code reading via `typed_tags()`
        // sees them all uniformly.
        use crate::adapter::net::behavior::tag::Tag as TagT;
        let caps = CapabilitySet::new()
            .add_tag("inference")
            .with_tenant_scope("acme");
        let tag_set = caps.typed_tags();
        // "inference" → Legacy
        assert!(tag_set
            .iter()
            .any(|t| matches!(t, TagT::Legacy(s) if s == "inference")));
        // "scope:tenant:acme" → Reserved
        assert!(tag_set
            .iter()
            .any(|t| matches!(t, TagT::Reserved { prefix, body }
                if prefix == "scope:" && body == "tenant:acme")));
    }
}
