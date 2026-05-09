//! Capability Announcements (CAP-ANN) for Phase 4A.
//!
//! This module provides:
//! - `CapabilitySet` - Structured capability representation
//! - `CapabilityAnnouncement` - Versioned capability broadcast
//! - `CapabilityIndex` - High-performance capability indexing with inverted indexes
//! - `CapabilityFilter` - Query capabilities by various criteria
//!
//! # Performance Targets
//! - Index throughput: 100k+ announcements/s
//! - Query latency (single tag): < 100µs
//! - Query latency (complex filter): < 1ms
//! - Memory per 10k nodes: < 50MB

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::hash::Hash;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

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
    /// VRAM in MB
    pub vram_mb: u32,
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
            vram_mb: 0,
            compute_units: 0,
            tensor_cores: 0,
            fp16_tflops_x10: 0,
        }
    }
}

impl GpuInfo {
    /// Create new GPU info
    pub fn new(vendor: GpuVendor, model: impl Into<String>, vram_mb: u32) -> Self {
        Self {
            vendor,
            model: model.into(),
            vram_mb,
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
    /// Memory in MB (if applicable)
    pub memory_mb: u32,
    /// TOPS (tera operations per second, scaled by 10)
    pub tops_x10: u16,
}

impl AcceleratorInfo {
    /// Create new accelerator info
    pub fn new(accel_type: AcceleratorType, model: impl Into<String>) -> Self {
        Self {
            accel_type,
            model: model.into(),
            memory_mb: 0,
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
    /// Total memory in MB
    pub memory_mb: u32,
    /// GPU info (if present)
    pub gpu: Option<GpuInfo>,
    /// Additional GPUs (for multi-GPU setups)
    pub additional_gpus: Vec<GpuInfo>,
    /// Storage in MB
    pub storage_mb: u64,
    /// Network bandwidth in Mbps
    pub network_mbps: u32,
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
    pub fn with_memory(mut self, memory_mb: u32) -> Self {
        self.memory_mb = memory_mb;
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
    pub fn with_storage(mut self, storage_mb: u64) -> Self {
        self.storage_mb = storage_mb;
        self
    }

    /// Set network bandwidth
    pub fn with_network(mut self, network_mbps: u32) -> Self {
        self.network_mbps = network_mbps;
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
    pub fn total_vram_mb(&self) -> u32 {
        let primary = self.gpu.as_ref().map(|g| g.vram_mb).unwrap_or(0);
        let additional: u32 = self.additional_gpus.iter().map(|g| g.vram_mb).sum();
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
/// [`scope_from_tags`].
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

/// Resolve a list of `CapabilitySet::tags` into the
/// announcer's effective [`CapabilityScope`]. Empty tenant /
/// region values (`scope:tenant:` with no id) are silently dropped
/// — defensive, since reading them as the empty string would let
/// a peer match any tenant query that also had an empty id.
pub(crate) fn scope_from_tags(tags: &[String]) -> CapabilityScope {
    let mut tenants = Vec::new();
    let mut regions = Vec::new();
    let mut subnet_local = false;

    for t in tags {
        if t == TAG_SCOPE_SUBNET_LOCAL {
            subnet_local = true;
        } else if let Some(id) = t.strip_prefix(TAG_SCOPE_TENANT_PREFIX) {
            if !id.is_empty() {
                tenants.push(id.to_string());
            }
        } else if let Some(name) = t.strip_prefix(TAG_SCOPE_REGION_PREFIX) {
            if !name.is_empty() {
                regions.push(name.to_string());
            }
        }
        // `scope:global` is the default; presence is a no-op.
    }

    if subnet_local {
        CapabilityScope::SubnetLocal
    } else {
        match (tenants.is_empty(), regions.is_empty()) {
            (true, true) => CapabilityScope::Global,
            (false, true) => CapabilityScope::Tenants(tenants),
            (true, false) => CapabilityScope::Regions(regions),
            (false, false) => CapabilityScope::TenantsAndRegions { tenants, regions },
        }
    }
}

/// Caller's intent for narrowing peer discovery by reserved scope
/// tags, paired with [`CapabilityIndex::find_nodes_scoped`] /
/// [`CapabilityIndex::find_best_node_scoped`].
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

/// Complete capability set for a node
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CapabilitySet {
    /// Hardware capabilities
    pub hardware: HardwareCapabilities,
    /// Software capabilities
    pub software: SoftwareCapabilities,
    /// Model capabilities
    pub models: Vec<ModelCapability>,
    /// Tool capabilities
    pub tools: Vec<ToolCapability>,
    /// Custom tags for filtering
    pub tags: Vec<String>,
    /// Resource limits
    pub limits: ResourceLimits,
}

impl CapabilitySet {
    /// Create empty capability set
    pub fn new() -> Self {
        Self::default()
    }

    /// Set hardware capabilities
    pub fn with_hardware(mut self, hardware: HardwareCapabilities) -> Self {
        self.hardware = hardware;
        self
    }

    /// Set software capabilities
    pub fn with_software(mut self, software: SoftwareCapabilities) -> Self {
        self.software = software;
        self
    }

    /// Add model capability
    pub fn add_model(mut self, model: ModelCapability) -> Self {
        self.models.push(model);
        self
    }

    /// Add tool capability
    pub fn add_tool(mut self, tool: ToolCapability) -> Self {
        self.tools.push(tool);
        self
    }

    /// Add tag
    pub fn add_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
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
        if !self.tags.iter().any(|t| t == &tag) {
            self.tags.push(tag);
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
        if !self.tags.iter().any(|t| t == &tag) {
            self.tags.push(tag);
        }
        self
    }

    /// Add the `scope:subnet-local` reserved tag, opting this
    /// announcement out of cross-subnet discovery. The strictest
    /// scope wins: any tenant / region tags also present on this
    /// set are ignored by the scope resolver while
    /// `scope:subnet-local` is set. Idempotent.
    pub fn with_subnet_local_scope(mut self) -> Self {
        if !self.tags.iter().any(|t| t == TAG_SCOPE_SUBNET_LOCAL) {
            self.tags.push(TAG_SCOPE_SUBNET_LOCAL.to_string());
        }
        self
    }

    /// Set resource limits
    pub fn with_limits(mut self, limits: ResourceLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Check if has a specific tag
    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t == tag)
    }

    /// Check if has a specific model
    pub fn has_model(&self, model_id: &str) -> bool {
        self.models.iter().any(|m| m.model_id == model_id)
    }

    /// Check if has a specific tool
    pub fn has_tool(&self, tool_id: &str) -> bool {
        self.tools.iter().any(|t| t.tool_id == tool_id)
    }

    /// Check if has GPU
    pub fn has_gpu(&self) -> bool {
        self.hardware.has_gpu()
    }

    /// Get all model IDs
    pub fn model_ids(&self) -> Vec<&str> {
        self.models.iter().map(|m| m.model_id.as_str()).collect()
    }

    /// Get all tool IDs
    pub fn tool_ids(&self) -> Vec<&str> {
        self.tools.iter().map(|t| t.tool_id.as_str()).collect()
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
    /// # use ai2070_net::adapter::net::behavior::capability::CapabilitySet;
    /// let caps = CapabilitySet::new();
    /// let views = caps.views();
    /// let _ = views.hardware;
    /// let _ = views.software;
    /// let _ = views.resource_limits;
    /// let _ = views.models;
    /// let _ = views.tools;
    /// ```
    pub fn views(&self) -> CapabilityViews {
        CapabilityViews {
            hardware: HardwareCapabilities::from(self),
            software: SoftwareCapabilities::from(self),
            resource_limits: ResourceLimits::from(self),
            models: self.models.clone(),
            tools: self.tools.clone(),
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
    // computed on demand (no field change); Phase A.5.2 may
    // introduce internal `tag_set: HashSet<Tag>` storage as a
    // performance optimization. Either way, the surface stays
    // stable.
    //
    // Migration path for downstream code:
    //
    // ```text
    // // Before (typed-struct field access):
    // if caps.hardware.gpu.is_some() { ... }
    // for tag in &caps.tags { ... }
    //
    // // After (typed-tag access):
    // if HardwareCapabilities::from(&caps).gpu.is_some() { ... }
    //   //                                  -- via Phase A.4 helpers
    // for tag in caps.typed_tags() { ... }
    //   //         -- via this method (Phase A.5.1)
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

/// All five typed-view projections of a [`CapabilitySet`].
/// Returned by [`CapabilitySet::views`]; consumers destructure the
/// fields they care about. Same shape across Rust / Node / Python /
/// Go bindings per the SDK plan's view-projection design decision.
#[derive(Debug, Clone, PartialEq)]
pub struct CapabilityViews {
    /// Hardware projection.
    pub hardware: HardwareCapabilities,
    /// Software projection.
    pub software: SoftwareCapabilities,
    /// Resource-limits projection.
    pub resource_limits: ResourceLimits,
    /// Loaded-model projection (`Vec<ModelCapability>` cloned).
    pub models: Vec<ModelCapability>,
    /// Available-tool projection (`Vec<ToolCapability>` cloned).
    pub tools: Vec<ToolCapability>,
}

// ============================================================================
// View projections — `From<&CapabilitySet>` for each typed struct.
//
// Phase A.4 implementation: each impl clones the matching field.
// Phase A.5 will migrate `CapabilitySet`'s wire format to
// `tags: HashSet<Tag>`; these impls become tag-set scans then,
// without changing any call site.
// ============================================================================

impl From<&CapabilitySet> for HardwareCapabilities {
    fn from(caps: &CapabilitySet) -> Self {
        caps.hardware.clone()
    }
}

impl From<&CapabilitySet> for SoftwareCapabilities {
    fn from(caps: &CapabilitySet) -> Self {
        caps.software.clone()
    }
}

impl From<&CapabilitySet> for ResourceLimits {
    fn from(caps: &CapabilitySet) -> Self {
        caps.limits.clone()
    }
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
}

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
    /// [`CapabilityIndex`], short enough that stale state clears on
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
    fn signed_payload(&self) -> Vec<u8> {
        let mut canonical = self.clone();
        canonical.signature = None;
        canonical.hop_count = 0;
        canonical.to_bytes()
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

    /// Deserialize from bytes
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        serde_json::from_slice(data).ok()
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
    /// Minimum memory in MB
    pub min_memory_mb: Option<u32>,
    /// Require GPU
    pub require_gpu: bool,
    /// Specific GPU vendor
    pub gpu_vendor: Option<GpuVendor>,
    /// Minimum VRAM in MB
    pub min_vram_mb: Option<u32>,
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
    pub fn with_min_memory(mut self, mb: u32) -> Self {
        self.min_memory_mb = Some(mb);
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
    pub fn with_min_vram(mut self, mb: u32) -> Self {
        self.min_vram_mb = Some(mb);
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

    /// Check if a capability set matches this filter
    pub fn matches(&self, caps: &CapabilitySet) -> bool {
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

        // Check memory
        if let Some(min_mem) = self.min_memory_mb {
            if caps.hardware.memory_mb < min_mem {
                return false;
            }
        }

        // Check GPU
        if self.require_gpu && !caps.has_gpu() {
            return false;
        }

        // Check GPU vendor
        if let Some(vendor) = self.gpu_vendor {
            if caps.hardware.gpu_vendor() != Some(vendor) {
                return false;
            }
        }

        // Check VRAM
        if let Some(min_vram) = self.min_vram_mb {
            if caps.hardware.total_vram_mb() < min_vram {
                return false;
            }
        }

        // Check context length
        if let Some(min_ctx) = self.min_context_length {
            let has_sufficient = caps.models.iter().any(|m| m.context_length >= min_ctx);
            if !has_sufficient {
                return false;
            }
        }

        // Check modalities
        for modality in &self.require_modalities {
            let has_modality = caps.models.iter().any(|m| m.modalities.contains(modality));
            if !has_modality {
                return false;
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

        let mut score = 1.0;

        // Memory score (normalized to 256GB)
        if self.prefer_more_memory > 0.0 {
            let mem_score = (caps.hardware.memory_mb as f32 / 262144.0).min(1.0);
            score += self.prefer_more_memory * mem_score;
        }

        // VRAM score (normalized to 80GB)
        if self.prefer_more_vram > 0.0 {
            let vram_score = (caps.hardware.total_vram_mb() as f32 / 81920.0).min(1.0);
            score += self.prefer_more_vram * vram_score;
        }

        // Inference speed score (normalized to 1000 tok/s)
        if self.prefer_faster_inference > 0.0 {
            let max_tps: u32 = caps
                .models
                .iter()
                .map(|m| m.tokens_per_sec)
                .max()
                .unwrap_or(0);
            let speed_score = (max_tps as f32 / 1000.0).min(1.0);
            score += self.prefer_faster_inference * speed_score;
        }

        // Loaded model score
        if self.prefer_loaded_models > 0.0 {
            let loaded_count = caps.models.iter().filter(|m| m.loaded).count();
            let loaded_ratio = if caps.models.is_empty() {
                0.0
            } else {
                loaded_count as f32 / caps.models.len() as f32
            };
            score += self.prefer_loaded_models * loaded_ratio;
        }

        score
    }
}

// ============================================================================
// Capability Index
// ============================================================================

/// Indexed node entry
#[derive(Debug, Clone)]
pub struct IndexedNode {
    /// Node ID
    pub node_id: u64,
    /// Capability set
    pub capabilities: CapabilitySet,
    /// Version
    pub version: u64,
    /// When indexed
    pub indexed_at: Instant,
    /// TTL
    pub ttl: Duration,
    /// Peer's public-facing `SocketAddr` as advertised on the
    /// announcement (stage 2 of `NAT_TRAVERSAL_PLAN.md`). `None`
    /// when the sender was compiled without `nat-traversal` or
    /// hasn't finished its classification sweep yet. Consumed by
    /// the rendezvous coordinator (stage 3) — R looks up the
    /// punch target's `reflex_addr` from the index instead of
    /// probing it directly.
    pub reflex_addr: Option<std::net::SocketAddr>,
}

/// High-performance capability index with inverted indexes
pub struct CapabilityIndex {
    /// Node ID -> indexed node
    nodes: DashMap<u64, IndexedNode>,
    /// Inverted index: tag -> set of node IDs
    by_tag: DashMap<String, HashSet<u64>>,
    /// Inverted index: model ID -> set of node IDs
    by_model: DashMap<String, HashSet<u64>>,
    /// Inverted index: tool ID -> set of node IDs
    by_tool: DashMap<String, HashSet<u64>>,
    /// Inverted index: GPU vendor -> set of node IDs
    by_gpu_vendor: DashMap<GpuVendor, HashSet<u64>>,
    /// Inverted index: has GPU -> set of node IDs
    gpu_nodes: DashMap<bool, HashSet<u64>>,
    /// Version tracking
    versions: DashMap<u64, u64>,
    /// Stats
    index_count: AtomicU64,
    query_count: AtomicU64,
}

impl CapabilityIndex {
    /// Create new capability index
    pub fn new() -> Self {
        Self {
            nodes: DashMap::new(),
            by_tag: DashMap::new(),
            by_model: DashMap::new(),
            by_tool: DashMap::new(),
            by_gpu_vendor: DashMap::new(),
            gpu_nodes: DashMap::new(),
            versions: DashMap::new(),
            index_count: AtomicU64::new(0),
            query_count: AtomicU64::new(0),
        }
    }

    /// Index a capability announcement.
    ///
    /// Rejects `is_expired()` up-front, and when computing the
    /// entry's TTL, takes the lesser of "now + ttl_secs" and
    /// "origin_timestamp + ttl_secs" so a clock-skew or replay
    /// scenario doesn't extend the announcement's effective
    /// lifetime past what the origin signed.
    ///
    /// Without these checks, the index would happily accept an
    /// old (still cryptographically valid) announcement and store
    /// `ttl: Duration::from_secs(ann.ttl_secs)` from
    /// `Instant::now()`, making the index entry alive for
    /// `ttl_secs` seconds *from local indexing time*. An attacker
    /// could replay a saved announcement to a fresh node and get
    /// the stale capabilities reinstated with a fresh local
    /// lease — useful for re-introducing a model/tag/scope an
    /// operator deliberately removed, or an old `reflex_addr` to
    /// misdirect NAT traversal.
    pub fn index(&self, ann: CapabilityAnnouncement) {
        // Reject already-expired announcements, but exempt the
        // legitimate `ttl_secs == 0` "announce-and-forget"
        // diagnostic case — those are intentionally short-lived
        // and the next `gc()` sweep evicts them (see
        // `gc_evicts_entries_with_ttl_zero`).
        if ann.ttl_secs > 0 && ann.is_expired() {
            return;
        }

        let node_id = ann.node_id;

        // Hold the versions entry across the whole update. This serializes
        // concurrent indexers for the same node_id and prevents a TOCTOU
        // where thread A's stale version v10 could overwrite thread B's
        // already-committed v11 between the version check and nodes.insert.
        // Lock ordering: versions before nodes (see `remove`).
        use dashmap::mapref::entry::Entry;
        let _version_guard = match self.versions.entry(node_id) {
            Entry::Occupied(mut e) => {
                if ann.version <= *e.get() {
                    return;
                }
                *e.get_mut() = ann.version;
                e.into_ref()
            }
            Entry::Vacant(e) => e.insert(ann.version),
        };

        // Remove old entries from inverted indexes
        if let Some(old) = self.nodes.get(&node_id) {
            self.remove_from_indexes(node_id, &old.capabilities);
        }

        // Add to inverted indexes
        self.add_to_indexes(node_id, &ann.capabilities);

        // Cap the local TTL by the origin's remaining lifetime.
        // `origin_remaining_ns = ann.timestamp_ns +
        // ttl_secs*1e9 - now_ns`. If positive, that's the
        // remaining lifetime according to the origin. The local
        // TTL is `min(local_ttl, origin_remaining)` so a replayed
        // announcement near its origin-side expiry doesn't get a
        // freshly-extended local lease.
        let local_ttl = Duration::from_secs(ann.ttl_secs as u64);
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        let origin_expiry_ns = ann
            .timestamp_ns
            .saturating_add((ann.ttl_secs as u64).saturating_mul(1_000_000_000));
        let origin_remaining_ns = origin_expiry_ns.saturating_sub(now_ns);
        let origin_remaining = Duration::from_nanos(origin_remaining_ns);
        let effective_ttl = local_ttl.min(origin_remaining);

        // Store node
        let indexed = IndexedNode {
            node_id,
            capabilities: ann.capabilities,
            version: ann.version,
            indexed_at: Instant::now(),
            ttl: effective_ttl,
            reflex_addr: ann.reflex_addr,
        };
        self.nodes.insert(node_id, indexed);

        self.index_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Remove node from index
    pub fn remove(&self, node_id: u64) {
        use dashmap::mapref::entry::Entry;
        // Hold the versions shard lock across the whole cleanup so a
        // concurrent `index(ann)` for this node_id serializes against us.
        // Without this, the sequence (versions.remove → index inserts
        // fresh version+node → nodes.remove clobbers the new entry) is
        // observable as a lost update. Lock ordering: versions before
        // nodes, matching `index`.
        let version_entry = self.versions.entry(node_id);
        if let Some((_, node)) = self.nodes.remove(&node_id) {
            self.remove_from_indexes(node_id, &node.capabilities);
        }
        if let Entry::Occupied(e) = version_entry {
            e.remove();
        }
    }

    /// Add node to inverted indexes.
    ///
    /// On the steady-state re-announcement path (peer re-broadcasts
    /// the same `CapabilitySet` periodically), the inverted-index
    /// entries for its tags / models / tools already exist. We do a
    /// borrowing `get_mut` first and only fall through to the
    /// owned-key `entry()` insert on a true cache miss — this skips
    /// the per-tag `String` clone for every existing key. The
    /// fallback is still atomic via DashMap's `entry().or_default()`,
    /// so concurrent first-time inserts of the same key are safe
    /// (the loser pays a redundant clone, which is the original
    /// cost; correctness is unchanged).
    fn add_to_indexes(&self, node_id: u64, caps: &CapabilitySet) {
        // Tags
        for tag in &caps.tags {
            if let Some(mut set) = self.by_tag.get_mut(tag) {
                set.insert(node_id);
            } else {
                self.by_tag.entry(tag.clone()).or_default().insert(node_id);
            }
        }

        // Models
        for model in &caps.models {
            if let Some(mut set) = self.by_model.get_mut(&model.model_id) {
                set.insert(node_id);
            } else {
                self.by_model
                    .entry(model.model_id.clone())
                    .or_default()
                    .insert(node_id);
            }
        }

        // Tools
        for tool in &caps.tools {
            if let Some(mut set) = self.by_tool.get_mut(&tool.tool_id) {
                set.insert(node_id);
            } else {
                self.by_tool
                    .entry(tool.tool_id.clone())
                    .or_default()
                    .insert(node_id);
            }
        }

        // GPU. Key is `bool`, no allocation either way.
        let has_gpu = caps.has_gpu();
        self.gpu_nodes.entry(has_gpu).or_default().insert(node_id);

        if let Some(vendor) = caps.hardware.gpu_vendor() {
            // Vendor key is `Copy` (small enum), so the entry-only
            // form is already allocation-free.
            self.by_gpu_vendor
                .entry(vendor)
                .or_default()
                .insert(node_id);
        }
    }

    /// Remove node from inverted indexes.
    ///
    /// After each `HashSet::remove`, the outer-map entry is dropped
    /// if the inner set is now empty. Without that drop, ephemeral
    /// tag / model-id / tool-id / vendor keys accumulated as empty
    /// `HashSet` shells in the outer `DashMap`s — a slow unbounded
    /// leak over long-running deployments with high peer churn.
    fn remove_from_indexes(&self, node_id: u64, caps: &CapabilitySet) {
        // Tags
        for tag in &caps.tags {
            if let Some(mut set) = self.by_tag.get_mut(tag) {
                set.remove(&node_id);
            }
            self.by_tag.remove_if(tag, |_, set| set.is_empty());
        }

        // Models
        for model in &caps.models {
            if let Some(mut set) = self.by_model.get_mut(&model.model_id) {
                set.remove(&node_id);
            }
            self.by_model
                .remove_if(&model.model_id, |_, set| set.is_empty());
        }

        // Tools
        for tool in &caps.tools {
            if let Some(mut set) = self.by_tool.get_mut(&tool.tool_id) {
                set.remove(&node_id);
            }
            self.by_tool
                .remove_if(&tool.tool_id, |_, set| set.is_empty());
        }

        // GPU (two-value bucket; entries are intentionally permanent
        // because lookups for both `true` and `false` are expected).
        let has_gpu = caps.has_gpu();
        if let Some(mut set) = self.gpu_nodes.get_mut(&has_gpu) {
            set.remove(&node_id);
        }

        if let Some(vendor) = caps.hardware.gpu_vendor() {
            if let Some(mut set) = self.by_gpu_vendor.get_mut(&vendor) {
                set.remove(&node_id);
            }
            self.by_gpu_vendor
                .remove_if(&vendor, |_, set| set.is_empty());
        }
    }

    /// Walk the inverted indexes to build the candidate set narrowed
    /// by `filter`'s indexed predicates (GPU, vendor, tags, models,
    /// tools). Returns:
    ///
    /// - `Some(set)` when at least one indexed predicate applied. The
    ///   set may be empty, in which case downstream filtering trivially
    ///   yields no results.
    /// - `None` when the filter has zero indexed predicates — callers
    ///   fall back to "all nodes" before applying any non-indexed
    ///   predicates.
    fn build_candidate_set(&self, filter: &CapabilityFilter) -> Option<HashSet<u64>> {
        let mut candidates: Option<HashSet<u64>> = None;

        // GPU filter (most selective often)
        if filter.require_gpu {
            match self.gpu_nodes.get(&true) {
                Some(gpu_nodes) => candidates = Some(gpu_nodes.clone()),
                None => return Some(HashSet::new()),
            }
        }

        // GPU vendor filter
        if let Some(vendor) = filter.gpu_vendor {
            match self.by_gpu_vendor.get(&vendor) {
                Some(vendor_nodes) => {
                    candidates = Some(match candidates {
                        Some(c) => c.intersection(&vendor_nodes).copied().collect(),
                        None => vendor_nodes.clone(),
                    });
                }
                None => return Some(HashSet::new()),
            }
        }

        // Tag filter (all required)
        for tag in &filter.require_tags {
            match self.by_tag.get(tag) {
                Some(tag_nodes) => {
                    candidates = Some(match candidates {
                        Some(c) => c.intersection(&tag_nodes).copied().collect(),
                        None => tag_nodes.clone(),
                    });
                }
                None => return Some(HashSet::new()),
            }
        }

        // Model filter (any required)
        if !filter.require_models.is_empty() {
            let mut model_candidates = HashSet::new();
            for model in &filter.require_models {
                if let Some(model_nodes) = self.by_model.get(model) {
                    model_candidates.extend(model_nodes.iter());
                }
            }
            if model_candidates.is_empty() {
                return Some(HashSet::new());
            }
            candidates = Some(match candidates {
                Some(c) => c.intersection(&model_candidates).copied().collect(),
                None => model_candidates,
            });
        }

        // Tool filter (any required)
        if !filter.require_tools.is_empty() {
            let mut tool_candidates = HashSet::new();
            for tool in &filter.require_tools {
                if let Some(tool_nodes) = self.by_tool.get(tool) {
                    tool_candidates.extend(tool_nodes.iter());
                }
            }
            if tool_candidates.is_empty() {
                return Some(HashSet::new());
            }
            candidates = Some(match candidates {
                Some(c) => c.intersection(&tool_candidates).copied().collect(),
                None => tool_candidates,
            });
        }

        candidates
    }

    /// Query nodes by filter
    pub fn query(&self, filter: &CapabilityFilter) -> Vec<u64> {
        self.query_count.fetch_add(1, Ordering::Relaxed);

        let candidates = self
            .build_candidate_set(filter)
            .unwrap_or_else(|| self.nodes.iter().map(|r| *r.key()).collect());

        // Re-check `filter.matches()` against each candidate's current
        // `nodes` entry, even when the filter only constrains indexed
        // dimensions. The inverted indexes update non-atomically with
        // `nodes` (`remove_from_indexes` → `add_to_indexes` →
        // `nodes.insert`), so during a re-announcement that swaps a
        // capability set the inverted index can list a node under a
        // tag/model/tool that the node's current `nodes` entry does
        // not actually advertise. Skipping the re-check on a "fast
        // path" lets that stale index leak into the result. The
        // matches() call here re-verifies under the current
        // capabilities and closes the window.
        candidates
            .into_iter()
            .filter(|&node_id| {
                self.nodes
                    .get(&node_id)
                    .map(|n| filter.matches(&n.capabilities))
                    .unwrap_or(false)
            })
            .collect()
    }

    /// Find best matching node using requirements.
    ///
    /// Iterates the index-narrowed candidate set once, folding the
    /// non-indexed-predicate check (when needed) and the score
    /// computation into a single `nodes.get()` per candidate.
    /// Previously this called [`Self::query`] and then re-fetched
    /// each candidate again to score it — a double DashMap lookup
    /// per candidate that has now collapsed to one.
    pub fn find_best(&self, req: &CapabilityRequirement) -> Option<u64> {
        self.query_count.fetch_add(1, Ordering::Relaxed);

        let candidates = self
            .build_candidate_set(&req.filter)
            .unwrap_or_else(|| self.nodes.iter().map(|r| *r.key()).collect());

        candidates
            .into_iter()
            .filter_map(|node_id| {
                let node = self.nodes.get(&node_id)?;
                // Always re-check the filter under the current
                // capabilities — the inverted indexes update
                // non-atomically with `nodes`, so a stale index can
                // otherwise advance a candidate that no longer
                // matches the filter into the scoring step. See
                // `query()` for the same fix.
                if !req.filter.matches(&node.capabilities) {
                    return None;
                }
                Some((node_id, req.score(&node.capabilities)))
            })
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(node_id, _)| node_id)
    }

    /// Like [`Self::query`], but additionally filters candidates
    /// through a [`ScopeFilter`] derived from each node's
    /// `scope:*` reserved tags. See `docs/SCOPED_CAPABILITIES_PLAN.md`
    /// for the resolution rules.
    ///
    /// `same_subnet_lookup` is invoked at most once per candidate
    /// — only when the filter is [`ScopeFilter::SameSubnet`] or
    /// the candidate resolves to `SubnetLocal` (which always
    /// requires same-subnet membership). The
    /// closure should return `true` when the candidate's subnet
    /// equals the caller's; for the warm-up case where one
    /// side's subnet is unknown, callers default to permissive
    /// (`true`), matching the channel-path warm-up behavior.
    ///
    /// The closure is supplied by the caller because
    /// [`CapabilityIndex`] does not own subnet state — that
    /// lives on `MeshNode::peer_subnets`.
    pub fn find_nodes_scoped(
        &self,
        filter: &CapabilityFilter,
        scope_filter: &ScopeFilter<'_>,
        mut same_subnet_lookup: impl FnMut(u64) -> bool,
    ) -> Vec<u64> {
        let base = self.query(filter);
        base.into_iter()
            .filter(|&node_id| {
                let Some(caps) = self.get(node_id) else {
                    return false;
                };
                let scope = scope_from_tags(&caps.tags);
                let needs_subnet = matches!(scope_filter, ScopeFilter::SameSubnet)
                    || matches!(scope, CapabilityScope::SubnetLocal);
                let same_subnet = if needs_subnet {
                    same_subnet_lookup(node_id)
                } else {
                    false
                };
                matches_scope(&scope, scope_filter, same_subnet)
            })
            .collect()
    }

    /// Scoped variant of [`Self::find_best`]. Same scope-resolution
    /// semantics as [`Self::find_nodes_scoped`]; selection picks the
    /// highest-scoring candidate within the scoped set.
    pub fn find_best_node_scoped(
        &self,
        req: &CapabilityRequirement,
        scope_filter: &ScopeFilter<'_>,
        mut same_subnet_lookup: impl FnMut(u64) -> bool,
    ) -> Option<u64> {
        self.query_count.fetch_add(1, Ordering::Relaxed);

        let candidates = self
            .build_candidate_set(&req.filter)
            .unwrap_or_else(|| self.nodes.iter().map(|r| *r.key()).collect());

        candidates
            .into_iter()
            .filter_map(|node_id| {
                let node = self.nodes.get(&node_id)?;
                if !req.filter.matches(&node.capabilities) {
                    return None;
                }
                let scope = scope_from_tags(&node.capabilities.tags);
                let needs_subnet = matches!(scope_filter, ScopeFilter::SameSubnet)
                    || matches!(scope, CapabilityScope::SubnetLocal);
                let same_subnet = if needs_subnet {
                    same_subnet_lookup(node_id)
                } else {
                    false
                };
                if !matches_scope(&scope, scope_filter, same_subnet) {
                    return None;
                }
                Some((node_id, req.score(&node.capabilities)))
            })
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(node_id, _)| node_id)
    }

    /// Get node capabilities
    pub fn get(&self, node_id: u64) -> Option<CapabilitySet> {
        self.nodes.get(&node_id).map(|n| n.capabilities.clone())
    }

    /// Get the peer's last-advertised reflex address from the
    /// index. Returns `None` when the peer hasn't indexed, or
    /// indexed a version with no `reflex_addr` attached. Consumed
    /// by the rendezvous coordinator (stage 3) — R looks up the
    /// punch target's public `SocketAddr` here.
    pub fn reflex_addr(&self, node_id: u64) -> Option<std::net::SocketAddr> {
        self.nodes.get(&node_id).and_then(|n| n.reflex_addr)
    }

    /// Get all node IDs
    pub fn all_nodes(&self) -> Vec<u64> {
        self.nodes.iter().map(|r| *r.key()).collect()
    }

    /// Get node count
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Garbage collect expired entries
    pub fn gc(&self) -> usize {
        let now = Instant::now();
        let mut removed = 0;

        let expired: Vec<u64> = self
            .nodes
            .iter()
            .filter(|r| now.duration_since(r.indexed_at) >= r.ttl)
            .map(|r| *r.key())
            .collect();

        for node_id in expired {
            self.remove(node_id);
            removed += 1;
        }

        removed
    }

    /// Get statistics
    pub fn stats(&self) -> CapabilityIndexStats {
        CapabilityIndexStats {
            node_count: self.nodes.len(),
            tag_count: self.by_tag.len(),
            model_count: self.by_model.len(),
            tool_count: self.by_tool.len(),
            total_indexed: self.index_count.load(Ordering::Relaxed),
            total_queries: self.query_count.load(Ordering::Relaxed),
        }
    }
}

impl Default for CapabilityIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// Capability index statistics
#[derive(Debug, Clone, Default)]
pub struct CapabilityIndexStats {
    /// Number of indexed nodes
    pub node_count: usize,
    /// Number of unique tags
    pub tag_count: usize,
    /// Number of unique models
    pub model_count: usize,
    /// Number of unique tools
    pub tool_count: usize,
    /// Total announcements indexed
    pub total_indexed: u64,
    /// Total queries processed
    pub total_queries: u64,
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

    fn sample_capability_set() -> CapabilitySet {
        let gpu = GpuInfo::new(GpuVendor::Nvidia, "RTX 4090", 24576)
            .with_compute_units(128)
            .with_tensor_cores(512)
            .with_fp16_tflops(82.5);

        let hardware = HardwareCapabilities::new()
            .with_cpu(16, 32)
            .with_memory(65536)
            .with_gpu(gpu)
            .with_storage(2_000_000)
            .with_network(10000);

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
        assert_eq!(caps.hardware.memory_mb, 65536);
    }

    #[test]
    fn test_capability_set_serialization() {
        let caps = sample_capability_set();
        let bytes = caps.to_bytes();
        let parsed = CapabilitySet::from_bytes(&bytes).unwrap();

        assert_eq!(caps.hardware.memory_mb, parsed.hardware.memory_mb);
        assert_eq!(caps.tags, parsed.tags);
        assert_eq!(caps.models.len(), parsed.models.len());
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
        let filter = CapabilityFilter::new().with_min_memory(32768);
        assert!(filter.matches(&caps));

        let filter = CapabilityFilter::new().with_min_memory(131072);
        assert!(!filter.matches(&caps));

        // Model filter
        let filter = CapabilityFilter::new().require_model("llama-3.1-70b");
        assert!(filter.matches(&caps));

        let filter = CapabilityFilter::new().require_model("gpt-4");
        assert!(!filter.matches(&caps));
    }

    #[test]
    fn test_capability_index() {
        let index = CapabilityIndex::new();

        // Index some nodes
        for i in 0..100 {
            let mut caps = sample_capability_set();
            if i % 2 == 0 {
                caps.tags.push("even".into());
            }
            if i % 3 == 0 {
                caps.tags.push("divisible_by_3".into());
            }

            let ann = CapabilityAnnouncement::new(i, test_entity(), 1, caps);
            index.index(ann);
        }

        assert_eq!(index.len(), 100);

        // Query by tag
        let filter = CapabilityFilter::new().require_tag("even");
        let results = index.query(&filter);
        assert_eq!(results.len(), 50);

        // Query by multiple tags
        let filter = CapabilityFilter::new()
            .require_tag("even")
            .require_tag("divisible_by_3");
        let results = index.query(&filter);
        // Nodes divisible by 6: 0, 6, 12, 18, 24, 30, 36, 42, 48, 54, 60, 66, 72, 78, 84, 90, 96
        assert_eq!(results.len(), 17);

        // Query by GPU
        let filter = CapabilityFilter::new().require_gpu();
        let results = index.query(&filter);
        assert_eq!(results.len(), 100);
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
    fn test_capability_index_version_handling() {
        let index = CapabilityIndex::new();

        // Index version 1
        let caps_v1 = CapabilitySet::new().add_tag("v1");
        let ann_v1 = CapabilityAnnouncement::new(1, test_entity(), 1, caps_v1);
        index.index(ann_v1);

        // Query should find v1 tag
        let filter = CapabilityFilter::new().require_tag("v1");
        assert_eq!(index.query(&filter).len(), 1);

        // Index version 2 (should replace v1)
        let caps_v2 = CapabilitySet::new().add_tag("v2");
        let ann_v2 = CapabilityAnnouncement::new(1, test_entity(), 2, caps_v2);
        index.index(ann_v2);

        // v1 tag should be gone, v2 should be present
        let filter = CapabilityFilter::new().require_tag("v1");
        assert_eq!(index.query(&filter).len(), 0);

        let filter = CapabilityFilter::new().require_tag("v2");
        assert_eq!(index.query(&filter).len(), 1);

        // Older version should be ignored
        let caps_old = CapabilitySet::new().add_tag("old");
        let ann_old = CapabilityAnnouncement::new(1, test_entity(), 1, caps_old);
        index.index(ann_old);

        // v2 should still be present
        let filter = CapabilityFilter::new().require_tag("v2");
        assert_eq!(index.query(&filter).len(), 1);
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

    // ========================================================================
    // CapabilityIndex::gc() boundary + race coverage (TEST_COVERAGE_PLAN §P1-2)
    // ========================================================================

    /// A zero-TTL announcement is evicted on the very next `gc()`
    /// sweep. Zero-TTL is a legitimate operator choice for
    /// "announce-and-forget" diagnostics; the index must respect
    /// it rather than silently promoting zero to some default.
    #[test]
    fn gc_evicts_entries_with_ttl_zero() {
        let index = CapabilityIndex::new();
        let mut ann = CapabilityAnnouncement::new(1, test_entity(), 1, sample_capability_set());
        ann.ttl_secs = 0;
        index.index(ann);

        assert_eq!(index.stats().node_count, 1, "entry should be indexed");
        // No sleep needed — with ttl=0, `now.duration_since(indexed_at)`
        // is already >= ttl on the first gc call.
        let removed = index.gc();
        assert_eq!(removed, 1, "zero-TTL entry must be evicted on first gc");
        assert_eq!(index.stats().node_count, 0);
        assert!(
            index.get(1).is_none(),
            "evicted entry must not be queryable"
        );
    }

    /// A u32::MAX-TTL announcement (~136 years in seconds) must
    /// NOT be evicted on the first gc sweep. Pins that the
    /// `Duration::from_secs(ttl_secs as u64)` conversion doesn't
    /// wrap or overflow — a regression here would produce a
    /// zero-valued `Duration` and evict long-lived entries
    /// immediately.
    #[test]
    fn gc_retains_entries_with_max_ttl_no_wraparound() {
        let index = CapabilityIndex::new();
        let mut ann = CapabilityAnnouncement::new(7, test_entity(), 1, sample_capability_set());
        ann.ttl_secs = u32::MAX;
        index.index(ann);

        assert_eq!(index.stats().node_count, 1);
        let removed = index.gc();
        assert_eq!(
            removed, 0,
            "u32::MAX-TTL entry must not be evicted — regression \
             would indicate the `ttl_secs as u64` widening wrapped \
             to zero and produced a zero-`Duration` ttl",
        );
        assert!(index.get(7).is_some(), "entry still queryable");
    }

    /// Concurrent `index()` on one thread + `gc()` on another —
    /// the two dashmap operations must not corrupt the index or
    /// panic. With a long TTL every indexed entry is gc-safe
    /// (not expired), so gc should never remove anything; we
    /// assert that invariant after the race completes. Guards
    /// the `versions` + `nodes` lock-ordering contract
    /// documented on `remove` (versions before nodes).
    #[test]
    fn gc_and_index_concurrent_race_is_panic_free_and_does_not_evict_live_entries() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let index = Arc::new(CapabilityIndex::new());
        let iters = 500u64;
        // Start barrier: both threads must be at their first loop
        // iteration before either runs. Without it the GC thread
        // could race ahead and finish all 500 gc() sweeps before
        // the indexer's thread even started — a silent green pass
        // that wouldn't have exercised the versions↔nodes
        // lock-ordering at all (cubic-flagged P2).
        let start = Arc::new(Barrier::new(2));

        // Indexer thread: reindex node_id 42 with a bumped
        // version each iteration. TTL default (300s), so no
        // entry is ever actually expired.
        let indexer = {
            let index = index.clone();
            let start = start.clone();
            thread::spawn(move || {
                start.wait();
                for v in 1..=iters {
                    let ann =
                        CapabilityAnnouncement::new(42, test_entity(), v, sample_capability_set());
                    index.index(ann);
                }
            })
        };

        // GC thread: run `gc()` repeatedly while the indexer
        // is active. Count removals — since all entries have a
        // 300-second TTL and the test takes milliseconds,
        // `gc` must return 0 every time.
        let gc_runner = {
            let index = index.clone();
            let start = start.clone();
            thread::spawn(move || {
                start.wait();
                let mut total_removed = 0usize;
                for _ in 0..iters {
                    total_removed += index.gc();
                }
                total_removed
            })
        };

        indexer.join().expect("indexer panicked");
        let gc_removed = gc_runner.join().expect("gc thread panicked");

        assert_eq!(
            gc_removed, 0,
            "gc must not evict any entry during the race — every \
             indexed version has a 300-second TTL and the test \
             completes in milliseconds. A nonzero removal count \
             indicates a lock-ordering bug that lets gc see an \
             indexed-then-still-present entry as expired.",
        );

        // Final state: node 42 is present, at some version
        // between 1 and `iters`. No data structure corruption.
        let final_entry = index.get(42);
        assert!(
            final_entry.is_some(),
            "node 42 must be indexed after the race"
        );
    }

    // ========================================================================
    // Custom TTL coverage (TEST_COVERAGE_PLAN §P3-16)
    //
    // Table-driven cases that exercise TTLs the default-300s unit
    // tests never touch: 0s, 1s, 1h, 1yr, u32::MAX. Two flavors —
    // one drives the index's `gc()` on freshly-indexed entries, the
    // other drives `CapabilityAnnouncement::is_expired()` directly
    // so the "age >= ttl" boundary can be pinned against an exact
    // past timestamp (Instant::now() is not manipulable at test
    // time, but `timestamp_ns` is).
    // ========================================================================

    /// `gc()` on a freshly-indexed entry evicts only when the TTL
    /// is zero. Everything from 1s up is a "not yet expired" case
    /// because less than a second has elapsed between `index()` and
    /// `gc()`. Pins the sign of the comparison in `gc` — a flipped
    /// inequality would evict an entry with a year-long TTL after
    /// one microsecond of wall-clock age.
    #[test]
    fn gc_respects_ttl_bounds_on_freshly_indexed_entries() {
        // (node_id, ttl_secs, expected_evicted_on_immediate_gc)
        //
        // NB: the smallest non-zero TTL is 10 s, not 1 s — a
        // 1 s bound is timing-sensitive (a paused scheduler or
        // CI VM stall between `index()` and `gc()` could push
        // wall-clock age past the boundary and flip the
        // assertion). 10 s leaves comfortable slack for CI
        // under load while still covering the "short, non-zero
        // TTL" class.
        let cases: &[(u64, u32, bool)] = &[
            (100, 0, true),
            (101, 10, false),         // 10 s — short but non-flaky
            (102, 3_600, false),      // 1 hour
            (103, 31_536_000, false), // 1 year
            (104, u32::MAX, false),   // ~136 years
        ];

        for &(node_id, ttl_secs, should_evict) in cases {
            let index = CapabilityIndex::new();
            let mut ann =
                CapabilityAnnouncement::new(node_id, test_entity(), 1, sample_capability_set());
            ann.ttl_secs = ttl_secs;
            index.index(ann);

            let removed = index.gc();
            if should_evict {
                assert_eq!(
                    removed, 1,
                    "TTL={ttl_secs}s: entry must be evicted on immediate gc",
                );
                assert!(
                    index.get(node_id).is_none(),
                    "TTL={ttl_secs}s: evicted entry must not be queryable",
                );
            } else {
                assert_eq!(
                    removed, 0,
                    "TTL={ttl_secs}s: fresh entry must not be evicted on immediate gc",
                );
                assert!(
                    index.get(node_id).is_some(),
                    "TTL={ttl_secs}s: live entry must remain queryable",
                );
            }
        }
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

    /// `with_ttl()` applied after construction must actually change
    /// the announcement's effective lifetime — pins that the
    /// builder-style setter doesn't silently ignore the new value
    /// or leak the `DEFAULT_TTL_SECS` default through.
    #[test]
    fn with_ttl_mutation_round_trips_through_is_expired_and_gc() {
        let ann =
            CapabilityAnnouncement::new(9, test_entity(), 1, sample_capability_set()).with_ttl(0);
        assert_eq!(ann.ttl_secs, 0, "with_ttl must write through");

        let index = CapabilityIndex::new();
        index.index(ann);
        // Immediate gc should evict because `with_ttl(0)` applied.
        assert_eq!(
            index.gc(),
            1,
            "with_ttl(0) must propagate into the indexed entry's TTL \
             so gc treats it the same as a fresh ttl_secs=0 announcement",
        );

        // Long TTL path: with_ttl(u32::MAX) keeps the entry
        // indefinitely on a fresh gc.
        let ann2 = CapabilityAnnouncement::new(10, test_entity(), 1, sample_capability_set())
            .with_ttl(u32::MAX);
        assert_eq!(ann2.ttl_secs, u32::MAX);
        let index = CapabilityIndex::new();
        index.index(ann2);
        assert_eq!(index.gc(), 0, "u32::MAX TTL must survive gc");
        assert!(index.get(10).is_some());
    }

    // ========================================================================
    // replayed/expired announcements must not be admitted
    // ========================================================================

    /// An already-expired announcement (origin timestamp older than
    /// `ttl_secs`) must be rejected by `index()` rather than stored
    /// with a freshly-extended local lease. Pre-fix, `index()` had
    /// no `is_expired()` check, so a captured-and-replayed
    /// announcement reinstated stale capabilities indefinitely on
    /// any node that received it.
    #[test]
    fn index_rejects_already_expired_announcement() {
        let index = CapabilityIndex::new();
        let mut ann = CapabilityAnnouncement::new(1, test_entity(), 1, sample_capability_set());
        // Origin signed this 1 hour ago with a 60s TTL — long expired.
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        ann.timestamp_ns = now_ns.saturating_sub(3600 * 1_000_000_000);
        ann.ttl_secs = 60;
        assert!(ann.is_expired(), "test setup: ann must be expired");

        index.index(ann);

        assert_eq!(
            index.stats().node_count,
            0,
            "expired announcement must not be indexed",
        );
        assert!(index.get(1).is_none());
    }

    /// A near-expiry replay (still cryptographically valid by
    /// `is_expired()`'s inclusive bound, but most of its lifetime
    /// already burned) must have its local TTL clamped by the
    /// origin's remaining lifetime — not reset to a fresh
    /// `ttl_secs` from `Instant::now()`. This pins the
    /// `effective_ttl = local_ttl.min(origin_remaining)` clamp:
    /// pre-fix, an attacker could capture an announcement at
    /// age=ttl-1s and replay it to gain ~ttl seconds of fresh
    /// lease per replay.
    #[test]
    fn index_clamps_local_ttl_to_origin_remaining_lifetime() {
        let index = CapabilityIndex::new();
        let mut ann = CapabilityAnnouncement::new(2, test_entity(), 1, sample_capability_set());
        // Origin signed 200s ago with 300s TTL — 100s remaining.
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        ann.timestamp_ns = now_ns.saturating_sub(200 * 1_000_000_000);
        ann.ttl_secs = 300;
        assert!(!ann.is_expired(), "test setup: ann must still be live");

        index.index(ann);

        // Access the IndexedNode via private `nodes` field — `get()`
        // returns only the CapabilitySet, but the clamp lives
        // on `IndexedNode::ttl`.
        let stored_ttl = index
            .nodes
            .get(&2)
            .map(|n| n.ttl)
            .expect("near-expiry ann must still be admitted");
        // The local TTL should be clamped to ~100s (origin_remaining),
        // not the full 300s. Allow generous slack for clock skew /
        // scheduling between `timestamp_ns` capture and `index()`.
        assert!(
            stored_ttl <= Duration::from_secs(105),
            "effective_ttl must be clamped to origin_remaining (~100s), \
             got {:?} — pre-fix bug would leave ttl=300s",
            stored_ttl,
        );
        assert!(
            stored_ttl >= Duration::from_secs(90),
            "effective_ttl must not over-clamp; expected ~100s, got {:?}",
            stored_ttl,
        );
    }

    /// Zero-TTL announcements remain admitted (and immediately
    /// gc-eligible). The `ttl_secs > 0 && is_expired()` guard in
    /// `index()` exempts them so the documented "announce-and-
    /// forget" diagnostic flow keeps working — guarded by
    /// `gc_evicts_entries_with_ttl_zero` already, but pinned here
    /// alongside the replay-rejection cluster so a future tightening of
    /// the rejection rule can't silently break zero-TTL.
    #[test]
    fn index_admits_zero_ttl_announcement_even_though_is_expired_returns_true() {
        let index = CapabilityIndex::new();
        let mut ann = CapabilityAnnouncement::new(3, test_entity(), 1, sample_capability_set());
        ann.ttl_secs = 0;
        assert!(
            ann.is_expired(),
            "is_expired() returns true for ttl_secs=0 by inclusive-bound rule",
        );

        index.index(ann);

        assert_eq!(
            index.stats().node_count,
            1,
            "zero-TTL exemption must keep announce-and-forget working",
        );
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
    fn index_stores_and_returns_reflex_addr() {
        // Stage 3 (rendezvous): the coordinator looks up the
        // punch target's reflex address in its capability index.
        // Regression-guard the storage path — without it, the
        // coordinator would never find any reflex, effectively
        // disabling the rendezvous optimization.
        let reflex: std::net::SocketAddr = "198.51.100.9:40000".parse().unwrap();
        let ann = CapabilityAnnouncement::new(42, test_entity(), 1, sample_capability_set())
            .with_reflex_addr(Some(reflex));
        let index = CapabilityIndex::new();
        index.index(ann);
        assert_eq!(index.reflex_addr(42), Some(reflex));
        // Unknown node returns None — not a panic, not a default.
        assert_eq!(index.reflex_addr(999), None);
    }

    #[test]
    fn index_reflex_addr_none_when_unset_on_announcement() {
        // A node compiled without nat-traversal (or that hasn't
        // classified yet) announces with `reflex_addr = None`.
        // The index round-trip must preserve that, not invent a
        // bogus default that the coordinator would then try to
        // punch to.
        let ann = CapabilityAnnouncement::new(77, test_entity(), 1, sample_capability_set());
        let index = CapabilityIndex::new();
        index.index(ann);
        assert_eq!(index.reflex_addr(77), None);
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

    #[test]
    fn signed_payload_stays_compatible_with_pre_hop_count_format() {
        use super::super::super::identity::{EntityId, EntityKeypair};

        // Mirror of the pre-M-1 `CapabilityAnnouncement` layout —
        // fields match in declaration order, no `hop_count`.
        #[derive(Serialize)]
        struct PreM1Announcement {
            node_id: u64,
            entity_id: EntityId,
            version: u64,
            timestamp_ns: u64,
            ttl_secs: u32,
            capabilities: CapabilitySet,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            signature: Option<Signature64>,
        }

        let keypair = EntityKeypair::generate();
        let caps = sample_capability_set();

        let ann = CapabilityAnnouncement::new(1, keypair.entity_id().clone(), 1, caps.clone());
        let pre_m1 = PreM1Announcement {
            node_id: ann.node_id,
            entity_id: ann.entity_id.clone(),
            version: ann.version,
            timestamp_ns: ann.timestamp_ns,
            ttl_secs: ann.ttl_secs,
            capabilities: ann.capabilities.clone(),
            signature: None,
        };
        let pre_m1_bytes = serde_json::to_vec(&pre_m1).expect("pre-M-1 serialize");

        // Post-M-1 signed_payload — clones, zeros hop_count, sets
        // signature=None, serializes via the derived Serialize
        // (same struct-order path as PreM1Announcement).
        let new_signed = ann.signed_payload();

        assert_eq!(
            pre_m1_bytes,
            new_signed,
            "signed_payload bytes must be byte-identical to pre-M-1 \
             serialization — otherwise signatures issued before M-1 \
             fail verification after a rolling upgrade.\n  \
             pre-M-1:  {}\n  post-M-1: {}",
            std::str::from_utf8(&pre_m1_bytes).unwrap_or("<non-utf8>"),
            std::str::from_utf8(&new_signed).unwrap_or("<non-utf8>"),
        );
        assert!(
            !std::str::from_utf8(&new_signed)
                .unwrap()
                .contains("hop_count"),
            "signed_payload must not contain 'hop_count' when zero",
        );

        // End-to-end: sign with pre-M-1 bytes (what an old node
        // would have produced), construct a wire payload carrying
        // that signature, parse via post-M-1 `from_bytes`, and
        // verify. Must succeed.
        let sig = keypair.sign(&pre_m1_bytes);
        let signed_mirror = PreM1Announcement {
            node_id: ann.node_id,
            entity_id: ann.entity_id.clone(),
            version: ann.version,
            timestamp_ns: ann.timestamp_ns,
            ttl_secs: ann.ttl_secs,
            capabilities: ann.capabilities.clone(),
            signature: Some(Signature64(sig.to_bytes())),
        };
        let wire_bytes = serde_json::to_vec(&signed_mirror).expect("serialize wire");
        let parsed = CapabilityAnnouncement::from_bytes(&wire_bytes)
            .expect("post-M-1 parses pre-M-1 wire format");
        assert_eq!(parsed.hop_count, 0);
        assert!(
            parsed.verify().is_ok(),
            "signature computed over pre-M-1 bytes must still verify \
             on a post-M-1 node — rolling-upgrade compatibility",
        );
    }

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

    /// `query()` re-checks `filter.matches()` per candidate against
    /// the live `nodes` entry, so a filter with an always-true
    /// non-indexed predicate (e.g., `min_memory_mb = 0`) must produce
    /// the same result as the equivalent indexed-only filter. This
    /// pins the equivalence under semantically-redundant predicate
    /// expansions and would catch a future regression that special-
    /// cased the index-only path again without re-running matches().
    #[test]
    fn query_indexed_only_matches_with_redundant_non_indexed_predicate() {
        let index = CapabilityIndex::new();

        for i in 0..30u64 {
            let mut caps = sample_capability_set();
            // sample_capability_set already has tag "inference" + "gpu"; vary
            // additional tags so filters discriminate.
            if i % 2 == 0 {
                caps.tags.push("even".into());
            }
            if i % 3 == 0 {
                caps.tags.push("triple".into());
            }
            let ann = CapabilityAnnouncement::new(i, test_entity(), 1, caps);
            index.index(ann);
        }

        let indexed_only = CapabilityFilter::new()
            .require_tag("even")
            .require_tag("inference");
        // Same predicates plus an always-true non-indexed predicate.
        // `memory_mb` is unsigned so `>= 0` is trivially true.
        let mut with_non_indexed = indexed_only.clone();
        with_non_indexed.min_memory_mb = Some(0);

        let mut a: Vec<u64> = index.query(&indexed_only);
        let mut b: Vec<u64> = index.query(&with_non_indexed);
        a.sort();
        b.sort();
        assert_eq!(
            a, b,
            "adding an always-true predicate must not change the result set"
        );
        assert!(!a.is_empty(), "sample data must produce non-empty results");
    }

    /// After the `find_best()` refactor that folds the index lookup
    /// and the score lookup into a single pass, the chosen node must
    /// still match what `query()` returns intersected with the
    /// highest-scoring candidate. Pins the contract: any future
    /// re-derivation of the candidate set must keep `find_best`'s
    /// answer inside `query`'s result set.
    #[test]
    fn find_best_returns_a_member_of_query_results() {
        let index = CapabilityIndex::new();

        for i in 0..20u64 {
            let mut caps = sample_capability_set();
            // Discriminator tag.
            if i % 4 == 0 {
                caps.tags.push("preferred".into());
            }
            // Vary memory so `prefer_memory` produces a real ordering.
            caps.hardware.memory_mb = 1024 * (i as u32 + 1);
            let ann = CapabilityAnnouncement::new(i, test_entity(), 1, caps);
            index.index(ann);
        }

        let filter = CapabilityFilter::new().require_tag("preferred");
        let req = CapabilityRequirement::from_filter(filter.clone()).prefer_memory(1.0);

        let candidates = index.query(&filter);
        let chosen = index
            .find_best(&req)
            .expect("non-empty candidate set must yield a winner");

        assert!(
            candidates.contains(&chosen),
            "find_best returned {} which is not in query() candidates {:?}",
            chosen,
            candidates,
        );

        // With `prefer_memory(1.0)` and our memory_mb assignment, the
        // largest-memory candidate must win — pins the score path.
        let expected_winner = candidates
            .iter()
            .max_by_key(|&&id| {
                index
                    .nodes
                    .get(&id)
                    .map(|n| n.capabilities.hardware.memory_mb)
                    .unwrap_or(0)
            })
            .copied()
            .expect("non-empty candidates");
        assert_eq!(
            chosen, expected_winner,
            "find_best must pick the highest-memory candidate under prefer_memory(1.0)"
        );
    }

    #[test]
    fn test_regression_query_rejects_stale_inverted_index_entry() {
        // Regression: `query()` had a fast path that, when the filter
        // only constrained indexed dimensions, returned every candidate
        // produced by the inverted indexes after only a `contains_key`
        // presence check on `nodes`. The inverted indexes update
        // non-atomically with `nodes` (`remove_from_indexes(old)` →
        // `add_to_indexes(new)` → `nodes.insert(new)`), so during a
        // capability re-announcement that swaps a tag the inverted
        // index could already advertise the node under the new tag
        // while `nodes` still held the old `CapabilitySet`. The fast
        // path then leaked the node into a query that did not actually
        // match its current capabilities.
        //
        // The fix removes the fast path and always re-runs
        // `filter.matches()` against the current capabilities. This
        // test deterministically reproduces the inconsistent state by
        // directly manipulating the private indexes — no thread race
        // needed.
        let index = CapabilityIndex::new();

        // Step 1: index the node honestly via the public API with
        // tags = ["alpha"].
        let caps_alpha = CapabilitySet::new().add_tag("alpha");
        let ann = CapabilityAnnouncement::new(1, test_entity(), 1, caps_alpha);
        index.index(ann);

        // Sanity: alpha matches, beta does not.
        let filter_alpha = CapabilityFilter::new().require_tag("alpha");
        let filter_beta = CapabilityFilter::new().require_tag("beta");
        assert_eq!(index.query(&filter_alpha), vec![1]);
        assert!(index.query(&filter_beta).is_empty());

        // Step 2: simulate the mid-reindex race window. Move node 1
        // from `by_tag["alpha"]` to `by_tag["beta"]` WITHOUT updating
        // `nodes`. This is exactly the state the inverted-index update
        // produces between `add_to_indexes(new)` and
        // `nodes.insert(new)` when "alpha"→"beta" tags swap.
        //
        // Faithful to production: `remove_from_indexes` drops the
        // outer-map entry once its inner set is empty (the `remove_if`
        // call that prevents an unbounded leak of empty `HashSet`
        // shells). Without that drop here, the simulation would leave
        // `by_tag["alpha"]` as an empty entry — observable to
        // `build_candidate_set` as `Some(empty)` instead of the real
        // `None`, which paper-thinly changes the candidate-set
        // construction shape compared to what production emits.
        index
            .by_tag
            .get_mut("alpha")
            .expect("alpha tag entry was indexed")
            .remove(&1);
        index.by_tag.remove_if("alpha", |_, set| set.is_empty());
        index
            .by_tag
            .entry("beta".to_string())
            .or_default()
            .insert(1);

        // Step 3: with the fix, query(beta) does NOT return node 1
        // because nodes[1] still has tags=["alpha"] which does not
        // match beta. The buggy fast path would return it via
        // contains_key alone.
        assert!(
            index.query(&filter_beta).is_empty(),
            "query(beta) leaked a node whose nodes[] entry does not advertise beta"
        );

        // Step 4: same invariant for find_best — its previous
        // implementation gated `filter.matches()` on
        // `needs_full_check()` and skipped the re-check on
        // index-only filters, surfacing the same leak as a chosen
        // node that did not actually match.
        let req = CapabilityRequirement::from_filter(filter_beta.clone());
        assert!(
            index.find_best(&req).is_none(),
            "find_best(beta) leaked a non-matching node from the stale inverted index"
        );
    }

    // ========================================================================
    // Scope helpers (`scope_from_tags` + `matches_scope`)
    // ========================================================================

    #[test]
    fn scope_from_tags_no_scope_tag_is_global() {
        assert!(matches!(scope_from_tags(&[]), CapabilityScope::Global));
        assert!(matches!(
            scope_from_tags(&["gpu".to_string(), "model:llama3".to_string()]),
            CapabilityScope::Global
        ));
        // Explicit `scope:global` resolves the same as no tag.
        assert!(matches!(
            scope_from_tags(&[TAG_SCOPE_GLOBAL.to_string()]),
            CapabilityScope::Global
        ));
    }

    #[test]
    fn scope_from_tags_subnet_local_wins() {
        // Even with tenants and regions present, `subnet-local` is
        // the strictest form and dominates.
        let tags = vec![
            TAG_SCOPE_SUBNET_LOCAL.to_string(),
            format!("{TAG_SCOPE_TENANT_PREFIX}foo"),
            format!("{TAG_SCOPE_REGION_PREFIX}eu-west"),
        ];
        assert_eq!(scope_from_tags(&tags), CapabilityScope::SubnetLocal);
    }

    #[test]
    fn scope_from_tags_multiple_tenants() {
        let tags = vec![
            format!("{TAG_SCOPE_TENANT_PREFIX}a"),
            format!("{TAG_SCOPE_TENANT_PREFIX}b"),
            "gpu".to_string(),
        ];
        match scope_from_tags(&tags) {
            CapabilityScope::Tenants(ts) => {
                assert_eq!(ts, vec!["a".to_string(), "b".to_string()]);
            }
            other => panic!("expected Tenants, got {other:?}"),
        }

        // Empty tenant id is silently dropped.
        let tags = vec![
            format!("{TAG_SCOPE_TENANT_PREFIX}"),
            format!("{TAG_SCOPE_TENANT_PREFIX}real"),
        ];
        match scope_from_tags(&tags) {
            CapabilityScope::Tenants(ts) => assert_eq!(ts, vec!["real".to_string()]),
            other => panic!("expected Tenants, got {other:?}"),
        }
    }

    #[test]
    fn scope_from_tags_tenants_and_regions() {
        let tags = vec![
            format!("{TAG_SCOPE_TENANT_PREFIX}oem-123"),
            format!("{TAG_SCOPE_REGION_PREFIX}eu-west"),
        ];
        match scope_from_tags(&tags) {
            CapabilityScope::TenantsAndRegions { tenants, regions } => {
                assert_eq!(tenants, vec!["oem-123".to_string()]);
                assert_eq!(regions, vec!["eu-west".to_string()]);
            }
            other => panic!("expected TenantsAndRegions, got {other:?}"),
        }
    }

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

        // The tag list resolves through `scope_from_tags` to the
        // expected variant — proves the builder writes the form
        // the resolver matches on.
        assert_eq!(
            scope_from_tags(&caps.tags),
            CapabilityScope::Tenants(vec!["oem-123".to_string()]),
        );
    }

    #[test]
    fn with_tenant_scope_is_idempotent_and_drops_empty() {
        let caps = CapabilitySet::new()
            .with_tenant_scope("oem-123")
            .with_tenant_scope("oem-123") // duplicate
            .with_tenant_scope(""); // empty — silently dropped
        let tenant_tags: Vec<&String> = caps
            .tags
            .iter()
            .filter(|t| t.starts_with(TAG_SCOPE_TENANT_PREFIX))
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
        // Region builder produces a Regions scope.
        let caps_region = CapabilitySet::new().with_region_scope("eu-west");
        assert!(caps_region.has_tag("scope:region:eu-west"));
        assert_eq!(
            scope_from_tags(&caps_region.tags),
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
        let local_tags: Vec<&String> = caps_local
            .tags
            .iter()
            .filter(|t| t.as_str() == TAG_SCOPE_SUBNET_LOCAL)
            .collect();
        assert_eq!(local_tags.len(), 1);
        assert_eq!(
            scope_from_tags(&caps_local.tags),
            CapabilityScope::SubnetLocal
        );
    }

    /// Repro for the failing Go `TestHardwareAndGpuFilter_Matches`:
    /// announce a node with NVIDIA GPU + memory, then query for
    /// `RequireGPU + GPUVendor=Nvidia + MinVRAMMB + MinMemoryMB`.
    /// Self-match must succeed.
    #[test]
    fn hardware_and_gpu_filter_self_matches() {
        let gpu = GpuInfo::new(GpuVendor::Nvidia, "h100", 81920);
        let hw = HardwareCapabilities::new()
            .with_cpu(16, 16)
            .with_memory(65536)
            .with_gpu(gpu);
        let caps = CapabilitySet::new().with_hardware(hw).add_tag("gpu");

        let index = CapabilityIndex::new();
        let ann = CapabilityAnnouncement::new(42u64, test_entity(), 1, caps);
        index.index(ann);

        let filter = CapabilityFilter::new()
            .require_gpu()
            .with_gpu_vendor(GpuVendor::Nvidia)
            .with_min_vram(40_000)
            .with_min_memory(32_768);
        let hits = index.query(&filter);
        assert!(hits.contains(&42u64), "self-match expected, got {:?}", hits);
    }

    /// Repro that exercises announcement signing + clone, mirroring
    /// what `MeshNode::announce_capabilities` does before calling
    /// `CapabilityIndex::index`. Catches any mutation of the GPU
    /// vendor field across that path.
    #[test]
    fn hardware_and_gpu_filter_self_matches_after_sign_and_clone() {
        use super::super::super::identity::EntityKeypair;
        let keypair = EntityKeypair::generate();

        let gpu = GpuInfo::new(GpuVendor::Nvidia, "h100", 81920);
        let hw = HardwareCapabilities::new()
            .with_cpu(16, 16)
            .with_memory(65536)
            .with_gpu(gpu);
        let caps = CapabilitySet::new().with_hardware(hw).add_tag("gpu");

        let mut ann = CapabilityAnnouncement::new(42u64, keypair.entity_id().clone(), 1, caps);
        ann.sign(&keypair);

        let index = CapabilityIndex::new();
        index.index(ann.clone());

        let filter = CapabilityFilter::new()
            .require_gpu()
            .with_gpu_vendor(GpuVendor::Nvidia)
            .with_min_vram(40_000)
            .with_min_memory(32_768);
        let hits = index.query(&filter);
        assert!(hits.contains(&42u64), "self-match expected, got {:?}", hits);
    }

    // ========================================================================
    // View projections — `From<&CapabilitySet>` + `CapabilitySet::views`.
    // Phase A.4: pin the contract so Phase A.5's wire-format migration
    // doesn't drift the projection semantics.
    // ========================================================================

    #[test]
    fn projection_hardware_round_trips_via_from_impl() {
        let caps = sample_capability_set();
        let hw_via_from: HardwareCapabilities = (&caps).into();
        // Phase A.4: trivial clone of the field. Phase A.5 will
        // reconstruct from the tag set, but the Eq comparison against
        // the original field must continue to hold.
        assert_eq!(hw_via_from, caps.hardware);
    }

    #[test]
    fn projection_software_and_resource_limits_round_trip() {
        let caps = sample_capability_set();
        let sw: SoftwareCapabilities = (&caps).into();
        assert_eq!(sw, caps.software);
        let limits: ResourceLimits = (&caps).into();
        assert_eq!(limits, caps.limits);
    }

    #[test]
    fn views_struct_returns_all_five_projections() {
        // Pin: `views()` returns the five typed projections together.
        // Cheaper than calling each From impl individually when a
        // consumer reads more than one view.
        let caps = sample_capability_set();
        let views = caps.views();
        assert_eq!(views.hardware, caps.hardware);
        assert_eq!(views.software, caps.software);
        assert_eq!(views.resource_limits, caps.limits);
        assert_eq!(views.models, caps.models);
        assert_eq!(views.tools, caps.tools);
    }

    #[test]
    fn views_clone_is_independent_of_caps() {
        // Pin: dropping the original `caps` after `views()` doesn't
        // dangle the views — they're owned clones, not references.
        let views = {
            let caps = sample_capability_set();
            caps.views()
        };
        // If `views` held references into `caps`, this would be a
        // dangling-reference compile error. The test passes by
        // virtue of compiling; the assert is just to use `views`.
        assert!(views.hardware.cpu_cores > 0);
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
        // Hardware / models / tools / limits round-trip
        // byte-for-byte (per Phase A.5.0c).
        assert_eq!(caps.hardware, caps2.hardware);
        assert_eq!(caps.models, caps2.models);
        assert_eq!(caps.tools, caps2.tools);
        assert_eq!(caps.limits, caps2.limits);
    }

    #[test]
    fn typed_tags_default_capability_set_is_empty() {
        // Pinned: a default CapabilitySet's typed-tag set is empty.
        // Future Phase A.5.2's wire-format change (omitting
        // empty-tag-set sets from the wire) depends on this.
        let caps = CapabilitySet::default();
        assert!(caps.typed_tags().is_empty());
    }

    #[test]
    fn wire_format_serialization_snapshot() {
        // Pin the current JSON wire-format shape so Phase A.5.2's
        // field-set migration is loud. If this test fails after
        // a CapabilitySet field change, the diff IS the wire-
        // format break and downstream consumers need migration.
        //
        // Snapshot a minimal CapabilitySet (one tag, one cpu
        // count) so the snapshot is short + readable. Full-set
        // snapshots would be brittle (every model/tool field
        // bumping the snapshot for cosmetic reasons).
        let caps = CapabilitySet::new()
            .with_hardware(HardwareCapabilities::new().with_cpu(8, 16))
            .add_tag("inference");
        let json = String::from_utf8(caps.to_bytes()).unwrap();
        // Pin the exact field-list shape. The current wire format
        // is `{ "hardware": {...}, "software": {...}, "models":
        // [...], "tools": [...], "tags": [...], "limits": {...} }`.
        // After Phase A.5.2 the shape becomes `{ "tags": [...],
        // "metadata": {...} }` (Phase C adds metadata). This test
        // failing after that change IS the migration signal —
        // downstream readers need to be updated in lockstep.
        assert!(json.contains("\"hardware\":"), "missing hardware field: {json}");
        assert!(json.contains("\"software\":"), "missing software field: {json}");
        assert!(json.contains("\"models\":"), "missing models field: {json}");
        assert!(json.contains("\"tools\":"), "missing tools field: {json}");
        assert!(json.contains("\"tags\":[\"inference\"]"), "missing tags field: {json}");
        assert!(json.contains("\"limits\":"), "missing limits field: {json}");
    }

    #[test]
    fn wire_format_round_trips_through_json() {
        // Pinned: a CapabilitySet round-trips through `to_bytes` →
        // `from_bytes`. Phase A.5.2's wire format change must
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
