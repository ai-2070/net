// `#[napi]` exports to JS leave items "unused" from Rust's POV, so
// clippy's dead-code analysis doesn't apply to this module. Suppress
// at file scope.
#![allow(dead_code)]

//! NAPI surface for capability declarations — the POJO shapes the TS
//! SDK passes through to the mesh, plus the conversions between them
//! and the core `CapabilitySet` / `CapabilityFilter` types.
//!
//! Design: the POJOs are a flattened, JS-friendly projection of the
//! core types. Unset fields (`None` / empty) convert to the core
//! default, so TS callers can write
//! `{ tags: ['gpu'] }` without touching hardware / software / limits.

use napi::bindgen_prelude::*;
use napi_derive::napi;

use net::adapter::net::behavior::capability::{
    AcceleratorInfo, AcceleratorType, CapabilityFilter, CapabilitySet, GpuInfo, GpuVendor,
    HardwareCapabilities, Modality, ModelCapability, ResourceLimits, SoftwareCapabilities,
    ToolCapability,
};
use net::adapter::net::behavior::Tag;

// =========================================================================
// POJO types
// =========================================================================

#[napi(object)]
pub struct CapabilitySetJs {
    pub hardware: Option<HardwareJs>,
    pub software: Option<SoftwareJs>,
    pub models: Option<Vec<ModelJs>>,
    pub tools: Option<Vec<ToolJs>>,
    pub tags: Option<Vec<String>>,
    pub limits: Option<CapabilityLimitsJs>,
}

#[napi(object)]
pub struct HardwareJs {
    pub cpu_cores: Option<u32>,
    pub cpu_threads: Option<u32>,
    pub memory_mb: Option<u32>,
    pub gpu: Option<GpuInfoJs>,
    pub additional_gpus: Option<Vec<GpuInfoJs>>,
    pub storage_mb: Option<BigInt>,
    pub network_mbps: Option<u32>,
    pub accelerators: Option<Vec<AcceleratorJs>>,
}

#[napi(object)]
pub struct GpuInfoJs {
    /// Lowercase vendor name: `nvidia` | `amd` | `intel` | `apple` |
    /// `qualcomm` | `unknown`.
    pub vendor: Option<String>,
    pub model: String,
    pub vram_mb: u32,
    pub compute_units: Option<u32>,
    pub tensor_cores: Option<u32>,
    pub fp16_tflops_x10: Option<u32>,
}

#[napi(object)]
pub struct AcceleratorJs {
    /// `tpu` | `npu` | `fpga` | `asic` | `dsp` | `unknown`.
    pub kind: String,
    pub model: String,
    pub memory_mb: Option<u32>,
    /// TOPS × 10 (matches the core's integer storage).
    pub tops_x10: Option<u32>,
}

#[napi(object)]
pub struct SoftwareJs {
    pub os: Option<String>,
    pub os_version: Option<String>,
    /// `[ [runtime_name, version] ]` pairs.
    pub runtimes: Option<Vec<Vec<String>>>,
    pub frameworks: Option<Vec<Vec<String>>>,
    pub cuda_version: Option<String>,
    pub drivers: Option<Vec<Vec<String>>>,
}

#[napi(object)]
pub struct ModelJs {
    pub model_id: String,
    pub family: Option<String>,
    /// Parameter count, billions × 10 (70 B ⇒ 700). Matches the
    /// core's integer encoding — no float precision loss.
    pub parameters_b_x10: Option<u32>,
    pub context_length: Option<u32>,
    pub quantization: Option<String>,
    /// Lowercase modality names: `text`, `image`, `audio`, `video`,
    /// `code`, `embedding`, `tool-use`.
    pub modalities: Option<Vec<String>>,
    pub tokens_per_sec: Option<u32>,
    pub loaded: Option<bool>,
}

#[napi(object)]
pub struct ToolJs {
    pub tool_id: String,
    pub name: Option<String>,
    pub version: Option<String>,
    pub input_schema: Option<String>,
    pub output_schema: Option<String>,
    pub requires: Option<Vec<String>>,
    pub estimated_time_ms: Option<u32>,
    pub stateless: Option<bool>,
}

#[napi(object)]
pub struct CapabilityLimitsJs {
    pub max_concurrent_requests: Option<u32>,
    pub max_tokens_per_request: Option<u32>,
    pub rate_limit_rpm: Option<u32>,
    pub max_batch_size: Option<u32>,
    pub max_input_bytes: Option<u32>,
    pub max_output_bytes: Option<u32>,
}

#[napi(object)]
pub struct CapabilityFilterJs {
    pub require_tags: Option<Vec<String>>,
    pub require_models: Option<Vec<String>>,
    pub require_tools: Option<Vec<String>>,
    pub min_memory_mb: Option<u32>,
    pub require_gpu: Option<bool>,
    /// Lowercase vendor name; see `GpuInfoJs::vendor`.
    pub gpu_vendor: Option<String>,
    pub min_vram_mb: Option<u32>,
    pub min_context_length: Option<u32>,
    pub require_modalities: Option<Vec<String>>,
}

// =========================================================================
// Conversions: JS POJO → core
// =========================================================================

fn parse_gpu_vendor(s: &str) -> GpuVendor {
    match s.to_ascii_lowercase().as_str() {
        "nvidia" => GpuVendor::Nvidia,
        "amd" => GpuVendor::Amd,
        "intel" => GpuVendor::Intel,
        "apple" => GpuVendor::Apple,
        "qualcomm" => GpuVendor::Qualcomm,
        _ => GpuVendor::Unknown,
    }
}

fn gpu_vendor_to_string(v: GpuVendor) -> String {
    match v {
        GpuVendor::Nvidia => "nvidia".into(),
        GpuVendor::Amd => "amd".into(),
        GpuVendor::Intel => "intel".into(),
        GpuVendor::Apple => "apple".into(),
        GpuVendor::Qualcomm => "qualcomm".into(),
        GpuVendor::Unknown => "unknown".into(),
    }
}

fn parse_modality(s: &str) -> Modality {
    match s.to_ascii_lowercase().as_str() {
        "text" => Modality::Text,
        "image" => Modality::Image,
        "audio" => Modality::Audio,
        "video" => Modality::Video,
        "code" => Modality::Code,
        "embedding" => Modality::Embedding,
        "tool-use" | "tool_use" | "tooluse" => Modality::ToolUse,
        _ => Modality::Text,
    }
}

fn parse_accelerator_type(s: &str) -> AcceleratorType {
    match s.to_ascii_lowercase().as_str() {
        "tpu" => AcceleratorType::Tpu,
        "npu" => AcceleratorType::Npu,
        "fpga" => AcceleratorType::Fpga,
        "asic" => AcceleratorType::Asic,
        "dsp" => AcceleratorType::Dsp,
        _ => AcceleratorType::Unknown,
    }
}

fn bigint_to_u64_or_zero(b: Option<BigInt>) -> u64 {
    b.and_then(|v| {
        let (signed, value, lossless) = v.get_u64();
        if signed || !lossless {
            None
        } else {
            Some(value)
        }
    })
    .unwrap_or(0)
}

fn pair_vec(xs: Option<Vec<Vec<String>>>) -> Vec<(String, String)> {
    xs.unwrap_or_default()
        .into_iter()
        .filter_map(|mut p| {
            if p.len() >= 2 {
                Some((std::mem::take(&mut p[0]), std::mem::take(&mut p[1])))
            } else {
                None
            }
        })
        .collect()
}

/// Clamp an untrusted JS `u32` into a core `u16` field, saturating
/// at `u16::MAX`. Bare `as u16` silently wraps on overflow — a
/// misbehaving / malicious caller could report 65536 cores and have
/// it land as 0 on the wire. Mirrors the accelerator `tops_x10`
/// site and keeps every capability conversion consistent.
#[inline]
fn saturating_u16(v: u32) -> u16 {
    v.min(u16::MAX as u32) as u16
}

fn gpu_info_from_js(g: GpuInfoJs) -> GpuInfo {
    let vendor = g
        .vendor
        .as_deref()
        .map(parse_gpu_vendor)
        .unwrap_or(GpuVendor::Unknown);
    let mut info = GpuInfo::new(vendor, g.model, g.vram_mb);
    if let Some(cu) = g.compute_units {
        info = info.with_compute_units(saturating_u16(cu));
    }
    if let Some(tc) = g.tensor_cores {
        info = info.with_tensor_cores(saturating_u16(tc));
    }
    if let Some(tf) = g.fp16_tflops_x10 {
        // CR-25: write the integer field directly. The
        // `with_fp16_tflops(tf as f32 / 10.0)` round-trip used to
        // re-multiply by 10 inside the builder, and f32's 24-bit
        // mantissa loses precision for values > 16,777,216:
        // tf=20_000_005 round-tripped to 20_000_004 or
        // 20_000_008. Mirrors the model.parameters_b_x10 path
        // which also writes the field directly.
        info.fp16_tflops_x10 = tf;
    }
    info
}

fn gpu_info_to_js(g: &GpuInfo) -> GpuInfoJs {
    GpuInfoJs {
        vendor: Some(gpu_vendor_to_string(g.vendor)),
        model: g.model.clone(),
        vram_mb: g.vram_mb,
        compute_units: Some(g.compute_units as u32),
        tensor_cores: Some(g.tensor_cores as u32),
        fp16_tflops_x10: Some(g.fp16_tflops_x10),
    }
}

fn accelerator_from_js(a: AcceleratorJs) -> AcceleratorInfo {
    AcceleratorInfo {
        accel_type: parse_accelerator_type(&a.kind),
        model: a.model,
        memory_mb: a.memory_mb.unwrap_or(0),
        tops_x10: a.tops_x10.map(saturating_u16).unwrap_or(0),
    }
}

fn hardware_from_js(h: HardwareJs) -> HardwareCapabilities {
    let mut hw = HardwareCapabilities::new();
    if let (Some(cores), Some(threads)) = (h.cpu_cores, h.cpu_threads) {
        hw = hw.with_cpu(saturating_u16(cores), saturating_u16(threads));
    } else if let Some(cores) = h.cpu_cores {
        let c = saturating_u16(cores);
        hw = hw.with_cpu(c, c);
    }
    if let Some(mb) = h.memory_mb {
        hw = hw.with_memory(mb);
    }
    if let Some(g) = h.gpu {
        hw = hw.with_gpu(gpu_info_from_js(g));
    }
    for g in h.additional_gpus.unwrap_or_default() {
        hw = hw.add_gpu(gpu_info_from_js(g));
    }
    if h.storage_mb.is_some() {
        hw = hw.with_storage(bigint_to_u64_or_zero(h.storage_mb));
    }
    if let Some(mbps) = h.network_mbps {
        hw = hw.with_network(mbps);
    }
    for a in h.accelerators.unwrap_or_default() {
        hw = hw.add_accelerator(accelerator_from_js(a));
    }
    hw
}

fn software_from_js(s: SoftwareJs) -> SoftwareCapabilities {
    let mut sw = SoftwareCapabilities::new()
        .with_os(s.os.unwrap_or_default(), s.os_version.unwrap_or_default());
    for (k, v) in pair_vec(s.runtimes) {
        sw = sw.add_runtime(k, v);
    }
    for (k, v) in pair_vec(s.frameworks) {
        sw = sw.add_framework(k, v);
    }
    if let Some(c) = s.cuda_version {
        sw = sw.with_cuda(c);
    }
    sw.drivers = pair_vec(s.drivers);
    sw
}

fn model_from_js(m: ModelJs) -> ModelCapability {
    let mut mc = ModelCapability::new(m.model_id, m.family.unwrap_or_default());
    if let Some(p) = m.parameters_b_x10 {
        // Field is public + stable — set directly to avoid the
        // builder's f32-roundtrip. Keeps billions-of-params precise.
        mc.parameters_b_x10 = p;
    }
    if let Some(c) = m.context_length {
        mc = mc.with_context_length(c);
    }
    if let Some(q) = m.quantization {
        mc = mc.with_quantization(q);
    }
    for mod_name in m.modalities.unwrap_or_default() {
        mc = mc.add_modality(parse_modality(&mod_name));
    }
    if let Some(t) = m.tokens_per_sec {
        mc = mc.with_tokens_per_sec(t);
    }
    if let Some(l) = m.loaded {
        mc = mc.with_loaded(l);
    }
    mc
}

fn tool_from_js(t: ToolJs) -> ToolCapability {
    let mut tc = ToolCapability::new(t.tool_id, t.name.unwrap_or_default());
    if let Some(v) = t.version {
        tc = tc.with_version(v);
    }
    if let Some(s) = t.input_schema {
        tc = tc.with_input_schema(s);
    }
    if let Some(s) = t.output_schema {
        tc = tc.with_output_schema(s);
    }
    for r in t.requires.unwrap_or_default() {
        tc = tc.requires(r);
    }
    if let Some(ms) = t.estimated_time_ms {
        tc = tc.with_estimated_time(ms);
    }
    if let Some(st) = t.stateless {
        tc = tc.with_stateless(st);
    }
    tc
}

fn limits_from_js(l: CapabilityLimitsJs) -> ResourceLimits {
    let mut rl = ResourceLimits::new();
    if let Some(n) = l.max_concurrent_requests {
        rl = rl.with_max_concurrent(n);
    }
    if let Some(n) = l.max_tokens_per_request {
        rl = rl.with_max_tokens(n);
    }
    if let Some(n) = l.rate_limit_rpm {
        rl = rl.with_rate_limit(n);
    }
    if let Some(n) = l.max_batch_size {
        rl = rl.with_max_batch(n);
    }
    if let Some(n) = l.max_input_bytes {
        rl.max_input_bytes = n;
    }
    if let Some(n) = l.max_output_bytes {
        rl.max_output_bytes = n;
    }
    rl
}

pub fn capability_set_from_js(caps: CapabilitySetJs) -> CapabilitySet {
    let mut cs = CapabilitySet::new();
    if let Some(h) = caps.hardware {
        cs = cs.with_hardware(hardware_from_js(h));
    }
    if let Some(s) = caps.software {
        cs = cs.with_software(software_from_js(s));
    }
    for m in caps.models.unwrap_or_default() {
        cs = cs.add_model(model_from_js(m));
    }
    for t in caps.tools.unwrap_or_default() {
        cs = cs.add_tool(tool_from_js(t));
    }
    // SDK consumers may supply reserved-prefix tags (`scope:*`,
    // `causal:*`, …). `CapabilitySet::add_tag` routes through
    // `Tag::parse_user`, which silently drops reserved prefixes —
    // correct for application-facing input, wrong at the binding
    // boundary where the JS caller is the SDK. Parse via the
    // unrestricted `Tag::parse` and insert directly.
    for tag in caps.tags.unwrap_or_default() {
        if let Ok(t) = Tag::parse(&tag) {
            cs.tags.insert(t);
        }
    }
    if let Some(l) = caps.limits {
        cs = cs.with_limits(limits_from_js(l));
    }
    cs
}

pub fn capability_filter_from_js(f: CapabilityFilterJs) -> CapabilityFilter {
    let mut cf = CapabilityFilter::new();
    for t in f.require_tags.unwrap_or_default() {
        cf = cf.require_tag(t);
    }
    for m in f.require_models.unwrap_or_default() {
        cf = cf.require_model(m);
    }
    for t in f.require_tools.unwrap_or_default() {
        cf = cf.require_tool(t);
    }
    if let Some(mb) = f.min_memory_mb {
        cf = cf.with_min_memory(mb);
    }
    if f.require_gpu.unwrap_or(false) {
        cf = cf.require_gpu();
    }
    if let Some(v) = f.gpu_vendor {
        cf = cf.with_gpu_vendor(parse_gpu_vendor(&v));
    }
    if let Some(mb) = f.min_vram_mb {
        cf = cf.with_min_vram(mb);
    }
    if let Some(n) = f.min_context_length {
        cf = cf.with_min_context(n);
    }
    for m in f.require_modalities.unwrap_or_default() {
        cf = cf.require_modality(parse_modality(&m));
    }
    cf
}

// =========================================================================
// Scope filter (reserved-tag discovery filter)
// =========================================================================

/// JS-side representation of [`net::adapter::net::behavior::capability::ScopeFilter`].
///
/// Tagged union by `kind`:
/// - `{ kind: 'any' }` — every non-`SubnetLocal` peer.
/// - `{ kind: 'globalOnly' }` — only peers with no `scope:*` tag.
/// - `{ kind: 'sameSubnet' }` — peers in the caller's subnet.
/// - `{ kind: 'tenant', tenant: '<id>' }` — that tenant + Global.
/// - `{ kind: 'tenants', tenants: ['<id>', ...] }` — any of the
///   listed tenants + Global.
/// - `{ kind: 'region', region: '<name>' }` — that region + Global.
/// - `{ kind: 'regions', regions: ['<name>', ...] }` — any of the
///   listed regions + Global.
///
/// Unknown `kind` values are treated as `'any'` defensively
/// (warns in `tracing`); real validation lives at the type-script
/// layer.
#[napi(object)]
pub struct ScopeFilterJs {
    pub kind: String,
    pub tenant: Option<String>,
    pub tenants: Option<Vec<String>>,
    pub region: Option<String>,
    pub regions: Option<Vec<String>>,
}

/// Owned form of [`net::adapter::net::behavior::capability::ScopeFilter`]
/// — the core enum borrows `&str` slices, which can't cross the NAPI
/// boundary. Callers convert the JS POJO into this owned shape, then
/// run the query inside [`with_scope_filter`] so the closure gets a
/// borrowed view that's alive for the duration of the call.
pub enum ScopeFilterOwned {
    Any,
    GlobalOnly,
    SameSubnet,
    Tenant(String),
    Tenants(Vec<String>),
    Region(String),
    Regions(Vec<String>),
}

/// Run `f` with a borrowed [`ScopeFilter`] projected from `owned`.
/// Encapsulates the `Vec<String>` → `Vec<&str>` → `&[&str]` chain
/// the core enum requires for its multi-element variants. The
/// intermediate borrows live on this function's stack so the slice
/// stays valid for the entire `f` call.
pub fn with_scope_filter<R>(
    owned: &ScopeFilterOwned,
    f: impl FnOnce(&net::adapter::net::behavior::capability::ScopeFilter<'_>) -> R,
) -> R {
    use net::adapter::net::behavior::capability::ScopeFilter as F;
    match owned {
        ScopeFilterOwned::Any => f(&F::Any),
        ScopeFilterOwned::GlobalOnly => f(&F::GlobalOnly),
        ScopeFilterOwned::SameSubnet => f(&F::SameSubnet),
        ScopeFilterOwned::Tenant(t) => f(&F::Tenant(t.as_str())),
        ScopeFilterOwned::Tenants(ts) => {
            let refs: Vec<&str> = ts.iter().map(|s| s.as_str()).collect();
            f(&F::Tenants(refs.as_slice()))
        }
        ScopeFilterOwned::Region(r) => f(&F::Region(r.as_str())),
        ScopeFilterOwned::Regions(rs) => {
            let refs: Vec<&str> = rs.iter().map(|s| s.as_str()).collect();
            f(&F::Regions(refs.as_slice()))
        }
    }
}

/// Convert a JS scope filter POJO to the owned form. Empty strings
/// or empty lists collapse to [`ScopeFilterOwned::Any`] —
/// `scope:tenant:` (no id) is rejected by [`scope_from_tags`] in
/// the core, so passing it as a query would never match anything;
/// `Any` is the more honest result.
pub fn scope_filter_from_js(f: ScopeFilterJs) -> ScopeFilterOwned {
    match f.kind.as_str() {
        "any" => ScopeFilterOwned::Any,
        "globalOnly" => ScopeFilterOwned::GlobalOnly,
        "sameSubnet" => ScopeFilterOwned::SameSubnet,
        "tenant" => match f.tenant {
            Some(t) if !t.is_empty() => ScopeFilterOwned::Tenant(t),
            _ => ScopeFilterOwned::Any,
        },
        "tenants" => match f.tenants {
            Some(ts) => {
                // Drop empty tenant ids — `scope_from_tags` rejects
                // empty announcements, so passing `[""]` through as a
                // query would never match real tenants and would only
                // pin to Global candidates (since `Tenants(["",])` is
                // a valid filter that matches no tenant tag).
                let cleaned: Vec<String> = ts.into_iter().filter(|t| !t.is_empty()).collect();
                if cleaned.is_empty() {
                    ScopeFilterOwned::Any
                } else {
                    ScopeFilterOwned::Tenants(cleaned)
                }
            }
            None => ScopeFilterOwned::Any,
        },
        "region" => match f.region {
            Some(r) if !r.is_empty() => ScopeFilterOwned::Region(r),
            _ => ScopeFilterOwned::Any,
        },
        "regions" => match f.regions {
            // Same reasoning as `tenants` above.
            Some(rs) => {
                let cleaned: Vec<String> = rs.into_iter().filter(|r| !r.is_empty()).collect();
                if cleaned.is_empty() {
                    ScopeFilterOwned::Any
                } else {
                    ScopeFilterOwned::Regions(cleaned)
                }
            }
            None => ScopeFilterOwned::Any,
        },
        // Unrecognized `kind` values fall through to Any. The
        // typescript layer's tagged union catches typos at compile
        // time; this is the runtime safety net for raw JS callers.
        _ => ScopeFilterOwned::Any,
    }
}

// =========================================================================
// Small helper exported to JS for TS-side vendor-string normalization.
// =========================================================================

/// Normalize a user-supplied GPU vendor string to the canonical
/// lowercase form used on-wire (`nvidia` | `amd` | `intel` | `apple` |
/// `qualcomm` | `unknown`). Unknown / misspelled inputs collapse to
/// `"unknown"`.
#[napi]
pub fn normalize_gpu_vendor(vendor: String) -> String {
    gpu_vendor_to_string(parse_gpu_vendor(&vendor))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression for a cubic-flagged P2: JS-supplied u32 values
    /// wider than u16::MAX silently wrapped via `as u16`, turning
    /// 65536 cores into 0 cores on the wire. Every conversion site
    /// now routes through `saturating_u16`, which clamps to
    /// `u16::MAX`.
    #[test]
    fn saturating_u16_clamps_at_u16_max() {
        assert_eq!(saturating_u16(0), 0);
        assert_eq!(saturating_u16(42), 42);
        assert_eq!(saturating_u16(u16::MAX as u32), u16::MAX);
        assert_eq!(saturating_u16(u16::MAX as u32 + 1), u16::MAX);
        assert_eq!(saturating_u16(u32::MAX), u16::MAX);
    }

    #[test]
    fn hardware_from_js_saturates_overflow_cpu_fields() {
        // 70_000 > u16::MAX (65_535). Pre-fix: 70_000 as u16 = 4464.
        // Post-fix: saturates to 65_535.
        let h = HardwareJs {
            cpu_cores: Some(70_000),
            cpu_threads: Some(200_000),
            memory_mb: None,
            gpu: None,
            additional_gpus: None,
            storage_mb: None,
            network_mbps: None,
            accelerators: None,
        };
        let hw = hardware_from_js(h);
        assert_eq!(hw.cpu_cores, u16::MAX);
        assert_eq!(hw.cpu_threads, u16::MAX);
    }

    #[test]
    fn gpu_info_from_js_saturates_overflow_compute_and_tensor_fields() {
        let g = GpuInfoJs {
            vendor: Some("nvidia".into()),
            model: "test".into(),
            vram_mb: 0,
            compute_units: Some(90_000),
            tensor_cores: Some(u32::MAX),
            fp16_tflops_x10: None,
        };
        let info = gpu_info_from_js(g);
        assert_eq!(info.compute_units, u16::MAX);
        assert_eq!(info.tensor_cores, u16::MAX);
    }
}
