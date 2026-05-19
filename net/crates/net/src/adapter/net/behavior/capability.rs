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
use std::cell::OnceCell;
use std::collections::{BTreeMap, HashSet};
use std::hash::Hash;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

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

/// Resolve a `CapabilitySet::tags` set into the announcer's
/// effective [`CapabilityScope`]. Empty tenant / region values
/// (`scope:tenant:` with no id) are silently dropped — defensive,
/// since reading them as the empty string would let a peer match
/// any tenant query that also had an empty id.
///
/// Phase A.5.N.2: signature now takes `&HashSet<Tag>`. Inspects
/// `Tag::Reserved` variants where prefix is `scope:`. The body
/// of those reserved tags is the post-`scope:` substring, e.g.
/// `tenant:foo` / `region:eu-west` / `subnet-local`.
pub(crate) fn scope_from_tags(tags: &HashSet<Tag>) -> CapabilityScope {
    let mut tenants = Vec::new();
    let mut regions = Vec::new();
    let mut subnet_local = false;

    for tag in tags {
        let Tag::Reserved { prefix, body } = tag else {
            continue;
        };
        if prefix.as_str() != "scope:" {
            continue;
        }
        if body == "subnet-local" {
            subnet_local = true;
        } else if let Some(id) = body.strip_prefix("tenant:") {
            if !id.is_empty() {
                tenants.push(id.to_string());
            }
        } else if let Some(name) = body.strip_prefix("region:") {
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
    pub fn add_tool(mut self, tool: ToolCapability) -> Self {
        let mut tools = self.views().tools().clone();
        tools.push(tool);
        self.set_tools(tools);
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
        use crate::adapter::net::behavior::tag::TaxonomyAxis;
        self.tags.iter().any(|tag| {
            let Some(key) = tag.axis_key() else {
                return false;
            };
            if key.axis != TaxonomyAxis::Software {
                return false;
            }
            let Some(rest) = key.key.strip_prefix("model.") else {
                return false;
            };
            let Some((_idx, sub)) = rest.split_once('.') else {
                return false;
            };
            sub == "id" && tag.value() == Some(model_id)
        })
    }

    /// Check if has a specific tool.
    ///
    /// Phase A.5.N.3: scans for `software.tool.<i>.tool_id=<tool_id>`
    /// directly in the canonical tag set.
    pub fn has_tool(&self, tool_id: &str) -> bool {
        use crate::adapter::net::behavior::tag::TaxonomyAxis;
        self.tags.iter().any(|tag| {
            let Some(key) = tag.axis_key() else {
                return false;
            };
            if key.axis != TaxonomyAxis::Software {
                return false;
            }
            let Some(rest) = key.key.strip_prefix("tool.") else {
                return false;
            };
            let Some((_idx, sub)) = rest.split_once('.') else {
                return false;
            };
            sub == "tool_id" && tag.value() == Some(tool_id)
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
            .get_or_init(|| sorted_tag_vec(&self.caps.tags))
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
        crate::adapter::net::behavior::tag_codec::hardware_from_tags(&sorted_tag_vec(&caps.tags))
    }
}

impl From<&CapabilitySet> for SoftwareCapabilities {
    fn from(caps: &CapabilitySet) -> Self {
        crate::adapter::net::behavior::tag_codec::software_from_tags(&sorted_tag_vec(&caps.tags))
    }
}

impl From<&CapabilitySet> for ResourceLimits {
    fn from(caps: &CapabilitySet) -> Self {
        crate::adapter::net::behavior::tag_codec::resource_limits_from_tags(&sorted_tag_vec(
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
    /// local use (`intent`, `colocate-with`, `priority`, `owner`,
    /// `tool::*`, …). Call this on every announcement decoded from
    /// an inbound peer before its metadata is consulted by greedy
    /// admission, placement scoring, or anything else that lets a
    /// metadata value steer substrate decisions: pre-fix a peer
    /// could stamp `intent = "high-priority-tenant-X"` on its own
    /// announcement and steer the receiver's admission to itself.
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

        // Phase 1 (lazy): each `views.X()` decodes only the
        // projection it needs. `views.hardware()` does NOT force
        // the model decoder; `views.models()` does NOT force
        // hardware. Predicates that early-return on a hardware
        // check pay zero cost for unused axes.
        let views = caps.views();

        // Check memory
        if let Some(min_mem) = self.min_memory_gb {
            if views.hardware().memory_gb < min_mem {
                return false;
            }
        }

        // Check GPU
        if self.require_gpu && !caps.has_gpu() {
            return false;
        }

        // Check GPU vendor
        if let Some(vendor) = self.gpu_vendor {
            if views.hardware().gpu_vendor() != Some(vendor) {
                return false;
            }
        }

        // Check VRAM
        if let Some(min_vram) = self.min_vram_gb {
            if views.hardware().total_vram_gb() < min_vram {
                return false;
            }
        }

        // Check context length
        if let Some(min_ctx) = self.min_context_length {
            let has_sufficient = views.models().iter().any(|m| m.context_length >= min_ctx);
            if !has_sufficient {
                return false;
            }
        }

        // Check modalities
        for modality in &self.require_modalities {
            let has_modality = views
                .models()
                .iter()
                .any(|m| m.modalities.contains(modality));
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
    /// Wire-form origin_hash for this node's entity: the low
    /// 32 bits of `EntityId::origin_hash()`, zero-extended back
    /// to `u64` so it matches the receiver-side
    /// `NetHeader::origin_hash.into()` projection inside the
    /// mesh dispatch path. Cached at index time so
    /// [`CapabilityIndex::remove`] can clean up the
    /// `by_origin_hash` side index without re-deriving the
    /// projection (which would require either an `EntityId` clone
    /// on every entry or a re-keying scan).
    pub origin_hash: u64,
    /// v0.4 capability-auth allow-list — explicit `NodeId`s that
    /// may invoke any capability listed on this announcement.
    /// Empty = permissive default. Carried verbatim from
    /// [`CapabilityAnnouncement::allowed_nodes`].
    pub allowed_nodes: Vec<u64>,
    /// v0.4 capability-auth allow-list — subnet ids whose members
    /// may invoke. Empty = permissive default. Carried from
    /// [`CapabilityAnnouncement::allowed_subnets`].
    pub allowed_subnets: Vec<super::subnet::SubnetId>,
    /// v0.4 capability-auth allow-list — group ids whose
    /// claimants may invoke. Empty = permissive default. Carried
    /// from [`CapabilityAnnouncement::allowed_groups`].
    pub allowed_groups: Vec<super::group::GroupId>,
    /// Self-declared subnet membership parsed from a `subnet:<hex32>`
    /// tag on the announcement. Resolves to `Some(s)` only when the
    /// announcement carries exactly one distinct subnet tag; zero
    /// or two-or-more distinct subnet tags collapse to `None`
    /// (out-of-model malformed input → no membership). The
    /// collapse keeps the gate's subnet-axis verdict deterministic
    /// across receivers — a previous "first valid wins" strategy
    /// was hash-order-dependent against `HashSet<Tag>` iteration.
    pub subnet: Option<super::subnet::SubnetId>,
    /// Self-declared group memberships parsed from every
    /// `group:<hex64>` tag on the announcement. Sorted by byte
    /// value so iteration order is stable across receivers
    /// regardless of local hash randomization; duplicates removed.
    pub groups: Vec<super::group::GroupId>,
}

/// One entry in the `by_origin_hash` side index.
///
/// `Single` is the common case — exactly one indexed `node_id`
/// claims this wire hash. `Multiple` is the truncation-collision
/// case; lookups return `None` so admission falls back to its
/// publisher-unknown path. The `Vec` is sized for two-element
/// adversarial cases — a real cluster sees zero or one entry per
/// slot, and the allocation is amortized against the collision
/// counter bump that already accompanies the transition.
#[derive(Debug, Clone)]
enum OriginHashSlot {
    Single(u64),
    Multiple(Vec<u64>),
}

impl OriginHashSlot {
    /// Add `node_id`. Returns `true` on the *first* transition to
    /// ambiguous state (Single → Multiple with a distinct id), so
    /// callers can bump the collision counter exactly once per
    /// new collision. Idempotent on re-add of the same id.
    fn insert(&mut self, node_id: u64) -> bool {
        match self {
            Self::Single(existing) => {
                if *existing == node_id {
                    false
                } else {
                    *self = Self::Multiple(vec![*existing, node_id]);
                    true
                }
            }
            Self::Multiple(ids) => {
                if !ids.contains(&node_id) {
                    ids.push(node_id);
                }
                false
            }
        }
    }

    /// Remove `node_id`. Returns `true` when the slot is now
    /// empty (caller drops the map entry). Demotes Multiple to
    /// Single when only one claimant survives.
    fn remove(&mut self, node_id: u64) -> bool {
        match self {
            Self::Single(existing) => *existing == node_id,
            Self::Multiple(ids) => {
                ids.retain(|id| *id != node_id);
                match ids.len() {
                    0 => true,
                    1 => {
                        *self = Self::Single(ids[0]);
                        false
                    }
                    _ => false,
                }
            }
        }
    }

    /// Resolve to a single `node_id`, or `None` when the slot is
    /// ambiguous. Callers treat `None` as fail-closed for any
    /// admission decision that consults publisher caps.
    fn unique(&self) -> Option<u64> {
        match self {
            Self::Single(id) => Some(*id),
            Self::Multiple(_) => None,
        }
    }
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
    /// Inverted index: metadata key -> { value -> set of node IDs }.
    ///
    /// Phase 5.B follow-on of `CAPABILITY_ENHANCEMENTS_PLAN.md`.
    /// Mirrors `by_tag` for the metadata side; lets the cardinality-
    /// aware planner refine `MetadataEquals` / `MetadataExists` /
    /// related leaf clauses with distinct-value counts (otherwise
    /// they'd fall back to plain `static_cost`).
    by_metadata: DashMap<String, DashMap<String, HashSet<u64>>>,
    /// Inverted index: axis tag key -> { value -> set of node IDs }.
    ///
    /// Phase 4 follow-on of `CAPABILITY_ENHANCEMENTS_PLAN.md`.
    /// Lets [`Self::axis_cardinality`] return O(1) instead of
    /// O(N) over the full `by_tag` table — the planner reads it
    /// once per leaf-clause-per-candidate, so the linear scan
    /// would dominate hot-path evaluation. Presence-only axis
    /// tags (`hardware.gpu` with no value) use the sentinel
    /// empty string `""` as the inner key.
    by_axis_key: DashMap<crate::adapter::net::behavior::tag::TagKey, DashMap<String, HashSet<u64>>>,
    /// Side index: `wire origin_hash -> set of node_ids`.
    ///
    /// Wire packets carry the publisher's `origin_hash` as a u32
    /// — the low 32 bits of `EntityId::origin_hash()` (BLAKE2s
    /// projection under `net-origin-v1`) after `with_origin`'s
    /// `as u32` truncation in
    /// [`NetHeader`](crate::adapter::net::protocol::NetHeader).
    /// Every other index keys on `node_id` (a separate projection
    /// under `net-node-v1`). Without this side index, code paths
    /// that have a wire origin_hash in hand (e.g. the inbound
    /// packet header) need to walk every entry looking for a
    /// matching tag or fall back to last-hop-peer heuristics —
    /// both fragile under multi-hop relay. Direct lookup via
    /// [`Self::get_by_origin_hash`] is O(1).
    ///
    /// Key shape is `u64` (zero-extended from the wire `u32`) to
    /// match the receiver-side `parsed.header.origin_hash.into()`
    /// projection inside `process_local_packet`.
    ///
    /// ## Collision behavior — fail-closed on ambiguity
    ///
    /// Two distinct entities can collide on the wire u32 (birthday
    /// paradox at ~2¹⁶ entities). The slot tracks every `node_id`
    /// claiming a given wire hash. [`Self::get_by_origin_hash`]
    /// returns `Some(caps)` only when exactly one `node_id` is
    /// mapped — when two or more claim the same slot the lookup
    /// returns `None`, the caller falls back to its
    /// publisher-unknown path, and scope/intent/colocation
    /// admission fails closed. A deliberately-colliding peer can
    /// therefore *suppress* a victim's caps from the admission
    /// surface, but **cannot impersonate them** to widen scope.
    /// The authorization surface is unaffected either way (ACLs
    /// key on `ChannelHash` / `node_id`, not wire `origin_hash`).
    ///
    /// `collision_count` below increments on every fresh
    /// transition from one claimant to two (i.e. the first time a
    /// slot becomes ambiguous); operators dashboard it to detect
    /// truncation-collision attacks. A full fix to disambiguate
    /// requires widening the wire `origin_hash` to `u64`
    /// (wire-format break), out of scope for v0.2.
    by_origin_hash: DashMap<u64, OriginHashSlot>,
    /// Version tracking
    versions: DashMap<u64, u64>,
    /// Stats
    index_count: AtomicU64,
    query_count: AtomicU64,
    /// Cumulative count of wire-origin_hash collisions detected
    /// at `index()` time — bumped when the `by_origin_hash` slot
    /// for a wire hash is overwritten by a different `node_id`.
    /// See the docstring on `by_origin_hash` for the DoS surface
    /// this counter surfaces. Read via [`Self::collision_count`].
    origin_hash_collisions: AtomicU64,
    /// Monotonic mutation version. Incremented on every
    /// [`Self::index`], [`Self::remove`], and [`Self::gc`] call
    /// — i.e. anything that can change observable capability
    /// state. Read via [`Self::version`]; used by the MeshDB
    /// result cache to pull-invalidate stale entries (per the
    /// MeshDB plan's locked decision #4). Per design we bump
    /// aggressively: if a workload sees too many invalidations
    /// the fix is `bypass_cache` or `CachePolicy::Permanent` on
    /// the affected queries, not softening this bump.
    mutation_version: AtomicU64,
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
            by_metadata: DashMap::new(),
            by_axis_key: DashMap::new(),
            by_origin_hash: DashMap::new(),
            versions: DashMap::new(),
            index_count: AtomicU64::new(0),
            query_count: AtomicU64::new(0),
            origin_hash_collisions: AtomicU64::new(0),
            mutation_version: AtomicU64::new(0),
        }
    }

    /// Monotonic mutation version. Bumps on every `index` /
    /// `remove` / `gc` call. The MeshDB result cache snapshots
    /// this at plan time and compares on lookup; any divergence
    /// is treated as a cache miss (pull-based invalidation).
    pub fn version(&self) -> u64 {
        self.mutation_version.load(Ordering::Acquire)
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

        // Store node. The wire form of `origin_hash` is the low
        // 32 bits of the entity projection (see `with_origin`'s
        // truncation in NetHeader); reproduce that here so the
        // side index keys match what receivers compute.
        let origin_hash = (ann.entity_id.origin_hash() as u32) as u64;
        // Parse subnet/group membership from the announcement's
        // own tags so the capability-auth gate (`may_execute`) can
        // resolve caller membership without re-walking the tag set
        // on every check. Subnet membership is single-valued by
        // design: exactly one distinct `subnet:<hex32>` tag →
        // `Some(s)`; zero or multiple distinct tags collapse to
        // `None` (treating multiples as out-of-model malformed
        // input keeps the gate's verdict deterministic across
        // receivers — `HashSet<Tag>` iteration order is unspecified
        // so "first valid wins" would be hash-order-dependent).
        // All distinct `group:<hex64>` tags accumulate, sorted by
        // byte value, deduplicated by Eq.
        let (parsed_subnet, parsed_groups) = parse_membership_tags(&ann.capabilities.tags);
        let indexed = IndexedNode {
            node_id,
            capabilities: ann.capabilities,
            version: ann.version,
            indexed_at: Instant::now(),
            ttl: effective_ttl,
            reflex_addr: ann.reflex_addr,
            origin_hash,
            allowed_nodes: ann.allowed_nodes,
            allowed_subnets: ann.allowed_subnets,
            allowed_groups: ann.allowed_groups,
            subnet: parsed_subnet,
            groups: parsed_groups,
        };
        self.nodes.insert(node_id, indexed);
        // Track collisions on the side index. A fresh Single
        // slot is the common case; the Occupied branch merges the
        // new node_id into the existing slot, bumping the
        // collision counter on the first transition from one
        // claimant to two distinct ones. See the collision-
        // behavior docstring on `by_origin_hash`.
        match self.by_origin_hash.entry(origin_hash) {
            dashmap::mapref::entry::Entry::Vacant(slot) => {
                slot.insert(OriginHashSlot::Single(node_id));
            }
            dashmap::mapref::entry::Entry::Occupied(mut slot) => {
                if slot.get_mut().insert(node_id) {
                    self.origin_hash_collisions.fetch_add(1, Ordering::Relaxed);
                }
            }
        }

        self.index_count.fetch_add(1, Ordering::Relaxed);
        self.mutation_version.fetch_add(1, Ordering::AcqRel);
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
            // Tear down this node's claim on the origin_hash slot.
            // Other colliding claimants stay; the slot is dropped
            // only when no node_id remains.
            if let dashmap::mapref::entry::Entry::Occupied(mut slot) =
                self.by_origin_hash.entry(node.origin_hash)
            {
                if slot.get_mut().remove(node_id) {
                    slot.remove();
                }
            }
        }
        if let Entry::Occupied(e) = version_entry {
            e.remove();
        }
        self.mutation_version.fetch_add(1, Ordering::AcqRel);
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
        // Phase A.5.5: read through views() once. `caps.tags` stays
        // direct because tags are not part of the typed-struct
        // projection (they're carried through `CapabilitySet::tags`
        // independently and survive Phase A.5.N).
        let views = caps.views();

        // Tags. `by_tag` is keyed by `String` (the wire-form
        // rendering) so query() can look up tags by their string
        // representation regardless of which Tag variant they
        // round-trip through. Phase A.5.N.2: render Tag → wire string.
        //
        // Phase 4 follow-on: also populate `by_axis_key` for axis-
        // shaped tags so axis_cardinality is O(1).
        for tag in &caps.tags {
            let key = tag.to_string();
            if let Some(mut set) = self.by_tag.get_mut(&key) {
                set.insert(node_id);
            } else {
                self.by_tag.entry(key).or_default().insert(node_id);
            }

            // Mirror axis-shaped tags into `by_axis_key`.
            // AxisPresent: value sentinel `""`. AxisValue: the
            // actual value. Reserved / Legacy variants don't
            // surface here (no axis_key).
            use crate::adapter::net::behavior::tag::Tag as TagEnum;
            let (axis_key, value) = match tag {
                TagEnum::AxisPresent { axis, key } => (
                    crate::adapter::net::behavior::tag::TagKey::new(*axis, key.clone()),
                    String::new(),
                ),
                TagEnum::AxisValue {
                    axis, key, value, ..
                } => (
                    crate::adapter::net::behavior::tag::TagKey::new(*axis, key.clone()),
                    value.clone(),
                ),
                _ => continue,
            };
            let inner = self.by_axis_key.entry(axis_key).or_default();
            inner.entry(value).or_default().insert(node_id);
        }

        // Models
        for model in views.models() {
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
        for tool in views.tools() {
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
        let has_gpu = views.hardware().has_gpu();
        self.gpu_nodes.entry(has_gpu).or_default().insert(node_id);

        if let Some(vendor) = views.hardware().gpu_vendor() {
            // Vendor key is `Copy` (small enum), so the entry-only
            // form is already allocation-free.
            self.by_gpu_vendor
                .entry(vendor)
                .or_default()
                .insert(node_id);
        }

        // Metadata: track per-key distinct values so the cardinality-
        // aware planner can score MetadataEquals / MetadataExists
        // leaves. Mirrors `by_tag` for the metadata side.
        for (k, v) in &caps.metadata {
            // Outer entry: key. Inner entry: value → node IDs.
            let inner = self.by_metadata.entry(k.clone()).or_default();
            inner.entry(v.clone()).or_default().insert(node_id);
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
        // Phase A.5.5: read through views() once — mirrors
        // `add_to_indexes` so add/remove see the same projection.
        let views = caps.views();

        // Tags
        for tag in &caps.tags {
            let key = tag.to_string();
            if let Some(mut set) = self.by_tag.get_mut(&key) {
                set.remove(&node_id);
            }
            self.by_tag.remove_if(&key, |_, set| set.is_empty());

            // Mirror prune in by_axis_key for axis-shaped tags.
            use crate::adapter::net::behavior::tag::Tag as TagEnum;
            let (axis_key, value) = match tag {
                TagEnum::AxisPresent { axis, key } => (
                    crate::adapter::net::behavior::tag::TagKey::new(*axis, key.clone()),
                    String::new(),
                ),
                TagEnum::AxisValue {
                    axis, key, value, ..
                } => (
                    crate::adapter::net::behavior::tag::TagKey::new(*axis, key.clone()),
                    value.clone(),
                ),
                _ => continue,
            };
            let mut inner_now_empty = false;
            if let Some(inner) = self.by_axis_key.get(&axis_key) {
                if let Some(mut set) = inner.get_mut(&value) {
                    set.remove(&node_id);
                }
                inner.remove_if(&value, |_, set| set.is_empty());
                inner_now_empty = inner.is_empty();
            }
            if inner_now_empty {
                self.by_axis_key
                    .remove_if(&axis_key, |_, inner| inner.is_empty());
            }
        }

        // Models
        for model in views.models() {
            if let Some(mut set) = self.by_model.get_mut(&model.model_id) {
                set.remove(&node_id);
            }
            self.by_model
                .remove_if(&model.model_id, |_, set| set.is_empty());
        }

        // Tools
        for tool in views.tools() {
            if let Some(mut set) = self.by_tool.get_mut(&tool.tool_id) {
                set.remove(&node_id);
            }
            self.by_tool
                .remove_if(&tool.tool_id, |_, set| set.is_empty());
        }

        // GPU (two-value bucket; entries are intentionally permanent
        // because lookups for both `true` and `false` are expected).
        let has_gpu = views.hardware().has_gpu();
        if let Some(mut set) = self.gpu_nodes.get_mut(&has_gpu) {
            set.remove(&node_id);
        }

        if let Some(vendor) = views.hardware().gpu_vendor() {
            if let Some(mut set) = self.by_gpu_vendor.get_mut(&vendor) {
                set.remove(&node_id);
            }
            self.by_gpu_vendor
                .remove_if(&vendor, |_, set| set.is_empty());
        }

        // Metadata: drop this node's contribution from each
        // (key, value) entry; prune empty inner / outer entries
        // so the by_metadata index doesn't accumulate empty
        // shells across high-churn deployments.
        for (k, v) in &caps.metadata {
            let mut inner_now_empty = false;
            if let Some(inner) = self.by_metadata.get(k) {
                if let Some(mut set) = inner.get_mut(v) {
                    set.remove(&node_id);
                }
                inner.remove_if(v, |_, set| set.is_empty());
                inner_now_empty = inner.is_empty();
            }
            if inner_now_empty {
                self.by_metadata.remove_if(k, |_, inner| inner.is_empty());
            }
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

    /// Find indexed nodes whose capabilities match `predicate`.
    ///
    /// Linear scan over the indexed-node table; each node's
    /// capabilities are evaluated against `predicate` via the
    /// cardinality-aware planner ([`super::predicate::Predicate::evaluate_with_index`]).
    /// `Self` provides the cardinality data — the same index that
    /// holds the candidates also informs the planner's clause
    /// ordering, so high-cardinality discriminating clauses run
    /// first.
    ///
    /// Phase 5.B follow-on of `CAPABILITY_ENHANCEMENTS_PLAN.md`.
    /// Bridges the substrate's `CapabilityIndex` with the
    /// `Predicate` AST so applications can do predicate-based
    /// discovery without building a `CapabilityFilter`.
    ///
    /// Cost: O(N) where N is the number of indexed nodes. Each
    /// per-node check materializes the node's tags as a Vec for
    /// the slice-based `EvalContext` — for hot loops over many
    /// predicates against the same index, callers may prefer to
    /// pre-extract a `Vec<(u64, Vec<Tag>, BTreeMap)>` once and
    /// reuse it.
    ///
    /// Returns matching node IDs in unspecified order. Callers
    /// that need a deterministic order should sort.
    pub fn find_nodes_matching(
        &self,
        predicate: &crate::adapter::net::behavior::Predicate,
    ) -> Vec<u64> {
        let mut matched = Vec::new();
        for entry in self.nodes.iter() {
            let node_id = *entry.key();
            let caps = &entry.value().capabilities;
            // Materialize tags for the slice-based EvalContext.
            let tags: Vec<Tag> = caps.tags.iter().cloned().collect();
            let ctx = crate::adapter::net::behavior::EvalContext::new(&tags, &caps.metadata);
            if predicate.evaluate_with_index(&ctx, self) {
                matched.push(node_id);
            }
        }
        matched
    }

    /// Distinct-value cardinality for a metadata key.
    ///
    /// Returns the count of distinct values seen for `key` across
    /// all currently-indexed nodes. The cardinality-aware planner
    /// uses this as a selectivity proxy for `MetadataEquals` /
    /// `MetadataExists` / similar leaves, parallel to
    /// [`Self::axis_cardinality`] for axis tags.
    ///
    /// Cost: O(1) — looks up the inner DashMap size; no scan.
    /// `by_metadata` maintains distinct-value tracking incrementally
    /// during `add_to_indexes` / `remove_from_indexes`.
    ///
    /// Returns 0 when the key is absent from the index. The
    /// planner falls back to `static_cost` in that case.
    pub fn metadata_value_cardinality(&self, key: &str) -> usize {
        self.by_metadata
            .get(key)
            .map(|inner| inner.len())
            .unwrap_or(0)
    }

    /// Distinct-value cardinality for an axis tag key.
    ///
    /// Returns the number of distinct values seen for the given
    /// `(axis, key)` across all currently-indexed nodes. Used by
    /// the predicate query planner (Phase 4 of
    /// `CAPABILITY_ENHANCEMENTS_PLAN.md`) as a selectivity proxy:
    /// a key with high cardinality has many possible values, so a
    /// predicate matching one of them is likely to filter most
    /// candidates — run such clauses first in `And`.
    ///
    /// Cost: O(1) — looks up the inner DashMap size in the
    /// dedicated `by_axis_key` index, maintained incrementally
    /// during `add_to_indexes` / `remove_from_indexes`. Phase 4
    /// follow-on of `CAPABILITY_ENHANCEMENTS_PLAN.md` replaced an
    /// earlier O(N) scan over `by_tag` because the planner reads
    /// this once per leaf-clause-per-candidate; the linear scan
    /// dominated hot-path evaluation.
    ///
    /// Returns:
    ///
    /// - For value-bearing keys (`hardware.cpu_cores=...`,
    ///   `hardware.gpu.vendor=...`): the count of distinct
    ///   value strings seen. Presence-form (no value) entries
    ///   under the same key, if any, count as one of those
    ///   distinct values via the empty-string sentinel.
    /// - For presence-only keys (`hardware.gpu`): `1` if any node
    ///   has the marker, `0` otherwise.
    /// - For unrecognized keys: `0`.
    ///
    /// Reserved-prefix tags (`scope:*`, `causal:*`, etc.) and
    /// legacy untyped tags don't fit the axis taxonomy; this
    /// primitive doesn't surface them.
    pub fn axis_cardinality(&self, key: &crate::adapter::net::behavior::tag::TagKey) -> usize {
        self.by_axis_key
            .get(key)
            .map(|inner| inner.len())
            .unwrap_or(0)
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

    /// Look up a node's `CapabilitySet` by `EntityId::origin_hash()`
    /// rather than `node_id`. Wire packets carry the publisher's
    /// `origin_hash` (a BLAKE2s projection of the entity key under
    /// the `net-origin-v1` domain); the index keys on `node_id`
    /// (a separate projection under `net-node-v1`). This accessor
    /// bridges the two via the `by_origin_hash` side index so a
    /// packet-side caller doesn't need to walk every entry.
    ///
    /// Returns `None` when either:
    ///
    /// 1. No announcement carrying that origin_hash has reached
    ///    this node yet.
    /// 2. Two or more distinct `node_id`s have claimed the same
    ///    wire `origin_hash` (a truncation collision — see the
    ///    collision-behavior docstring on `by_origin_hash`).
    ///
    /// Both cases are fail-closed for scope / intent / colocation
    /// admission: the caller cannot tell them apart and treats a
    /// missing result as "publisher unknown."
    pub fn get_by_origin_hash(&self, origin_hash: u64) -> Option<CapabilitySet> {
        let node_id = {
            let slot = self.by_origin_hash.get(&origin_hash)?;
            slot.unique()?
        };
        self.get(node_id)
    }

    /// Cumulative count of wire-`origin_hash` collisions detected
    /// at `index()` time. Bumped every time a slot transitions
    /// from one claimant to two distinct `node_id`s — i.e. the
    /// first time the slot becomes ambiguous. Re-asserting a
    /// node_id that already shares the slot is idempotent and
    /// does not bump.
    ///
    /// Operators monitor this counter to detect deliberate
    /// truncation-collision attacks against the scope-axis
    /// admission gate — see the collision-behavior docstring on
    /// `by_origin_hash` above. A non-zero count on a production
    /// cluster is a signal worth investigating; the collision
    /// itself doesn't bypass authorization or impersonate the
    /// victim's caps, but the *victim's* caps are suppressed from
    /// `get_by_origin_hash` for the duration of the collision.
    pub fn collision_count(&self) -> u64 {
        self.origin_hash_collisions.load(Ordering::Relaxed)
    }

    /// `true` when two or more distinct `node_id`s currently
    /// claim the same wire `origin_hash`. Distinct from
    /// "publisher not yet announced" (slot vacant) — the dispatch
    /// path uses this to fail-closed on adversarial collision
    /// while keeping the empty-caps fallback for the benign
    /// cap-propagation race where the publisher's announcement
    /// just hasn't arrived yet.
    pub fn is_origin_hash_ambiguous(&self, origin_hash: u64) -> bool {
        self.by_origin_hash
            .get(&origin_hash)
            .map(|slot| slot.unique().is_none())
            .unwrap_or(false)
    }

    /// Run `f` against the node's `CapabilitySet` without cloning.
    ///
    /// Holds the `DashMap` shard's read lock for the duration of
    /// the closure — `f` MUST NOT acquire any other index locks
    /// (e.g. via `find_nodes` / `query`), or it'll deadlock
    /// against a concurrent insert. Use [`Self::get`] when the
    /// result needs to outlive the scoring call.
    ///
    /// Hot-path use: `StandardPlacement::placement_score` calls
    /// this once per scoring decision, dispatching the in-tree
    /// axes against the borrowed caps. Saves the `CapabilitySet`
    /// clone per scoring call (a HashSet + BTreeMap allocation
    /// pair), which dominates the per-candidate cost in benches.
    pub fn with_caps<R>(&self, node_id: u64, f: impl FnOnce(&CapabilitySet) -> R) -> Option<R> {
        self.nodes.get(&node_id).map(|n| f(&n.capabilities))
    }

    /// Get the peer's last-advertised reflex address from the
    /// index. Returns `None` when the peer hasn't indexed, or
    /// indexed a version with no `reflex_addr` attached. Consumed
    /// by the rendezvous coordinator (stage 3) — R looks up the
    /// punch target's public `SocketAddr` here.
    pub fn reflex_addr(&self, node_id: u64) -> Option<std::net::SocketAddr> {
        self.nodes.get(&node_id).and_then(|n| n.reflex_addr)
    }

    /// Caller's self-declared subnet membership, parsed from a
    /// `subnet:<hex32>` tag on the caller's own announcement.
    /// Returns `None` when the caller has no announcement yet or
    /// emitted no `subnet:` tag. v0.4 capability-auth uses this
    /// inside [`Self::may_execute`].
    pub fn subnet_of(&self, node_id: u64) -> Option<super::subnet::SubnetId> {
        self.nodes.get(&node_id).and_then(|n| n.subnet)
    }

    /// Caller's self-declared group memberships, parsed from
    /// every `group:<hex64>` tag on the caller's own
    /// announcement. Returns an empty Vec when the caller has no
    /// announcement yet or emitted no `group:` tags. Order
    /// matches the announcement's tag-set iteration order
    /// (deterministic for a given index entry but not stable
    /// across rebuilds).
    pub fn groups_of(&self, node_id: u64) -> Vec<super::group::GroupId> {
        self.nodes
            .get(&node_id)
            .map(|n| n.groups.clone())
            .unwrap_or_default()
    }

    /// v0.4 capability-auth execute-gate. Returns `true` when
    /// `caller_node` is allowed to invoke `capability_tag` on
    /// `target_node`.
    ///
    /// See `docs/plans/CAPABILITY_AUTH_PLAN.md` §3 for the model.
    /// The gate is permissive by default — an announcement with
    /// all three allow-lists empty admits any caller. Once any
    /// list is non-empty, the union of all three is enforced
    /// (node OR subnet OR group); the scan short-circuits on the
    /// first match.
    ///
    /// Returns `false` when:
    /// - the target has no indexed announcement (you can't
    ///   address what you can't see);
    /// - the target's announcement doesn't list the requested
    ///   `capability_tag`;
    /// - the target restricts and the caller matches no axis.
    ///
    /// The caller's identity is taken from the wire binding only —
    /// the gate does NOT consult the caller's own announcement
    /// for capability claims. It does consult it for subnet /
    /// group membership lookup, because that's the only place
    /// self-declared membership lives.
    pub fn may_execute(&self, target_node: u64, capability_tag: &str, caller_node: u64) -> bool {
        let Some(target) = self.nodes.get(&target_node) else {
            return false;
        };
        if !target.capabilities.has_tag(capability_tag) {
            return false;
        }
        if target.allowed_nodes.is_empty()
            && target.allowed_subnets.is_empty()
            && target.allowed_groups.is_empty()
        {
            return true;
        }
        if target.allowed_nodes.contains(&caller_node) {
            return true;
        }
        if !target.allowed_subnets.is_empty() {
            if let Some(caller_subnet) = self.nodes.get(&caller_node).and_then(|n| n.subnet) {
                if target.allowed_subnets.contains(&caller_subnet) {
                    return true;
                }
            }
        }
        if !target.allowed_groups.is_empty() {
            if let Some(caller_entry) = self.nodes.get(&caller_node) {
                for group in &caller_entry.groups {
                    if target.allowed_groups.contains(group) {
                        return true;
                    }
                }
            }
        }
        false
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

// =============================================================================
// CardinalityProvider trait + CardinalityCache — Phase 4 follow-on of
// CAPABILITY_ENHANCEMENTS_PLAN.md "Cached cardinality estimates".
//
// `axis_cardinality` and `metadata_value_cardinality` on
// `CapabilityIndex` are O(1) but each call hits DashMap — a sharded
// lookup with atomic ops + per-shard locks. For the predicate
// planner reading the same key many times in a hot evaluation
// loop (one read per leaf-clause-per-candidate), the contention
// cost compounds. `CardinalityCache` snapshots cardinality numbers
// on first read and serves subsequent reads from a plain `HashMap`,
// dropping the DashMap traffic.
//
// The cache is bound to a TTL — after `ttl` elapses, the next
// access recomputes from the underlying index. Heartbeat-aligned
// TTLs are the canonical use case (per-heartbeat planner snapshot).
// =============================================================================

/// Source of per-key cardinality data for the predicate query
/// planner. Implemented by [`CapabilityIndex`] (direct, O(1) DashMap
/// lookups) and [`CardinalityCache`] (TTL-cached, O(1) HashMap
/// lookups after warmup).
///
/// The planner ([`crate::adapter::net::behavior::predicate::Predicate::evaluate_with_index`])
/// is generic over this trait so callers pick whichever shape
/// matches their workload — direct for single-shot evaluation,
/// cache for hot-loop or long-lived service handlers.
pub trait CardinalityProvider {
    /// Distinct-value count for the given axis tag key. Returns 0
    /// when the key is absent — planner treats this as "no data,
    /// fall back to static cost".
    fn axis_cardinality(&self, key: &crate::adapter::net::behavior::tag::TagKey) -> usize;

    /// Distinct-value count for the given metadata key.
    fn metadata_value_cardinality(&self, key: &str) -> usize;
}

impl CardinalityProvider for CapabilityIndex {
    fn axis_cardinality(&self, key: &crate::adapter::net::behavior::tag::TagKey) -> usize {
        // Inherent method (resolved via Rust's preference for
        // inherent over trait when the receiver is concrete).
        Self::axis_cardinality(self, key)
    }

    fn metadata_value_cardinality(&self, key: &str) -> usize {
        Self::metadata_value_cardinality(self, key)
    }
}

/// Per-call TTL cache around a [`CapabilityIndex`] for cardinality
/// lookups.
///
/// Phase 4 follow-on of `CAPABILITY_ENHANCEMENTS_PLAN.md`. Drops
/// DashMap shard contention on hot loops over many predicates
/// against the same index by snapshotting cardinality numbers
/// into a plain `HashMap` on first access. Subsequent reads of
/// the same key skip the DashMap entirely.
///
/// The cache invalidates after `ttl` elapses — the next lookup
/// past the deadline triggers a refresh (clear + reset deadline);
/// fresh lookups go to the index again. Heartbeat-aligned TTLs
/// are the canonical use case: a service handler builds a cache
/// at the start of each heartbeat-bounded request batch, evaluates
/// many predicates against it, then lets it drop.
///
/// Concurrent access is safe — internal `parking_lot::Mutex`
/// guards the HashMap. Two threads racing on the same uncached
/// key may both compute via the index (the work is idempotent),
/// but neither will read stale data.
///
/// ```ignore
/// let cache = CardinalityCache::new(&index, Duration::from_secs(5));
/// for candidate in candidates {
///     let ctx = build_eval_context(candidate);
///     if predicate.evaluate_with_index(&ctx, &cache) { /* ... */ }
/// }
/// ```
pub struct CardinalityCache<'a> {
    index: &'a CapabilityIndex,
    ttl: Duration,
    state: parking_lot::Mutex<CardinalityCacheState>,
}

/// Inner state guarded by the cache's mutex.
struct CardinalityCacheState {
    deadline: Instant,
    axis: std::collections::HashMap<crate::adapter::net::behavior::tag::TagKey, usize>,
    metadata: std::collections::HashMap<String, usize>,
}

impl<'a> CardinalityCache<'a> {
    /// Build a cache bound to `index` with `ttl` lifetime. The
    /// cache's deadline starts at `Instant::now() + ttl`.
    pub fn new(index: &'a CapabilityIndex, ttl: Duration) -> Self {
        Self {
            index,
            ttl,
            state: parking_lot::Mutex::new(CardinalityCacheState {
                deadline: Instant::now() + ttl,
                axis: std::collections::HashMap::new(),
                metadata: std::collections::HashMap::new(),
            }),
        }
    }

    /// True if the cache's TTL has elapsed.
    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.state.lock().deadline
    }

    /// Force-refresh the cache: clear all entries and reset the
    /// deadline to `now + ttl`. Called automatically when an
    /// access notices the cache is expired; callers can invoke
    /// directly to align with their own heartbeat.
    pub fn refresh(&self) {
        let mut guard = self.state.lock();
        guard.deadline = Instant::now() + self.ttl;
        guard.axis.clear();
        guard.metadata.clear();
    }
}

impl CardinalityProvider for CardinalityCache<'_> {
    fn axis_cardinality(&self, key: &crate::adapter::net::behavior::tag::TagKey) -> usize {
        // Acquire the lock once per call; check deadline and
        // cache under the same critical section to avoid TOCTOU.
        {
            let mut guard = self.state.lock();
            if Instant::now() >= guard.deadline {
                guard.deadline = Instant::now() + self.ttl;
                guard.axis.clear();
                guard.metadata.clear();
            }
            if let Some(&v) = guard.axis.get(key) {
                return v;
            }
        }
        // Cache miss: compute via the underlying index without
        // holding the cache lock (axis_cardinality acquires the
        // index's own DashMap shard lock; cross-locking would
        // serialize unnecessarily).
        let v = self.index.axis_cardinality(key);
        self.state.lock().axis.insert(key.clone(), v);
        v
    }

    fn metadata_value_cardinality(&self, key: &str) -> usize {
        {
            let mut guard = self.state.lock();
            if Instant::now() >= guard.deadline {
                guard.deadline = Instant::now() + self.ttl;
                guard.axis.clear();
                guard.metadata.clear();
            }
            if let Some(&v) = guard.metadata.get(key) {
                return v;
            }
        }
        let v = self.index.metadata_value_cardinality(key);
        self.state.lock().metadata.insert(key.to_string(), v);
        v
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

    /// `strip_reserved_metadata` drops every exact-match reserved
    /// key and every key under a reserved prefix, leaves all other
    /// keys intact. The substrate calls this on every inbound peer
    /// announcement before downstream consumers (greedy admission,
    /// placement scoring) read metadata, so a peer can't steer
    /// receiver decisions by stamping `intent`, `colocate-with`,
    /// `priority`, `owner`, or any `tool::*` key.
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
        ann.capabilities
            .metadata
            .insert("tool::pwn".into(), "go-brrr".into());
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
        assert!(!ann.capabilities.metadata.contains_key("tool::pwn"));
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
    fn version_bumps_on_index_remove_and_gc() {
        // Phase F locked decision: every mutation bumps the
        // mutation_version monotonically. The MeshDB cache
        // pull-invalidates on any divergence.
        let index = CapabilityIndex::new();
        assert_eq!(index.version(), 0);

        let entity = super::super::super::identity::EntityId::from_bytes([7u8; 32]);
        index.index(CapabilityAnnouncement::new(
            1u64,
            entity,
            1,
            sample_capability_set(),
        ));
        let after_first_index = index.version();
        assert!(after_first_index >= 1);

        index.index(CapabilityAnnouncement::new(
            2u64,
            super::super::super::identity::EntityId::from_bytes([8u8; 32]),
            1,
            sample_capability_set(),
        ));
        let after_second_index = index.version();
        assert!(after_second_index > after_first_index);

        index.remove(1u64);
        let after_remove = index.version();
        assert!(after_remove > after_second_index);

        // gc delegates to remove; if the entries are still
        // alive (we haven't aged them out) nothing happens.
        // So set a TTL-zero announcement and gc to confirm
        // the bump path covers gc-induced removes too.
        let mut ann = CapabilityAnnouncement::new(
            3u64,
            super::super::super::identity::EntityId::from_bytes([9u8; 32]),
            1,
            sample_capability_set(),
        );
        ann.ttl_secs = 0;
        index.index(ann);
        let after_zero_ttl_index = index.version();
        assert!(after_zero_ttl_index > after_remove);
        index.gc();
        assert!(index.version() > after_zero_ttl_index);
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

    /// CR-16: `with_metadata` silently drops writes whose key
    /// starts with a reserved prefix (`tool::`). Same shape as
    /// `add_tag`'s rejection of reserved tag prefixes via
    /// `Tag::parse_user`. Exact-match reserved keys (`intent`,
    /// `owner`, …) are NOT gated — those are well-known
    /// user-facing scheduler hints.
    #[test]
    fn with_metadata_drops_reserved_prefix_keys() {
        // `tool::*` writes silently dropped.
        let caps = CapabilitySet::new()
            .with_metadata("tool::evil::input_schema", "spoof")
            .with_metadata("region", "us-east");
        assert!(
            !caps.metadata.contains_key("tool::evil::input_schema"),
            "with_metadata must drop reserved-prefix keys: {:?}",
            caps.metadata
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
    fn test_capability_index() {
        let index = CapabilityIndex::new();

        // Index some nodes
        for i in 0..100 {
            let mut caps = sample_capability_set();
            if i % 2 == 0 {
                caps = caps.add_tag("even");
            }
            if i % 3 == 0 {
                caps = caps.add_tag("divisible_by_3");
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

    // ========================================================================
    // CapabilityIndex::axis_cardinality — Phase 4 follow-on of
    // CAPABILITY_ENHANCEMENTS_PLAN.md.
    // ========================================================================

    fn index_with_node(index: &CapabilityIndex, node_id: u64, caps: CapabilitySet) {
        let ann = CapabilityAnnouncement::new(node_id, test_entity(), 1, caps);
        index.index(ann);
    }

    #[test]
    fn axis_cardinality_counts_distinct_value_tags() {
        // 3 nodes with different memory_gb values. Cardinality
        // for `hardware.memory_gb` should be 3.
        let index = CapabilityIndex::new();
        for (i, gb) in [16u32, 32, 64].iter().enumerate() {
            let caps =
                CapabilitySet::new().with_hardware(HardwareCapabilities::new().with_memory(*gb));
            index_with_node(&index, i as u64, caps);
        }
        let key = crate::adapter::net::behavior::tag::TagKey::new(
            crate::adapter::net::behavior::tag::TaxonomyAxis::Hardware,
            "memory_gb",
        );
        assert_eq!(index.axis_cardinality(&key), 3);
    }

    #[test]
    fn axis_cardinality_dedupes_repeated_values() {
        // 5 nodes, only 2 distinct gpu vendors. Cardinality = 2.
        let index = CapabilityIndex::new();
        let vendors = [
            GpuVendor::Nvidia,
            GpuVendor::Nvidia,
            GpuVendor::Amd,
            GpuVendor::Nvidia,
            GpuVendor::Amd,
        ];
        for (i, v) in vendors.iter().enumerate() {
            let caps = CapabilitySet::new()
                .with_hardware(HardwareCapabilities::new().with_gpu(GpuInfo::new(*v, "x", 1)));
            index_with_node(&index, i as u64, caps);
        }
        let key = crate::adapter::net::behavior::tag::TagKey::new(
            crate::adapter::net::behavior::tag::TaxonomyAxis::Hardware,
            "gpu.vendor",
        );
        assert_eq!(index.axis_cardinality(&key), 2);
    }

    #[test]
    fn axis_cardinality_returns_one_for_presence_keys_with_any_match() {
        // Presence-only key (`hardware.gpu` marker, no `=value`).
        // Any node with the marker → cardinality 1.
        let index = CapabilityIndex::new();
        let caps = CapabilitySet::new().with_hardware(
            HardwareCapabilities::new().with_gpu(GpuInfo::new(GpuVendor::Nvidia, "h100", 80)),
        );
        index_with_node(&index, 1, caps);
        let key = crate::adapter::net::behavior::tag::TagKey::new(
            crate::adapter::net::behavior::tag::TaxonomyAxis::Hardware,
            "gpu",
        );
        assert_eq!(index.axis_cardinality(&key), 1);
    }

    #[test]
    fn axis_cardinality_returns_zero_for_unknown_keys() {
        let index = CapabilityIndex::new();
        let caps = CapabilitySet::new().with_hardware(HardwareCapabilities::new().with_memory(64));
        index_with_node(&index, 1, caps);

        let unknown_key = crate::adapter::net::behavior::tag::TagKey::new(
            crate::adapter::net::behavior::tag::TaxonomyAxis::Devices,
            "qpu",
        );
        assert_eq!(index.axis_cardinality(&unknown_key), 0);
    }

    #[test]
    fn axis_cardinality_returns_zero_on_empty_index() {
        let index = CapabilityIndex::new();
        let key = crate::adapter::net::behavior::tag::TagKey::new(
            crate::adapter::net::behavior::tag::TaxonomyAxis::Hardware,
            "memory_gb",
        );
        assert_eq!(index.axis_cardinality(&key), 0);
    }

    #[test]
    fn axis_cardinality_excludes_presence_when_value_form_present() {
        // Edge case: if the key has BOTH presence-form
        // (`hardware.gpu`) and value-form sub-keys
        // (`hardware.gpu.vendor=...`), `axis_cardinality(gpu)`
        // returns 1 (the presence count) — the gpu.vendor sub-key
        // has its own cardinality measurement under a different
        // TagKey.
        let index = CapabilityIndex::new();
        let caps = CapabilitySet::new().with_hardware(
            HardwareCapabilities::new().with_gpu(GpuInfo::new(GpuVendor::Nvidia, "h100", 80)),
        );
        index_with_node(&index, 1, caps);

        let gpu_presence = crate::adapter::net::behavior::tag::TagKey::new(
            crate::adapter::net::behavior::tag::TaxonomyAxis::Hardware,
            "gpu",
        );
        assert_eq!(index.axis_cardinality(&gpu_presence), 1);

        let gpu_vendor = crate::adapter::net::behavior::tag::TagKey::new(
            crate::adapter::net::behavior::tag::TaxonomyAxis::Hardware,
            "gpu.vendor",
        );
        assert_eq!(index.axis_cardinality(&gpu_vendor), 1);
    }

    #[test]
    fn axis_cardinality_handles_high_cardinality_keys() {
        // 100 nodes with distinct memory values → cardinality 100.
        // Pin: the O(1) by_axis_key lookup handles modest sizes
        // correctly (was an O(N) scan in the previous implementation).
        let index = CapabilityIndex::new();
        for i in 0..100u32 {
            let caps =
                CapabilitySet::new().with_hardware(HardwareCapabilities::new().with_memory(1 + i));
            index_with_node(&index, i as u64, caps);
        }
        let key = crate::adapter::net::behavior::tag::TagKey::new(
            crate::adapter::net::behavior::tag::TaxonomyAxis::Hardware,
            "memory_gb",
        );
        assert_eq!(index.axis_cardinality(&key), 100);
    }

    #[test]
    fn axis_cardinality_decrements_on_node_removal() {
        // Pin: removing a node updates `by_axis_key` — its values
        // are pruned if no other node carries them. Mirrors the
        // metadata_value_cardinality lifecycle test.
        let index = CapabilityIndex::new();
        // Node 1: memory_gb=1
        index_with_node(
            &index,
            1,
            CapabilitySet::new().with_hardware(HardwareCapabilities::new().with_memory(1)),
        );
        // Node 2: memory_gb=2
        index_with_node(
            &index,
            2,
            CapabilitySet::new().with_hardware(HardwareCapabilities::new().with_memory(2)),
        );
        let memory_key = crate::adapter::net::behavior::tag::TagKey::new(
            crate::adapter::net::behavior::tag::TaxonomyAxis::Hardware,
            "memory_gb",
        );
        assert_eq!(index.axis_cardinality(&memory_key), 2);

        // Remove node 1; only memory_gb=2 left → cardinality 1.
        index.remove(1);
        assert_eq!(index.axis_cardinality(&memory_key), 1);

        // Remove node 2; both memory values pruned → cardinality 0.
        index.remove(2);
        assert_eq!(index.axis_cardinality(&memory_key), 0);
    }

    // ========================================================================
    // CapabilityIndex::find_nodes_matching — Phase 5.B follow-on of
    // CAPABILITY_ENHANCEMENTS_PLAN.md.
    // ========================================================================

    #[test]
    fn find_nodes_matching_simple_predicate() {
        // 4 nodes: 2 with GPUs, 2 without. Predicate selects GPU nodes.
        let index = CapabilityIndex::new();
        index_with_node(
            &index,
            1,
            CapabilitySet::new().with_hardware(HardwareCapabilities::new().with_gpu(GpuInfo::new(
                GpuVendor::Nvidia,
                "h100",
                1,
            ))),
        );
        index_with_node(
            &index,
            2,
            CapabilitySet::new().with_hardware(HardwareCapabilities::new().with_memory(64)),
        );
        index_with_node(
            &index,
            3,
            CapabilitySet::new().with_hardware(HardwareCapabilities::new().with_gpu(GpuInfo::new(
                GpuVendor::Amd,
                "x",
                1,
            ))),
        );
        index_with_node(
            &index,
            4,
            CapabilitySet::new().with_hardware(HardwareCapabilities::new().with_cpu(8, 16)),
        );

        let pred = crate::adapter::net::behavior::Predicate::Exists {
            key: crate::adapter::net::behavior::tag::TagKey::new(
                crate::adapter::net::behavior::tag::TaxonomyAxis::Hardware,
                "gpu",
            ),
        };
        let mut matched = index.find_nodes_matching(&pred);
        matched.sort();
        assert_eq!(matched, vec![1, 3]);
    }

    #[test]
    fn find_nodes_matching_composite_predicate() {
        // 4 nodes; the predicate selects nodes with GPU AND
        // intent=ml-training metadata. Pin the And + metadata
        // composition path.
        let index = CapabilityIndex::new();
        // GPU + ml-training → match
        index_with_node(
            &index,
            10,
            CapabilitySet::new()
                .with_hardware(HardwareCapabilities::new().with_gpu(GpuInfo::new(
                    GpuVendor::Nvidia,
                    "h100",
                    80,
                )))
                .with_metadata("intent", "ml-training"),
        );
        // GPU but wrong intent → miss
        index_with_node(
            &index,
            11,
            CapabilitySet::new()
                .with_hardware(HardwareCapabilities::new().with_gpu(GpuInfo::new(
                    GpuVendor::Amd,
                    "x",
                    1,
                )))
                .with_metadata("intent", "embedding-cache"),
        );
        // ml-training but no GPU → miss
        index_with_node(
            &index,
            12,
            CapabilitySet::new()
                .with_hardware(HardwareCapabilities::new().with_memory(64))
                .with_metadata("intent", "ml-training"),
        );
        // Neither → miss
        index_with_node(
            &index,
            13,
            CapabilitySet::new().with_hardware(HardwareCapabilities::new().with_cpu(4, 8)),
        );

        let pred = crate::adapter::net::behavior::Predicate::And(vec![
            crate::adapter::net::behavior::Predicate::Exists {
                key: crate::adapter::net::behavior::tag::TagKey::new(
                    crate::adapter::net::behavior::tag::TaxonomyAxis::Hardware,
                    "gpu",
                ),
            },
            crate::adapter::net::behavior::Predicate::MetadataEquals {
                key: "intent".into(),
                value: "ml-training".into(),
            },
        ]);
        let matched = index.find_nodes_matching(&pred);
        assert_eq!(matched, vec![10]);
    }

    #[test]
    fn find_nodes_matching_no_match_returns_empty() {
        let index = CapabilityIndex::new();
        index_with_node(
            &index,
            1,
            CapabilitySet::new().with_hardware(HardwareCapabilities::new().with_memory(1)),
        );
        let pred = crate::adapter::net::behavior::Predicate::Exists {
            key: crate::adapter::net::behavior::tag::TagKey::new(
                crate::adapter::net::behavior::tag::TaxonomyAxis::Hardware,
                "gpu",
            ),
        };
        let matched = index.find_nodes_matching(&pred);
        assert!(matched.is_empty());
    }

    #[test]
    fn find_nodes_matching_empty_index_returns_empty() {
        let index = CapabilityIndex::new();
        let pred = crate::adapter::net::behavior::Predicate::Exists {
            key: crate::adapter::net::behavior::tag::TagKey::new(
                crate::adapter::net::behavior::tag::TaxonomyAxis::Hardware,
                "gpu",
            ),
        };
        let matched = index.find_nodes_matching(&pred);
        assert!(matched.is_empty());
    }

    // ========================================================================
    // CapabilityIndex::metadata_value_cardinality — Phase 5.B follow-on of
    // CAPABILITY_ENHANCEMENTS_PLAN.md.
    // ========================================================================

    #[test]
    fn metadata_value_cardinality_counts_distinct_values() {
        let index = CapabilityIndex::new();
        // 5 nodes with 3 distinct intent values.
        let intents = [
            "ml-training",
            "ml-training",
            "embedding-cache",
            "ml-training",
            "scratchpad",
        ];
        for (i, v) in intents.iter().enumerate() {
            let caps = CapabilitySet::new().with_metadata("intent", *v);
            index_with_node(&index, i as u64, caps);
        }
        assert_eq!(index.metadata_value_cardinality("intent"), 3);
    }

    #[test]
    fn metadata_value_cardinality_returns_zero_for_unknown_key() {
        let index = CapabilityIndex::new();
        let caps = CapabilitySet::new().with_metadata("intent", "ml-training");
        index_with_node(&index, 1, caps);
        assert_eq!(index.metadata_value_cardinality("nonexistent"), 0);
    }

    #[test]
    fn metadata_value_cardinality_empty_index() {
        let index = CapabilityIndex::new();
        assert_eq!(index.metadata_value_cardinality("intent"), 0);
    }

    #[test]
    fn metadata_value_cardinality_dedupes_repeated_values() {
        let index = CapabilityIndex::new();
        // 10 nodes all with the same intent → cardinality = 1.
        for i in 0..10u64 {
            let caps = CapabilitySet::new().with_metadata("intent", "ml-training");
            index_with_node(&index, i, caps);
        }
        assert_eq!(index.metadata_value_cardinality("intent"), 1);
    }

    #[test]
    fn metadata_value_cardinality_decrements_on_node_removal() {
        // Pin: removing a node updates the metadata index — its
        // values are pruned if no other node carries them.
        let index = CapabilityIndex::new();
        // Node 1: intent=A, owner=alice
        index_with_node(
            &index,
            1,
            CapabilitySet::new()
                .with_metadata("intent", "A")
                .with_metadata("owner", "alice"),
        );
        // Node 2: intent=B, owner=alice (same owner, different intent)
        index_with_node(
            &index,
            2,
            CapabilitySet::new()
                .with_metadata("intent", "B")
                .with_metadata("owner", "alice"),
        );
        assert_eq!(index.metadata_value_cardinality("intent"), 2);
        assert_eq!(index.metadata_value_cardinality("owner"), 1);

        // Remove node 1; intent A's only node is gone, so
        // intent's cardinality drops to 1. Owner alice still has
        // node 2, so owner's cardinality stays 1.
        index.remove(1);
        assert_eq!(index.metadata_value_cardinality("intent"), 1);
        assert_eq!(index.metadata_value_cardinality("owner"), 1);

        // Remove node 2; both intent and owner are now empty.
        index.remove(2);
        assert_eq!(index.metadata_value_cardinality("intent"), 0);
        assert_eq!(index.metadata_value_cardinality("owner"), 0);
    }

    // ========================================================================
    // CardinalityCache — Phase 4 follow-on of CAPABILITY_ENHANCEMENTS_PLAN.md
    // "Cached cardinality estimates".
    // ========================================================================

    fn small_index_with_metadata() -> CapabilityIndex {
        let index = CapabilityIndex::new();
        for i in 0..5u64 {
            let caps = CapabilitySet::new()
                .with_hardware(HardwareCapabilities::new().with_memory(1 + i as u32))
                .with_metadata("intent", "ml-training");
            index_with_node(&index, i, caps);
        }
        index
    }

    #[test]
    fn cardinality_cache_returns_same_values_as_index() {
        // Pin: the cache is a transparent passthrough on first
        // access — every key returns the same value as direct
        // index lookup.
        let index = small_index_with_metadata();
        let cache = CardinalityCache::new(&index, std::time::Duration::from_secs(60));

        let memory_key = crate::adapter::net::behavior::tag::TagKey::new(
            crate::adapter::net::behavior::tag::TaxonomyAxis::Hardware,
            "memory_gb",
        );
        assert_eq!(
            cache.axis_cardinality(&memory_key),
            index.axis_cardinality(&memory_key),
        );
        assert_eq!(
            cache.metadata_value_cardinality("intent"),
            index.metadata_value_cardinality("intent"),
        );
    }

    #[test]
    fn cardinality_cache_serves_repeat_reads_from_cache() {
        // Pin: a second read of the same key returns the cached
        // value even if the underlying index changes — the cache
        // hides updates until the TTL elapses or refresh() is
        // called explicitly.
        let index = small_index_with_metadata();
        let cache = CardinalityCache::new(&index, std::time::Duration::from_secs(60));

        let memory_key = crate::adapter::net::behavior::tag::TagKey::new(
            crate::adapter::net::behavior::tag::TaxonomyAxis::Hardware,
            "memory_gb",
        );
        let first = cache.axis_cardinality(&memory_key);

        // Mutate the underlying index. The cache shouldn't see this
        // until refresh / TTL elapse.
        index_with_node(
            &index,
            999,
            CapabilitySet::new().with_hardware(HardwareCapabilities::new().with_memory(100)),
        );

        let second = cache.axis_cardinality(&memory_key);
        assert_eq!(
            second, first,
            "cached value should not reflect post-cache mutation"
        );
    }

    #[test]
    fn cardinality_cache_refresh_picks_up_new_data() {
        // Pin: refresh() invalidates the cache; the next access
        // reads fresh data from the index.
        let index = small_index_with_metadata();
        let cache = CardinalityCache::new(&index, std::time::Duration::from_secs(60));

        let memory_key = crate::adapter::net::behavior::tag::TagKey::new(
            crate::adapter::net::behavior::tag::TaxonomyAxis::Hardware,
            "memory_gb",
        );
        let first = cache.axis_cardinality(&memory_key);

        // Mutate index, then explicitly refresh.
        index_with_node(
            &index,
            999,
            CapabilitySet::new().with_hardware(HardwareCapabilities::new().with_memory(100)),
        );
        cache.refresh();

        let after_refresh = cache.axis_cardinality(&memory_key);
        assert_eq!(
            after_refresh,
            first + 1,
            "post-refresh read should see the new node"
        );
    }

    #[test]
    fn cardinality_cache_auto_refresh_on_ttl_expiry() {
        // Pin: the cache auto-refreshes when its TTL elapses.
        let index = small_index_with_metadata();
        // 1 ms TTL — guarantees expiry between two ordinary reads.
        let cache = CardinalityCache::new(&index, std::time::Duration::from_millis(1));

        let memory_key = crate::adapter::net::behavior::tag::TagKey::new(
            crate::adapter::net::behavior::tag::TaxonomyAxis::Hardware,
            "memory_gb",
        );
        let first = cache.axis_cardinality(&memory_key);

        // Mutate index, then sleep past the TTL.
        index_with_node(
            &index,
            999,
            CapabilitySet::new().with_hardware(HardwareCapabilities::new().with_memory(100)),
        );
        std::thread::sleep(std::time::Duration::from_millis(5));

        let after_expiry = cache.axis_cardinality(&memory_key);
        assert_eq!(
            after_expiry,
            first + 1,
            "post-TTL-expiry read should see the new node",
        );
    }

    #[test]
    fn cardinality_cache_is_expired_reflects_deadline() {
        let index = CapabilityIndex::new();
        let cache = CardinalityCache::new(&index, std::time::Duration::from_millis(1));
        // Fresh cache: not expired.
        assert!(!cache.is_expired());
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(cache.is_expired());
    }

    #[test]
    fn cardinality_cache_works_as_planner_provider() {
        // Pin: the planner accepts &CardinalityCache transparently
        // (drop-in replacement for &CapabilityIndex).
        use crate::adapter::net::behavior::Predicate;
        let index = small_index_with_metadata();
        let cache = CardinalityCache::new(&index, std::time::Duration::from_secs(60));

        let pred = Predicate::And(vec![
            Predicate::Exists {
                key: crate::adapter::net::behavior::tag::TagKey::new(
                    crate::adapter::net::behavior::tag::TaxonomyAxis::Hardware,
                    "memory_gb",
                ),
            },
            Predicate::MetadataEquals {
                key: "intent".into(),
                value: "ml-training".into(),
            },
        ]);

        // Use the cache as the planner provider; pin equivalence
        // with the direct-index path.
        let candidate_caps = CapabilitySet::new()
            .with_hardware(HardwareCapabilities::new().with_memory(2))
            .with_metadata("intent", "ml-training");
        let tags: Vec<crate::adapter::net::behavior::Tag> =
            candidate_caps.tags.iter().cloned().collect();
        let ctx = crate::adapter::net::behavior::EvalContext::new(&tags, &candidate_caps.metadata);
        assert_eq!(
            pred.evaluate_with_index(&ctx, &cache),
            pred.evaluate_with_index(&ctx, &index),
        );
    }

    #[test]
    fn find_nodes_matching_uses_cardinality_aware_planner() {
        // Build an index where one node matches a high-cardinality
        // discriminator AND a low-cardinality clause; a different
        // node matches only the low-cardinality clause. The
        // cardinality-aware planner runs the high-cardinality
        // (more selective) clause first → fewer evaluations of the
        // low-cardinality clause. Result is the same; this test
        // pins the *result*, not the ordering (which is internal).
        let index = CapabilityIndex::new();
        // Many distinct memory values → high cardinality of memory_gb
        for i in 0..20u64 {
            let mut caps = CapabilitySet::new().with_hardware(
                HardwareCapabilities::new()
                    .with_memory(1 + i as u32)
                    .with_gpu(GpuInfo::new(
                        if i % 2 == 0 {
                            GpuVendor::Nvidia
                        } else {
                            GpuVendor::Amd
                        },
                        "x",
                        1,
                    )),
            );
            if i == 5 {
                caps = caps.with_metadata("intent", "ml-training");
            }
            index_with_node(&index, i, caps);
        }

        // Predicate: intent=ml-training (low-card metadata) AND memory=6 (high-card axis)
        let pred = crate::adapter::net::behavior::Predicate::And(vec![
            crate::adapter::net::behavior::Predicate::MetadataEquals {
                key: "intent".into(),
                value: "ml-training".into(),
            },
            crate::adapter::net::behavior::Predicate::Equals {
                key: crate::adapter::net::behavior::tag::TagKey::new(
                    crate::adapter::net::behavior::tag::TaxonomyAxis::Hardware,
                    "memory_gb",
                ),
                value: "6".into(),
            },
        ]);
        // Only node 5 matches both clauses.
        let matched = index.find_nodes_matching(&pred);
        assert_eq!(matched, vec![5]);
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

    // ─────────────────────────────────────────────────────────────────
    // v0.4 capability-auth: may_execute gate semantics
    // (CAPABILITY_AUTH_PLAN.md §3 / §7 conformance scenarios)
    // ─────────────────────────────────────────────────────────────────

    /// Build an announcement carrying a single capability tag,
    /// optional `subnet:` / `group:` membership tags, and the three
    /// allow-lists. Helper used across the gate tests.
    #[allow(clippy::too_many_arguments)]
    fn auth_ann(
        node_id: u64,
        version: u64,
        capability_tag: &str,
        membership_subnet: Option<super::super::subnet::SubnetId>,
        membership_groups: &[super::super::group::GroupId],
        allowed_nodes: Vec<u64>,
        allowed_subnets: Vec<super::super::subnet::SubnetId>,
        allowed_groups: Vec<super::super::group::GroupId>,
    ) -> CapabilityAnnouncement {
        let mut caps = CapabilitySet::new().add_tag(capability_tag);
        if let Some(s) = membership_subnet {
            caps = caps.add_tag(s.to_tag());
        }
        for g in membership_groups {
            caps = caps.add_tag(g.to_tag());
        }
        let entity = super::super::super::identity::EntityId::from_bytes([node_id as u8; 32]);
        let mut ann = CapabilityAnnouncement::new(node_id, entity, version, caps);
        ann.allowed_nodes = allowed_nodes;
        ann.allowed_subnets = allowed_subnets;
        ann.allowed_groups = allowed_groups;
        ann
    }

    #[test]
    fn may_execute_permissive_default_when_all_lists_empty() {
        let index = CapabilityIndex::new();
        index.index(auth_ann(
            1,
            1,
            "nrpc:echo",
            None,
            &[],
            vec![],
            vec![],
            vec![],
        ));
        // Caller has no announcement; permissive default still
        // admits — the gate trusts the wire-level binding for
        // the caller's identity, not its announcement.
        assert!(index.may_execute(1, "nrpc:echo", 2));
    }

    #[test]
    fn may_execute_denies_when_target_unknown() {
        let index = CapabilityIndex::new();
        assert!(!index.may_execute(99, "nrpc:echo", 2));
    }

    #[test]
    fn may_execute_denies_when_target_lacks_tag() {
        let index = CapabilityIndex::new();
        index.index(auth_ann(
            1,
            1,
            "nrpc:echo",
            None,
            &[],
            vec![],
            vec![],
            vec![],
        ));
        assert!(!index.may_execute(1, "nrpc:other", 2));
    }

    #[test]
    fn may_execute_allow_by_node_admits_listed_caller_only() {
        let index = CapabilityIndex::new();
        index.index(auth_ann(
            1,
            1,
            "nrpc:echo",
            None,
            &[],
            vec![2],
            vec![],
            vec![],
        ));
        assert!(index.may_execute(1, "nrpc:echo", 2));
        assert!(!index.may_execute(1, "nrpc:echo", 3));
    }

    #[test]
    fn may_execute_allow_by_subnet_uses_caller_announcement_tag() {
        let index = CapabilityIndex::new();
        let subnet = super::super::subnet::SubnetId([0x42; 16]);
        // Target restricts to a subnet.
        index.index(auth_ann(
            1,
            1,
            "nrpc:echo",
            None,
            &[],
            vec![],
            vec![subnet],
            vec![],
        ));
        // Caller declares membership via its own announcement.
        index.index(auth_ann(
            2,
            1,
            "nrpc:other-caller-tag",
            Some(subnet),
            &[],
            vec![],
            vec![],
            vec![],
        ));
        // Outsider declares no subnet.
        index.index(auth_ann(
            3,
            1,
            "nrpc:other-caller-tag",
            None,
            &[],
            vec![],
            vec![],
            vec![],
        ));
        assert!(index.may_execute(1, "nrpc:echo", 2));
        assert!(!index.may_execute(1, "nrpc:echo", 3));
    }

    #[test]
    fn may_execute_allow_by_group_admits_any_member() {
        let index = CapabilityIndex::new();
        let group = super::super::group::GroupId([0x77; 32]);
        index.index(auth_ann(
            1,
            1,
            "nrpc:echo",
            None,
            &[],
            vec![],
            vec![],
            vec![group],
        ));
        index.index(auth_ann(
            2,
            1,
            "nrpc:caller-x",
            None,
            &[group],
            vec![],
            vec![],
            vec![],
        ));
        index.index(auth_ann(
            3,
            1,
            "nrpc:caller-y",
            None,
            &[],
            vec![],
            vec![],
            vec![],
        ));
        assert!(index.may_execute(1, "nrpc:echo", 2));
        assert!(!index.may_execute(1, "nrpc:echo", 3));
    }

    #[test]
    fn may_execute_union_semantics_short_circuit() {
        // All three lists non-empty; caller matches only the
        // node list. Pin that we return true on the first axis
        // match (we don't require all axes to agree).
        let index = CapabilityIndex::new();
        let unmatched_subnet = super::super::subnet::SubnetId([0xAA; 16]);
        let unmatched_group = super::super::group::GroupId([0xBB; 32]);
        index.index(auth_ann(
            1,
            1,
            "nrpc:echo",
            None,
            &[],
            vec![2],
            vec![unmatched_subnet],
            vec![unmatched_group],
        ));
        // Caller declares neither the allowed subnet nor the
        // allowed group, but is in `allowed_nodes`.
        index.index(auth_ann(
            2,
            1,
            "nrpc:caller",
            None,
            &[],
            vec![],
            vec![],
            vec![],
        ));
        assert!(index.may_execute(1, "nrpc:echo", 2));
    }

    #[test]
    fn may_execute_revocation_via_new_announcement_supersedes() {
        // v1 is permissive; B can execute. v2 tightens to
        // [only target] and B is locked out — revocation is
        // just a new announcement with a stricter allow-list.
        let index = CapabilityIndex::new();
        index.index(auth_ann(
            1,
            1,
            "nrpc:echo",
            None,
            &[],
            vec![],
            vec![],
            vec![],
        ));
        assert!(index.may_execute(1, "nrpc:echo", 2));
        index.index(auth_ann(
            1,
            2,
            "nrpc:echo",
            None,
            &[],
            vec![1],
            vec![],
            vec![],
        ));
        assert!(!index.may_execute(1, "nrpc:echo", 2));
    }

    #[test]
    fn may_execute_caller_with_no_announcement_can_still_match_node_list() {
        // Caller's identity is the wire `node_id`; no caller
        // announcement is required for the node-list axis (only
        // for subnet/group lookup, where membership self-
        // declaration lives).
        let index = CapabilityIndex::new();
        index.index(auth_ann(
            1,
            1,
            "nrpc:echo",
            None,
            &[],
            vec![999],
            vec![],
            vec![],
        ));
        assert!(index.may_execute(1, "nrpc:echo", 999));
    }

    #[test]
    fn subnet_of_and_groups_of_parse_self_declared_tags() {
        let index = CapabilityIndex::new();
        let subnet = super::super::subnet::SubnetId([0x11; 16]);
        let g1 = super::super::group::GroupId([0x22; 32]);
        let g2 = super::super::group::GroupId([0x33; 32]);
        index.index(auth_ann(
            7,
            1,
            "nrpc:echo",
            Some(subnet),
            &[g1, g2],
            vec![],
            vec![],
            vec![],
        ));
        assert_eq!(index.subnet_of(7), Some(subnet));
        // Group order is now deterministic (sorted by byte value),
        // so callers don't need to re-sort to compare against an
        // expected sequence.
        assert_eq!(index.groups_of(7), vec![g1, g2]);
        // Unknown node returns None / empty — never a panic.
        assert_eq!(index.subnet_of(999), None);
        assert!(index.groups_of(999).is_empty());
    }

    /// H4 regression — multiple `subnet:<hex>` tags on one
    /// announcement used to pick a hash-order-dependent winner,
    /// making the gate's subnet verdict non-deterministic across
    /// receivers. The model treats subnet membership as
    /// single-valued; multiple distinct subnet tags now collapse
    /// to `None` (out-of-model malformed input → no membership)
    /// rather than admitting one arbitrarily.
    #[test]
    fn subnet_of_returns_none_when_announcement_carries_multiple_subnet_tags() {
        let index = CapabilityIndex::new();
        let s1 = super::super::subnet::SubnetId([0x11; 16]);
        let s2 = super::super::subnet::SubnetId([0x22; 16]);
        // Hand-build a CapabilitySet with two distinct subnet
        // tags. `auth_ann` only takes one Option<SubnetId>, so
        // bypass it.
        let caps = CapabilitySet::new()
            .add_tag(s1.to_tag())
            .add_tag(s2.to_tag())
            .add_tag("nrpc:echo");
        let entity = super::super::super::identity::EntityId::from_bytes([0xCD; 32]);
        let ann = CapabilityAnnouncement::new(5, entity, 1, caps);
        index.index(ann);
        assert_eq!(
            index.subnet_of(5),
            None,
            "two distinct subnet tags must collapse to no membership",
        );
    }

    /// Duplicate `subnet:<hex>` tags pointing at the SAME subnet
    /// are not malformed (set semantics dedup them at the wire
    /// level) — confirm the parser still returns the single value
    /// when only one distinct subnet survives.
    #[test]
    fn subnet_of_picks_single_distinct_value_even_when_set_has_one_entry() {
        let index = CapabilityIndex::new();
        let s1 = super::super::subnet::SubnetId([0x33; 16]);
        // `add_tag` parses and inserts into a HashSet<Tag> — two
        // identical tag strings collapse to one entry. This pins
        // that the dedup happens at the tag-set layer, not at
        // the parser.
        let caps = CapabilitySet::new()
            .add_tag(s1.to_tag())
            .add_tag(s1.to_tag())
            .add_tag("nrpc:echo");
        let entity = super::super::super::identity::EntityId::from_bytes([0xCE; 32]);
        let ann = CapabilityAnnouncement::new(6, entity, 1, caps);
        index.index(ann);
        assert_eq!(index.subnet_of(6), Some(s1));
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

    /// `query()` re-checks `filter.matches()` per candidate against
    /// the live `nodes` entry, so a filter with an always-true
    /// non-indexed predicate (e.g., `min_memory_gb = 0`) must produce
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
                caps = caps.add_tag("even");
            }
            if i % 3 == 0 {
                caps = caps.add_tag("triple");
            }
            let ann = CapabilityAnnouncement::new(i, test_entity(), 1, caps);
            index.index(ann);
        }

        let indexed_only = CapabilityFilter::new()
            .require_tag("even")
            .require_tag("inference");
        // Same predicates plus an always-true non-indexed predicate.
        // `memory_gb` is unsigned so `>= 0` is trivially true.
        let mut with_non_indexed = indexed_only.clone();
        with_non_indexed.min_memory_gb = Some(0);

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
                caps = caps.add_tag("preferred");
            }
            // Vary memory so `prefer_memory` produces a real ordering.
            // Phase A.5.N.3: read-modify-write through views()/setter
            // since the typed `hardware` field is gone.
            let mut hw = caps.views().hardware().clone();
            hw.memory_gb = i as u32 + 1;
            caps.set_hardware(hw);
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

        // With `prefer_memory(1.0)` and our memory_gb assignment, the
        // largest-memory candidate must win — pins the score path.
        let expected_winner = candidates
            .iter()
            .max_by_key(|&&id| {
                index
                    .nodes
                    .get(&id)
                    .map(|n| n.capabilities.views().hardware().memory_gb)
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

    /// Phase A.5.N.2: scope tests now exercise `&HashSet<Tag>`.
    /// Helper parses each input string through the permissive
    /// `Tag::parse` so reserved-prefix tags (`scope:tenant:foo`)
    /// land as `Tag::Reserved`, mirroring real wire-form decoding.
    fn tags_from(strs: &[&str]) -> HashSet<Tag> {
        strs.iter().filter_map(|s| Tag::parse(s).ok()).collect()
    }

    #[test]
    fn scope_from_tags_no_scope_tag_is_global() {
        assert!(matches!(
            scope_from_tags(&tags_from(&[])),
            CapabilityScope::Global
        ));
        assert!(matches!(
            scope_from_tags(&tags_from(&["gpu", "model:llama3"])),
            CapabilityScope::Global
        ));
        // Explicit `scope:global` resolves the same as no tag.
        assert!(matches!(
            scope_from_tags(&tags_from(&[TAG_SCOPE_GLOBAL])),
            CapabilityScope::Global
        ));
    }

    #[test]
    fn scope_from_tags_subnet_local_wins() {
        // Even with tenants and regions present, `subnet-local` is
        // the strictest form and dominates.
        let tags = tags_from(&[
            TAG_SCOPE_SUBNET_LOCAL,
            &format!("{TAG_SCOPE_TENANT_PREFIX}foo"),
            &format!("{TAG_SCOPE_REGION_PREFIX}eu-west"),
        ]);
        assert_eq!(scope_from_tags(&tags), CapabilityScope::SubnetLocal);
    }

    #[test]
    fn scope_from_tags_multiple_tenants() {
        let tags = tags_from(&[
            &format!("{TAG_SCOPE_TENANT_PREFIX}a"),
            &format!("{TAG_SCOPE_TENANT_PREFIX}b"),
            "gpu",
        ]);
        match scope_from_tags(&tags) {
            CapabilityScope::Tenants(mut ts) => {
                // HashSet iteration is unordered; sort for stable comparison.
                ts.sort();
                assert_eq!(ts, vec!["a".to_string(), "b".to_string()]);
            }
            other => panic!("expected Tenants, got {other:?}"),
        }

        // Empty tenant id is silently dropped.
        let tags = tags_from(&[
            TAG_SCOPE_TENANT_PREFIX,
            &format!("{TAG_SCOPE_TENANT_PREFIX}real"),
        ]);
        match scope_from_tags(&tags) {
            CapabilityScope::Tenants(ts) => assert_eq!(ts, vec!["real".to_string()]),
            other => panic!("expected Tenants, got {other:?}"),
        }
    }

    #[test]
    fn scope_from_tags_tenants_and_regions() {
        let tags = tags_from(&[
            &format!("{TAG_SCOPE_TENANT_PREFIX}oem-123"),
            &format!("{TAG_SCOPE_REGION_PREFIX}eu-west"),
        ]);
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
        let local_tags: Vec<String> = caps_local
            .tags
            .iter()
            .map(|t| t.to_string())
            .filter(|s| s.as_str() == TAG_SCOPE_SUBNET_LOCAL)
            .collect();
        assert_eq!(local_tags.len(), 1);
        assert_eq!(
            scope_from_tags(&caps_local.tags),
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

    /// Repro for the failing Go `TestHardwareAndGpuFilter_Matches`:
    /// announce a node with NVIDIA GPU + memory, then query for
    /// `RequireGPU + GPUVendor=Nvidia + MinVRAMGB + MinMemoryGB`.
    /// Self-match must succeed.
    #[test]
    fn hardware_and_gpu_filter_self_matches() {
        let gpu = GpuInfo::new(GpuVendor::Nvidia, "h100", 80);
        let hw = HardwareCapabilities::new()
            .with_cpu(16, 16)
            .with_memory(64)
            .with_gpu(gpu);
        let caps = CapabilitySet::new().with_hardware(hw).add_tag("gpu");

        let index = CapabilityIndex::new();
        let ann = CapabilityAnnouncement::new(42u64, test_entity(), 1, caps);
        index.index(ann);

        let filter = CapabilityFilter::new()
            .require_gpu()
            .with_gpu_vendor(GpuVendor::Nvidia)
            .with_min_vram(40)
            .with_min_memory(32);
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

        let gpu = GpuInfo::new(GpuVendor::Nvidia, "h100", 80);
        let hw = HardwareCapabilities::new()
            .with_cpu(16, 16)
            .with_memory(64)
            .with_gpu(gpu);
        let caps = CapabilitySet::new().with_hardware(hw).add_tag("gpu");

        let mut ann = CapabilityAnnouncement::new(42u64, keypair.entity_id().clone(), 1, caps);
        ann.sign(&keypair);

        let index = CapabilityIndex::new();
        index.index(ann.clone());

        let filter = CapabilityFilter::new()
            .require_gpu()
            .with_gpu_vendor(GpuVendor::Nvidia)
            .with_min_vram(40)
            .with_min_memory(32);
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

    // ========================================================================
    // origin_hash side index (PR-5g)
    // ========================================================================

    /// `get_by_origin_hash` returns the indexed `CapabilitySet`
    /// when an announcement with a matching wire-form origin_hash
    /// has been indexed. Key shape is the low 32 bits of
    /// `EntityId::origin_hash()` (zero-extended back to u64) so
    /// it matches the receiver-side `NetHeader::origin_hash.into()`
    /// projection.
    #[test]
    fn get_by_origin_hash_returns_indexed_caps() {
        // Use a non-zero entity so origin_hash is non-trivial —
        // [0; 32] derives a fixed value, but a different key
        // exercises the index more honestly.
        let entity = super::super::super::identity::EntityId::from_bytes([7u8; 32]);
        let wire_origin = (entity.origin_hash() as u32) as u64;
        let index = CapabilityIndex::new();
        let caps = CapabilitySet::new().add_tag("test-marker");
        index.index(CapabilityAnnouncement::new(42, entity.clone(), 1, caps));

        let resolved = index
            .get_by_origin_hash(wire_origin)
            .expect("side index must surface the indexed caps");
        assert!(resolved
            .tags
            .iter()
            .any(|t| matches!(t, super::super::tag::Tag::Legacy(s) if s == "test-marker")));
    }

    /// Unknown origin_hash returns `None` — fail-closed for the
    /// caller (mesh dispatch path defaults to empty caps and
    /// scope-axis admission rejects).
    #[test]
    fn get_by_origin_hash_returns_none_for_unknown_origin() {
        let index = CapabilityIndex::new();
        let entity = super::super::super::identity::EntityId::from_bytes([1u8; 32]);
        index.index(CapabilityAnnouncement::new(
            1,
            entity,
            1,
            CapabilitySet::new(),
        ));
        // A different origin_hash that wasn't announced.
        assert!(index.get_by_origin_hash(0xDEAD_BEEF_u64).is_none());
    }

    /// `remove` tears down the side-index mapping so a stale
    /// lookup doesn't surface caps from an evicted node.
    #[test]
    fn remove_clears_origin_hash_side_index() {
        let entity = super::super::super::identity::EntityId::from_bytes([2u8; 32]);
        let wire_origin = (entity.origin_hash() as u32) as u64;
        let index = CapabilityIndex::new();
        index.index(CapabilityAnnouncement::new(
            99,
            entity.clone(),
            1,
            CapabilitySet::new().add_tag("before"),
        ));
        assert!(index.get_by_origin_hash(wire_origin).is_some());

        index.remove(99);
        assert!(
            index.get_by_origin_hash(wire_origin).is_none(),
            "side index must clean up on remove"
        );
    }

    /// Re-indexing the same `node_id` with a newer version replaces
    /// the side index entry; the wire origin_hash continues to
    /// resolve to the same node_id (entity is the same), and the
    /// surfaced caps reflect the latest announcement.
    #[test]
    fn reindex_replaces_caps_under_same_origin_hash() {
        let entity = super::super::super::identity::EntityId::from_bytes([3u8; 32]);
        let wire_origin = (entity.origin_hash() as u32) as u64;
        let index = CapabilityIndex::new();
        index.index(CapabilityAnnouncement::new(
            123,
            entity.clone(),
            1,
            CapabilitySet::new().add_tag("v1-marker"),
        ));
        index.index(CapabilityAnnouncement::new(
            123,
            entity,
            2,
            CapabilitySet::new().add_tag("v2-marker"),
        ));

        let resolved = index
            .get_by_origin_hash(wire_origin)
            .expect("side index must point at the freshly-indexed node");
        assert!(resolved
            .tags
            .iter()
            .any(|t| matches!(t, super::super::tag::Tag::Legacy(s) if s == "v2-marker")));
        // The old version's caps shouldn't surface.
        assert!(!resolved
            .tags
            .iter()
            .any(|t| matches!(t, super::super::tag::Tag::Legacy(s) if s == "v1-marker")));
    }

    /// Brute-force two distinct `EntityId`s whose wire-form
    /// `origin_hash` (low 32 bits of the BLAKE2s projection)
    /// collides. Birthday paradox in a 32-bit space: ~50 %
    /// collision probability at ~2¹⁶ = 65 k samples; in practice
    /// a 128 k sample sweep finds one in milliseconds.
    ///
    /// Returns `(entity_a, entity_b, wire_origin_hash)`. Both
    /// entities have node_ids that match their `EntityId::node_id()`
    /// derivation — both pass `node_id == entity_id.node_id()`
    /// inside `handle_capability_announcement` — so the colliding
    /// wire origin_hash is a real semantic concern, not a
    /// constructible-only edge case.
    fn find_colliding_entities() -> (
        super::super::super::identity::EntityId,
        super::super::super::identity::EntityId,
        u64,
    ) {
        use super::super::super::identity::EntityId;
        use std::collections::HashMap;
        // `EntityId::origin_hash()` runs its own BLAKE2s over the
        // 32-byte pubkey under domain `net-origin-v1`. We just
        // need varied 32-byte inputs to sample the u32 wire-hash
        // space — `seed.to_le_bytes()` in the first 8 bytes
        // (zero padding elsewhere) gives us 2^64 distinct inputs
        // without pulling in BLAKE3 or any other hash dep. Each
        // seed → unique EntityId → effectively-random u32 wire
        // origin_hash (per the BLAKE2s distribution).
        let mut seen: HashMap<u32, EntityId> = HashMap::new();
        for seed in 0u64..1 << 20 {
            let mut bytes = [0u8; 32];
            bytes[..8].copy_from_slice(&seed.to_le_bytes());
            let entity = EntityId::from_bytes(bytes);
            let wire = entity.origin_hash() as u32;
            if let Some(prev) = seen.get(&wire) {
                if prev.as_bytes() != entity.as_bytes() {
                    return (prev.clone(), entity, wire as u64);
                }
            }
            seen.insert(wire, entity);
        }
        panic!("no wire-origin_hash collision found in 2^20 trials — birthday paradox math says this should be effectively impossible; check the derivation");
    }

    /// **Fail-closed on wire-origin_hash collision.** Two distinct
    /// entities whose `EntityId::origin_hash()` truncates to the
    /// same `u32` share a `by_origin_hash` slot. Under the
    /// `OriginHashSlot::Multiple` shape, `get_by_origin_hash`
    /// returns `None` for the duration of the collision so
    /// admission cannot use either entity's caps. Both entities
    /// remain individually reachable via `get(node_id_*)`.
    ///
    /// This shape suppresses the *victim's* caps from the
    /// admission surface but cannot impersonate them: a colliding
    /// peer can deny scope-axis admit decisions for legitimate
    /// publishers, but cannot widen them. The mesh's
    /// authorization paths (`ChannelHash` / `node_id`) are
    /// untouched.
    #[test]
    fn get_by_origin_hash_returns_none_on_truncation_collision() {
        let (entity_a, entity_b, wire) = find_colliding_entities();
        let node_a: u64 = 0xAAAA_AAAA_AAAA_AAAA;
        let node_b: u64 = 0xBBBB_BBBB_BBBB_BBBB;

        let index = CapabilityIndex::new();
        // Index A first; the slot is Single(A) and lookup resolves.
        index.index(CapabilityAnnouncement::new(
            node_a,
            entity_a.clone(),
            1,
            CapabilitySet::new().add_tag("from-A"),
        ));
        let resolved_a = index
            .get_by_origin_hash(wire)
            .expect("A must surface via wire origin_hash before B is indexed");
        assert!(resolved_a
            .tags
            .iter()
            .any(|t| matches!(t, super::super::tag::Tag::Legacy(s) if s == "from-A")));

        // Index B with the colliding wire origin_hash.
        index.index(CapabilityAnnouncement::new(
            node_b,
            entity_b.clone(),
            1,
            CapabilitySet::new().add_tag("from-B"),
        ));

        // Lookup is now ambiguous → None. Neither A nor B's caps
        // surface via the side index.
        assert!(
            index.get_by_origin_hash(wire).is_none(),
            "ambiguous slot must fail-closed via None — the caller treats this as publisher-unknown for admission"
        );

        // Both entities are still independently indexed under
        // their distinct node_ids — the collision only affects
        // the origin_hash → node_id projection.
        let direct_a = index
            .get(node_a)
            .expect("A's caps must still be reachable via node_id");
        assert!(direct_a
            .tags
            .iter()
            .any(|t| matches!(t, super::super::tag::Tag::Legacy(s) if s == "from-A")));
        let direct_b = index
            .get(node_b)
            .expect("B's caps must still be reachable via node_id");
        assert!(direct_b
            .tags
            .iter()
            .any(|t| matches!(t, super::super::tag::Tag::Legacy(s) if s == "from-B")));
    }

    /// Removing one of two colliders demotes the slot back to
    /// `Single` and the surviving entity resolves again. The
    /// resolved caps must be the survivor's — never stale state
    /// from the departed claimant.
    #[test]
    fn remove_under_collision_demotes_slot_to_survivor() {
        let (entity_a, entity_b, wire) = find_colliding_entities();
        let node_a: u64 = 0x1111_1111_1111_1111;
        let node_b: u64 = 0x2222_2222_2222_2222;
        let index = CapabilityIndex::new();
        index.index(CapabilityAnnouncement::new(
            node_a,
            entity_a,
            1,
            CapabilitySet::new().add_tag("a-caps"),
        ));
        index.index(CapabilityAnnouncement::new(
            node_b,
            entity_b,
            1,
            CapabilitySet::new().add_tag("b-caps"),
        ));
        // While both are indexed the slot is ambiguous.
        assert!(
            index.get_by_origin_hash(wire).is_none(),
            "both colliders indexed → ambiguous"
        );

        // Remove A — slot demotes to Single(B), lookup resolves to B.
        index.remove(node_a);
        let after = index
            .get_by_origin_hash(wire)
            .expect("survivor (B) must resolve after collider A is removed");
        assert!(after
            .tags
            .iter()
            .any(|t| matches!(t, super::super::tag::Tag::Legacy(s) if s == "b-caps")));
        // Sanity: A's stale caps must NOT surface.
        assert!(
            !after
                .tags
                .iter()
                .any(|t| matches!(t, super::super::tag::Tag::Legacy(s) if s == "a-caps")),
            "stale collider caps must not leak after remove"
        );

        // Remove B too — slot disappears entirely.
        index.remove(node_b);
        assert!(
            index.get_by_origin_hash(wire).is_none(),
            "slot drops when no claimants remain"
        );
    }

    /// Indexing two distinct entities at the same wire
    /// `origin_hash` bumps `collision_count` exactly once. The
    /// counter measures *new* ambiguity events; re-asserting a
    /// node_id that already claims the slot is idempotent.
    #[test]
    fn collision_count_bumps_once_per_new_ambiguity_event() {
        let (entity_a, entity_b, _wire) = find_colliding_entities();
        let index = CapabilityIndex::new();
        assert_eq!(index.collision_count(), 0, "counter starts at zero");
        index.index(CapabilityAnnouncement::new(
            0xAAAA,
            entity_a,
            1,
            CapabilitySet::new().add_tag("a-caps"),
        ));
        assert_eq!(
            index.collision_count(),
            0,
            "first indexing of a fresh wire-hash slot is not a collision"
        );
        index.index(CapabilityAnnouncement::new(
            0xBBBB,
            entity_b,
            1,
            CapabilitySet::new().add_tag("b-caps"),
        ));
        assert_eq!(
            index.collision_count(),
            1,
            "Single → Multiple transition with a distinct node_id bumps once"
        );

        // Re-asserting the SAME node_id (A) does not introduce
        // new ambiguity; the slot already holds {A, B}. Counter
        // stays at 1.
        let (entity_a2, _, _) = find_colliding_entities();
        index.index(CapabilityAnnouncement::new(
            0xAAAA,
            entity_a2,
            2,
            CapabilitySet::new().add_tag("a-caps-v2"),
        ));
        assert_eq!(
            index.collision_count(),
            1,
            "re-asserting an existing claimant must not bump the counter"
        );
    }

    /// Pinning the security boundary: the dispatch-side admission
    /// surface uses `get_by_origin_hash` to resolve the publisher
    /// of an inbound event. When the wire origin_hash is
    /// ambiguous, the lookup must return None so the caller skips
    /// the admission gate entirely — admitting via either
    /// claimant's caps (or via the default `Mesh` scope of an
    /// empty `CapabilitySet`) would be a privilege-escalation
    /// surface for a colliding attacker.
    #[test]
    fn admission_lookup_is_fail_closed_under_collision() {
        let (entity_a, entity_b, wire) = find_colliding_entities();
        let node_a: u64 = 0xA0A0_A0A0_A0A0_A0A0;
        let node_b: u64 = 0xB0B0_B0B0_B0B0_B0B0;
        let index = CapabilityIndex::new();

        // A claims a narrow scope (Zone). B claims a wide scope
        // (Mesh). Under the prior last-writer-wins semantics, B
        // overwrites A's slot and a victim's narrow-scope claim
        // is laundered into a Mesh claim — admission would admit
        // mesh-wide pulls under A's wire hash.
        index.index(CapabilityAnnouncement::new(
            node_a,
            entity_a,
            1,
            CapabilitySet::new()
                .add_tag("a-publisher")
                .add_tag("dataforts.greedy.scope=zone"),
        ));
        index.index(CapabilityAnnouncement::new(
            node_b,
            entity_b,
            1,
            CapabilitySet::new()
                .add_tag("b-publisher")
                .add_tag("dataforts.greedy.scope=mesh"),
        ));

        // The fail-closed contract: lookup returns None. Caller
        // (mesh dispatch) treats this as publisher-unknown and
        // skips the greedy admission entirely.
        assert!(
            index.get_by_origin_hash(wire).is_none(),
            "ambiguous publisher caps must NOT resolve through the admission lookup"
        );
        assert_eq!(
            index.collision_count(),
            1,
            "collision counter records the ambiguity event for operator observability"
        );
    }
}
