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
//!     memory_mb: 65536,
//!     gpu: Some(GpuInfo { vendor: Nvidia, model: "RTX 4090", vram_mb: 24576, ... }),
//!     storage_mb: 2_000_000,
//!     network_mbps: 10000,
//!     ..
//! }
//! ```
//!
//! becomes:
//!
//! ```text
//! hardware.cpu_cores=16
//! hardware.cpu_threads=32
//! hardware.memory_mb=65536
//! hardware.gpu                              ← presence marker
//! hardware.gpu.vendor=nvidia
//! hardware.gpu.model=RTX 4090
//! hardware.gpu.vram_mb=24576
//! hardware.gpu.compute_units=128
//! hardware.gpu.tensor_cores=512
//! hardware.gpu.fp16_tflops_x10=825
//! hardware.storage_mb=2000000
//! hardware.network_mbps=10000
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

use crate::adapter::net::behavior::capability::{
    GpuInfo, GpuVendor, HardwareCapabilities, ResourceLimits, SoftwareCapabilities,
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
    if hw.memory_mb > 0 {
        tags.push(axis_value("memory_mb", &hw.memory_mb.to_string()));
    }
    if let Some(gpu) = &hw.gpu {
        // Presence marker first so callers can existence-check via
        // `hardware.gpu` without having to enumerate sub-keys.
        tags.push(axis_present("gpu"));
        encode_gpu_into("gpu", gpu, &mut tags);
    }
    if hw.storage_mb > 0 {
        tags.push(axis_value("storage_mb", &hw.storage_mb.to_string()));
    }
    if hw.network_mbps > 0 {
        tags.push(axis_value("network_mbps", &hw.network_mbps.to_string()));
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
    if gpu.vram_mb > 0 {
        tags.push(axis_value(
            &format!("{prefix}.vram_mb"),
            &gpu.vram_mb.to_string(),
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
            "memory_mb" => {
                hw.memory_mb = value.parse().unwrap_or(0);
            }
            "storage_mb" => {
                hw.storage_mb = value.parse().unwrap_or(0);
            }
            "network_mbps" => {
                hw.network_mbps = value.parse().unwrap_or(0);
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
        "vram_mb" => {
            gpu.vram_mb = value.parse().unwrap_or(0);
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
        tags.push(software_value(&format!("runtime.{name}"), version));
    }
    for (name, version) in &sw.frameworks {
        tags.push(software_value(&format!("framework.{name}"), version));
    }
    if let Some(cuda) = &sw.cuda_version {
        tags.push(software_value("cuda_version", cuda));
    }
    for (name, version) in &sw.drivers {
        tags.push(software_value(&format!("driver.{name}"), version));
    }

    tags
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
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::capability::{
        AcceleratorInfo, AcceleratorType, GpuInfo, GpuVendor, HardwareCapabilities,
    };

    fn full_hardware() -> HardwareCapabilities {
        let gpu = GpuInfo::new(GpuVendor::Nvidia, "RTX 4090", 24_576)
            .with_compute_units(128)
            .with_tensor_cores(512)
            .with_fp16_tflops(82.5);
        HardwareCapabilities::new()
            .with_cpu(16, 32)
            .with_memory(65_536)
            .with_gpu(gpu)
            .with_storage(2_000_000)
            .with_network(10_000)
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
                "hardware.memory_mb=65536",
                "hardware.gpu",
                "hardware.gpu.vendor=nvidia",
                "hardware.gpu.model=RTX 4090",
                "hardware.gpu.vram_mb=24576",
                "hardware.gpu.compute_units=128",
                "hardware.gpu.tensor_cores=512",
                "hardware.gpu.fp16_tflops_x10=825",
                "hardware.storage_mb=2000000",
                "hardware.network_mbps=10000",
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
        assert_eq!(strs, vec!["hardware.cpu_cores=8", "hardware.cpu_threads=16"]);
    }

    #[test]
    fn gpu_with_only_required_fields_emits_only_those() {
        // Pinned: a GpuInfo with vendor + model + vram set but
        // compute_units / tensor_cores / fp16 zero only emits the
        // first three sub-tags (plus the presence marker). Sparse
        // emission keeps the wire format small.
        let gpu = GpuInfo::new(GpuVendor::Apple, "M2 Ultra", 64_000);
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
                "hardware.gpu.vram_mb=64000",
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
        let gpu = GpuInfo::new(GpuVendor::Amd, "MI300X", 192_000);
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
        let reparsed: Vec<Tag> =
            wire_strs.iter().map(|s| Tag::parse(s).unwrap()).collect();
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
        let primary = GpuInfo::new(GpuVendor::Nvidia, "H100", 80_000);
        let secondary = GpuInfo::new(GpuVendor::Nvidia, "A100", 40_000);
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
        assert_eq!(
            resource_limits_from_tags(&resource_limits_to_tags(&l)),
            l
        );
    }

    #[test]
    fn round_trip_default_limits() {
        let l = ResourceLimits::default();
        assert_eq!(
            resource_limits_from_tags(&resource_limits_to_tags(&l)),
            l
        );
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
}
