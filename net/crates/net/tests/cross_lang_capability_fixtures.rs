//! Cross-binding capability fixtures — Rust reference test.
//!
//! Loads `tests/cross_lang_capability/predicate_nrpc_envelope.json`
//! and `tests/cross_lang_capability/capability_set_diff.json` and
//! asserts the Rust implementation matches what the fixtures pin.
//! The same fixtures drive future Node / Python / Go binding compat
//! tests; failure of either fixture against any binding signals a
//! cross-binding wire-format drift.
//!
//! Phase 5.B + 1 of `docs/plans/CAPABILITY_ENHANCEMENTS_PLAN.md`.
//! Surfaced through Phase 9 of `docs/plans/CAPABILITY_SYSTEM_SDK_PLAN.md`.
//!
//! Run: `cargo test --features net --test cross_lang_capability_fixtures`

#![cfg(feature = "net")]

use std::collections::BTreeMap;

use net::adapter::net::behavior::fold::{capability_bridge, CapabilityFold, Fold};
use net::adapter::net::behavior::{
    global_placement_filter_registry, validate_capabilities, Artifact, CapabilityAnnouncement,
    CapabilitySet, ClauseTrace, EvalContext, MetadataChange, PlacementFilter, PlacementNodeId,
    Predicate, PredicateDebugReport, PredicateWire, SchemaError, ScopeLabel, StandardPlacement,
    Tag, ValidationWarning, ValueType, RPC_WHERE_HEADER,
};
use net::adapter::net::identity::EntityId;
use serde_json::Value;
use std::sync::Arc;

fn read_fixture(name: &str) -> String {
    let path = format!("tests/cross_lang_capability/{name}");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

// =============================================================================
// predicate_nrpc_envelope.json — round-trip pins.
//
// For each case: deserialize the `wire` JSON into a PredicateWire,
// convert to Predicate, convert back to PredicateWire, re-serialize
// to JSON, assert byte-equal to the fixture's wire JSON.
//
// This pins:
//   - The PredicateWire encoder / decoder are self-consistent.
//   - The wire JSON shape is exactly what the substrate emits;
//     bindings' encoders must produce the same shape, decoders
//     must accept it.
// =============================================================================

#[test]
fn predicate_nrpc_envelope_fixture_round_trips() {
    let raw = read_fixture("predicate_nrpc_envelope.json");
    let v: Value = serde_json::from_str(&raw).expect("parse fixture");
    assert_eq!(
        v["header_name"], RPC_WHERE_HEADER,
        "fixture header_name diverged from RPC_WHERE_HEADER constant"
    );

    let cases = v["cases"].as_array().expect("cases is array");
    assert!(
        !cases.is_empty(),
        "fixture has zero cases — useless as a contract"
    );

    let mut covered_kinds = std::collections::HashSet::<String>::new();

    for (i, case) in cases.iter().enumerate() {
        let name = case["name"].as_str().unwrap_or("<unnamed>");
        let wire_json = &case["wire"];

        // Deserialize into PredicateWire (the structural form).
        let wire: PredicateWire = serde_json::from_value(wire_json.clone()).unwrap_or_else(|e| {
            panic!("case[{i}] {name}: deserialize wire: {e}\nfixture wire: {wire_json:#}",)
        });

        // Convert to Predicate AST. Catches structural integrity
        // bugs (cycles, OOB indices) — the fixture must be a
        // legal post-order tree.
        let pred = wire
            .clone()
            .into_predicate()
            .unwrap_or_else(|e| panic!("case[{i}] {name}: into_predicate: {e}"));

        // Round-trip: AST → wire → JSON → assert matches fixture.
        let regenerated_wire = pred.to_wire();
        let regenerated_json =
            serde_json::to_value(&regenerated_wire).expect("serialize regenerated wire");

        assert_eq!(
            regenerated_json, *wire_json,
            "case[{i}] {name}: round-trip wire diverged from fixture\n  \
             fixture:    {wire_json:#}\n  regenerated: {regenerated_json:#}",
        );

        // Track coverage: every PredicateNodeWire variant (`kind`)
        // we touch. Asserted at the end so a future variant addition
        // without a fixture entry surfaces.
        for node in wire.nodes.iter() {
            let kind = serde_json::to_value(node)
                .ok()
                .and_then(|v| v.get("kind").and_then(|k| k.as_str()).map(String::from));
            if let Some(k) = kind {
                covered_kinds.insert(k);
            }
        }
    }

    let expected_kinds: std::collections::HashSet<String> = [
        "exists",
        "equals",
        "numeric_at_least",
        "numeric_at_most",
        "numeric_in_range",
        "semver_at_least",
        "semver_at_most",
        "semver_compatible",
        "string_prefix",
        "string_matches",
        "metadata_exists",
        "metadata_equals",
        "metadata_matches",
        "metadata_numeric_at_least",
        "and",
        "or",
        "not",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let missing: Vec<&String> = expected_kinds.difference(&covered_kinds).collect();
    assert!(
        missing.is_empty(),
        "fixture missing coverage for variants: {missing:?}"
    );
}

#[test]
fn predicate_nrpc_envelope_evaluation_smoke_check() {
    // Pin: every fixture entry decodes into a *usable* Predicate
    // — `evaluate()` must return without panicking against an
    // empty context. Adversarial wire payloads that decode but
    // panic at evaluate-time would be a soundness regression.
    let raw = read_fixture("predicate_nrpc_envelope.json");
    let v: Value = serde_json::from_str(&raw).expect("parse fixture");
    let cases = v["cases"].as_array().unwrap();

    let empty_meta = BTreeMap::<String, String>::new();
    let empty_tags: Vec<net::adapter::net::behavior::Tag> = Vec::new();
    let ctx = net::adapter::net::behavior::EvalContext::new(&empty_tags, &empty_meta);

    for case in cases {
        let name = case["name"].as_str().unwrap_or("<unnamed>");
        let wire: PredicateWire = serde_json::from_value(case["wire"].clone())
            .unwrap_or_else(|e| panic!("case {name}: deserialize wire: {e}"));
        let pred = wire.into_predicate().expect("rebuild");
        // Evaluating against the empty context must not panic.
        // Result value is irrelevant for this smoke check.
        let _ = pred.evaluate(&ctx);
        let _ = pred.evaluate_unplanned(&ctx);
    }
}

// =============================================================================
// capability_set_diff.json — diff-output pins.
//
// For each case: parse prev + curr CapabilitySets from their JSON
// wire form; compute curr.diff(prev); normalize the output (sort
// tag arrays by wire form; sort metadata changes by key);
// assert it matches the case's expected_* fields.
// =============================================================================

#[test]
fn capability_set_diff_fixture_matches_rust_implementation() {
    let raw = read_fixture("capability_set_diff.json");
    let v: Value = serde_json::from_str(&raw).expect("parse fixture");
    let cases = v["cases"].as_array().expect("cases is array");
    assert!(!cases.is_empty(), "fixture has zero cases");

    for (i, case) in cases.iter().enumerate() {
        let name = case["name"].as_str().unwrap_or("<unnamed>");

        let prev: CapabilitySet =
            serde_json::from_value(case["prev"].clone()).unwrap_or_else(|e| {
                panic!(
                    "case[{i}] {name}: parse prev: {e}\nprev: {:#}",
                    case["prev"]
                )
            });
        let curr: CapabilitySet =
            serde_json::from_value(case["curr"].clone()).unwrap_or_else(|e| {
                panic!(
                    "case[{i}] {name}: parse curr: {e}\ncurr: {:#}",
                    case["curr"]
                )
            });

        let diff = curr.diff(&prev);

        // Normalize added_tags / removed_tags to sorted-by-wire-form arrays.
        let mut added: Vec<String> = diff.added_tags.iter().map(|t| t.to_string()).collect();
        added.sort();
        let mut removed: Vec<String> = diff.removed_tags.iter().map(|t| t.to_string()).collect();
        removed.sort();

        let expected_added: Vec<String> = case["expected_added_tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap().to_string())
            .collect();
        let expected_removed: Vec<String> = case["expected_removed_tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap().to_string())
            .collect();

        assert_eq!(
            added, expected_added,
            "case[{i}] {name}: added_tags mismatch",
        );
        assert_eq!(
            removed, expected_removed,
            "case[{i}] {name}: removed_tags mismatch",
        );

        // Normalize metadata_changes to a sorted-by-key array of
        // structured shape: { kind, key, ... }. The
        // `expected_metadata_changes` fixture is already in this shape.
        let mut actual_changes: Vec<Value> = diff
            .changed_metadata
            .iter()
            .map(|c| match c {
                MetadataChange::Added { key, value } => serde_json::json!({
                    "kind": "added",
                    "key": key,
                    "value": value,
                }),
                MetadataChange::Removed { key, prev_value } => serde_json::json!({
                    "kind": "removed",
                    "key": key,
                    "prev_value": prev_value,
                }),
                MetadataChange::Updated {
                    key,
                    prev_value,
                    new_value,
                } => serde_json::json!({
                    "kind": "updated",
                    "key": key,
                    "prev_value": prev_value,
                    "new_value": new_value,
                }),
            })
            .collect();
        // The substrate emits in BTreeMap-iteration order (sorted by
        // key). Our expected_metadata_changes is in the same order.
        // Belt-and-suspenders: sort both by key before comparing.
        actual_changes.sort_by(|a, b| a["key"].as_str().cmp(&b["key"].as_str()));

        let mut expected_changes = case["expected_metadata_changes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        expected_changes.sort_by(|a, b| a["key"].as_str().cmp(&b["key"].as_str()));

        assert_eq!(
            actual_changes, expected_changes,
            "case[{i}] {name}: metadata_changes mismatch\n  \
             actual:   {actual_changes:#?}\n  expected: {expected_changes:#?}",
        );
    }
}

#[test]
fn capability_set_diff_fixture_round_trips_caps_through_serde() {
    // Pin: the prev/curr JSON shapes in the fixture are exactly
    // what `CapabilitySet`'s default serde produces — modulo the
    // `tags` array order (HashSet<Tag> iteration is unspecified).
    // Compare with tag arrays sorted on both sides; metadata
    // (BTreeMap) iteration IS deterministic so its JSON object
    // order is stable.
    let raw = read_fixture("capability_set_diff.json");
    let v: Value = serde_json::from_str(&raw).expect("parse fixture");
    let cases = v["cases"].as_array().unwrap();

    fn normalize(json: &Value) -> Value {
        let mut copy = json.clone();
        if let Some(tags) = copy.get_mut("tags").and_then(|t| t.as_array_mut()) {
            tags.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
        }
        copy
    }

    for case in cases {
        let name = case["name"].as_str().unwrap_or("<unnamed>");

        // Round-trip prev: JSON → CapabilitySet → JSON, then
        // sort tags before equality check.
        let prev_json = &case["prev"];
        let prev: CapabilitySet = serde_json::from_value(prev_json.clone())
            .unwrap_or_else(|e| panic!("case {name}: parse prev: {e}"));
        let prev_round_trip = serde_json::to_value(&prev).expect("re-serialize prev");
        assert_eq!(
            normalize(&prev_round_trip),
            normalize(prev_json),
            "case {name}: prev JSON round-trip diverged (after tag-array sort)",
        );

        // Same for curr.
        let curr_json = &case["curr"];
        let curr: CapabilitySet = serde_json::from_value(curr_json.clone())
            .unwrap_or_else(|e| panic!("case {name}: parse curr: {e}"));
        let curr_round_trip = serde_json::to_value(&curr).expect("re-serialize curr");
        assert_eq!(
            normalize(&curr_round_trip),
            normalize(curr_json),
            "case {name}: curr JSON round-trip diverged (after tag-array sort)",
        );
    }
}

// =============================================================================
// predicate_eval.json — evaluation pins.
//
// Per-case input: a `wire` PredicateWire + a `tags` array (wire-format
// tag strings) + a `metadata` object + an `expected` boolean. The
// substrate evaluates `Predicate::evaluate_unplanned(ctx)` against the
// (tags, metadata) context; the result must match `expected`.
//
// This fixture is the ground-truth contract for cross-binding predicate
// evaluators. SDKs that re-implement evaluation in their host language
// (TS / Python / Go) load the same fixture and assert byte-identical
// boolean results — pins agreement on leaf semantics (axis matching,
// semver parsing, numeric coercion) AND composite recursion.
// =============================================================================

#[test]
fn predicate_eval_fixture_matches_substrate() {
    let raw = read_fixture("predicate_eval.json");
    let v: Value = serde_json::from_str(&raw).expect("parse fixture");
    let cases = v["cases"].as_array().expect("cases is array");
    assert!(!cases.is_empty(), "fixture has zero cases");

    for (i, case) in cases.iter().enumerate() {
        let name = case["name"].as_str().unwrap_or("<unnamed>");

        // Decode the predicate from its wire form.
        let wire: PredicateWire = serde_json::from_value(case["wire"].clone())
            .unwrap_or_else(|e| panic!("case[{i}] {name}: deserialize wire: {e}"));
        let pred: Predicate = wire
            .into_predicate()
            .unwrap_or_else(|e| panic!("case[{i}] {name}: into_predicate: {e}"));

        // Build the evaluation context from the fixture's tags + metadata.
        let tag_strings: Vec<String> = case["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap().to_string())
            .collect();
        let tags: Vec<Tag> = tag_strings
            .iter()
            .map(|s| {
                Tag::parse(s).unwrap_or_else(|e| panic!("case[{i}] {name}: parse tag {s:?}: {e}"))
            })
            .collect();

        let metadata: BTreeMap<String, String> = case["metadata"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
            .collect();

        let ctx = EvalContext::new(&tags, &metadata);

        let expected = case["expected"]
            .as_bool()
            .unwrap_or_else(|| panic!("case[{i}] {name}: `expected` not a bool"));

        // `evaluate_unplanned` is the canonical SDK-portable path —
        // declaration-order eval with no planner reordering.
        let got_unplanned = pred.evaluate_unplanned(&ctx);
        assert_eq!(
            got_unplanned, expected,
            "case[{i}] {name}: evaluate_unplanned diverged from expected\n  \
             pred: {pred:?}\n  tags: {tag_strings:?}\n  metadata: {metadata:?}",
        );

        // Sanity: the planned variant must produce the same result.
        // The planner reorders And / Or children by static cost; the
        // boolean answer is invariant to reordering.
        let got_planned = pred.evaluate(&ctx);
        assert_eq!(
            got_planned, expected,
            "case[{i}] {name}: planned evaluate diverged from expected (planner-equivalence regression)",
        );
    }
}

// =============================================================================
// capability_validation.json — Phase 9a contract.
//
// Per case: parse the wire-format `caps`, run `validate_capabilities`,
// project the resulting `ValidationReport` onto the canonical wire
// shape (lowercase `kind` discriminator, axis as lowercase string,
// ValueType as lowercase string), sort each list canonically by
// JSON-string comparison, and assert it matches the fixture's
// `expected_errors` + `expected_warnings`.
//
// Bindings consume the same fixture and assert their own validators
// produce the same canonical output.
// =============================================================================

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
        SchemaError::UnknownAxis { axis_prefix, tag } => serde_json::json!({
            "kind": "unknown_axis",
            "axis_prefix": axis_prefix,
            "tag": tag,
        }),
        SchemaError::TypeMismatch {
            axis,
            key,
            expected,
            actual,
        } => serde_json::json!({
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
        } => serde_json::json!({
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
        ValidationWarning::UnknownKey { axis, key } => serde_json::json!({
            "kind": "unknown_key",
            "axis": axis.as_str(),
            "key": key,
        }),
        ValidationWarning::MetadataOversize {
            soft_cap_bytes,
            actual_bytes,
        } => serde_json::json!({
            "kind": "metadata_oversize",
            "soft_cap_bytes": soft_cap_bytes,
            "actual_bytes": actual_bytes,
        }),
        ValidationWarning::LegacyTag { tag } => serde_json::json!({
            "kind": "legacy_tag",
            "tag": tag,
        }),
        // Metadata-key reservation warnings. Wire shape
        // mirrors `src/ffi/schema.rs`.
        ValidationWarning::MetadataReservedKey { key } => serde_json::json!({
            "kind": "metadata_reserved_key",
            "key": key,
        }),
        ValidationWarning::MetadataReservedPrefix { key, prefix } => serde_json::json!({
            "kind": "metadata_reserved_prefix",
            "key": key,
            "prefix": prefix,
        }),
    }
}

fn canonical_sort(v: &mut [Value]) {
    v.sort_by_key(|x| x.to_string());
}

#[test]
fn capability_validation_fixture_matches_substrate() {
    let raw = read_fixture("capability_validation.json");
    let v: Value = serde_json::from_str(&raw).expect("parse fixture");
    let cases = v["cases"].as_array().expect("cases is array");
    assert!(!cases.is_empty(), "fixture has zero cases");

    for (i, case) in cases.iter().enumerate() {
        let name = case["name"].as_str().unwrap_or("<unnamed>");

        let caps: CapabilitySet = serde_json::from_value(case["caps"].clone())
            .unwrap_or_else(|e| panic!("case[{i}] {name}: parse caps: {e}"));

        let report = validate_capabilities(&caps);

        let mut got_errors: Vec<Value> = report.errors.iter().map(schema_error_to_wire).collect();
        let mut got_warnings: Vec<Value> = report
            .warnings
            .iter()
            .map(validation_warning_to_wire)
            .collect();
        canonical_sort(&mut got_errors);
        canonical_sort(&mut got_warnings);

        let mut expected_errors = case["expected_errors"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut expected_warnings = case["expected_warnings"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        canonical_sort(&mut expected_errors);
        canonical_sort(&mut expected_warnings);

        assert_eq!(
            got_errors, expected_errors,
            "case[{i}] {name}: errors diverged\n  got:      {got_errors:#?}\n  expected: {expected_errors:#?}",
        );
        assert_eq!(
            got_warnings, expected_warnings,
            "case[{i}] {name}: warnings diverged\n  got:      {got_warnings:#?}\n  expected: {expected_warnings:#?}",
        );
    }
}

// =============================================================================
// predicate_trace.json — Phase 9d slice contract.
//
// Per case: decode wire → Predicate, build EvalContext from tags +
// metadata, run `evaluate_with_trace`, project the resulting
// `ClauseTrace` tree onto the canonical wire shape (`{label, result,
// children}`), assert it matches the case's `expected_trace`. Also
// asserts the boolean result matches `expected_result`.
//
// This pins the substrate's `evaluate_with_trace` output as the
// ground truth that bindings re-implement and verify against.
// =============================================================================

fn clause_trace_to_wire(t: &ClauseTrace) -> Value {
    serde_json::json!({
        "label": t.label,
        "result": t.result,
        "children": t.children.iter().map(clause_trace_to_wire).collect::<Vec<_>>(),
    })
}

#[test]
fn predicate_trace_fixture_matches_substrate() {
    let raw = read_fixture("predicate_trace.json");
    let v: Value = serde_json::from_str(&raw).expect("parse fixture");
    let cases = v["cases"].as_array().expect("cases is array");
    assert!(!cases.is_empty(), "fixture has zero cases");

    for (i, case) in cases.iter().enumerate() {
        let name = case["name"].as_str().unwrap_or("<unnamed>");

        let wire: PredicateWire = serde_json::from_value(case["wire"].clone())
            .unwrap_or_else(|e| panic!("case[{i}] {name}: deserialize wire: {e}"));
        let pred: Predicate = wire
            .into_predicate()
            .unwrap_or_else(|e| panic!("case[{i}] {name}: into_predicate: {e}"));

        let tag_strings: Vec<String> = case["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap().to_string())
            .collect();
        let tags: Vec<Tag> = tag_strings
            .iter()
            .map(|s| {
                Tag::parse(s).unwrap_or_else(|e| panic!("case[{i}] {name}: parse tag {s:?}: {e}"))
            })
            .collect();

        let metadata: BTreeMap<String, String> = case["metadata"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
            .collect();

        let ctx = EvalContext::new(&tags, &metadata);

        let (got_result, got_trace) = pred.evaluate_with_trace(&ctx);

        let expected_result = case["expected_result"]
            .as_bool()
            .unwrap_or_else(|| panic!("case[{i}] {name}: `expected_result` not bool"));
        assert_eq!(
            got_result, expected_result,
            "case[{i}] {name}: result diverged",
        );

        let got_wire = clause_trace_to_wire(&got_trace);
        let expected_wire = case["expected_trace"].clone();
        assert_eq!(
            got_wire, expected_wire,
            "case[{i}] {name}: trace diverged\n  got:      {got_wire:#}\n  expected: {expected_wire:#}",
        );
    }
}

// =============================================================================
// predicate_debug_report.json — Phase 9d full contract.
//
// Per case: decode wire → Predicate, build N EvalContexts from the
// fixture's `contexts` array, run `PredicateDebugReport::from_evaluations`,
// project the report onto the canonical wire shape and assert it
// matches `expected_*`.
//
// `clause_stats` is a BTreeMap → array sorted by label. The fixture's
// `expected_clause_stats` is already in that order.
// =============================================================================

#[test]
fn predicate_debug_report_fixture_matches_substrate() {
    let raw = read_fixture("predicate_debug_report.json");
    let v: Value = serde_json::from_str(&raw).expect("parse fixture");
    let cases = v["cases"].as_array().expect("cases is array");
    assert!(!cases.is_empty(), "fixture has zero cases");

    for (i, case) in cases.iter().enumerate() {
        let name = case["name"].as_str().unwrap_or("<unnamed>");

        let wire: PredicateWire = serde_json::from_value(case["wire"].clone())
            .unwrap_or_else(|e| panic!("case[{i}] {name}: deserialize wire: {e}"));
        let pred: Predicate = wire
            .into_predicate()
            .unwrap_or_else(|e| panic!("case[{i}] {name}: into_predicate: {e}"));

        // Collect the corpus into owned (tags, metadata) pairs first
        // so `EvalContext::new` can borrow them. The substrate's
        // `EvalContext` borrows `&[Tag]` and `&BTreeMap`, so each
        // context's owned data must outlive the iteration.
        let owned: Vec<(Vec<Tag>, BTreeMap<String, String>)> = case["contexts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                let tags: Vec<Tag> = c["tags"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|t| Tag::parse(t.as_str().unwrap()).expect("parse tag"))
                    .collect();
                let metadata: BTreeMap<String, String> = c["metadata"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
                    .collect();
                (tags, metadata)
            })
            .collect();

        let report = PredicateDebugReport::from_evaluations(
            &pred,
            owned
                .iter()
                .map(|(tags, meta)| EvalContext::new(tags, meta)),
        );

        let expected_total = case["expected_total_candidates"].as_u64().unwrap() as usize;
        let expected_matched = case["expected_matched"].as_u64().unwrap() as usize;
        assert_eq!(
            report.total_candidates, expected_total,
            "case[{i}] {name}: total_candidates",
        );
        assert_eq!(
            report.matched, expected_matched,
            "case[{i}] {name}: matched",
        );

        // BTreeMap iter() is in label order — same as the fixture's
        // expected_clause_stats array.
        let got_stats: Vec<Value> = report
            .clause_stats
            .values()
            .map(|s| {
                serde_json::json!({
                    "label": s.label,
                    "evaluated": s.evaluated,
                    "matched": s.matched,
                })
            })
            .collect();
        let expected_stats = case["expected_clause_stats"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            got_stats, expected_stats,
            "case[{i}] {name}: clause_stats diverged\n  got:      {got_stats:#?}\n  expected: {expected_stats:#?}",
        );
    }
}

// =============================================================================
// SDK Phase 7 cross-binding compat — wrap a predicate as a custom
// `PlacementFilter` and route it through the full Phase 7 path:
//
//   global_placement_filter_registry().register(id, wrapper, "test")
//   StandardPlacement::new(&fold).with_custom_filter_id(id)
//   placement.placement_score(target, artifact)
//
// `predicate_eval.json` is the same fixture every binding's predicate
// evaluator consumes (Rust SDK + Node + Python + Go). This test pins
// that the custom-filter callback path produces booleans IDENTICAL to
// direct `Predicate::evaluate_unplanned`. Each binding ships an
// equivalent test (replace "test" with the binding label) — divergence
// across bindings shows up as a fixture-driven CI failure on the
// drifting binding.
//
// Together with `predicate_eval_fixture_matches_substrate` (above),
// this proves: every fixture case passes through both the direct
// IR path AND the registered-callback path with byte-identical
// behavior, in every binding.
// =============================================================================

/// Wraps a `Predicate` + `Arc<Fold<CapabilityFold>>` as a
/// `PlacementFilter`. Fixture-driven test impl — production bindings
/// (Node TSFN, Python PyAny, Go cgo) all reach the same place via
/// different mechanics.
struct PredicatePlacementFilter {
    pred: Predicate,
    fold: Arc<Fold<CapabilityFold>>,
}

impl PlacementFilter for PredicatePlacementFilter {
    fn placement_score(&self, target: &PlacementNodeId, _artifact: &Artifact<'_>) -> Option<f32> {
        // Synthesize the candidate's CapabilitySet from the fold's
        // tag set (metadata isn't carried through the fold payload —
        // see `synthesize_capability_set` doc).
        let caps = capability_bridge::synthesize_capability_set(&self.fold, *target);
        let tags: Vec<Tag> = caps.tags.iter().cloned().collect();
        let ctx = EvalContext::new(&tags, &caps.metadata);
        if self.pred.evaluate_unplanned(&ctx) {
            Some(1.0)
        } else {
            None
        }
    }
}

#[test]
fn predicate_eval_fixture_matches_via_placement_filter_callback() {
    let raw = read_fixture("predicate_eval.json");
    let v: Value = serde_json::from_str(&raw).expect("parse fixture");
    let cases = v["cases"].as_array().expect("cases is array");
    assert!(!cases.is_empty(), "fixture has zero cases");

    for (i, case) in cases.iter().enumerate() {
        let name = case["name"].as_str().unwrap_or("<unnamed>");

        // Decode the predicate from its wire form.
        let wire: PredicateWire = serde_json::from_value(case["wire"].clone())
            .unwrap_or_else(|e| panic!("case[{i}] {name}: deserialize wire: {e}"));
        let pred: Predicate = wire
            .into_predicate()
            .unwrap_or_else(|e| panic!("case[{i}] {name}: into_predicate: {e}"));

        // Build the candidate's CapabilitySet from the fixture.
        let tag_strings: Vec<String> = case["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap().to_string())
            .collect();
        let mut caps = CapabilitySet::default();
        for s in &tag_strings {
            caps = caps.add_tag(s.clone());
        }
        for (k, v) in case["metadata"].as_object().unwrap().iter() {
            caps = caps.with_metadata(k, v.as_str().unwrap());
        }

        let expected = case["expected"]
            .as_bool()
            .unwrap_or_else(|| panic!("case[{i}] {name}: `expected` not a bool"));

        // Stage a single candidate node carrying the case's caps.
        let target_node: PlacementNodeId = 0x1234_5678_DEAD_BEEF;
        let fold: Arc<Fold<CapabilityFold>> = Arc::new(
            Fold::<CapabilityFold>::with_sweep_interval(std::time::Duration::ZERO),
        );
        let eid = EntityId::from_bytes([0u8; 32]);
        capability_bridge::apply_legacy_announcement(
            &fold,
            CapabilityAnnouncement::new(target_node, eid, 1, caps.clone()),
            None,
            0,
        )
        .expect("apply legacy announcement in fixture");

        // Register the predicate-backed filter under a fixture-scoped
        // id; binding label `"test"` so concurrent fixture runs don't
        // collide with production bindings' counters.
        let id = format!("pf-fixture-eval-{i}-{name}");
        let registry = global_placement_filter_registry();
        // Defensive cleanup from any prior aborted run.
        let _ = registry.unregister(&id);
        let wrapper: Arc<dyn PlacementFilter> = Arc::new(PredicatePlacementFilter {
            pred,
            fold: fold.clone(),
        });
        assert!(
            registry.register(id.clone(), wrapper, "test"),
            "case[{i}] {name}: registry.register failed",
        );

        // Configure StandardPlacement to consume the registered filter
        // via the full Phase 7 path.
        let placement = StandardPlacement::new(&fold).with_custom_filter_id(&id);

        // Empty Daemon artifact — the predicate evaluates against the
        // candidate's caps, not the artifact's required/optional sets.
        let req = CapabilitySet::default();
        let opt = CapabilitySet::default();
        let artifact = Artifact::Daemon {
            daemon_id: [0u8; 32],
            required: &req,
            optional: &opt,
        };

        let score = placement.placement_score(&target_node, &artifact);

        // Must always cleanup BEFORE asserting so a failure doesn't
        // leak the registration into subsequent cases.
        let kept = score.is_some();
        registry.unregister(&id);

        assert_eq!(
            kept, expected,
            "case[{i}] {name}: custom-filter-callback path diverged from direct evaluation\n  \
             tags: {tag_strings:?}\n  expected: {expected}, score: {score:?}",
        );

        // When kept, score is in (0.0, 1.0]. We don't pin Some(1.0)
        // strictly — the in-tree resource axis (always-on; default
        // config) may produce a sub-1.0 score for tags like
        // `hardware.memory_gb=64`. The boolean outcome (kept vs
        // vetoed) is what the cross-binding fixture pins; absolute
        // score values are an in-tree-axes concern that lives in
        // the placement.rs unit tests.
        if expected {
            let s = score.expect("kept must be Some");
            assert!(
                s > 0.0 && s <= 1.0,
                "case[{i}] {name}: kept candidate score must be in (0.0, 1.0], got {s}",
            );
        }
    }
}

// =============================================================================
// placement_score.json — `StandardPlacement` scoring matrix.
//
// Per case: parse the config / candidate / artifact, build a
// `StandardPlacement` against an index containing the candidate,
// score, and assert the result matches `expected_score` to within
// 1e-6 (or that both are veto / `null`).
//
// Locks the in-tree axis composition matrix so any drift in the
// scope / hard-required / resource-permissive paths trips the
// fixture's assertion. Bindings that ship local
// `StandardPlacement` reimplementations consume the same fixture
// and assert byte-identical results.
//
// Limitations:
//
// - `proximity` axis is excluded — requires a runtime `RttLookup`
//   closure that's not serializable. Cases configure `scope` /
//   resource axes only.
// - `anti_affinity` axis is excluded — requires a runtime
//   `LeadershipStatsLookup` closure. Same reason.
// - `intent` / `colocation` axes — left for follow-up cases as
//   the registry / metadata wiring is non-trivial; the current
//   fixture covers scope + hard-required + default-permissive
//   axes.
// =============================================================================

const PLACEMENT_SCORE_TOLERANCE: f32 = 1e-6;

/// Parse a `"0xN"` hex string into `u64`. Used for the candidate
/// `node_id` field in fixture cases.
fn parse_hex_node_id(raw: &str, ctx: &str) -> u64 {
    let stripped = raw
        .strip_prefix("0x")
        .or_else(|| raw.strip_prefix("0X"))
        .unwrap_or(raw);
    u64::from_str_radix(stripped, 16)
        .unwrap_or_else(|e| panic!("{ctx}: parse node_id {raw:?} as u64 hex: {e}"))
}

/// Convert a fixture-format `CapabilitySet` (`{tags: [...],
/// metadata: {...}}`) into the substrate type. Used for both the
/// candidate's caps and the artifact's required / optional sets.
///
/// Tags are inserted via `Tag::parse` (privileged) rather than
/// `add_tag` / `parse_user` so reserved-prefix tags like
/// `scope:tenant:foo` survive — fixture cases for the scope axis
/// require this; the wire format already permits reserved tags
/// even when the user-builder API rejects them.
fn caps_from_fixture_obj(v: &Value, ctx: &str) -> CapabilitySet {
    let mut caps = CapabilitySet::default();
    if let Some(tags) = v.get("tags").and_then(|t| t.as_array()) {
        for tag in tags {
            let s = tag
                .as_str()
                .unwrap_or_else(|| panic!("{ctx}: tag is not a string: {tag}"));
            let parsed = Tag::parse(s).unwrap_or_else(|e| panic!("{ctx}: parse tag {s:?}: {e}"));
            caps.tags.insert(parsed);
        }
    }
    if let Some(meta) = v.get("metadata").and_then(|m| m.as_object()) {
        for (k, val) in meta {
            let s = val
                .as_str()
                .unwrap_or_else(|| panic!("{ctx}: metadata value for {k:?} is not a string"));
            caps = caps.with_metadata(k.clone(), s.to_string());
        }
    }
    caps
}

#[test]
fn placement_score_fixture_matches_substrate() {
    let raw = read_fixture("placement_score.json");
    let v: Value = serde_json::from_str(&raw).expect("parse placement_score.json");
    let cases = v["cases"]
        .as_array()
        .expect("placement_score.json: cases is array");
    assert!(
        !cases.is_empty(),
        "placement_score.json fixture has zero cases — useless as a contract",
    );

    for (i, case) in cases.iter().enumerate() {
        let name = case["name"].as_str().unwrap_or("<unnamed>");

        // Build the candidate's caps and stage them in a fresh
        // CapabilityIndex (single node per case keeps the
        // index minimal and the scoring deterministic).
        let cand = &case["candidate"];
        let node_id = parse_hex_node_id(
            cand["node_id"]
                .as_str()
                .unwrap_or_else(|| panic!("case[{i}] {name}: candidate.node_id missing")),
            &format!("case[{i}] {name}"),
        );
        let cand_caps = caps_from_fixture_obj(cand, &format!("case[{i}] {name} candidate"));

        let fold: Arc<Fold<CapabilityFold>> = Arc::new(
            Fold::<CapabilityFold>::with_sweep_interval(std::time::Duration::ZERO),
        );
        let eid = EntityId::from_bytes([0u8; 32]);
        capability_bridge::apply_legacy_announcement(
            &fold,
            CapabilityAnnouncement::new(node_id, eid, 1, cand_caps),
            None,
            0,
        )
        .expect("apply legacy announcement in fixture");

        // Build the StandardPlacement from the case's config
        // subset. Only the fields that exist in the case JSON
        // are applied; missing fields stay at the default (axis
        // disabled / identity).
        let mut placement = StandardPlacement::new(&fold);
        let cfg = &case["config"];
        if let Some(scope_filter) = cfg.get("scope_filter").and_then(|s| s.as_array()) {
            let labels: Vec<ScopeLabel> = scope_filter
                .iter()
                .map(|l| {
                    let s = l.as_str().unwrap_or_else(|| {
                        panic!("case[{i}] {name}: scope_filter entry not a string")
                    });
                    ScopeLabel::new(s.to_string())
                })
                .collect();
            placement = placement.with_scope_filter(labels);
        }

        // Build the artifact (currently only `daemon` kind is
        // supported in the fixture; `chain` / `replica` are
        // reserved for future cases).
        let art = &case["artifact"];
        let art_kind = art["kind"]
            .as_str()
            .unwrap_or_else(|| panic!("case[{i}] {name}: artifact.kind missing"));
        let req_caps = caps_from_fixture_obj(
            &art["required"],
            &format!("case[{i}] {name} artifact.required"),
        );
        let opt_caps = caps_from_fixture_obj(
            &art["optional"],
            &format!("case[{i}] {name} artifact.optional"),
        );
        let artifact = match art_kind {
            "daemon" => Artifact::Daemon {
                daemon_id: [0u8; 32],
                required: &req_caps,
                optional: &opt_caps,
            },
            other => panic!(
                "case[{i}] {name}: unsupported artifact.kind {other:?}; only `daemon` is wired today",
            ),
        };

        let got = placement.placement_score(&node_id, &artifact);

        // `expected_score: null` → veto. Numeric expected_score →
        // score must be Some(_) within tolerance.
        let expected_raw = &case["expected_score"];
        if expected_raw.is_null() {
            assert!(
                got.is_none(),
                "case[{i}] {name}: expected veto (null), got {got:?}",
            );
        } else {
            let expected = expected_raw.as_f64().unwrap_or_else(|| {
                panic!(
                    "case[{i}] {name}: expected_score must be null or a number, got {expected_raw}"
                )
            }) as f32;
            let actual = got.unwrap_or_else(|| {
                panic!("case[{i}] {name}: expected score {expected}, got veto (None)",)
            });
            assert!(
                (actual - expected).abs() <= PLACEMENT_SCORE_TOLERANCE,
                "case[{i}] {name}: score mismatch — expected {expected}, got {actual} (tolerance {PLACEMENT_SCORE_TOLERANCE})",
            );
        }
    }
}
