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

use net::adapter::net::behavior::{
    CapabilitySet, EvalContext, MetadataChange, Predicate, PredicateWire, RPC_WHERE_HEADER, Tag,
};
use serde_json::Value;

fn read_fixture(name: &str) -> String {
    let path = format!("tests/cross_lang_capability/{name}");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {path}: {e}"))
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

    let cases = v["cases"]
        .as_array()
        .expect("cases is array");
    assert!(
        !cases.is_empty(),
        "fixture has zero cases — useless as a contract"
    );

    let mut covered_kinds = std::collections::HashSet::<String>::new();

    for (i, case) in cases.iter().enumerate() {
        let name = case["name"].as_str().unwrap_or("<unnamed>");
        let wire_json = &case["wire"];

        // Deserialize into PredicateWire (the structural form).
        let wire: PredicateWire =
            serde_json::from_value(wire_json.clone()).unwrap_or_else(|e| {
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
                panic!("case[{i}] {name}: parse prev: {e}\nprev: {:#}", case["prev"])
            });
        let curr: CapabilitySet =
            serde_json::from_value(case["curr"].clone()).unwrap_or_else(|e| {
                panic!("case[{i}] {name}: parse curr: {e}\ncurr: {:#}", case["curr"])
            });

        let diff = curr.diff(&prev);

        // Normalize added_tags / removed_tags to sorted-by-wire-form arrays.
        let mut added: Vec<String> = diff.added_tags.iter().map(|t| t.to_string()).collect();
        added.sort();
        let mut removed: Vec<String> =
            diff.removed_tags.iter().map(|t| t.to_string()).collect();
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
                Tag::parse(s).unwrap_or_else(|e| {
                    panic!("case[{i}] {name}: parse tag {s:?}: {e}")
                })
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
