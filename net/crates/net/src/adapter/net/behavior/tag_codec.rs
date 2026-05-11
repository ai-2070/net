//! Typed-struct ↔ tag-set bijection — Phase A.5.0 of the
//! Capability System Plan.
//!
//! Builds the encoding scheme that Phase A.5.1+ uses to migrate
//! `CapabilitySet`'s wire format from typed fields to
//! `tags: HashSet<Tag>`. This module ships the conversion logic
//! and the round-trip tests that pin the encoding; the actual
//! struct migration lands in a follow-up.
//!
//! ## Encoding scheme
//!
//! Each typed-struct field becomes one or more axis-prefixed tags:
//!
//! ```text
//! HardwareCapabilities {
//!     cpu_cores: 16,
//!     cpu_threads: 32,
//!     memory_gb: 64,
//!     gpu: Some(GpuInfo { vendor: Nvidia, model: "RTX 4090", vram_gb: 24, ... }),
//!     storage_gb: 2000,
//!     network_gbps: 10,
//!     ..
//! }
//! ```
//!
//! becomes:
//!
//! ```text
//! hardware.cpu_cores=16
//! hardware.cpu_threads=32
//! hardware.memory_gb=64
//! hardware.gpu                              ← presence marker
//! hardware.gpu.vendor=nvidia
//! hardware.gpu.model=RTX 4090
//! hardware.gpu.vram_gb=24
//! hardware.gpu.compute_units=128
//! hardware.gpu.tensor_cores=512
//! hardware.gpu.fp16_tflops_x10=825
//! hardware.storage_gb=2000
//! hardware.network_gbps=10
//! ```
//!
//! Zero-valued / empty / `None` fields are omitted from emission,
//! so a default `HardwareCapabilities` round-trips through an
//! empty tag set. The reverse direction skips axis-prefixed tags
//! whose key isn't recognized — keeps forward compatibility when
//! a newer-version peer emits a key this binary doesn't know yet.
//!
//! ## Lossiness (deferred items)
//!
//! Multi-GPU (`HardwareCapabilities::additional_gpus`) and
//! accelerators (`accelerators: Vec<AcceleratorInfo>`) are NOT
//! encoded in this commit. The bijection is exact for the
//! single-GPU / no-accelerator case; multi-device encoding lands
//! with Phase A.5.1 (likely an indexed-key scheme like
//! `hardware.gpu.0.*` / `hardware.gpu.1.*`). The current encoding
//! is documented in tests as a "lossy round-trip drops
//! additional_gpus / accelerators" so a future regression is loud.

use std::collections::{BTreeMap, HashSet};

use crate::adapter::net::behavior::capability::{
    CapabilitySet, GpuInfo, GpuVendor, HardwareCapabilities, Modality, ModelCapability,
    ResourceLimits, SoftwareCapabilities, ToolCapability,
};
use crate::adapter::net::behavior::tag::{AxisSeparator, Tag, TaxonomyAxis};

// =============================================================================
// Forward direction: HardwareCapabilities → Vec<Tag>
// =============================================================================

/// Encode a `HardwareCapabilities` into the canonical axis-prefixed
/// tag list. Order is stable (matches struct-field declaration
/// order) so byte-equal serializations produce byte-equal tag
/// sequences.
///
/// See module docs for the encoding scheme.
pub fn hardware_to_tags(hw: &HardwareCapabilities) -> Vec<Tag> {
    let mut tags = Vec::new();

    if hw.cpu_cores > 0 {
        tags.push(axis_value("cpu_cores", &hw.cpu_cores.to_string()));
    }
    if hw.cpu_threads > 0 {
        tags.push(axis_value("cpu_threads", &hw.cpu_threads.to_string()));
    }
    if hw.memory_gb > 0 {
        tags.push(axis_value("memory_gb", &hw.memory_gb.to_string()));
    }
    if let Some(gpu) = &hw.gpu {
        // Presence marker first so callers can existence-check via
        // `hardware.gpu` without having to enumerate sub-keys.
        tags.push(axis_present("gpu"));
        encode_gpu_into("gpu", gpu, &mut tags);
    }
    if hw.storage_gb > 0 {
        tags.push(axis_value("storage_gb", &hw.storage_gb.to_string()));
    }
    if hw.network_gbps > 0 {
        tags.push(axis_value("network_gbps", &hw.network_gbps.to_string()));
    }

    // additional_gpus + accelerators: deferred to Phase A.5.1.
    // Document the lossiness in tests; emitter intentionally skips
    // them to keep this slice small.

    tags
}

/// Encode a `GpuInfo` under the given key prefix
/// (`gpu` / future: `gpu.0` / `gpu.1` for multi-GPU).
fn encode_gpu_into(prefix: &str, gpu: &GpuInfo, tags: &mut Vec<Tag>) {
    if gpu.vendor != GpuVendor::Unknown {
        tags.push(axis_value(
            &format!("{prefix}.vendor"),
            gpu_vendor_str(gpu.vendor),
        ));
    }
    if !gpu.model.is_empty() {
        tags.push(axis_value(&format!("{prefix}.model"), &gpu.model));
    }
    if gpu.vram_gb > 0 {
        tags.push(axis_value(
            &format!("{prefix}.vram_gb"),
            &gpu.vram_gb.to_string(),
        ));
    }
    if gpu.compute_units > 0 {
        tags.push(axis_value(
            &format!("{prefix}.compute_units"),
            &gpu.compute_units.to_string(),
        ));
    }
    if gpu.tensor_cores > 0 {
        tags.push(axis_value(
            &format!("{prefix}.tensor_cores"),
            &gpu.tensor_cores.to_string(),
        ));
    }
    if gpu.fp16_tflops_x10 > 0 {
        tags.push(axis_value(
            &format!("{prefix}.fp16_tflops_x10"),
            &gpu.fp16_tflops_x10.to_string(),
        ));
    }
}

/// Build a `hardware.<key>` presence tag.
fn axis_present(key: &str) -> Tag {
    Tag::AxisPresent {
        axis: TaxonomyAxis::Hardware,
        key: key.to_string(),
    }
}

/// Build a `hardware.<key>=<value>` value tag.
fn axis_value(key: &str, value: &str) -> Tag {
    Tag::AxisValue {
        axis: TaxonomyAxis::Hardware,
        key: key.to_string(),
        value: value.to_string(),
        separator: AxisSeparator::Eq,
    }
}

/// Lowercase string form of a `GpuVendor` for tag emission. Inverse
/// of [`gpu_vendor_from_str`].
fn gpu_vendor_str(v: GpuVendor) -> &'static str {
    match v {
        GpuVendor::Unknown => "unknown",
        GpuVendor::Nvidia => "nvidia",
        GpuVendor::Amd => "amd",
        GpuVendor::Intel => "intel",
        GpuVendor::Apple => "apple",
        GpuVendor::Qualcomm => "qualcomm",
    }
}

/// Inverse of [`gpu_vendor_str`]. Unknown spellings parse as
/// `GpuVendor::Unknown` (forward-compat: a newer peer's vendor
/// string shouldn't fault our parser).
fn gpu_vendor_from_str(s: &str) -> GpuVendor {
    match s {
        "nvidia" => GpuVendor::Nvidia,
        "amd" => GpuVendor::Amd,
        "intel" => GpuVendor::Intel,
        "apple" => GpuVendor::Apple,
        "qualcomm" => GpuVendor::Qualcomm,
        _ => GpuVendor::Unknown,
    }
}

// =============================================================================
// Reverse direction: &[Tag] → HardwareCapabilities
// =============================================================================

/// Decode a `HardwareCapabilities` from a tag list. Tags that
/// don't belong to the `hardware` axis are ignored; tags whose
/// axis is `hardware` but whose key isn't recognized are also
/// ignored (forward compatibility).
///
/// Numeric / vendor parse failures fall back to defaults — a
/// malformed peer tag shouldn't fault our reconstruction.
pub fn hardware_from_tags(tags: &[Tag]) -> HardwareCapabilities {
    let mut hw = HardwareCapabilities::new();
    let mut gpu: Option<GpuInfo> = None;

    for tag in tags {
        let Some(key) = tag.axis_key() else { continue };
        if key.axis != TaxonomyAxis::Hardware {
            continue;
        }
        let value = tag.value().unwrap_or("");
        match key.key.as_str() {
            "cpu_cores" => {
                hw.cpu_cores = value.parse().unwrap_or(0);
            }
            "cpu_threads" => {
                hw.cpu_threads = value.parse().unwrap_or(0);
            }
            "memory_gb" => {
                hw.memory_gb = value.parse().unwrap_or(0);
            }
            "storage_gb" => {
                hw.storage_gb = value.parse().unwrap_or(0);
            }
            "network_gbps" => {
                hw.network_gbps = value.parse().unwrap_or(0);
            }
            "gpu" => {
                // Presence marker — initialize an empty GpuInfo
                // that subsequent `gpu.*` tags fill in.
                gpu.get_or_insert_with(GpuInfo::default);
            }
            other if other.starts_with("gpu.") => {
                let sub = &other["gpu.".len()..];
                let g = gpu.get_or_insert_with(GpuInfo::default);
                decode_gpu_field(g, sub, value);
            }
            // Forward compat: unknown keys silently ignored.
            _ => {}
        }
    }

    hw.gpu = gpu;
    hw
}

/// Set one field on `gpu` from a `(sub_key, value)` pair where
/// `sub_key` is the part after `hardware.gpu.`. Unknown sub-keys
/// are silently ignored (forward compat).
fn decode_gpu_field(gpu: &mut GpuInfo, sub_key: &str, value: &str) {
    match sub_key {
        "vendor" => {
            gpu.vendor = gpu_vendor_from_str(value);
        }
        "model" => {
            gpu.model = value.to_string();
        }
        "vram_gb" => {
            gpu.vram_gb = value.parse().unwrap_or(0);
        }
        "compute_units" => {
            gpu.compute_units = value.parse().unwrap_or(0);
        }
        "tensor_cores" => {
            gpu.tensor_cores = value.parse().unwrap_or(0);
        }
        "fp16_tflops_x10" => {
            gpu.fp16_tflops_x10 = value.parse().unwrap_or(0);
        }
        _ => {}
    }
}

// =============================================================================
// SoftwareCapabilities ↔ tags
// =============================================================================

/// Encode a `SoftwareCapabilities` into the canonical axis-prefixed
/// tag list. Order is stable: `os` / `os_version` / `runtimes` (in
/// emission order) / `frameworks` (in emission order) / `cuda_version`
/// / `drivers` (in emission order). Empty / `None` fields are
/// omitted so a default `SoftwareCapabilities` produces an empty
/// tag list.
///
/// Encoding scheme:
///
/// ```text
/// software.os=linux
/// software.os_version=6.1
/// software.runtime.python=3.11             ← one tag per (name, version) tuple
/// software.runtime.node=20
/// software.framework.pytorch=2.1
/// software.framework.tensorflow=2.15
/// software.cuda_version=12.1
/// software.driver.nvidia=535.86.10
/// ```
pub fn software_to_tags(sw: &SoftwareCapabilities) -> Vec<Tag> {
    let mut tags = Vec::new();

    if !sw.os.is_empty() {
        tags.push(software_value("os", &sw.os));
    }
    if !sw.os_version.is_empty() {
        tags.push(software_value("os_version", &sw.os_version));
    }
    for (name, version) in &sw.runtimes {
        if is_round_trippable_subkey(name) {
            tags.push(software_value(&format!("runtime.{name}"), version));
        }
    }
    for (name, version) in &sw.frameworks {
        if is_round_trippable_subkey(name) {
            tags.push(software_value(&format!("framework.{name}"), version));
        }
    }
    if let Some(cuda) = &sw.cuda_version {
        tags.push(software_value("cuda_version", cuda));
    }
    for (name, version) in &sw.drivers {
        if is_round_trippable_subkey(name) {
            tags.push(software_value(&format!("driver.{name}"), version));
        }
    }

    tags
}

/// CR-24: a `software.runtime.{name}={version}` tag round-trips
/// via `Tag::parse`, which splits on the first `=` (or `:`).
/// A name containing `=`, `:`, or `.` smears the split: e.g.
/// `name="python=foo"` produces `software.runtime.python=foo=3.11`,
/// which parses back as key=`runtime.python`, value=`foo=3.11`,
/// silently truncating the name. Skip such names at encode time —
/// matches the codec's existing forward-compat skip pattern for
/// unrecognized keys (silent drop, not panic).
fn is_round_trippable_subkey(name: &str) -> bool {
    !name.is_empty() && !name.chars().any(|c| matches!(c, '=' | ':' | '.'))
}

/// Decode a `SoftwareCapabilities` from a tag list. Tags whose
/// axis isn't `software` are ignored; unrecognized `software.*`
/// keys are also ignored (forward compat). Runtime / framework /
/// driver order is preserved across the round-trip via `Vec`
/// insertion order.
pub fn software_from_tags(tags: &[Tag]) -> SoftwareCapabilities {
    let mut sw = SoftwareCapabilities::new();

    for tag in tags {
        let Some(key) = tag.axis_key() else { continue };
        if key.axis != TaxonomyAxis::Software {
            continue;
        }
        let value = tag.value().unwrap_or("");
        match key.key.as_str() {
            "os" => sw.os = value.to_string(),
            "os_version" => sw.os_version = value.to_string(),
            "cuda_version" => sw.cuda_version = Some(value.to_string()),
            other if other.starts_with("runtime.") => {
                let name = &other["runtime.".len()..];
                if !name.is_empty() {
                    sw.runtimes.push((name.to_string(), value.to_string()));
                }
            }
            other if other.starts_with("framework.") => {
                let name = &other["framework.".len()..];
                if !name.is_empty() {
                    sw.frameworks.push((name.to_string(), value.to_string()));
                }
            }
            other if other.starts_with("driver.") => {
                let name = &other["driver.".len()..];
                if !name.is_empty() {
                    sw.drivers.push((name.to_string(), value.to_string()));
                }
            }
            _ => {}
        }
    }

    sw
}

/// Build a `software.<key>=<value>` tag. Sub-key separators inside
/// `<key>` (e.g. `runtime.python`) are part of the key itself.
fn software_value(key: &str, value: &str) -> Tag {
    Tag::AxisValue {
        axis: TaxonomyAxis::Software,
        key: key.to_string(),
        value: value.to_string(),
        separator: AxisSeparator::Eq,
    }
}

// =============================================================================
// ResourceLimits ↔ tags
// =============================================================================

/// Encode `ResourceLimits` into the canonical axis-prefixed tag
/// list. Resource limits sit under the `hardware.limits.*` sub-key
/// (operational caps tied to the node's compute capacity); zero-
/// valued fields are omitted.
///
/// Encoding scheme:
///
/// ```text
/// hardware.limits.max_concurrent_requests=10
/// hardware.limits.max_tokens_per_request=4096
/// hardware.limits.rate_limit_rpm=600
/// hardware.limits.max_batch_size=32
/// hardware.limits.max_input_bytes=1048576
/// hardware.limits.max_output_bytes=1048576
/// ```
pub fn resource_limits_to_tags(limits: &ResourceLimits) -> Vec<Tag> {
    let mut tags = Vec::new();

    if limits.max_concurrent_requests > 0 {
        tags.push(limits_value(
            "max_concurrent_requests",
            &limits.max_concurrent_requests.to_string(),
        ));
    }
    if limits.max_tokens_per_request > 0 {
        tags.push(limits_value(
            "max_tokens_per_request",
            &limits.max_tokens_per_request.to_string(),
        ));
    }
    if limits.rate_limit_rpm > 0 {
        tags.push(limits_value(
            "rate_limit_rpm",
            &limits.rate_limit_rpm.to_string(),
        ));
    }
    if limits.max_batch_size > 0 {
        tags.push(limits_value(
            "max_batch_size",
            &limits.max_batch_size.to_string(),
        ));
    }
    if limits.max_input_bytes > 0 {
        tags.push(limits_value(
            "max_input_bytes",
            &limits.max_input_bytes.to_string(),
        ));
    }
    if limits.max_output_bytes > 0 {
        tags.push(limits_value(
            "max_output_bytes",
            &limits.max_output_bytes.to_string(),
        ));
    }

    tags
}

/// Decode `ResourceLimits` from a tag list. Tags outside the
/// `hardware.limits.*` namespace are ignored; malformed numerics
/// fall back to defaults.
pub fn resource_limits_from_tags(tags: &[Tag]) -> ResourceLimits {
    let mut limits = ResourceLimits::new();

    for tag in tags {
        let Some(key) = tag.axis_key() else { continue };
        if key.axis != TaxonomyAxis::Hardware {
            continue;
        }
        let Some(sub) = key.key.strip_prefix("limits.") else {
            continue;
        };
        let value = tag.value().unwrap_or("");
        match sub {
            "max_concurrent_requests" => {
                limits.max_concurrent_requests = value.parse().unwrap_or(0);
            }
            "max_tokens_per_request" => {
                limits.max_tokens_per_request = value.parse().unwrap_or(0);
            }
            "rate_limit_rpm" => {
                limits.rate_limit_rpm = value.parse().unwrap_or(0);
            }
            "max_batch_size" => {
                limits.max_batch_size = value.parse().unwrap_or(0);
            }
            "max_input_bytes" => {
                limits.max_input_bytes = value.parse().unwrap_or(0);
            }
            "max_output_bytes" => {
                limits.max_output_bytes = value.parse().unwrap_or(0);
            }
            _ => {}
        }
    }

    limits
}

/// Build a `hardware.limits.<key>=<value>` tag. The `limits.` sub-
/// prefix is part of the key, not a separate axis — `ResourceLimits`
/// are operational caps on the hardware, so they live under the
/// hardware axis namespace.
fn limits_value(key: &str, value: &str) -> Tag {
    Tag::AxisValue {
        axis: TaxonomyAxis::Hardware,
        key: format!("limits.{key}"),
        value: value.to_string(),
        separator: AxisSeparator::Eq,
    }
}

// =============================================================================
// ModelCapability list ↔ tags
// =============================================================================

/// Encode a `&[ModelCapability]` into the canonical axis-prefixed
/// tag list. Each model gets a numeric index so the round-trip
/// preserves order:
///
/// ```text
/// software.model.0.id=llama-3.1-70b
/// software.model.0.family=llama
/// software.model.0.parameters_b_x10=700
/// software.model.0.context_length=128000
/// software.model.0.quantization=fp16
/// software.model.0.modalities=text,code
/// software.model.0.tokens_per_sec=50
/// software.model.0.loaded=true
/// software.model.1.id=mistral-7b
/// software.model.1.family=mistral
/// ...
/// ```
///
/// Numeric indexing matches the multi-GPU encoding scheme noted as
/// a TODO in the hardware codec — same pattern, applied here for
/// the `Vec<ModelCapability>` case.
pub fn models_to_tags(models: &[ModelCapability]) -> Vec<Tag> {
    let mut tags = Vec::new();
    for (i, model) in models.iter().enumerate() {
        let prefix = format!("model.{i}");
        if !model.model_id.is_empty() {
            tags.push(software_value(&format!("{prefix}.id"), &model.model_id));
        }
        if !model.family.is_empty() {
            tags.push(software_value(&format!("{prefix}.family"), &model.family));
        }
        if model.parameters_b_x10 > 0 {
            tags.push(software_value(
                &format!("{prefix}.parameters_b_x10"),
                &model.parameters_b_x10.to_string(),
            ));
        }
        if model.context_length > 0 {
            tags.push(software_value(
                &format!("{prefix}.context_length"),
                &model.context_length.to_string(),
            ));
        }
        if let Some(q) = &model.quantization {
            tags.push(software_value(&format!("{prefix}.quantization"), q));
        }
        if !model.modalities.is_empty() {
            let csv = model
                .modalities
                .iter()
                .map(|m| modality_str(*m))
                .collect::<Vec<_>>()
                .join(",");
            tags.push(software_value(&format!("{prefix}.modalities"), &csv));
        }
        if model.tokens_per_sec > 0 {
            tags.push(software_value(
                &format!("{prefix}.tokens_per_sec"),
                &model.tokens_per_sec.to_string(),
            ));
        }
        if model.loaded {
            tags.push(software_value(&format!("{prefix}.loaded"), "true"));
        }
    }
    tags
}

/// Decode a `Vec<ModelCapability>` from a tag list. Models are
/// indexed by their numeric position in the encoding; output Vec
/// is sorted by index so round-trip ordering matches input.
pub fn models_from_tags(tags: &[Tag]) -> Vec<ModelCapability> {
    // Group sub-tags by model index so we can reconstruct the
    // ModelCapability at each index in one pass.
    let mut by_index: BTreeMap<u32, ModelFields> = BTreeMap::new();
    for tag in tags {
        let Some(key) = tag.axis_key() else { continue };
        if key.axis != TaxonomyAxis::Software {
            continue;
        }
        let Some(rest) = key.key.strip_prefix("model.") else {
            continue;
        };
        let Some((idx_str, sub)) = rest.split_once('.') else {
            continue;
        };
        let Ok(idx) = idx_str.parse::<u32>() else {
            continue;
        };
        let value = tag.value().unwrap_or("");
        let entry = by_index.entry(idx).or_default();
        match sub {
            "id" => entry.model_id = Some(value.to_string()),
            "family" => entry.family = Some(value.to_string()),
            "parameters_b_x10" => {
                entry.parameters_b_x10 = value.parse().ok();
            }
            "context_length" => {
                entry.context_length = value.parse().ok();
            }
            "quantization" => entry.quantization = Some(value.to_string()),
            "modalities" => {
                entry.modalities = value.split(',').map(modality_from_str).collect::<Vec<_>>();
            }
            "tokens_per_sec" => {
                entry.tokens_per_sec = value.parse().ok();
            }
            "loaded" => {
                entry.loaded = Some(value == "true");
            }
            _ => {}
        }
    }
    by_index
        .into_values()
        .map(|f| f.into_model_capability())
        .collect()
}

/// Intermediate accumulator for a single `ModelCapability` during
/// decode. Each Option tracks "did we see this sub-tag?" so the
/// final `into_model_capability` knows which substrate-default
/// values to fall back to.
#[derive(Default)]
struct ModelFields {
    model_id: Option<String>,
    family: Option<String>,
    parameters_b_x10: Option<u32>,
    context_length: Option<u32>,
    quantization: Option<String>,
    modalities: Vec<Modality>,
    tokens_per_sec: Option<u32>,
    loaded: Option<bool>,
}

impl ModelFields {
    fn into_model_capability(self) -> ModelCapability {
        ModelCapability {
            model_id: self.model_id.unwrap_or_default(),
            family: self.family.unwrap_or_default(),
            parameters_b_x10: self.parameters_b_x10.unwrap_or(0),
            context_length: self.context_length.unwrap_or(0),
            quantization: self.quantization,
            // Empty modalities list is meaningful — applications
            // build a model with explicit modalities, never
            // implicitly defaulting to text. Match that here:
            // if no modalities tag was emitted, the decoded list
            // is empty (matches the substrate field being empty).
            modalities: self.modalities,
            tokens_per_sec: self.tokens_per_sec.unwrap_or(0),
            loaded: self.loaded.unwrap_or(false),
        }
    }
}

/// Lowercase string form of a `Modality`. Inverse of
/// [`modality_from_str`].
fn modality_str(m: Modality) -> &'static str {
    match m {
        Modality::Text => "text",
        Modality::Image => "image",
        Modality::Audio => "audio",
        Modality::Video => "video",
        Modality::Code => "code",
        Modality::Embedding => "embedding",
        Modality::ToolUse => "tool_use",
    }
}

/// Inverse of [`modality_str`]. Unknown spellings fall back to
/// `Modality::Text` (forward compat: a peer's new modality string
/// shouldn't fault our parser).
fn modality_from_str(s: &str) -> Modality {
    match s {
        "text" => Modality::Text,
        "image" => Modality::Image,
        "audio" => Modality::Audio,
        "video" => Modality::Video,
        "code" => Modality::Code,
        "embedding" => Modality::Embedding,
        "tool_use" => Modality::ToolUse,
        _ => Modality::Text,
    }
}

// =============================================================================
// ToolCapability list ↔ tags
// =============================================================================

/// Encode a `&[ToolCapability]` into the canonical axis-prefixed
/// tag list. Same indexed-key pattern as models:
///
/// ```text
/// software.tool.0.tool_id=python_repl
/// software.tool.0.name=Python REPL
/// software.tool.0.version=1.0.0
/// software.tool.0.estimated_time_ms=100
/// software.tool.0.stateless=true
/// software.tool.0.requires=python:3.11,sqlite
/// ```
///
/// **Lossiness**: `input_schema` / `output_schema` (JSON Schema
/// strings) are NOT encoded — they contain `=`, `:`, `,`, etc.
/// that can't safely round-trip through the tag wire format.
/// Phase C's metadata field is the natural carrier for those
/// (key-value blobs); the codec defers to it.
pub fn tools_to_tags(tools: &[ToolCapability]) -> Vec<Tag> {
    let mut tags = Vec::new();
    for (i, tool) in tools.iter().enumerate() {
        let prefix = format!("tool.{i}");
        if !tool.tool_id.is_empty() {
            tags.push(software_value(&format!("{prefix}.tool_id"), &tool.tool_id));
        }
        if !tool.name.is_empty() {
            tags.push(software_value(&format!("{prefix}.name"), &tool.name));
        }
        if !tool.version.is_empty() {
            tags.push(software_value(&format!("{prefix}.version"), &tool.version));
        }
        // input_schema / output_schema deferred — see fn doc.
        if !tool.requires.is_empty() {
            let csv = tool.requires.join(",");
            tags.push(software_value(&format!("{prefix}.requires"), &csv));
        }
        if tool.estimated_time_ms > 0 {
            tags.push(software_value(
                &format!("{prefix}.estimated_time_ms"),
                &tool.estimated_time_ms.to_string(),
            ));
        }
        // `stateless` always emitted because the substrate's
        // ToolCapability::new defaults it to true; round-trip
        // needs to preserve the bool faithfully.
        tags.push(software_value(
            &format!("{prefix}.stateless"),
            if tool.stateless { "true" } else { "false" },
        ));
    }
    tags
}

/// Decode a `Vec<ToolCapability>` from a tag list. Same indexed-
/// reconstruction pattern as models.
pub fn tools_from_tags(tags: &[Tag]) -> Vec<ToolCapability> {
    let mut by_index: BTreeMap<u32, ToolFields> = BTreeMap::new();
    for tag in tags {
        let Some(key) = tag.axis_key() else { continue };
        if key.axis != TaxonomyAxis::Software {
            continue;
        }
        let Some(rest) = key.key.strip_prefix("tool.") else {
            continue;
        };
        let Some((idx_str, sub)) = rest.split_once('.') else {
            continue;
        };
        let Ok(idx) = idx_str.parse::<u32>() else {
            continue;
        };
        let value = tag.value().unwrap_or("");
        let entry = by_index.entry(idx).or_default();
        match sub {
            "tool_id" => entry.tool_id = Some(value.to_string()),
            "name" => entry.name = Some(value.to_string()),
            "version" => entry.version = Some(value.to_string()),
            "requires" if !value.is_empty() => {
                entry.requires = value.split(',').map(|s| s.to_string()).collect::<Vec<_>>();
            }
            "estimated_time_ms" => {
                entry.estimated_time_ms = value.parse().ok();
            }
            "stateless" => {
                entry.stateless = Some(value == "true");
            }
            _ => {}
        }
    }
    by_index
        .into_values()
        .map(|f| f.into_tool_capability())
        .collect()
}

#[derive(Default)]
struct ToolFields {
    tool_id: Option<String>,
    name: Option<String>,
    version: Option<String>,
    requires: Vec<String>,
    estimated_time_ms: Option<u32>,
    stateless: Option<bool>,
}

impl ToolFields {
    fn into_tool_capability(self) -> ToolCapability {
        ToolCapability {
            tool_id: self.tool_id.unwrap_or_default(),
            name: self.name.unwrap_or_default(),
            // ToolCapability::new sets version to "1.0.0" by default,
            // not "" — preserve the substrate convention so a
            // tool created via `ToolCapability::new` round-trips
            // unchanged when the user didn't override it.
            version: self.version.unwrap_or_else(|| "1.0.0".to_string()),
            input_schema: None, // deferred (Phase C metadata carrier)
            output_schema: None,
            requires: self.requires,
            estimated_time_ms: self.estimated_time_ms.unwrap_or(0),
            stateless: self.stateless.unwrap_or(true),
        }
    }
}

// =============================================================================
// Combined CapabilitySet ↔ tag-set bijection
// =============================================================================

/// Encode a `CapabilitySet` into the canonical typed-tag set.
/// Composes the per-struct codecs in declaration order:
///
/// 1. `hardware_to_tags` (`hardware.*` axis tags)
/// 2. `software_to_tags` (`software.*` axis tags)
/// 3. `models_to_tags` (`software.model.<i>.*`)
/// 4. `tools_to_tags` (`software.tool.<i>.*`)
/// 5. `resource_limits_to_tags` (`hardware.limits.*`)
/// 6. Legacy untyped tags from `caps.tags: Vec<String>` parsed via
///    [`Tag::parse`] — preserves reserved-prefix tags (e.g.
///    `scope:tenant:foo`) and lets pre-Warriors untyped tags ride
///    through during the deprecation window.
///
/// Returns a `HashSet<Tag>` because the typed-tag wire format the
/// substrate plan §1 pins is set-membership; deterministic ordering
/// is the encoder's responsibility (per-struct codecs already emit
/// in stable declaration order).
///
/// Phase A.5.N.3: with the typed-struct fields removed,
/// `CapabilitySet::tags` IS the canonical tag set; this function
/// collapses to a clone. Kept as a public API for symmetry with
/// `capability_set_from_tag_set` and so callers don't need to
/// know the storage shape.
pub fn capability_set_to_tag_set(caps: &CapabilitySet) -> HashSet<Tag> {
    caps.tags.clone()
}

/// Decode a `CapabilitySet` from a typed-tag set. Inverse of
/// [`capability_set_to_tag_set`]; uses the per-struct decoders
/// against the same tag set, with the legacy `Vec<String>`
/// carrier reconstructed from any `Tag::Legacy` and `Tag::Reserved`
/// values plus axis-prefixed tags that aren't consumed by the
/// other decoders (e.g. `scope:` tags).
///
/// **Order non-preservation note**: Vec-valued fields whose tag
/// encoding is non-indexed (`runtimes` / `frameworks` / `drivers`
/// on `SoftwareCapabilities`) come out in lexicographic-by-name
/// order, NOT the insertion order of the original
/// `CapabilitySet`. This is a fundamental limitation of the
/// `HashSet<Tag>` wire format — a tag set is unordered by
/// definition. Vec-valued fields whose encoding IS indexed
/// (`models` / `tools`) do preserve insertion order via the
/// numeric index. Phase A.5.1 will revisit whether to migrate
/// runtimes / frameworks / drivers to indexed encoding too;
/// pinned for now in `software_runtime_order_normalized_through_tag_set`.
///
/// This is the inverse for the round-trip pinned by tests; Phase
/// A.5.1 will use it as the deserialization path when
/// `CapabilitySet`'s wire format becomes the typed-tag set.
pub fn capability_set_from_tag_set(tags: &HashSet<Tag>) -> CapabilitySet {
    // Phase A.5.N.3: tags ARE the canonical storage; the
    // per-struct decoders are now exercised by `CapabilitySet::views()`
    // / the `From` impls, on demand. This function simply hands
    // the tag set across into a fresh `CapabilitySet` with empty
    // metadata. Tool schemas (which would have lived in metadata)
    // can't be reconstructed from a bare tag set — callers that
    // need schemas should use the `with_metadata` builder or
    // `set_tools` to seed them.
    CapabilitySet {
        tags: tags.clone(),
        metadata: BTreeMap::new(),
    }
}

// Phase A.5.N.3 dropped the previous `is_struct_owned_tag` /
// `is_hardware_struct_key` / `is_software_struct_key` helpers —
// they routed un-owned tags into a legacy `Vec<String>` carrier
// that no longer exists. The post-A.5.N.3 boundary is named by
// the public `is_*_owned_tag` predicates below, which the
// `set_*` mutators use to clear axis-relevant tags before
// re-encoding.

// =============================================================================
// Phase A.5.N.3 — axis-owned tag predicates for in-place re-encoding.
//
// `set_*` / `with_*` mutators on `CapabilitySet` clear the tags
// owned by one struct before re-emitting the new ones. These
// predicates name the boundaries.
// =============================================================================

/// True if `tag` is a `hardware.*` tag owned by `HardwareCapabilities`
/// (cpu / memory / gpu / storage / network / accelerators), excluding
/// `hardware.limits.*` which is owned by `ResourceLimits`.
pub fn is_hardware_owned_tag(tag: &Tag) -> bool {
    let Some(key) = tag.axis_key() else {
        return false;
    };
    if key.axis != TaxonomyAxis::Hardware {
        return false;
    }
    !key.key.starts_with("limits.")
}

/// True if `tag` is a `hardware.limits.*` tag owned by
/// `ResourceLimits`.
pub fn is_resource_limits_owned_tag(tag: &Tag) -> bool {
    let Some(key) = tag.axis_key() else {
        return false;
    };
    key.axis == TaxonomyAxis::Hardware && key.key.starts_with("limits.")
}

/// True if `tag` is a `software.*` tag owned by `SoftwareCapabilities`
/// (os / cuda / runtimes / frameworks / drivers), excluding the
/// `software.model.*` and `software.tool.*` indexed sub-keys owned
/// by `Vec<ModelCapability>` and `Vec<ToolCapability>`.
pub fn is_software_owned_tag(tag: &Tag) -> bool {
    let Some(key) = tag.axis_key() else {
        return false;
    };
    if key.axis != TaxonomyAxis::Software {
        return false;
    }
    !key.key.starts_with("model.") && !key.key.starts_with("tool.")
}

/// True if `tag` is a `software.model.*` tag owned by
/// `Vec<ModelCapability>`.
pub fn is_models_owned_tag(tag: &Tag) -> bool {
    let Some(key) = tag.axis_key() else {
        return false;
    };
    key.axis == TaxonomyAxis::Software && key.key.starts_with("model.")
}

/// True if `tag` is a `software.tool.*` tag owned by
/// `Vec<ToolCapability>`.
pub fn is_tools_owned_tag(tag: &Tag) -> bool {
    let Some(key) = tag.axis_key() else {
        return false;
    };
    key.axis == TaxonomyAxis::Software && key.key.starts_with("tool.")
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::capability::{
        AcceleratorInfo, AcceleratorType, GpuInfo, GpuVendor, HardwareCapabilities,
    };

    fn full_hardware() -> HardwareCapabilities {
        let gpu = GpuInfo::new(GpuVendor::Nvidia, "RTX 4090", 24)
            .with_compute_units(128)
            .with_tensor_cores(512)
            .with_fp16_tflops(82.5);
        HardwareCapabilities::new()
            .with_cpu(16, 32)
            .with_memory(64)
            .with_gpu(gpu)
            .with_storage(2000)
            .with_network(10)
    }

    // ---- forward direction: hardware → tags ----------------------------

    #[test]
    fn empty_hardware_emits_no_tags() {
        // Pinned: a default `HardwareCapabilities` produces an empty
        // tag list. Otherwise every node would carry baseline
        // hardware-axis tags they didn't author.
        let hw = HardwareCapabilities::default();
        assert!(hardware_to_tags(&hw).is_empty());
    }

    #[test]
    fn full_hardware_emits_canonical_tag_set() {
        let hw = full_hardware();
        let tags = hardware_to_tags(&hw);

        // Convert to wire-form strings for readability.
        let strs: Vec<String> = tags.iter().map(|t| t.to_string()).collect();

        // Pin every emitted tag string. Order matches struct-field
        // declaration order so a struct reorder is loud.
        assert_eq!(
            strs,
            vec![
                "hardware.cpu_cores=16",
                "hardware.cpu_threads=32",
                "hardware.memory_gb=64",
                "hardware.gpu",
                "hardware.gpu.vendor=nvidia",
                "hardware.gpu.model=RTX 4090",
                "hardware.gpu.vram_gb=24",
                "hardware.gpu.compute_units=128",
                "hardware.gpu.tensor_cores=512",
                "hardware.gpu.fp16_tflops_x10=825",
                "hardware.storage_gb=2000",
                "hardware.network_gbps=10",
            ]
        );
    }

    #[test]
    fn cpu_only_hardware_emits_only_cpu_tags() {
        let hw = HardwareCapabilities::new().with_cpu(8, 16);
        let strs: Vec<String> = hardware_to_tags(&hw)
            .iter()
            .map(|t| t.to_string())
            .collect();
        assert_eq!(
            strs,
            vec!["hardware.cpu_cores=8", "hardware.cpu_threads=16"]
        );
    }

    #[test]
    fn gpu_with_only_required_fields_emits_only_those() {
        // Pinned: a GpuInfo with vendor + model + vram set but
        // compute_units / tensor_cores / fp16 zero only emits the
        // first three sub-tags (plus the presence marker). Sparse
        // emission keeps the wire format small.
        let gpu = GpuInfo::new(GpuVendor::Apple, "M2 Ultra", 64);
        let hw = HardwareCapabilities::new().with_gpu(gpu);
        let strs: Vec<String> = hardware_to_tags(&hw)
            .iter()
            .map(|t| t.to_string())
            .collect();
        assert_eq!(
            strs,
            vec![
                "hardware.gpu",
                "hardware.gpu.vendor=apple",
                "hardware.gpu.model=M2 Ultra",
                "hardware.gpu.vram_gb=64",
            ]
        );
    }

    // ---- reverse direction: tags → hardware ----------------------------

    #[test]
    fn empty_tags_decode_to_default_hardware() {
        let hw = hardware_from_tags(&[]);
        assert_eq!(hw, HardwareCapabilities::default());
    }

    #[test]
    fn unknown_axis_tags_are_ignored() {
        // Pinned: tags from other axes don't pollute the
        // hardware reconstruction. Forward compat — bigger
        // capability sets keep working when only the hardware
        // slice is reconstructed.
        let tags = [
            Tag::parse("software.runtime=cuda-12").unwrap(),
            Tag::parse("devices.lidar").unwrap(),
            Tag::parse("scope:prod").unwrap(),
            Tag::parse("hardware.cpu_cores=8").unwrap(),
        ];
        let hw = hardware_from_tags(&tags);
        assert_eq!(hw.cpu_cores, 8);
        assert_eq!(hw.cpu_threads, 0);
    }

    #[test]
    fn unknown_hardware_keys_are_ignored() {
        // Forward compat: a newer peer's `hardware.qpu_qubits=512`
        // doesn't fault — we just skip it and decode what we know.
        let tags = [
            Tag::parse("hardware.qpu_qubits=512").unwrap(),
            Tag::parse("hardware.cpu_cores=8").unwrap(),
        ];
        let hw = hardware_from_tags(&tags);
        assert_eq!(hw.cpu_cores, 8);
    }

    #[test]
    fn malformed_numeric_falls_back_to_default() {
        // Pinned: a peer emitting `hardware.cpu_cores=many` doesn't
        // fault our parser — the value just falls back to 0.
        let tags = [Tag::parse("hardware.cpu_cores=many").unwrap()];
        let hw = hardware_from_tags(&tags);
        assert_eq!(hw.cpu_cores, 0);
    }

    #[test]
    fn unknown_gpu_vendor_falls_back_to_unknown() {
        let tags = [
            Tag::parse("hardware.gpu").unwrap(),
            Tag::parse("hardware.gpu.vendor=brand-x").unwrap(),
        ];
        let hw = hardware_from_tags(&tags);
        let gpu = hw.gpu.expect("gpu presence tag should populate gpu");
        assert_eq!(gpu.vendor, GpuVendor::Unknown);
    }

    // ---- round-trip ----------------------------------------------------

    #[test]
    fn round_trip_full_hardware() {
        // The load-bearing test for Phase A.5.0. A
        // `HardwareCapabilities` with CPU + GPU + storage + network
        // round-trips byte-for-byte through `to_tags` → `from_tags`.
        // If this fails on any field, the wire-format swap in
        // Phase A.5.1+ would silently lose data.
        let hw = full_hardware();
        let tags = hardware_to_tags(&hw);
        let hw2 = hardware_from_tags(&tags);
        assert_eq!(hw, hw2);
    }

    #[test]
    fn round_trip_default_hardware() {
        let hw = HardwareCapabilities::default();
        let tags = hardware_to_tags(&hw);
        let hw2 = hardware_from_tags(&tags);
        assert_eq!(hw, hw2);
    }

    #[test]
    fn round_trip_cpu_only() {
        let hw = HardwareCapabilities::new().with_cpu(4, 8);
        assert_eq!(hardware_from_tags(&hardware_to_tags(&hw)), hw);
    }

    #[test]
    fn round_trip_gpu_only_no_optional_fields() {
        let gpu = GpuInfo::new(GpuVendor::Amd, "MI300X", 192);
        let hw = HardwareCapabilities::new().with_gpu(gpu);
        assert_eq!(hardware_from_tags(&hardware_to_tags(&hw)), hw);
    }

    #[test]
    fn round_trip_through_tag_string_serialization() {
        // Full pipeline: typed → tags → wire strings → parsed tags
        // → typed. Pinned because Phase A.5.1+ ships the
        // `Vec<String>` legacy tag carrier as the cross-binding
        // wire format until the typed-tag wire is fully wired.
        let hw = full_hardware();
        let tags = hardware_to_tags(&hw);
        let wire_strs: Vec<String> = tags.iter().map(|t| t.to_string()).collect();
        let reparsed: Vec<Tag> = wire_strs.iter().map(|s| Tag::parse(s).unwrap()).collect();
        assert_eq!(reparsed, tags);
        let hw2 = hardware_from_tags(&reparsed);
        assert_eq!(hw, hw2);
    }

    // ---- documented lossiness ------------------------------------------

    #[test]
    fn additional_gpus_dropped_until_phase_a5_1() {
        // Pinned: multi-GPU encoding is deferred. A
        // `HardwareCapabilities` with two GPUs round-trips with
        // ONLY the primary GPU preserved; `additional_gpus` is
        // dropped. Phase A.5.1 will land an indexed-key encoding
        // (`hardware.gpu.0.*` / `hardware.gpu.1.*`).
        let primary = GpuInfo::new(GpuVendor::Nvidia, "H100", 80);
        let secondary = GpuInfo::new(GpuVendor::Nvidia, "A100", 40);
        let mut hw = HardwareCapabilities::new().with_gpu(primary.clone());
        hw.additional_gpus.push(secondary);
        let hw2 = hardware_from_tags(&hardware_to_tags(&hw));
        // Primary preserved.
        assert_eq!(hw2.gpu, Some(primary));
        // additional_gpus lost — TODO Phase A.5.1.
        assert!(hw2.additional_gpus.is_empty());
    }

    #[test]
    fn accelerators_dropped_until_phase_a5_1() {
        // Same deferral story for accelerators (TPU / NPU / FPGA).
        let mut hw = HardwareCapabilities::new();
        hw.accelerators
            .push(AcceleratorInfo::new(AcceleratorType::Tpu, "Google TPU v4"));
        let hw2 = hardware_from_tags(&hardware_to_tags(&hw));
        assert!(hw2.accelerators.is_empty());
    }

    // ====================================================================
    // SoftwareCapabilities ↔ tags
    // ====================================================================

    fn full_software() -> SoftwareCapabilities {
        SoftwareCapabilities::new()
            .with_os("linux", "6.1")
            .add_runtime("python", "3.11")
            .add_runtime("node", "20")
            .add_framework("pytorch", "2.1")
            .add_framework("tensorflow", "2.15")
            .with_cuda("12.1")
    }

    #[test]
    fn empty_software_emits_no_tags() {
        let sw = SoftwareCapabilities::default();
        assert!(software_to_tags(&sw).is_empty());
    }

    #[test]
    fn full_software_emits_canonical_tag_set() {
        let sw = full_software();
        let strs: Vec<String> = software_to_tags(&sw)
            .iter()
            .map(|t| t.to_string())
            .collect();
        assert_eq!(
            strs,
            vec![
                "software.os=linux",
                "software.os_version=6.1",
                "software.runtime.python=3.11",
                "software.runtime.node=20",
                "software.framework.pytorch=2.1",
                "software.framework.tensorflow=2.15",
                "software.cuda_version=12.1",
            ]
        );
    }

    #[test]
    fn round_trip_full_software() {
        let sw = full_software();
        let tags = software_to_tags(&sw);
        let sw2 = software_from_tags(&tags);
        assert_eq!(sw, sw2);
    }

    #[test]
    fn round_trip_default_software() {
        let sw = SoftwareCapabilities::default();
        assert_eq!(software_from_tags(&software_to_tags(&sw)), sw);
    }

    #[test]
    fn software_runtime_order_preserved_round_trip() {
        // Pinned: runtimes Vec order is preserved across the round
        // trip via insertion order on the encode side and Vec append
        // on the decode side. A `Vec<(String, String)>` carrier
        // post-Phase-A.5.1 would have the same property.
        let sw = SoftwareCapabilities::new()
            .add_runtime("a", "1")
            .add_runtime("b", "2")
            .add_runtime("c", "3");
        let sw2 = software_from_tags(&software_to_tags(&sw));
        assert_eq!(sw2.runtimes, sw.runtimes);
    }

    #[test]
    fn software_unknown_axis_tags_ignored() {
        let tags = [
            Tag::parse("hardware.cpu_cores=8").unwrap(),
            Tag::parse("software.os=linux").unwrap(),
        ];
        let sw = software_from_tags(&tags);
        assert_eq!(sw.os, "linux");
    }

    #[test]
    fn software_unknown_subkey_ignored() {
        // Forward compat: a peer's `software.future_thing=foo`
        // doesn't fault.
        let tags = [
            Tag::parse("software.future_thing=foo").unwrap(),
            Tag::parse("software.os=linux").unwrap(),
        ];
        let sw = software_from_tags(&tags);
        assert_eq!(sw.os, "linux");
    }

    #[test]
    fn software_runtime_with_empty_name_skipped() {
        // Pinned: a malformed `software.runtime.=1.0` (empty name
        // after the prefix) is dropped, not emitted as
        // `("", "1.0")`. Cross-binding peers shouldn't see synthetic
        // empty-name runtimes.
        let tags = [Tag::parse("software.runtime.=1.0").unwrap()];
        let sw = software_from_tags(&tags);
        assert!(sw.runtimes.is_empty());
    }

    /// CR-24: `software_to_tags` skips runtime / framework / driver
    /// names that contain `=`, `:`, or `.`. A name like `python=foo`
    /// would otherwise produce `software.runtime.python=foo=3.11`,
    /// which `Tag::parse` splits at the first `=` — silently
    /// truncating the name on the receive side. Defensive skip at
    /// encode time matches the codec's forward-compat pattern.
    #[test]
    fn software_to_tags_skips_subkeys_with_separator_chars() {
        let sw = SoftwareCapabilities::new()
            .add_runtime("python=evil", "1.0") // contains '='
            .add_runtime("good-name", "2.0")
            .add_framework("a:b", "9.0") // contains ':'
            .add_framework("normal", "1.1");
        let tags = software_to_tags(&sw);
        let strs: Vec<String> = tags.iter().map(|t| t.to_string()).collect();
        assert!(
            strs.iter().any(|s| s == "software.runtime.good-name=2.0"),
            "valid runtime name must be emitted: {strs:?}"
        );
        assert!(
            strs.iter().any(|s| s == "software.framework.normal=1.1"),
            "valid framework name must be emitted: {strs:?}"
        );
        // Bad names dropped — no `python=evil` or `a:b` smearing.
        assert!(
            !strs.iter().any(|s| s.contains("python=evil")),
            "name with '=' must be dropped: {strs:?}"
        );
        assert!(
            !strs.iter().any(|s| s.contains("a:b")),
            "name with ':' must be dropped: {strs:?}"
        );
    }

    // ====================================================================
    // ResourceLimits ↔ tags
    // ====================================================================

    fn full_limits() -> ResourceLimits {
        ResourceLimits::new()
            .with_max_concurrent(10)
            .with_max_tokens(4096)
            .with_rate_limit(600)
            .with_max_batch(32)
    }

    #[test]
    fn empty_limits_emits_no_tags() {
        let l = ResourceLimits::default();
        assert!(resource_limits_to_tags(&l).is_empty());
    }

    #[test]
    fn full_limits_emits_canonical_tag_set() {
        let l = full_limits();
        let strs: Vec<String> = resource_limits_to_tags(&l)
            .iter()
            .map(|t| t.to_string())
            .collect();
        assert_eq!(
            strs,
            vec![
                "hardware.limits.max_concurrent_requests=10",
                "hardware.limits.max_tokens_per_request=4096",
                "hardware.limits.rate_limit_rpm=600",
                "hardware.limits.max_batch_size=32",
            ]
        );
    }

    #[test]
    fn round_trip_full_limits() {
        let l = full_limits();
        assert_eq!(resource_limits_from_tags(&resource_limits_to_tags(&l)), l);
    }

    #[test]
    fn round_trip_default_limits() {
        let l = ResourceLimits::default();
        assert_eq!(resource_limits_from_tags(&resource_limits_to_tags(&l)), l);
    }

    #[test]
    fn limits_top_level_hardware_keys_ignored() {
        // Pinned: a `hardware.cpu_cores=8` tag does NOT decode into
        // `ResourceLimits`. Only `hardware.limits.*` sub-keys
        // contribute. Phase A.5.1's combined `CapabilitySet`
        // decoder routes hardware vs. limits via this prefix.
        let tags = [
            Tag::parse("hardware.cpu_cores=8").unwrap(),
            Tag::parse("hardware.limits.rate_limit_rpm=120").unwrap(),
        ];
        let l = resource_limits_from_tags(&tags);
        assert_eq!(l.rate_limit_rpm, 120);
        assert_eq!(l.max_concurrent_requests, 0);
    }

    #[test]
    fn limits_unknown_subkey_ignored() {
        let tags = [
            Tag::parse("hardware.limits.future_field=999").unwrap(),
            Tag::parse("hardware.limits.rate_limit_rpm=120").unwrap(),
        ];
        let l = resource_limits_from_tags(&tags);
        assert_eq!(l.rate_limit_rpm, 120);
    }

    #[test]
    fn hardware_decode_skips_limits_subkeys() {
        // Pinned: the hardware decoder MUST skip `hardware.limits.*`
        // tags so they don't pollute the typed-struct fields.
        // Phase A.5.1 will rely on this clean separation when
        // `CapabilitySet` decodes into both HardwareCapabilities
        // and ResourceLimits from the same tag set.
        let tags = [
            Tag::parse("hardware.cpu_cores=8").unwrap(),
            Tag::parse("hardware.limits.rate_limit_rpm=120").unwrap(),
        ];
        let hw = hardware_from_tags(&tags);
        assert_eq!(hw.cpu_cores, 8);
        // No hardware-struct field should have been touched by the
        // `hardware.limits.*` tag.
    }

    // ====================================================================
    // ModelCapability list ↔ tags
    // ====================================================================

    fn full_models() -> Vec<ModelCapability> {
        vec![
            ModelCapability::new("llama-3.1-70b", "llama")
                .with_parameters(70.0)
                .with_context_length(128_000)
                .with_quantization("fp16")
                .add_modality(Modality::Code)
                .with_tokens_per_sec(50)
                .with_loaded(true),
            ModelCapability::new("mistral-7b", "mistral")
                .with_parameters(7.0)
                .with_context_length(32_000),
        ]
    }

    #[test]
    fn empty_models_emits_no_tags() {
        assert!(models_to_tags(&[]).is_empty());
    }

    #[test]
    fn full_models_emits_canonical_tag_set() {
        let models = full_models();
        let strs: Vec<String> = models_to_tags(&models)
            .iter()
            .map(|t| t.to_string())
            .collect();
        // First model has all fields populated (loaded=true);
        // second has only id / family / parameters / context.
        // ModelCapability::new defaults modalities=[Text]; the
        // first model adds Code, so the modalities tag is "text,code".
        assert_eq!(
            strs,
            vec![
                "software.model.0.id=llama-3.1-70b",
                "software.model.0.family=llama",
                "software.model.0.parameters_b_x10=700",
                "software.model.0.context_length=128000",
                "software.model.0.quantization=fp16",
                "software.model.0.modalities=text,code",
                "software.model.0.tokens_per_sec=50",
                "software.model.0.loaded=true",
                "software.model.1.id=mistral-7b",
                "software.model.1.family=mistral",
                "software.model.1.parameters_b_x10=70",
                "software.model.1.context_length=32000",
                "software.model.1.modalities=text",
            ]
        );
    }

    #[test]
    fn round_trip_full_models() {
        let models = full_models();
        let tags = models_to_tags(&models);
        let models2 = models_from_tags(&tags);
        assert_eq!(models, models2);
    }

    #[test]
    fn round_trip_empty_models() {
        assert_eq!(
            models_from_tags(&models_to_tags(&[])),
            Vec::<ModelCapability>::new()
        );
    }

    #[test]
    fn models_index_order_preserved() {
        // Pinned: models are reconstructed in numeric-index
        // order via BTreeMap, so the input Vec order is
        // preserved across the round-trip even when the encoder
        // emits multiple sub-tags per model and they interleave
        // arbitrarily on the wire.
        let m1 = ModelCapability::new("a", "fam");
        let m2 = ModelCapability::new("b", "fam");
        let m3 = ModelCapability::new("c", "fam");
        let original = vec![m1, m2, m3];
        let tags = models_to_tags(&original);
        let decoded = models_from_tags(&tags);
        let ids: Vec<_> = decoded.iter().map(|m| m.model_id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn models_decode_skips_non_software_axis() {
        // Cross-axis tags shouldn't pollute the model decoder.
        let tags = [
            Tag::parse("hardware.cpu_cores=8").unwrap(),
            Tag::parse("software.model.0.id=llama").unwrap(),
            Tag::parse("software.model.0.family=llama").unwrap(),
        ];
        let models = models_from_tags(&tags);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].model_id, "llama");
    }

    #[test]
    fn models_decode_handles_unknown_modality_gracefully() {
        // Forward compat: a peer's new modality string falls back
        // to Text rather than panicking.
        let tags = [
            Tag::parse("software.model.0.id=foo").unwrap(),
            Tag::parse("software.model.0.family=bar").unwrap(),
            Tag::parse("software.model.0.modalities=text,quantum,code").unwrap(),
        ];
        let models = models_from_tags(&tags);
        assert_eq!(models.len(), 1);
        // "quantum" → fallback Text; result is [Text, Text, Code].
        assert_eq!(
            models[0].modalities,
            vec![Modality::Text, Modality::Text, Modality::Code]
        );
    }

    #[test]
    fn models_decode_skips_malformed_index() {
        // `software.model.bogus.id=foo` — index isn't numeric, drop.
        let tags = [
            Tag::parse("software.model.bogus.id=foo").unwrap(),
            Tag::parse("software.model.0.id=real").unwrap(),
            Tag::parse("software.model.0.family=fam").unwrap(),
        ];
        let models = models_from_tags(&tags);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].model_id, "real");
    }

    // ====================================================================
    // ToolCapability list ↔ tags
    // ====================================================================

    fn full_tools() -> Vec<ToolCapability> {
        vec![
            ToolCapability::new("python_repl", "Python REPL")
                .with_version("1.0.0")
                .with_estimated_time(100)
                .requires("python:3.11"),
            ToolCapability::new("ffmpeg", "FFmpeg").with_version("7.0"),
        ]
    }

    #[test]
    fn empty_tools_emits_no_tags() {
        assert!(tools_to_tags(&[]).is_empty());
    }

    #[test]
    fn full_tools_emits_canonical_tag_set() {
        let tools = full_tools();
        let strs: Vec<String> = tools_to_tags(&tools)
            .iter()
            .map(|t| t.to_string())
            .collect();
        assert_eq!(
            strs,
            vec![
                "software.tool.0.tool_id=python_repl",
                "software.tool.0.name=Python REPL",
                "software.tool.0.version=1.0.0",
                "software.tool.0.requires=python:3.11",
                "software.tool.0.estimated_time_ms=100",
                "software.tool.0.stateless=true",
                "software.tool.1.tool_id=ffmpeg",
                "software.tool.1.name=FFmpeg",
                "software.tool.1.version=7.0",
                "software.tool.1.stateless=true",
            ]
        );
    }

    #[test]
    fn round_trip_full_tools() {
        let tools = full_tools();
        let tools2 = tools_from_tags(&tools_to_tags(&tools));
        assert_eq!(tools, tools2);
    }

    #[test]
    fn round_trip_empty_tools() {
        assert_eq!(
            tools_from_tags(&tools_to_tags(&[])),
            Vec::<ToolCapability>::new()
        );
    }

    #[test]
    fn tools_input_output_schemas_dropped_until_phase_c() {
        // Pinned: JSON Schema strings can't safely round-trip
        // through the tag wire format. The codec drops them; a
        // future Phase C metadata-field carrier will restore them.
        let tool = ToolCapability::new("validator", "JSON Validator")
            .with_input_schema(r#"{"type":"object"}"#)
            .with_output_schema(r#"{"type":"boolean"}"#);
        let decoded = tools_from_tags(&tools_to_tags(std::slice::from_ref(&tool)));
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].input_schema, None);
        assert_eq!(decoded[0].output_schema, None);
        // Other fields preserved.
        assert_eq!(decoded[0].tool_id, "validator");
        assert_eq!(decoded[0].name, "JSON Validator");
    }

    #[test]
    fn tools_decode_default_version_when_missing() {
        // Pinned: ToolCapability::new defaults version to "1.0.0".
        // A decoder reconstructing a ToolCapability with no version
        // tag should fall back to that same default — preserves
        // the substrate's "version is always populated" invariant.
        let tags = [
            Tag::parse("software.tool.0.tool_id=foo").unwrap(),
            Tag::parse("software.tool.0.name=Foo").unwrap(),
            Tag::parse("software.tool.0.stateless=true").unwrap(),
        ];
        let tools = tools_from_tags(&tags);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].version, "1.0.0");
    }

    #[test]
    fn tools_stateless_round_trips_explicit_false() {
        // ToolCapability::new defaults stateless=true. A tool with
        // stateless=false must round-trip that explicit value, not
        // collapse back to the default.
        let tool = ToolCapability::new("coffee_pot", "Stateful Coffee Pot").with_stateless(false);
        let decoded = tools_from_tags(&tools_to_tags(std::slice::from_ref(&tool)));
        assert!(!decoded[0].stateless);
    }

    // ====================================================================
    // CapabilitySet ↔ tag-set bijection (combined codec)
    //
    // The load-bearing tests for Phase A.5.0c. A full
    // `CapabilitySet` with hardware + software + models + tools +
    // limits + legacy tags round-trips through
    // `capability_set_to_tag_set` → `capability_set_from_tag_set`.
    // ====================================================================

    fn full_capability_set() -> CapabilitySet {
        CapabilitySet::new()
            .with_hardware(full_hardware())
            .with_software(full_software())
            .add_model(full_models().remove(0))
            .add_tool(full_tools().remove(0))
            .with_limits(full_limits())
            .add_tag("inference")
            .add_tag("gpu")
    }

    #[test]
    fn round_trip_full_capability_set() {
        let caps = full_capability_set();
        let tag_set = capability_set_to_tag_set(&caps);
        let caps2 = capability_set_from_tag_set(&tag_set);

        // Phase A.5.N.3: round-trip via the canonical tag set.
        // Compare projections through `views()`. Hardware / models
        // / tools / limits decode byte-for-byte from the indexed
        // encoding.
        let v1 = caps.views();
        let v2 = caps2.views();
        assert_eq!(v1.hardware(), v2.hardware());
        assert_eq!(v1.models(), v2.models());
        assert_eq!(v1.resource_limits(), v2.resource_limits());
        // Tools' non-schema fields round-trip cleanly; schemas live
        // in metadata, which `from_tag_set` doesn't reconstruct.
        let v1_tools = v1.tools();
        let v2_tools = v2.tools();
        assert_eq!(v1_tools.len(), v2_tools.len());
        for (a, b) in v1_tools.iter().zip(v2_tools.iter()) {
            assert_eq!(a.tool_id, b.tool_id);
            assert_eq!(a.name, b.name);
        }

        // SoftwareCapabilities: runtimes / frameworks / drivers
        // are encoded non-indexed and so come out in lex order
        // rather than insertion order. Compare as sets.
        let v1_sw = v1.software();
        let v2_sw = v2.software();
        assert_eq!(v1_sw.os, v2_sw.os);
        assert_eq!(v1_sw.os_version, v2_sw.os_version);
        assert_eq!(v1_sw.cuda_version, v2_sw.cuda_version);
        let lhs_runtimes: std::collections::HashSet<_> = v1_sw.runtimes.iter().cloned().collect();
        let rhs_runtimes: std::collections::HashSet<_> = v2_sw.runtimes.iter().cloned().collect();
        assert_eq!(lhs_runtimes, rhs_runtimes);
        let lhs_fw: std::collections::HashSet<_> = v1_sw.frameworks.iter().cloned().collect();
        let rhs_fw: std::collections::HashSet<_> = v2_sw.frameworks.iter().cloned().collect();
        assert_eq!(lhs_fw, rhs_fw);

        // Tag sets: round-trip is identity.
        assert_eq!(caps.tags, caps2.tags);
    }

    #[test]
    fn software_runtime_order_normalized_through_tag_set() {
        // Pinned: SoftwareCapabilities runtimes lose insertion
        // order through the HashSet<Tag> round-trip. They come
        // out in lex-by-name order regardless of input order.
        // This is the documented limitation of the non-indexed
        // encoding; users who care about runtime order use the
        // typed struct directly. Phase A.5.1 may switch to
        // indexed encoding (`software.runtime.<i>.<name>`).
        let caps = CapabilitySet::new().with_software(
            SoftwareCapabilities::new()
                .add_runtime("python", "3.11")
                .add_runtime("node", "20")
                .add_runtime("rust", "1.78"),
        );
        let tag_set = capability_set_to_tag_set(&caps);
        let caps2 = capability_set_from_tag_set(&tag_set);
        // Decoded order is lex-sorted by tag wire form, which puts
        // runtime names in alphabetical order.
        let views = caps2.views();
        let sw = views.software();
        let names: Vec<_> = sw.runtimes.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["node", "python", "rust"]);
        // The (name, version) pairs are still preserved per pair.
        let by_name: std::collections::HashMap<_, _> = sw.runtimes.iter().cloned().collect();
        assert_eq!(by_name.get("python"), Some(&"3.11".to_string()));
        assert_eq!(by_name.get("node"), Some(&"20".to_string()));
        assert_eq!(by_name.get("rust"), Some(&"1.78".to_string()));
    }

    #[test]
    fn round_trip_default_capability_set() {
        let caps = CapabilitySet::default();
        let tag_set = capability_set_to_tag_set(&caps);
        // A default CapabilitySet emits no axis tags and no legacy
        // tags; the resulting tag set is empty.
        assert!(tag_set.is_empty());
        let caps2 = capability_set_from_tag_set(&tag_set);
        // Phase A.5.N.3: assert through `views()` since typed
        // fields are gone.
        let v = caps2.views();
        assert_eq!(*v.hardware(), HardwareCapabilities::default());
        assert_eq!(*v.software(), SoftwareCapabilities::default());
        assert!(v.models().is_empty());
        assert!(v.tools().is_empty());
        assert_eq!(*v.resource_limits(), ResourceLimits::default());
        assert!(caps2.tags.is_empty());
    }

    #[test]
    fn reserved_prefix_tags_ride_through_legacy_carrier() {
        // Pinned: scope tags (`scope:prod`, `scope:tenant:foo`)
        // and other reserved-prefix tags survive the round-trip via
        // the legacy `tags: Vec<String>` carrier. Phase A.5.0c
        // preserves their semantics until Phase A.5.1+ migrates
        // the wire format fully.
        let caps = CapabilitySet::new()
            .with_tenant_scope("acme")
            .with_region_scope("eu-west")
            .with_subnet_local_scope();
        let tag_set = capability_set_to_tag_set(&caps);
        let caps2 = capability_set_from_tag_set(&tag_set);
        let original_set: std::collections::HashSet<_> = caps.tags.iter().cloned().collect();
        let round_tripped_set: std::collections::HashSet<_> = caps2.tags.iter().cloned().collect();
        assert_eq!(original_set, round_tripped_set);
    }

    #[test]
    fn unknown_axis_tags_ride_through_legacy_carrier() {
        // A `devices.lidar` tag isn't claimed by any per-struct
        // decoder — it survives the round-trip via the legacy
        // carrier.
        let caps = CapabilitySet::new()
            .add_tag("devices.lidar")
            .add_tag("dataforts.tier:hot");
        let tag_set = capability_set_to_tag_set(&caps);
        let caps2 = capability_set_from_tag_set(&tag_set);
        let original_set: std::collections::HashSet<_> = caps.tags.iter().cloned().collect();
        let round_tripped_set: std::collections::HashSet<_> = caps2.tags.iter().cloned().collect();
        assert_eq!(original_set, round_tripped_set);
    }

    #[test]
    fn struct_owned_tags_dont_leak_into_legacy_carrier() {
        // Pinned: a CapabilitySet with hardware fields populated
        // produces axis-prefixed tags, but those tags are routed
        // back into the typed-struct fields on decode — NOT into
        // the legacy `tags: Vec<String>` carrier. Otherwise the
        // legacy carrier would balloon with reflection of every
        // typed-struct field.
        let caps = CapabilitySet::new()
            .with_hardware(HardwareCapabilities::new().with_cpu(8, 16))
            .add_tag("inference"); // legacy untyped
        let tag_set = capability_set_to_tag_set(&caps);
        let caps2 = capability_set_from_tag_set(&tag_set);
        // Hardware fields preserved (read via projection).
        assert_eq!(caps2.views().hardware().cpu_cores, 8);
        // Tag set holds the legacy untyped tag *and* the typed
        // hardware tags from the with_hardware encoding (Phase
        // A.5.N.3 — the canonical tag set IS the storage).
        let strs: std::collections::HashSet<String> =
            caps2.tags.iter().map(|t| t.to_string()).collect();
        assert!(strs.contains("inference"));
        assert!(strs.contains("hardware.cpu_cores=8"));
    }

    #[test]
    fn empty_legacy_tag_strings_dropped() {
        // Pinned: an empty legacy tag string parses as
        // `CapabilityTagError::Empty` — silently dropped during
        // encoding so the round-trip doesn't produce phantom
        // empty entries.
        let caps = CapabilitySet::new().add_tag("").add_tag("real-tag");
        let tag_set = capability_set_to_tag_set(&caps);
        // Only the non-empty tag survives.
        assert_eq!(tag_set.len(), 1);
        let caps2 = capability_set_from_tag_set(&tag_set);
        let leftover: Vec<String> = caps2.tags.iter().map(|t| t.to_string()).collect();
        assert_eq!(leftover, vec!["real-tag".to_string()]);
    }

    #[test]
    fn capability_set_tag_set_size_is_bounded_by_input() {
        // Sanity: a sparsely populated CapabilitySet doesn't blow
        // up to a huge tag set. Pinned to catch a future
        // accidental "emit every field even when default" change.
        let caps = CapabilitySet::new().with_hardware(HardwareCapabilities::new().with_cpu(4, 8));
        let tag_set = capability_set_to_tag_set(&caps);
        // Two tags: cpu_cores=4 and cpu_threads=8.
        assert_eq!(tag_set.len(), 2);
    }
}
