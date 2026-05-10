//! CI guard: drift between `docs/CAPABILITIES_SCHEMA.md` and the
//! substrate's `behavior::schema::AXIS_SCHEMA` const.
//!
//! The doc is the canonical source of truth. The substrate const +
//! per-binding mirrors (`sdk-ts/src/capability-schema.ts`,
//! `sdk-py/src/net_sdk/capability_schema.py`,
//! `bindings/go/net/capability_schema.go`) are hand-maintained
//! mirrors. This test parses the markdown table rows under the
//! `## hardware axis` and `## software axis` sections, normalizes
//! both sides to a `(axis, canonical_key)` set, and asserts the two
//! sets are equal — so adding a key on one side without updating
//! the other fails CI.
//!
//! Phase 9a finish of `CAPABILITY_SYSTEM_SDK_PLAN.md`. The
//! `devices` and `dataforts` axes are reserved-empty in the
//! substrate today; the doc describes their future shape but the
//! substrate enumerates none — those sections are skipped by the
//! guard until the substrate adds entries, at which point this
//! test catches the drift loudly.
//!
//! Run: `cargo test --features net --test capability_schema_doc_guard`.

#![cfg(feature = "net")]

use std::collections::HashSet;

use net::adapter::net::behavior::{AxisEntry, KeyShapeKind, AXIS_SCHEMA};

fn read_schema_doc() -> String {
    let path = "docs/CAPABILITIES_SCHEMA.md";
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// Extract every `axis.key` mention from a markdown table row of the
/// form `| `axis.key` | type | ... |`. Handles the doc's placeholder
/// syntax: `software.model.<i>.id` becomes the canonical key
/// `model.<i>.id`; `software.runtime.<name>` becomes `runtime.<name>`.
fn parse_doc_keys(doc: &str) -> HashSet<(String, String)> {
    let mut keys = HashSet::new();
    let mut current_axis: Option<&str> = None;

    for line in doc.lines() {
        // Detect axis section headers like `## \`hardware\` axis`.
        if let Some(rest) = line.strip_prefix("## `") {
            if let Some(end) = rest.find('`') {
                let candidate = &rest[..end];
                current_axis = match candidate {
                    "hardware" => Some("hardware"),
                    "software" => Some("software"),
                    _ => None, // devices / dataforts skipped — see module docs.
                };
            }
            continue;
        }
        let Some(axis) = current_axis else { continue };

        // Match table rows: `| \`<axis>.<key>\` | ...`.
        let trimmed = line.trim_start();
        if !trimmed.starts_with("| `") {
            continue;
        }
        // Skip the header divider and column-header rows.
        let Some(first_close) = trimmed[3..].find('`') else {
            continue;
        };
        let key_with_axis = &trimmed[3..3 + first_close];
        let prefix = format!("{axis}.");
        let Some(key) = key_with_axis.strip_prefix(&prefix) else {
            continue;
        };
        keys.insert((axis.to_string(), key.to_string()));
    }
    keys
}

/// Project the substrate's `AxisEntry` onto the same canonical key
/// shape the doc uses: fixed keys become `<key>`; KeyedMap shapes
/// become `<prefix><name>`; IndexedCollection shapes become
/// `<prefix><i>.<sub_key>` for every sub-key.
fn substrate_axis_keys(axis: &str, entry: &AxisEntry) -> HashSet<(String, String)> {
    let mut out = HashSet::new();
    for k in entry.keys {
        out.insert((axis.to_string(), k.key.to_string()));
    }
    for shape in entry.shapes {
        match shape.kind {
            KeyShapeKind::KeyedMap { .. } => {
                // `runtime.` → `runtime.<name>`
                out.insert((axis.to_string(), format!("{}<name>", shape.prefix)));
            }
            KeyShapeKind::IndexedCollection => {
                for sub in shape.sub_keys {
                    // `model.` + `id` → `model.<i>.id`
                    out.insert((axis.to_string(), format!("{}<i>.{}", shape.prefix, sub.key)));
                }
            }
        }
    }
    out
}

#[test]
fn capability_schema_doc_matches_substrate_axis_schema() {
    let doc = read_schema_doc();
    let doc_keys = parse_doc_keys(&doc);

    let mut substrate_keys: HashSet<(String, String)> = HashSet::new();
    substrate_keys.extend(substrate_axis_keys("hardware", &AXIS_SCHEMA.hardware));
    substrate_keys.extend(substrate_axis_keys("software", &AXIS_SCHEMA.software));

    let only_in_doc: Vec<_> = doc_keys.difference(&substrate_keys).cloned().collect();
    let only_in_substrate: Vec<_> = substrate_keys.difference(&doc_keys).cloned().collect();

    if !only_in_doc.is_empty() || !only_in_substrate.is_empty() {
        let mut only_in_doc = only_in_doc;
        only_in_doc.sort();
        let mut only_in_substrate = only_in_substrate;
        only_in_substrate.sort();
        panic!(
            "CAPABILITIES_SCHEMA.md drift detected.\n\
             \n\
             Keys in doc but not in `behavior::schema::AXIS_SCHEMA`:\n  {only_in_doc:#?}\n\
             \n\
             Keys in `behavior::schema::AXIS_SCHEMA` but not in doc:\n  {only_in_substrate:#?}\n\
             \n\
             Resolution: update either side to match. The doc \
             (`net/crates/net/docs/CAPABILITIES_SCHEMA.md`) is the \
             canonical source of truth; the substrate const \
             (`net/crates/net/src/adapter/net/behavior/schema.rs`) is \
             the hand-maintained mirror."
        );
    }
}

#[test]
fn doc_parser_handles_known_hardware_keys() {
    // Self-test: pin the parser behavior against known doc rows so
    // a refactor of the parser surfaces clearly.
    let doc = "## `hardware` axis\n\n\
               | Key | Type | Range | Notes |\n\
               |---|---|---|---|\n\
               | `hardware.cpu_cores` | `number` (u16) | `1..=u16::MAX` | x |\n\
               | `hardware.gpu` | `presence` | — | y |\n\
               ";
    let keys = parse_doc_keys(doc);
    assert!(keys.contains(&("hardware".into(), "cpu_cores".into())));
    assert!(keys.contains(&("hardware".into(), "gpu".into())));
}

#[test]
fn doc_parser_handles_indexed_and_keyed_software_shapes() {
    let doc = "## `software` axis\n\n\
               | Key | Type | Notes |\n\
               |---|---|---|\n\
               | `software.runtime.<name>` | `keyed<string>` | x |\n\
               | `software.model.<i>.id` | `indexed<string>` | y |\n\
               ";
    let keys = parse_doc_keys(doc);
    assert!(keys.contains(&("software".into(), "runtime.<name>".into())));
    assert!(keys.contains(&("software".into(), "model.<i>.id".into())));
}
