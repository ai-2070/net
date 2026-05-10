//! C FFI for stateless predicate evaluation (Phase 9c of
//! `CAPABILITY_SYSTEM_SDK_PLAN.md`).
//!
//! Pure helpers — no handles, no state. The substrate's
//! `Predicate::evaluate_unplanned(ctx)` is mirrored across all
//! four bindings (Rust SDK, TS, Python, Go) at the SDK layer;
//! this module brings the same surface to raw C consumers
//! (C / C++ / Zig / Swift / Java JNI / etc.) so they can
//! evaluate predicates locally without going through nRPC.
//!
//! All inputs cross as NUL-terminated UTF-8 JSON strings:
//!
//!   - `predicate_json` — wire-format `PredicateWire` JSON.
//!     The same shape every binding emits / accepts; cross-binding
//!     compat is pinned by
//!     `tests/cross_lang_capability/predicate_eval.json`.
//!   - `tags_json` — JSON array of tag strings, e.g.
//!     `["hardware.gpu", "scope:tenant:foo"]`. Reserved-prefix
//!     tags are accepted (parsed via the privileged path).
//!   - `metadata_json` — JSON object of `string -> string`,
//!     e.g. `{"intent": "ml-training", "region": "us-east"}`.
//!
//! Cross-binding contract: the same `(predicate, tags, metadata)`
//! triple produces identical booleans across every binding. Drift
//! between the C surface and the substrate's evaluator surfaces
//! as a fixture-driven CI failure on the offending side.

use std::collections::BTreeMap;
use std::ffi::c_char;
use std::os::raw::c_int;

use super::NetError;
use crate::adapter::net::behavior::{EvalContext, PredicateWire, Tag};

/// Evaluate a wire-format `Predicate` against a `(tags, metadata)`
/// context. Mirrors `Predicate::evaluate_unplanned(ctx)` from the
/// substrate.
///
/// All three inputs MUST be NUL-terminated UTF-8 JSON strings.
///
/// Return values:
///
///   - `1`  — predicate evaluated to `true`.
///   - `0`  — predicate evaluated to `false`.
///   - `NetError::NullPointer` (negative) — any of the three
///     pointers is NULL.
///   - `NetError::InvalidUtf8` (negative) — input bytes aren't
///     valid UTF-8.
///   - `NetError::InvalidJson` (negative) — failed to parse the
///     `predicate_json` as a `PredicateWire`, the `tags_json` as
///     a `Vec<String>`, the `metadata_json` as an object, OR
///     any tag string failed to parse.
///
/// Stateless. Thread-safe. The substrate's evaluator visits the
/// predicate AST in declaration order without planner reordering;
/// boolean results are invariant under planning, so callers that
/// want planned evaluation can call this and trust the answer.
///
/// # Safety
///
/// `predicate_json`, `tags_json`, and `metadata_json` MUST point
/// at NUL-terminated UTF-8 strings valid for the duration of the
/// call. The buffers are not retained after return.
#[unsafe(no_mangle)]
pub extern "C" fn net_predicate_evaluate(
    predicate_json: *const c_char,
    tags_json: *const c_char,
    metadata_json: *const c_char,
) -> c_int {
    if predicate_json.is_null() || tags_json.is_null() || metadata_json.is_null() {
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

    let tag_strings: Vec<String> = match serde_json::from_str(&tags_s) {
        Ok(v) => v,
        Err(_) => return NetError::InvalidJson.into(),
    };
    let tags: Result<Vec<Tag>, _> = tag_strings.iter().map(|s| Tag::parse(s)).collect();
    let tags = match tags {
        Ok(t) => t,
        Err(_) => return NetError::InvalidJson.into(),
    };

    let metadata: BTreeMap<String, String> = match serde_json::from_str(&meta_s) {
        Ok(m) => m,
        Err(_) => return NetError::InvalidJson.into(),
    };

    let ctx = EvalContext::new(&tags, &metadata);
    if predicate.evaluate_unplanned(&ctx) {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    /// Tiny round-trip: a 2-leaf `(exists hardware.gpu) AND
    /// (metadata_equals region us-east)` predicate should match a
    /// candidate carrying the gpu tag and the right metadata.
    #[test]
    fn evaluates_true_for_matching_context() {
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

        let rc = net_predicate_evaluate(pred.as_ptr(), tags.as_ptr(), meta.as_ptr());
        assert_eq!(rc, 1);
    }

    #[test]
    fn evaluates_false_when_metadata_differs() {
        let pred = CString::new(
            r#"{"nodes":[
                {"kind":"metadata_equals","key":"region","value":"us-east"}
            ],"root_idx":0}"#,
        )
        .unwrap();
        let tags = CString::new(r#"[]"#).unwrap();
        let meta = CString::new(r#"{"region":"us-west"}"#).unwrap();

        let rc = net_predicate_evaluate(pred.as_ptr(), tags.as_ptr(), meta.as_ptr());
        assert_eq!(rc, 0);
    }

    #[test]
    fn returns_null_pointer_on_any_null_input() {
        let pred = CString::new(r#"{"nodes":[],"root_idx":0}"#).unwrap();
        let tags = CString::new(r#"[]"#).unwrap();
        let meta = CString::new(r#"{}"#).unwrap();

        let rc = net_predicate_evaluate(std::ptr::null(), tags.as_ptr(), meta.as_ptr());
        assert!(rc < 0);
        let rc = net_predicate_evaluate(pred.as_ptr(), std::ptr::null(), meta.as_ptr());
        assert!(rc < 0);
        let rc = net_predicate_evaluate(pred.as_ptr(), tags.as_ptr(), std::ptr::null());
        assert!(rc < 0);
    }

    #[test]
    fn returns_invalid_json_on_unparseable_predicate() {
        let pred = CString::new(r#"{"nodes":[],not-json"#).unwrap();
        let tags = CString::new(r#"[]"#).unwrap();
        let meta = CString::new(r#"{}"#).unwrap();

        let rc = net_predicate_evaluate(pred.as_ptr(), tags.as_ptr(), meta.as_ptr());
        assert!(rc < 0);
    }

    /// Reserved-prefix tags (`scope:tenant:foo`) survive the FFI
    /// tag-array parse — `Tag::parse` (privileged) accepts them
    /// even though `parse_user` would reject. Predicate keyed on
    /// the wire-form tag string still evaluates correctly.
    /// Pin that reserved tags survive the FFI roundtrip so scope-
    /// driven filters work from C consumers.
    #[test]
    fn accepts_reserved_prefix_tags() {
        // Predicate `MetadataExists("region")` over a context where
        // metadata is empty AND a reserved-prefix tag is present.
        // We just need the tags array to parse without erroring;
        // the predicate evaluation result is incidental.
        let pred = CString::new(
            r#"{"nodes":[
                {"kind":"metadata_exists","key":"region"}
            ],"root_idx":0}"#,
        )
        .unwrap();
        let tags = CString::new(r#"["scope:tenant:foo","hardware.gpu"]"#).unwrap();
        let meta = CString::new(r#"{}"#).unwrap();

        let rc = net_predicate_evaluate(pred.as_ptr(), tags.as_ptr(), meta.as_ptr());
        // `>= 0` = the tag array parsed cleanly. `< 0` means we
        // hit `InvalidJson` on the tag parse, which would mean
        // `Tag::parse` is rejecting the reserved prefix
        // (regression).
        assert!(
            rc >= 0,
            "reserved-prefix tag must parse via privileged path, got {rc}",
        );
    }
}
