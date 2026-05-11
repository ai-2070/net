//! PyO3 surface for `CapabilitySet` / `CapabilityFilter` — mirror of
//! `bindings/node/src/capabilities.rs`.
//!
//! Python callers pass plain `dict`s (POJO-equivalent) rather than
//! pyclass instances, keeping the surface light. Conversions live in
//! this module and stay parallel to the JS helpers so behavioural
//! quirks (vendor casing, modality aliases, x10 integer encodings)
//! stay in sync across the two bindings.

use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use net::adapter::net::behavior::capability::{
    AcceleratorInfo, AcceleratorType, CapabilityFilter, CapabilitySet, GpuInfo, GpuVendor,
    HardwareCapabilities, Modality, ModelCapability, ResourceLimits, SoftwareCapabilities,
    ToolCapability,
};
use net::adapter::net::behavior::Tag;

// =========================================================================
// Dict helpers
// =========================================================================

fn dict_get<'py>(d: &Bound<'py, PyDict>, key: &str) -> PyResult<Option<Bound<'py, PyAny>>> {
    d.get_item(key)
}

fn get_opt_u32(d: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<u32>> {
    match dict_get(d, key)? {
        Some(v) if !v.is_none() => Ok(Some(v.extract()?)),
        _ => Ok(None),
    }
}

fn get_opt_u64(d: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<u64>> {
    match dict_get(d, key)? {
        Some(v) if !v.is_none() => Ok(Some(v.extract()?)),
        _ => Ok(None),
    }
}

fn get_opt_bool(d: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<bool>> {
    match dict_get(d, key)? {
        Some(v) if !v.is_none() => Ok(Some(v.extract()?)),
        _ => Ok(None),
    }
}

fn get_opt_str(d: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<String>> {
    match dict_get(d, key)? {
        Some(v) if !v.is_none() => Ok(Some(v.extract()?)),
        _ => Ok(None),
    }
}

fn get_opt_str_list(d: &Bound<'_, PyDict>, key: &str) -> PyResult<Vec<String>> {
    match dict_get(d, key)? {
        Some(v) if !v.is_none() => v.extract(),
        _ => Ok(Vec::new()),
    }
}

fn get_opt_dict<'py>(d: &Bound<'py, PyDict>, key: &str) -> PyResult<Option<Bound<'py, PyDict>>> {
    match dict_get(d, key)? {
        Some(v) if !v.is_none() => {
            Ok(Some(v.cast_into::<PyDict>().map_err(|_| {
                PyTypeError::new_err(format!("field {:?} must be a dict", key))
            })?))
        }
        _ => Ok(None),
    }
}

fn get_opt_list<'py>(d: &Bound<'py, PyDict>, key: &str) -> PyResult<Option<Bound<'py, PyList>>> {
    match dict_get(d, key)? {
        Some(v) if !v.is_none() => {
            Ok(Some(v.cast_into::<PyList>().map_err(|_| {
                PyTypeError::new_err(format!("field {:?} must be a list", key))
            })?))
        }
        _ => Ok(None),
    }
}

fn pair_vec_from_list(list: Option<Bound<'_, PyList>>) -> PyResult<Vec<(String, String)>> {
    let mut out = Vec::new();
    if let Some(list) = list {
        for item in list.iter() {
            let pair: Vec<String> = item.extract()?;
            if pair.len() >= 2 {
                out.push((pair[0].clone(), pair[1].clone()));
            }
        }
    }
    Ok(out)
}

// =========================================================================
// Enum parsers — match JS helpers byte-for-byte
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

// =========================================================================
// Subsection dict → core
// =========================================================================

/// Clamp an untrusted Python `int` (already extracted as `u32`)
/// into a core `u16` field, saturating at `u16::MAX`. Bare
/// `as u16` silently wraps on overflow — a caller reporting 65536
/// cores could land 0 on the wire. Applied uniformly so every
/// capability dict conversion is consistent with the NAPI + Go
/// paths.
#[inline]
fn saturating_u16(v: u32) -> u16 {
    v.min(u16::MAX as u32) as u16
}

fn gpu_info_from_dict(d: &Bound<'_, PyDict>) -> PyResult<GpuInfo> {
    let vendor = get_opt_str(d, "vendor")?
        .as_deref()
        .map(parse_gpu_vendor)
        .unwrap_or(GpuVendor::Unknown);
    let model = get_opt_str(d, "model")?.unwrap_or_default();
    let vram = get_opt_u32(d, "vram_gb")?.unwrap_or(0);
    let mut info = GpuInfo::new(vendor, model, vram);
    if let Some(cu) = get_opt_u32(d, "compute_units")? {
        info = info.with_compute_units(saturating_u16(cu));
    }
    if let Some(tc) = get_opt_u32(d, "tensor_cores")? {
        info = info.with_tensor_cores(saturating_u16(tc));
    }
    if let Some(tf) = get_opt_u32(d, "fp16_tflops_x10")? {
        // CR-25: bypass the f32 round-trip in `with_fp16_tflops`.
        // The substrate field is u32; routing through f32 (24-bit
        // mantissa) loses precision for values > 16,777,216.
        // Same shape as the Node binding fix.
        info.fp16_tflops_x10 = tf;
    }
    Ok(info)
}

fn accelerator_from_dict(d: &Bound<'_, PyDict>) -> PyResult<AcceleratorInfo> {
    let kind = get_opt_str(d, "kind")?.unwrap_or_default();
    Ok(AcceleratorInfo {
        accel_type: parse_accelerator_type(&kind),
        model: get_opt_str(d, "model")?.unwrap_or_default(),
        memory_gb: get_opt_u32(d, "memory_gb")?.unwrap_or(0),
        tops_x10: get_opt_u32(d, "tops_x10")?.map(saturating_u16).unwrap_or(0),
    })
}

fn hardware_from_dict(d: &Bound<'_, PyDict>) -> PyResult<HardwareCapabilities> {
    let mut hw = HardwareCapabilities::new();
    let cores = get_opt_u32(d, "cpu_cores")?;
    let threads = get_opt_u32(d, "cpu_threads")?;
    if let (Some(c), Some(t)) = (cores, threads) {
        hw = hw.with_cpu(saturating_u16(c), saturating_u16(t));
    } else if let Some(c) = cores {
        let c16 = saturating_u16(c);
        hw = hw.with_cpu(c16, c16);
    }
    if let Some(mb) = get_opt_u32(d, "memory_gb")? {
        hw = hw.with_memory(mb);
    }
    if let Some(gpu_dict) = get_opt_dict(d, "gpu")? {
        hw = hw.with_gpu(gpu_info_from_dict(&gpu_dict)?);
    }
    if let Some(list) = get_opt_list(d, "additional_gpus")? {
        for item in list.iter() {
            let sub = item
                .cast_into::<PyDict>()
                .map_err(|_| PyTypeError::new_err("additional_gpus items must be dicts"))?;
            hw = hw.add_gpu(gpu_info_from_dict(&sub)?);
        }
    }
    if let Some(mb) = get_opt_u64(d, "storage_gb")? {
        hw = hw.with_storage(mb);
    }
    if let Some(mbps) = get_opt_u32(d, "network_mbps")? {
        hw = hw.with_network(mbps);
    }
    if let Some(list) = get_opt_list(d, "accelerators")? {
        for item in list.iter() {
            let sub = item
                .cast_into::<PyDict>()
                .map_err(|_| PyTypeError::new_err("accelerators items must be dicts"))?;
            hw = hw.add_accelerator(accelerator_from_dict(&sub)?);
        }
    }
    Ok(hw)
}

fn software_from_dict(d: &Bound<'_, PyDict>) -> PyResult<SoftwareCapabilities> {
    let os = get_opt_str(d, "os")?.unwrap_or_default();
    let os_version = get_opt_str(d, "os_version")?.unwrap_or_default();
    let mut sw = SoftwareCapabilities::new().with_os(os, os_version);
    for (k, v) in pair_vec_from_list(get_opt_list(d, "runtimes")?)? {
        sw = sw.add_runtime(k, v);
    }
    for (k, v) in pair_vec_from_list(get_opt_list(d, "frameworks")?)? {
        sw = sw.add_framework(k, v);
    }
    if let Some(c) = get_opt_str(d, "cuda_version")? {
        sw = sw.with_cuda(c);
    }
    sw.drivers = pair_vec_from_list(get_opt_list(d, "drivers")?)?;
    Ok(sw)
}

fn model_from_dict(d: &Bound<'_, PyDict>) -> PyResult<ModelCapability> {
    let model_id = get_opt_str(d, "model_id")?.unwrap_or_default();
    let family = get_opt_str(d, "family")?.unwrap_or_default();
    let mut mc = ModelCapability::new(model_id, family);
    if let Some(p) = get_opt_u32(d, "parameters_b_x10")? {
        mc.parameters_b_x10 = p;
    }
    if let Some(c) = get_opt_u32(d, "context_length")? {
        mc = mc.with_context_length(c);
    }
    if let Some(q) = get_opt_str(d, "quantization")? {
        mc = mc.with_quantization(q);
    }
    for m in get_opt_str_list(d, "modalities")? {
        mc = mc.add_modality(parse_modality(&m));
    }
    if let Some(t) = get_opt_u32(d, "tokens_per_sec")? {
        mc = mc.with_tokens_per_sec(t);
    }
    if let Some(l) = get_opt_bool(d, "loaded")? {
        mc = mc.with_loaded(l);
    }
    Ok(mc)
}

fn tool_from_dict(d: &Bound<'_, PyDict>) -> PyResult<ToolCapability> {
    let tool_id = get_opt_str(d, "tool_id")?.unwrap_or_default();
    let name = get_opt_str(d, "name")?.unwrap_or_default();
    let mut tc = ToolCapability::new(tool_id, name);
    if let Some(v) = get_opt_str(d, "version")? {
        tc = tc.with_version(v);
    }
    if let Some(s) = get_opt_str(d, "input_schema")? {
        tc = tc.with_input_schema(s);
    }
    if let Some(s) = get_opt_str(d, "output_schema")? {
        tc = tc.with_output_schema(s);
    }
    for r in get_opt_str_list(d, "requires")? {
        tc = tc.requires(r);
    }
    if let Some(ms) = get_opt_u32(d, "estimated_time_ms")? {
        tc = tc.with_estimated_time(ms);
    }
    if let Some(st) = get_opt_bool(d, "stateless")? {
        tc = tc.with_stateless(st);
    }
    Ok(tc)
}

fn limits_from_dict(d: &Bound<'_, PyDict>) -> PyResult<ResourceLimits> {
    let mut rl = ResourceLimits::new();
    if let Some(n) = get_opt_u32(d, "max_concurrent_requests")? {
        rl = rl.with_max_concurrent(n);
    }
    if let Some(n) = get_opt_u32(d, "max_tokens_per_request")? {
        rl = rl.with_max_tokens(n);
    }
    if let Some(n) = get_opt_u32(d, "rate_limit_rpm")? {
        rl = rl.with_rate_limit(n);
    }
    if let Some(n) = get_opt_u32(d, "max_batch_size")? {
        rl = rl.with_max_batch(n);
    }
    if let Some(n) = get_opt_u32(d, "max_input_bytes")? {
        rl.max_input_bytes = n;
    }
    if let Some(n) = get_opt_u32(d, "max_output_bytes")? {
        rl.max_output_bytes = n;
    }
    Ok(rl)
}

// =========================================================================
// Top-level dict → core
// =========================================================================

pub fn capability_set_from_py(d: &Bound<'_, PyDict>) -> PyResult<CapabilitySet> {
    let mut cs = CapabilitySet::new();
    if let Some(h) = get_opt_dict(d, "hardware")? {
        cs = cs.with_hardware(hardware_from_dict(&h)?);
    }
    if let Some(s) = get_opt_dict(d, "software")? {
        cs = cs.with_software(software_from_dict(&s)?);
    }
    if let Some(list) = get_opt_list(d, "models")? {
        for item in list.iter() {
            let sub = item
                .cast_into::<PyDict>()
                .map_err(|_| PyTypeError::new_err("models items must be dicts"))?;
            cs = cs.add_model(model_from_dict(&sub)?);
        }
    }
    if let Some(list) = get_opt_list(d, "tools")? {
        for item in list.iter() {
            let sub = item
                .cast_into::<PyDict>()
                .map_err(|_| PyTypeError::new_err("tools items must be dicts"))?;
            cs = cs.add_tool(tool_from_dict(&sub)?);
        }
    }
    // SDK consumers may supply reserved-prefix tags (`scope:*`,
    // `causal:*`, …). `CapabilitySet::add_tag` routes through
    // `Tag::parse_user`, which silently drops reserved prefixes —
    // correct for application-facing input, wrong at the binding
    // boundary where the Python caller is the SDK. Parse via the
    // unrestricted `Tag::parse` and insert directly.
    for tag in get_opt_str_list(d, "tags")? {
        if let Ok(t) = Tag::parse(&tag) {
            cs.tags.insert(t);
        }
    }
    if let Some(l) = get_opt_dict(d, "limits")? {
        cs = cs.with_limits(limits_from_dict(&l)?);
    }
    Ok(cs)
}

pub fn capability_filter_from_py(d: &Bound<'_, PyDict>) -> PyResult<CapabilityFilter> {
    let mut cf = CapabilityFilter::new();
    for t in get_opt_str_list(d, "require_tags")? {
        cf = cf.require_tag(t);
    }
    for m in get_opt_str_list(d, "require_models")? {
        cf = cf.require_model(m);
    }
    for t in get_opt_str_list(d, "require_tools")? {
        cf = cf.require_tool(t);
    }
    if let Some(mb) = get_opt_u32(d, "min_memory_gb")? {
        cf = cf.with_min_memory(mb);
    }
    if get_opt_bool(d, "require_gpu")?.unwrap_or(false) {
        cf = cf.require_gpu();
    }
    if let Some(v) = get_opt_str(d, "gpu_vendor")? {
        cf = cf.with_gpu_vendor(parse_gpu_vendor(&v));
    }
    if let Some(mb) = get_opt_u32(d, "min_vram_gb")? {
        cf = cf.with_min_vram(mb);
    }
    if let Some(n) = get_opt_u32(d, "min_context_length")? {
        cf = cf.with_min_context(n);
    }
    for m in get_opt_str_list(d, "require_modalities")? {
        cf = cf.require_modality(parse_modality(&m));
    }
    Ok(cf)
}

// =========================================================================
// Scope filter (reserved-tag discovery filter)
// =========================================================================

/// Owned form of [`net::adapter::net::behavior::capability::ScopeFilter`].
/// The core enum borrows `&str` — Python dicts don't survive across
/// lifetimes that way. Callers convert the dict to this owned shape,
/// then run the query inside [`with_scope_filter`] so the borrowed
/// view is alive for the actual call.
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
/// Multi-element variants (`Tenants` / `Regions`) require an
/// intermediate `Vec<&str>`; that intermediate lives on this
/// function's stack so the slice stays valid for `f`.
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

/// Convert a Python scope-filter dict to the owned form.
///
/// Accepted shapes (driven by the dict's `kind` key):
/// - `{"kind": "any"}`
/// - `{"kind": "global_only"}` (also `"globalOnly"`)
/// - `{"kind": "same_subnet"}` (also `"sameSubnet"`)
/// - `{"kind": "tenant", "tenant": "<id>"}`
/// - `{"kind": "tenants", "tenants": ["<id>", ...]}`
/// - `{"kind": "region", "region": "<name>"}`
/// - `{"kind": "regions", "regions": ["<name>", ...]}`
///
/// Unknown `kind` falls through to `Any` defensively. Empty
/// strings / lists collapse to `Any` (an empty tenant id is
/// rejected by the resolver, so `Any` is the more useful default).
pub fn scope_filter_from_py(d: &Bound<'_, PyDict>) -> PyResult<ScopeFilterOwned> {
    let kind = get_opt_str(d, "kind")?.unwrap_or_else(|| "any".to_string());
    Ok(match kind.as_str() {
        "any" => ScopeFilterOwned::Any,
        "global_only" | "globalOnly" => ScopeFilterOwned::GlobalOnly,
        "same_subnet" | "sameSubnet" => ScopeFilterOwned::SameSubnet,
        "tenant" => match get_opt_str(d, "tenant")? {
            Some(t) if !t.is_empty() => ScopeFilterOwned::Tenant(t),
            _ => ScopeFilterOwned::Any,
        },
        "tenants" => {
            // Drop empty tenant ids before constructing the filter.
            // `scope_from_tags` rejects empty announcements, so a
            // query containing `[""]` would never match a real
            // tenant and would only pin to Global candidates. Fall
            // back to Any when the cleaned list is empty.
            let ts = get_opt_str_list(d, "tenants")?;
            let cleaned: Vec<String> = ts.into_iter().filter(|t| !t.is_empty()).collect();
            if cleaned.is_empty() {
                ScopeFilterOwned::Any
            } else {
                ScopeFilterOwned::Tenants(cleaned)
            }
        }
        "region" => match get_opt_str(d, "region")? {
            Some(r) if !r.is_empty() => ScopeFilterOwned::Region(r),
            _ => ScopeFilterOwned::Any,
        },
        "regions" => {
            // Same reasoning as `tenants` above.
            let rs = get_opt_str_list(d, "regions")?;
            let cleaned: Vec<String> = rs.into_iter().filter(|r| !r.is_empty()).collect();
            if cleaned.is_empty() {
                ScopeFilterOwned::Any
            } else {
                ScopeFilterOwned::Regions(cleaned)
            }
        }
        _ => ScopeFilterOwned::Any,
    })
}

// =========================================================================
// Module-level helpers
// =========================================================================

/// Normalize a GPU vendor string to its canonical lowercase form.
/// Matches `bindings/node/src/capabilities.rs::normalize_gpu_vendor`.
#[pyfunction]
pub fn normalize_gpu_vendor(vendor: &str) -> String {
    gpu_vendor_to_string(parse_gpu_vendor(vendor))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression for a cubic-flagged P2: Python-supplied `int`
    /// values wider than u16::MAX silently wrapped via `as u16`
    /// in `gpu_info_from_dict` / `accelerator_from_dict` /
    /// `hardware_from_dict`, turning 65536 cores into 0. Every
    /// conversion site now routes through `saturating_u16`.
    ///
    /// End-to-end observability through `find_nodes` is limited —
    /// the `CapabilityFilter` surface doesn't filter on
    /// `cpu_cores` / `cpu_threads` / `compute_units` /
    /// `tensor_cores`. The helper is the contract; NAPI's tests
    /// exercise the wiring with full POJO inputs.
    #[test]
    fn saturating_u16_clamps_at_u16_max() {
        assert_eq!(saturating_u16(0), 0);
        assert_eq!(saturating_u16(42), 42);
        assert_eq!(saturating_u16(u16::MAX as u32), u16::MAX);
        assert_eq!(saturating_u16(u16::MAX as u32 + 1), u16::MAX);
        assert_eq!(saturating_u16(u32::MAX), u16::MAX);
    }
}
