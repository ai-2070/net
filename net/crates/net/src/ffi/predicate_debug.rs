//! C FFI for predicate debug-session helpers (Phase 9d of
//! `CAPABILITY_SYSTEM_SDK_PLAN.md`).
//!
//! Three pure helpers — no handles, no state — exposing what
//! every other binding ships at the SDK layer:
//!
//!   - [`net_predicate_evaluate_with_trace`] — single-evaluation
//!     trace tree (per-clause `label` / `result` / `children`).
//!   - [`net_predicate_aggregate_debug_report`] — corpus-wide
//!     aggregator: total / matched / per-clause `(evaluated,
//!     matched)` rollup keyed by debug label.
//!   - [`net_predicate_redact_metadata_keys`] — host-side scrubber
//!     that rewrites metadata-clause labels before persistence.
//!     The substrate doesn't ship a redaction implementation
//!     (Phase 6 of `CAPABILITY_ENHANCEMENTS_PLAN.md` defined the
//!     API but only the trace + aggregator landed); each binding
//!     implements it. This module ports the same logic the TS /
//!     Python / Go SDKs ship.
//!
//! Cross-binding contracts pinned by:
//!
//!   - `tests/cross_lang_capability/predicate_trace.json`
//!   - `tests/cross_lang_capability/predicate_debug_report.json`
//!   - `tests/cross_lang_capability/predicate_debug_report_redacted.json`
//!
//! Wire shapes mirror the test renderers in
//! `tests/cross_lang_capability_fixtures.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::c_char;
use std::os::raw::c_int;

use serde_json::{json, Value};

use super::NetError;
use crate::adapter::net::behavior::{
    ClauseTrace, EvalContext, PredicateDebugReport, PredicateWire, Tag,
};

// =========================================================================
// Wire-format renderers — mirror the cross-binding fixture canonical
// shape. Duplicate of the test renderer; if both diverge, the
// fixture-comparison tests trip on the offending side.
// =========================================================================

fn clause_trace_to_wire(t: &ClauseTrace) -> Value {
    json!({
        "label": t.label,
        "result": t.result,
        "children": t.children.iter().map(clause_trace_to_wire).collect::<Vec<_>>(),
    })
}

/// Render a `PredicateDebugReport` to its canonical wire shape.
/// `clause_stats` becomes a label-sorted array (matches the
/// `BTreeMap` iteration order on the substrate side).
fn report_to_wire(report: &PredicateDebugReport) -> Value {
    let stats: Vec<Value> = report
        .clause_stats
        .values()
        .map(|s| {
            json!({
                "label": s.label,
                "evaluated": s.evaluated,
                "matched": s.matched,
            })
        })
        .collect();
    json!({
        "total_candidates": report.total_candidates,
        "matched": report.matched,
        "clause_stats": stats,
    })
}

// =========================================================================
// Helpers shared with `ffi::predicate` — keeping them private here so
// the slice stays self-contained. Both modules go through `c_str_to_string`
// + `write_string_out` from `super::mesh`.
// =========================================================================

/// Parse a JSON `Vec<String>` of tag wire-form strings into typed
/// `Tag`s via the privileged path (so reserved-prefix tags
/// survive). Returns the parsed vector or `None` on any parse
/// failure.
fn parse_tag_array(tags_json_str: &str) -> Option<Vec<Tag>> {
    let strings: Vec<String> = serde_json::from_str(tags_json_str).ok()?;
    strings.iter().map(|s| Tag::parse(s)).collect::<Result<_, _>>().ok()
}

/// Parse a `BTreeMap<String, String>` from JSON.
fn parse_metadata(metadata_json_str: &str) -> Option<BTreeMap<String, String>> {
    serde_json::from_str(metadata_json_str).ok()
}

// =========================================================================
// Phase 9d — evaluate_with_trace
// =========================================================================

/// Evaluate a wire-format `Predicate` against `(tags, metadata)`
/// and write a [`ClauseTrace`] tree to the out-param.
///
/// Mirrors `Predicate::evaluate_with_trace(ctx)`. The trace
/// preserves the planner's short-circuit behavior: descendants
/// that didn't run are absent from the tree.
///
/// Inputs (NUL-terminated UTF-8 JSON):
///
///   - `predicate_json` — wire-format `PredicateWire`.
///   - `tags_json`      — JSON array of tag strings.
///   - `metadata_json`  — JSON object of `string -> string`.
///
/// Outputs:
///
///   - `out_result` — set to `1` if the predicate matched, `0`
///     otherwise.
///   - `out_trace_json` / `out_trace_len` — the trace tree's
///     JSON. Free with `net_free_string`. Wire shape:
///     `{"label": str, "result": bool, "children": [...]}`
///     recursively.
///
/// Returns `0` on success, `NetError::*` (negative) on failure.
///
/// # Safety
///
/// All input pointers MUST point at NUL-terminated UTF-8 strings
/// valid for the duration of the call. `out_*` pointers must be
/// writable; on success the caller owns the trace buffer and
/// frees it via `net_free_string`.
#[unsafe(no_mangle)]
pub extern "C" fn net_predicate_evaluate_with_trace(
    predicate_json: *const c_char,
    tags_json: *const c_char,
    metadata_json: *const c_char,
    out_result: *mut c_int,
    out_trace_json: *mut *mut c_char,
    out_trace_len: *mut usize,
) -> c_int {
    if predicate_json.is_null()
        || tags_json.is_null()
        || metadata_json.is_null()
        || out_result.is_null()
        || out_trace_json.is_null()
        || out_trace_len.is_null()
    {
        return NetError::NullPointer.into();
    }

    let pred_s = match unsafe { super::mesh::c_str_to_string(predicate_json) } {
        Some(s) => s,
        None => return NetError::InvalidUtf8.into(),
    };
    let tags_s = match unsafe { super::mesh::c_str_to_string(tags_json) } {
        Some(s) => s,
        None => return NetError::InvalidUtf8.into(),
    };
    let meta_s = match unsafe { super::mesh::c_str_to_string(metadata_json) } {
        Some(s) => s,
        None => return NetError::InvalidUtf8.into(),
    };

    let wire: PredicateWire = match serde_json::from_str(&pred_s) {
        Ok(w) => w,
        Err(_) => return NetError::InvalidJson.into(),
    };
    let predicate = match wire.into_predicate() {
        Ok(p) => p,
        Err(_) => return NetError::InvalidJson.into(),
    };
    let Some(tags) = parse_tag_array(&tags_s) else {
        return NetError::InvalidJson.into();
    };
    let Some(metadata) = parse_metadata(&meta_s) else {
        return NetError::InvalidJson.into();
    };

    let ctx = EvalContext::new(&tags, &metadata);
    let (result, trace) = predicate.evaluate_with_trace(&ctx);

    unsafe {
        *out_result = if result { 1 } else { 0 };
    }
    let payload = clause_trace_to_wire(&trace);
    super::mesh::write_string_out(
        serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string()),
        out_trace_json,
        out_trace_len,
    )
}

// =========================================================================
// Phase 9d — aggregate_debug_report
// =========================================================================

/// Run `predicate` against every entry in `contexts_json` and
/// write a [`PredicateDebugReport`] to the out-param. Mirrors
/// `PredicateDebugReport::from_evaluations(pred, contexts)`.
///
/// Inputs (NUL-terminated UTF-8 JSON):
///
///   - `predicate_json` — wire-format `PredicateWire`.
///   - `contexts_json`  — JSON array of evaluation contexts:
///     `[{"tags": [...], "metadata": {...}}, ...]`. Each context
///     contributes one corpus row.
///
/// Outputs:
///
///   - `out_report_json` / `out_report_len` — the report JSON.
///     Free with `net_free_string`. Wire shape:
///
/// ```json
/// {
///   "total_candidates": <usize>,
///   "matched": <usize>,
///   "clause_stats": [
///     {"label": "<debug-label>", "evaluated": <usize>, "matched": <usize>},
///     ...
///   ]
/// }
/// ```
///
/// `clause_stats` is sorted by label (the substrate uses
/// `BTreeMap`, so iteration is in label order).
///
/// Returns `0` on success, `NetError::*` (negative) on parse /
/// null-pointer failure.
///
/// # Safety
///
/// All input pointers MUST point at NUL-terminated UTF-8 strings.
/// On success the caller owns the report buffer and frees it via
/// `net_free_string`.
#[unsafe(no_mangle)]
pub extern "C" fn net_predicate_aggregate_debug_report(
    predicate_json: *const c_char,
    contexts_json: *const c_char,
    out_report_json: *mut *mut c_char,
    out_report_len: *mut usize,
) -> c_int {
    if predicate_json.is_null()
        || contexts_json.is_null()
        || out_report_json.is_null()
        || out_report_len.is_null()
    {
        return NetError::NullPointer.into();
    }

    let pred_s = match unsafe { super::mesh::c_str_to_string(predicate_json) } {
        Some(s) => s,
        None => return NetError::InvalidUtf8.into(),
    };
    let ctx_s = match unsafe { super::mesh::c_str_to_string(contexts_json) } {
        Some(s) => s,
        None => return NetError::InvalidUtf8.into(),
    };

    let wire: PredicateWire = match serde_json::from_str(&pred_s) {
        Ok(w) => w,
        Err(_) => return NetError::InvalidJson.into(),
    };
    let predicate = match wire.into_predicate() {
        Ok(p) => p,
        Err(_) => return NetError::InvalidJson.into(),
    };

    // Decode the corpus into owned `(Vec<Tag>, BTreeMap)` pairs
    // so each `EvalContext` can borrow them. `EvalContext::new`
    // takes a `&[Tag]` slice; the owning Vec must outlive the
    // iteration. Same shape the test renderer uses.
    #[derive(serde::Deserialize)]
    struct CtxJson {
        tags: Vec<String>,
        metadata: BTreeMap<String, String>,
    }
    let raw_contexts: Vec<CtxJson> = match serde_json::from_str(&ctx_s) {
        Ok(v) => v,
        Err(_) => return NetError::InvalidJson.into(),
    };
    let mut owned: Vec<(Vec<Tag>, BTreeMap<String, String>)> =
        Vec::with_capacity(raw_contexts.len());
    for c in raw_contexts {
        let tags: Result<Vec<Tag>, _> = c.tags.iter().map(|s| Tag::parse(s)).collect();
        let Ok(tags) = tags else {
            return NetError::InvalidJson.into();
        };
        owned.push((tags, c.metadata));
    }

    let report = PredicateDebugReport::from_evaluations(
        &predicate,
        owned.iter().map(|(tags, meta)| EvalContext::new(tags, meta)),
    );

    let payload = report_to_wire(&report);
    super::mesh::write_string_out(
        serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string()),
        out_report_json,
        out_report_len,
    )
}

// =========================================================================
// Phase 9d — redact_metadata_keys
//
// Pure host-side label rewriter. The substrate doesn't ship a
// redaction impl; each binding implements it. This module ports
// the logic from sdk-py / sdk-ts / Go SDK so raw C consumers get
// parity.
//
// Redaction rules (only metadata-clause labels carrying values
// are rewritten; everything else passes through):
//
//   MetadataEquals(<key>=<value>)            → MetadataEquals(<key>=<redacted>)
//   MetadataMatches(<key> contains "<pat>")  → MetadataMatches(<key> contains "<redacted>")
//   MetadataNumericAtLeast(<key> >= <thr>)   → MetadataNumericAtLeast(<key> >= <redacted>)
//   MetadataExists(<key>)                    — unchanged (no value)
//   non-metadata labels                      — unchanged
//
// After rewriting, stats with the same redacted label are merged
// (`evaluated` and `matched` summed). Output is sorted by label.
// Idempotent: redact(redact(r, k), k) == redact(r, k).
// =========================================================================

/// Strip a prefix and suffix from a label, returning the inside
/// or `None` if either anchor doesn't match.
fn strip_label<'a>(label: &'a str, prefix: &str, suffix: &str) -> Option<&'a str> {
    label
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_suffix(suffix))
}

/// Redact a single label per the rules above. Returns the
/// rewritten label (owned `String`); falls through for non-
/// metadata or non-targeted-key labels.
fn redact_label(label: &str, keys: &BTreeSet<String>) -> String {
    // MetadataEquals(<key>=<value>)
    if let Some(inner) = strip_label(label, "MetadataEquals(", ")") {
        if let Some(eq_idx) = inner.find('=') {
            let (key, _value) = inner.split_at(eq_idx);
            if keys.contains(key) {
                return format!("MetadataEquals({key}=<redacted>)");
            }
        }
        return label.to_string();
    }
    // MetadataMatches(<key> contains "<pattern>")
    if let Some(inner) = strip_label(label, "MetadataMatches(", ")") {
        let needle = " contains \"";
        if let Some(at) = inner.find(needle) {
            let (key, rest) = inner.split_at(at);
            if rest.ends_with('"') && keys.contains(key) {
                return format!("MetadataMatches({key} contains \"<redacted>\")");
            }
        }
        return label.to_string();
    }
    // MetadataNumericAtLeast(<key> >= <threshold>)
    if let Some(inner) = strip_label(label, "MetadataNumericAtLeast(", ")") {
        let needle = " >= ";
        if let Some(at) = inner.find(needle) {
            let (key, _rest) = inner.split_at(at);
            if keys.contains(key) {
                return format!("MetadataNumericAtLeast({key} >= <redacted>)");
            }
        }
        return label.to_string();
    }
    // Anything else passes through (`MetadataExists`, all non-
    // metadata leaves, composites).
    label.to_string()
}

/// Apply [`redact_label`] across a wire-format report and write
/// the redacted report to the out-param.
///
/// Inputs (NUL-terminated UTF-8 JSON):
///
///   - `report_json` — wire-format `PredicateDebugReport`
///     (output of [`net_predicate_aggregate_debug_report`]).
///   - `keys_json`   — JSON array of metadata key names whose
///     values should be scrubbed:
///     `["api_key", "secret_token"]`.
///
/// Outputs:
///
///   - `out_redacted_json` / `out_redacted_len` — the redacted
///     report JSON. Free with `net_free_string`. Same wire shape
///     as the input report; `clause_stats` re-sorted by label
///     after redaction (since redacted labels may collide and
///     merge).
///
/// Returns `0` on success, `NetError::*` (negative) on parse /
/// null-pointer failure.
///
/// Idempotent: redacting an already-redacted report with the
/// same keys is a no-op.
///
/// # Safety
///
/// All input pointers MUST point at NUL-terminated UTF-8 strings.
/// On success the caller owns the redacted-report buffer and
/// frees it via `net_free_string`.
#[unsafe(no_mangle)]
pub extern "C" fn net_predicate_redact_metadata_keys(
    report_json: *const c_char,
    keys_json: *const c_char,
    out_redacted_json: *mut *mut c_char,
    out_redacted_len: *mut usize,
) -> c_int {
    if report_json.is_null()
        || keys_json.is_null()
        || out_redacted_json.is_null()
        || out_redacted_len.is_null()
    {
        return NetError::NullPointer.into();
    }

    let report_s = match unsafe { super::mesh::c_str_to_string(report_json) } {
        Some(s) => s,
        None => return NetError::InvalidUtf8.into(),
    };
    let keys_s = match unsafe { super::mesh::c_str_to_string(keys_json) } {
        Some(s) => s,
        None => return NetError::InvalidUtf8.into(),
    };

    let report: Value = match serde_json::from_str(&report_s) {
        Ok(v) => v,
        Err(_) => return NetError::InvalidJson.into(),
    };
    let keys_vec: Vec<String> = match serde_json::from_str(&keys_s) {
        Ok(v) => v,
        Err(_) => return NetError::InvalidJson.into(),
    };
    let keys: BTreeSet<String> = keys_vec.into_iter().collect();

    // Walk `clause_stats`, redact each label, merge collisions.
    let stats = match report.get("clause_stats").and_then(|s| s.as_array()) {
        Some(s) => s,
        None => return NetError::InvalidJson.into(),
    };
    let mut merged: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for entry in stats {
        let label = match entry.get("label").and_then(|l| l.as_str()) {
            Some(l) => l.to_string(),
            None => return NetError::InvalidJson.into(),
        };
        let evaluated = entry
            .get("evaluated")
            .and_then(|n| n.as_u64())
            .unwrap_or(0);
        let matched = entry.get("matched").and_then(|n| n.as_u64()).unwrap_or(0);
        let new_label = redact_label(&label, &keys);
        let slot = merged.entry(new_label).or_insert((0, 0));
        slot.0 += evaluated;
        slot.1 += matched;
    }
    let new_stats: Vec<Value> = merged
        .into_iter()
        .map(|(label, (evaluated, matched))| {
            json!({
                "label": label,
                "evaluated": evaluated,
                "matched": matched,
            })
        })
        .collect();

    // Preserve the top-level counters from the input report.
    let total = report
        .get("total_candidates")
        .and_then(|n| n.as_u64())
        .unwrap_or(0);
    let matched = report.get("matched").and_then(|n| n.as_u64()).unwrap_or(0);

    let payload = json!({
        "total_candidates": total,
        "matched": matched,
        "clause_stats": new_stats,
    });
    super::mesh::write_string_out(
        serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string()),
        out_redacted_json,
        out_redacted_len,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{CStr, CString};

    /// Helper: read a CString out-param, free it, return owned String.
    fn read_and_free(ptr: *mut c_char) -> String {
        assert!(!ptr.is_null());
        let s = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_string();
        unsafe {
            let _ = CString::from_raw(ptr);
        }
        s
    }

    /// `evaluate_with_trace` for a 2-leaf AND. The matching path
    /// should produce a trace tree with the And label and both
    /// children's results.
    #[test]
    fn evaluate_with_trace_records_full_tree() {
        let pred = CString::new(
            r#"{"nodes":[
                {"kind":"exists","key":{"axis":"hardware","key":"gpu"}},
                {"kind":"metadata_equals","key":"region","value":"us-east"},
                {"kind":"and","children":[0,1]}
            ],"root_idx":2}"#,
        )
        .unwrap();
        let tags = CString::new(r#"["hardware.gpu"]"#).unwrap();
        let meta = CString::new(r#"{"region":"us-east"}"#).unwrap();

        let mut result: c_int = -1;
        let mut out_ptr: *mut c_char = std::ptr::null_mut();
        let mut out_len: usize = 0;
        let rc = net_predicate_evaluate_with_trace(
            pred.as_ptr(),
            tags.as_ptr(),
            meta.as_ptr(),
            &mut result,
            &mut out_ptr,
            &mut out_len,
        );
        assert_eq!(rc, 0);
        assert_eq!(result, 1);

        let trace_json = read_and_free(out_ptr);
        let v: Value = serde_json::from_str(&trace_json).unwrap();
        assert!(v["label"].as_str().unwrap().starts_with("And"));
        assert_eq!(v["result"], true);
        let children = v["children"].as_array().unwrap();
        assert_eq!(children.len(), 2);
        // Both leaves matched.
        assert!(children.iter().all(|c| c["result"] == true));
    }

    /// `aggregate_debug_report` over a 3-row corpus. Should
    /// produce `total=3`, `matched` = how many matched, and
    /// per-clause stats.
    #[test]
    fn aggregate_debug_report_rolls_up_per_clause_stats() {
        let pred = CString::new(
            r#"{"nodes":[
                {"kind":"metadata_equals","key":"region","value":"us-east"}
            ],"root_idx":0}"#,
        )
        .unwrap();
        let contexts = CString::new(
            r#"[
                {"tags":[],"metadata":{"region":"us-east"}},
                {"tags":[],"metadata":{"region":"us-west"}},
                {"tags":[],"metadata":{"region":"us-east"}}
            ]"#,
        )
        .unwrap();

        let mut out_ptr: *mut c_char = std::ptr::null_mut();
        let mut out_len: usize = 0;
        let rc = net_predicate_aggregate_debug_report(
            pred.as_ptr(),
            contexts.as_ptr(),
            &mut out_ptr,
            &mut out_len,
        );
        assert_eq!(rc, 0);

        let report_json = read_and_free(out_ptr);
        let v: Value = serde_json::from_str(&report_json).unwrap();
        assert_eq!(v["total_candidates"], 3);
        assert_eq!(v["matched"], 2);
        let stats = v["clause_stats"].as_array().unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0]["evaluated"], 3);
        assert_eq!(stats[0]["matched"], 2);
    }

    /// `redact_metadata_keys` rewrites `MetadataEquals(api_key=...)`
    /// to `MetadataEquals(api_key=<redacted>)` and leaves
    /// non-metadata labels untouched.
    #[test]
    fn redact_metadata_keys_rewrites_targeted_labels() {
        let report = CString::new(
            r#"{
                "total_candidates": 10,
                "matched": 4,
                "clause_stats": [
                    {"label": "MetadataEquals(api_key=sk-secret-1)", "evaluated": 10, "matched": 4},
                    {"label": "MetadataEquals(region=us-east)", "evaluated": 10, "matched": 7},
                    {"label": "Exists(hardware.gpu)", "evaluated": 10, "matched": 8}
                ]
            }"#,
        )
        .unwrap();
        let keys = CString::new(r#"["api_key"]"#).unwrap();

        let mut out_ptr: *mut c_char = std::ptr::null_mut();
        let mut out_len: usize = 0;
        let rc = net_predicate_redact_metadata_keys(
            report.as_ptr(),
            keys.as_ptr(),
            &mut out_ptr,
            &mut out_len,
        );
        assert_eq!(rc, 0);

        let redacted = read_and_free(out_ptr);
        let v: Value = serde_json::from_str(&redacted).unwrap();
        assert_eq!(v["total_candidates"], 10);
        assert_eq!(v["matched"], 4);
        let stats = v["clause_stats"].as_array().unwrap();
        let labels: Vec<&str> = stats.iter().map(|s| s["label"].as_str().unwrap()).collect();
        assert!(labels.contains(&"MetadataEquals(api_key=<redacted>)"));
        assert!(labels.contains(&"MetadataEquals(region=us-east)"));
        assert!(labels.contains(&"Exists(hardware.gpu)"));
    }

    /// Redaction is idempotent: a second pass with the same keys
    /// produces the same report.
    #[test]
    fn redact_metadata_keys_is_idempotent() {
        let report = CString::new(
            r#"{
                "total_candidates": 5,
                "matched": 2,
                "clause_stats": [
                    {"label": "MetadataEquals(secret=foo)", "evaluated": 5, "matched": 2}
                ]
            }"#,
        )
        .unwrap();
        let keys = CString::new(r#"["secret"]"#).unwrap();

        // First pass.
        let mut out1: *mut c_char = std::ptr::null_mut();
        let mut len1: usize = 0;
        net_predicate_redact_metadata_keys(report.as_ptr(), keys.as_ptr(), &mut out1, &mut len1);
        let pass1 = read_and_free(out1);

        // Second pass over the already-redacted output.
        let pass1_cs = CString::new(pass1.clone()).unwrap();
        let mut out2: *mut c_char = std::ptr::null_mut();
        let mut len2: usize = 0;
        net_predicate_redact_metadata_keys(
            pass1_cs.as_ptr(),
            keys.as_ptr(),
            &mut out2,
            &mut len2,
        );
        let pass2 = read_and_free(out2);

        assert_eq!(pass1, pass2, "redaction must be idempotent");
    }

    /// NULL inputs return `NullPointer` from each function.
    #[test]
    fn null_inputs_return_null_pointer_across_all_three() {
        let pred = CString::new(r#"{"nodes":[],"root_idx":0}"#).unwrap();
        let tags = CString::new(r#"[]"#).unwrap();
        let meta = CString::new(r#"{}"#).unwrap();
        let ctxs = CString::new(r#"[]"#).unwrap();
        let report = CString::new(
            r#"{"total_candidates":0,"matched":0,"clause_stats":[]}"#,
        )
        .unwrap();
        let keys = CString::new(r#"[]"#).unwrap();

        let mut result: c_int = 0;
        let mut out_ptr: *mut c_char = std::ptr::null_mut();
        let mut out_len: usize = 0;

        // evaluate_with_trace
        assert!(
            net_predicate_evaluate_with_trace(
                std::ptr::null(),
                tags.as_ptr(),
                meta.as_ptr(),
                &mut result,
                &mut out_ptr,
                &mut out_len,
            ) < 0
        );

        // aggregate_debug_report
        assert!(
            net_predicate_aggregate_debug_report(
                pred.as_ptr(),
                std::ptr::null(),
                &mut out_ptr,
                &mut out_len,
            ) < 0
        );

        // redact_metadata_keys
        assert!(
            net_predicate_redact_metadata_keys(
                report.as_ptr(),
                std::ptr::null(),
                &mut out_ptr,
                &mut out_len,
            ) < 0
        );
        // Also check report null
        assert!(
            net_predicate_redact_metadata_keys(
                std::ptr::null(),
                keys.as_ptr(),
                &mut out_ptr,
                &mut out_len,
            ) < 0
        );
        // Avoid `unused` on ctxs
        let _ = ctxs;
    }
}
