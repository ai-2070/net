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
use crate::adapter::net::behavior::{
    predicate_to_rpc_header, EvalContext, PredicateWire, Tag, MAX_PREDICATE_RPC_HEADER_VALUE_LEN,
    RPC_WHERE_HEADER,
};

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

// =========================================================================
// Phase 9b — predicate-pushdown header helper
//
// Builds the canonical `cyberdeck-where:` request-header pair for a
// wire-format predicate. Mirrors the Go SDK's `WhereHeader` helper
// (`bindings/go/net/capability.go`). The returned `(name, value)`
// pair drops into any `request_headers`-shaped option list once a
// header-bearing call variant ships in `libnet_rpc`; today's C
// ABI in `net_rpc.h` doesn't accept request headers yet, so the
// helper is documentation + future-proofing.
//
// Wire format pinned by
// `tests/cross_lang_capability/predicate_nrpc_envelope.json`.
// =========================================================================

/// Encode a wire-format `Predicate` as the canonical
/// `cyberdeck-where:` request-header value.
///
/// Inputs:
///   - `predicate_json` — NUL-terminated UTF-8 `PredicateWire`
///     JSON. The same shape `net_predicate_evaluate` and the
///     SDK-layer `predicateToWire` produce.
///
/// Outputs:
///   - `*out_header_name`  — owned `char*` containing `"cyberdeck-where"`.
///                          Free via `net_free_string`.
///   - `*out_header_name_len` — strlen of the header name.
///   - `*out_value_ptr`    — owned `uint8_t*` containing the
///                          canonical JSON bytes. Free via
///                          `net_free_string` (the buffer was
///                          allocated as a `CString::into_raw`,
///                          same release path as other string-
///                          out helpers in this module).
///   - `*out_value_len`    — byte length of the value buffer.
///
/// Returns:
///   - `0` on success.
///   - `NetError::NullPointer` (negative) — any pointer NULL.
///   - `NetError::InvalidUtf8` (negative) — input bytes not UTF-8.
///   - `NetError::InvalidJson` (negative) — predicate failed to
///     parse, OR encoded bytes exceed
///     `MAX_PREDICATE_RPC_HEADER_VALUE_LEN` (4096) per the
///     substrate's wire-cap rule.
///
/// Stateless. Thread-safe.
///
/// # Safety
///
/// `predicate_json` MUST point at a NUL-terminated UTF-8 string
/// valid for the duration of the call. Out-pointers must be
/// writable; on success the caller owns both buffers and frees
/// them via `net_free_string`.
#[unsafe(no_mangle)]
pub extern "C" fn net_predicate_to_where_header(
    predicate_json: *const c_char,
    out_header_name: *mut *mut c_char,
    out_header_name_len: *mut usize,
    out_value_ptr: *mut *mut c_char,
    out_value_len: *mut usize,
) -> c_int {
    if predicate_json.is_null()
        || out_header_name.is_null()
        || out_header_name_len.is_null()
        || out_value_ptr.is_null()
        || out_value_len.is_null()
    {
        return NetError::NullPointer.into();
    }

    let pred_s = match unsafe { super::mesh::c_str_to_string(predicate_json) } {
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

    // Encode via the substrate's wire-cap-respecting helper.
    let (name, value_bytes) = match predicate_to_rpc_header(&predicate) {
        Ok(pair) => pair,
        Err(_) => return NetError::InvalidJson.into(),
    };
    debug_assert_eq!(name, RPC_WHERE_HEADER);
    debug_assert!(value_bytes.len() <= MAX_PREDICATE_RPC_HEADER_VALUE_LEN);

    // Write header name out via the existing string helper. The
    // value bytes are JSON (always UTF-8 since serde_json emits
    // it that way), so we route through the same `write_string_out`
    // path — caller frees both via `net_free_string`.
    let name_rc = super::mesh::write_string_out(name, out_header_name, out_header_name_len);
    if name_rc != 0 {
        return name_rc;
    }
    // SAFETY: serde_json output is guaranteed valid UTF-8.
    let value_string = unsafe { String::from_utf8_unchecked(value_bytes) };
    super::mesh::write_string_out(value_string, out_value_ptr, out_value_len)
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

    /// `net_predicate_to_where_header` emits the canonical
    /// `cyberdeck-where` header name + a JSON-encoded
    /// `PredicateWire` value. Round-trip the value through
    /// `serde_json` and assert it decodes to the same predicate.
    #[test]
    fn to_where_header_emits_canonical_name_and_round_trip_value() {
        use std::ffi::CStr;

        let pred = CString::new(
            r#"{"nodes":[
                {"kind":"exists","key":{"axis":"hardware","key":"gpu"}}
            ],"root_idx":0}"#,
        )
        .unwrap();

        let mut out_name: *mut c_char = std::ptr::null_mut();
        let mut name_len: usize = 0;
        let mut out_value: *mut c_char = std::ptr::null_mut();
        let mut value_len: usize = 0;

        let rc = net_predicate_to_where_header(
            pred.as_ptr(),
            &mut out_name,
            &mut name_len,
            &mut out_value,
            &mut value_len,
        );
        assert_eq!(rc, 0);

        // Header name == "cyberdeck-where".
        let name = unsafe { CStr::from_ptr(out_name) }
            .to_str()
            .unwrap()
            .to_string();
        assert_eq!(name, "cyberdeck-where");
        assert_eq!(name_len, "cyberdeck-where".len());

        // Header value parses as PredicateWire and round-trips
        // back to the same predicate.
        let value = unsafe { CStr::from_ptr(out_value) }
            .to_str()
            .unwrap()
            .to_string();
        assert_eq!(value_len, value.len());
        let parsed: PredicateWire = serde_json::from_str(&value).unwrap();
        let original: PredicateWire = serde_json::from_str(
            r#"{"nodes":[{"kind":"exists","key":{"axis":"hardware","key":"gpu"}}],"root_idx":0}"#,
        )
        .unwrap();
        assert_eq!(parsed.nodes.len(), original.nodes.len());
        assert_eq!(parsed.root_idx, original.root_idx);

        // Free.
        unsafe {
            let _ = CString::from_raw(out_name);
            let _ = CString::from_raw(out_value);
        }
    }

    /// `to_where_header` rejects malformed predicate JSON via
    /// `InvalidJson`.
    #[test]
    fn to_where_header_rejects_malformed_predicate() {
        let pred = CString::new(r#"{"nodes":[],not-json"#).unwrap();
        let mut out_name: *mut c_char = std::ptr::null_mut();
        let mut name_len: usize = 0;
        let mut out_value: *mut c_char = std::ptr::null_mut();
        let mut value_len: usize = 0;

        let rc = net_predicate_to_where_header(
            pred.as_ptr(),
            &mut out_name,
            &mut name_len,
            &mut out_value,
            &mut value_len,
        );
        assert!(rc < 0);
        // Out-pointers should remain NULL since we returned early.
        assert!(out_name.is_null());
        assert!(out_value.is_null());
    }

    /// NULL inputs return `NullPointer`.
    #[test]
    fn to_where_header_null_inputs_return_null_pointer() {
        let pred = CString::new(r#"{"nodes":[],"root_idx":0}"#).unwrap();
        let mut out_name: *mut c_char = std::ptr::null_mut();
        let mut name_len: usize = 0;
        let mut out_value: *mut c_char = std::ptr::null_mut();
        let mut value_len: usize = 0;

        // predicate NULL
        let rc = net_predicate_to_where_header(
            std::ptr::null(),
            &mut out_name,
            &mut name_len,
            &mut out_value,
            &mut value_len,
        );
        assert!(rc < 0);

        // out_name NULL
        let rc = net_predicate_to_where_header(
            pred.as_ptr(),
            std::ptr::null_mut(),
            &mut name_len,
            &mut out_value,
            &mut value_len,
        );
        assert!(rc < 0);
    }
}
