//! C FFI for stateless capability-set validation (Phase 9a of
//! `CAPABILITY_SYSTEM_SDK_PLAN.md`).
//!
//! Pure helper — no handles, no state. The substrate's
//! `validate_capabilities(caps)` checks a `CapabilitySet` against
//! the canonical `AXIS_SCHEMA` and produces a `ValidationReport`
//! of `errors` (operator-must-fix) + `warnings` (forward-compat
//! / hygiene). This module mirrors the report wire shape every
//! SDK already ships at the language layer (TS / Python / Go),
//! exposed at the C ABI for raw consumers.
//!
//! Cross-binding contract: the same caps payload produces
//! identical `errors` + `warnings` output across every binding.
//! Pinned by `tests/cross_lang_capability/capability_validation.json`.
//!
//! Wire shape of the returned report:
//!
//! ```json
//! {
//!   "errors": [
//!     {"kind": "unknown_axis", "axis_prefix": "compute", "tag": "compute.gpu"},
//!     {"kind": "type_mismatch", "axis": "hardware", "key": "memory_mb",
//!      "expected": "number", "actual": "lots"},
//!     {"kind": "index_malformed", "axis": "software", "prefix": "model.",
//!      "index": "bogus", "tag": "software.model.bogus.id=foo"}
//!   ],
//!   "warnings": [
//!     {"kind": "unknown_key", "axis": "hardware", "key": "future_field"},
//!     {"kind": "metadata_oversize", "soft_cap_bytes": 4096, "actual_bytes": 5120},
//!     {"kind": "legacy_tag", "tag": "nat:full-cone"}
//!   ]
//! }
//! ```
//!
//! Both arrays are sorted by their JSON-stringified entry so the
//! cross-binding fixture comparison is order-independent. The
//! `metadata_oversize` warning fires off the
//! `METADATA_SOFT_CAP_BYTES` constant (4 KB) — bindings that want
//! a tighter cap promote warnings to errors at their layer.

use std::ffi::c_char;
use std::os::raw::c_int;

use serde_json::{json, Value};

use super::NetError;
use crate::adapter::net::behavior::{
    validate_capabilities, CapabilitySet, SchemaError, ValidationWarning, ValueType,
};

/// Validate a wire-format `CapabilitySet` and write the
/// `ValidationReport` (also wire-format JSON) to the out-param.
///
/// Mirrors the SDK-layer `validate_capabilities` surface every
/// binding ships, exposed at the C ABI for raw consumers.
///
/// Inputs (NUL-terminated UTF-8 JSON):
///
///   - `caps_json` — wire-format `CapabilitySet`:
///     `{"tags": [...], "metadata": {...}}`. Reserved-prefix tags
///     (`scope:`, `causal:`, etc.) accepted via the privileged
///     parse path; legacy untyped tags surface as `legacy_tag`
///     warnings on the way out.
///
/// Outputs:
///
///   - `out_report_json` / `out_report_len` — the report's JSON
///     bytes are written here. Free with `net_free_string`. Wire
///     shape documented in module-level docs.
///
/// Return values:
///
///   - `0` on success.
///   - `NetError::NullPointer` (negative) — `caps_json` /
///     `out_report_json` / `out_report_len` is NULL.
///   - `NetError::InvalidUtf8` (negative) — input bytes aren't
///     valid UTF-8.
///   - `NetError::InvalidJson` (negative) — input failed to
///     parse as a `CapabilitySet`.
///
/// Stateless. Thread-safe. The validator runs against the
/// canonical `AXIS_SCHEMA` baked in at substrate build time;
/// per-binding schema overrides are not (yet) exposed at the C
/// ABI — Rust SDK callers needing a custom schema use
/// `validate_capabilities_against` directly.
///
/// # Safety
///
/// `caps_json` MUST be a NUL-terminated UTF-8 string valid for
/// the duration of the call. `out_report_json` / `out_report_len`
/// must point to writable memory; on success the caller owns the
/// returned buffer and must free it via `net_free_string`.
#[unsafe(no_mangle)]
pub extern "C" fn net_validate_capabilities(
    caps_json: *const c_char,
    out_report_json: *mut *mut c_char,
    out_report_len: *mut usize,
) -> c_int {
    if caps_json.is_null() || out_report_json.is_null() || out_report_len.is_null() {
        return NetError::NullPointer.into();
    }

    let caps_s = match unsafe { super::mesh::c_str_to_string(caps_json) } {
        Some(s) => s,
        None => return NetError::InvalidUtf8.into(),
    };

    let caps: CapabilitySet = match serde_json::from_str(&caps_s) {
        Ok(c) => c,
        Err(_) => return NetError::InvalidJson.into(),
    };

    let report = validate_capabilities(&caps);

    // Render to wire form. Sort each list by JSON-stringified
    // entry so the cross-binding fixture comparison is order-
    // independent (matches the test renderer in
    // `tests/cross_lang_capability_fixtures.rs`).
    let mut errors: Vec<Value> = report.errors.iter().map(schema_error_to_wire).collect();
    let mut warnings: Vec<Value> = report
        .warnings
        .iter()
        .map(validation_warning_to_wire)
        .collect();
    canonical_sort(&mut errors);
    canonical_sort(&mut warnings);

    let payload = json!({
        "errors": errors,
        "warnings": warnings,
    });

    super::mesh::write_string_out(
        serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string()),
        out_report_json,
        out_report_len,
    )
}

// =========================================================================
// Wire-format renderers — mirror the cross-binding fixture
// canonical shape. Duplicates `tests/cross_lang_capability_fixtures.rs`
// renderers; if both diverge, the fixture-comparison test fails
// and operators see the drift.
// =========================================================================

fn value_type_to_wire(t: ValueType) -> &'static str {
    match t {
        ValueType::Presence => "presence",
        ValueType::Number => "number",
        ValueType::String => "string",
        ValueType::Enumeration => "enumeration",
        ValueType::Bool => "bool",
        ValueType::Csv => "csv",
    }
}

fn schema_error_to_wire(e: &SchemaError) -> Value {
    match e {
        SchemaError::UnknownAxis { axis_prefix, tag } => json!({
            "kind": "unknown_axis",
            "axis_prefix": axis_prefix,
            "tag": tag,
        }),
        SchemaError::TypeMismatch {
            axis,
            key,
            expected,
            actual,
        } => json!({
            "kind": "type_mismatch",
            "axis": axis.as_str(),
            "key": key,
            "expected": value_type_to_wire(*expected),
            "actual": actual,
        }),
        SchemaError::IndexMalformed {
            axis,
            prefix,
            index,
            tag,
        } => json!({
            "kind": "index_malformed",
            "axis": axis.as_str(),
            "prefix": prefix,
            "index": index,
            "tag": tag,
        }),
    }
}

fn validation_warning_to_wire(w: &ValidationWarning) -> Value {
    match w {
        ValidationWarning::UnknownKey { axis, key } => json!({
            "kind": "unknown_key",
            "axis": axis.as_str(),
            "key": key,
        }),
        ValidationWarning::MetadataOversize {
            soft_cap_bytes,
            actual_bytes,
        } => json!({
            "kind": "metadata_oversize",
            "soft_cap_bytes": soft_cap_bytes,
            "actual_bytes": actual_bytes,
        }),
        ValidationWarning::LegacyTag { tag } => json!({
            "kind": "legacy_tag",
            "tag": tag,
        }),
        // CR-14: metadata-key reservation warnings.
        ValidationWarning::MetadataReservedKey { key } => json!({
            "kind": "metadata_reserved_key",
            "key": key,
        }),
        ValidationWarning::MetadataReservedPrefix { key, prefix } => json!({
            "kind": "metadata_reserved_prefix",
            "key": key,
            "prefix": prefix,
        }),
    }
}

fn canonical_sort(v: &mut [Value]) {
    v.sort_by_key(|x| x.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{CStr, CString};

    /// Helper: run `net_validate_capabilities` over a JSON caps
    /// payload, return the report-JSON string. Frees the FFI-
    /// returned buffer.
    fn validate(caps_json: &str) -> String {
        let cs = CString::new(caps_json).unwrap();
        let mut out_ptr: *mut c_char = std::ptr::null_mut();
        let mut out_len: usize = 0;
        let rc = net_validate_capabilities(cs.as_ptr(), &mut out_ptr, &mut out_len);
        assert_eq!(rc, 0, "expected ok, got {rc}");
        assert!(!out_ptr.is_null());
        // Read back as &str (NUL-terminated; out_len is just the
        // byte count for callers that don't want to call strlen).
        let out = unsafe { CStr::from_ptr(out_ptr) }
            .to_str()
            .unwrap()
            .to_string();
        unsafe {
            // Same free path the substrate's `net_free_string`
            // takes — `CString::from_raw` reclaims the
            // `into_raw` allocation.
            let _ = CString::from_raw(out_ptr);
        }
        out
    }

    /// Empty caps → empty errors + empty warnings.
    #[test]
    fn empty_caps_produces_clean_report() {
        let out = validate(r#"{"tags": [], "metadata": {}}"#);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["errors"].as_array().unwrap().len(), 0);
        assert_eq!(v["warnings"].as_array().unwrap().len(), 0);
    }

    /// A non-canonical axis prefix (e.g. `compute.gpu`) reaches
    /// the validator as `Tag::Legacy` after wire-format
    /// deserialization — the parser folds shapes whose first
    /// segment isn't a known axis into `Legacy` rather than into
    /// an `AxisValue` with an unknown axis. So they surface as
    /// `legacy_tag` warnings, NOT as `unknown_axis` errors. Pin
    /// that contract: bindings consuming peer-side caps see
    /// hygiene warnings, not validation failures, for unknown
    /// shapes.
    #[test]
    fn unknown_axis_shape_surfaces_as_legacy_warning() {
        let out = validate(r#"{"tags": ["compute.gpu"], "metadata": {}}"#);
        let v: Value = serde_json::from_str(&out).unwrap();
        let warnings = v["warnings"].as_array().unwrap();
        assert_eq!(v["errors"].as_array().unwrap().len(), 0);
        assert_eq!(warnings.len(), 1, "report = {v}");
        assert_eq!(warnings[0]["kind"], "legacy_tag");
        assert_eq!(warnings[0]["tag"], "compute.gpu");
    }

    /// Numeric axis-key with a non-numeric value → `type_mismatch`
    /// error.
    #[test]
    fn numeric_key_with_garbage_value_emits_type_mismatch() {
        let out = validate(r#"{"tags": ["hardware.memory_mb=lots"], "metadata": {}}"#);
        let v: Value = serde_json::from_str(&out).unwrap();
        let errors = v["errors"].as_array().unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0]["kind"], "type_mismatch");
        assert_eq!(errors[0]["axis"], "hardware");
        assert_eq!(errors[0]["expected"], "number");
        assert_eq!(errors[0]["actual"], "lots");
    }

    /// NULL inputs return `NullPointer`.
    #[test]
    fn null_inputs_return_null_pointer() {
        let cs = CString::new("{}").unwrap();
        let mut out_ptr: *mut c_char = std::ptr::null_mut();
        let mut out_len: usize = 0;

        let rc = net_validate_capabilities(std::ptr::null(), &mut out_ptr, &mut out_len);
        assert!(rc < 0);
        let rc = net_validate_capabilities(cs.as_ptr(), std::ptr::null_mut(), &mut out_len);
        assert!(rc < 0);
        let rc = net_validate_capabilities(cs.as_ptr(), &mut out_ptr, std::ptr::null_mut());
        assert!(rc < 0);
    }

    /// Malformed JSON returns `InvalidJson`.
    #[test]
    fn malformed_caps_returns_invalid_json() {
        let cs = CString::new(r#"{"tags": [not-json"#).unwrap();
        let mut out_ptr: *mut c_char = std::ptr::null_mut();
        let mut out_len: usize = 0;
        let rc = net_validate_capabilities(cs.as_ptr(), &mut out_ptr, &mut out_len);
        assert!(rc < 0);
    }
}
