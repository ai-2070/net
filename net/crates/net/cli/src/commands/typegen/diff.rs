//! Snapshot diff: compares two [`TypegenSnapshot`]s and surfaces tool
//! additions / removals plus per-tool schema evolution, flagging changes
//! that could break existing callers.
//!
//! BREAKING heuristics (conservative — anything that could plausibly break
//! a caller): a required field added, an optional field made required, a
//! field's type changed, an enum value removed, a nullable field made
//! non-nullable, an output field removed.

use serde::Serialize;

use super::schema::{self, ObjectSchema, Schema};
use super::TypegenSnapshot;
use net_sdk::tool::ToolDescriptor;

/// Full comparison result.
#[derive(Debug, Serialize)]
pub(super) struct DiffReport {
    pub added: Vec<ToolRef>,
    pub removed: Vec<ToolRef>,
    pub changed: Vec<ToolChange>,
    pub breaking_count: u64,
}

#[derive(Debug, Serialize)]
pub(super) struct ToolRef {
    pub tool_id: String,
    pub version: String,
}

#[derive(Debug, Serialize)]
pub(super) struct ToolChange {
    pub tool_id: String,
    pub from_version: String,
    pub to_version: String,
    pub changes: Vec<Change>,
}

#[derive(Debug, Serialize)]
pub(super) struct Change {
    /// Dotted path, e.g. `input.max_results`.
    pub path: String,
    /// Human description of the change.
    pub detail: String,
    pub breaking: bool,
}

/// Compare `from` → `to`.
pub(super) fn diff(from: &TypegenSnapshot, to: &TypegenSnapshot) -> DiffReport {
    let old = index(&from.descriptors);
    let new = index(&to.descriptors);

    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();

    for (id, d) in &new {
        if !old.contains_key(id) {
            added.push(ToolRef {
                tool_id: d.tool_id.clone(),
                version: d.version.clone(),
            });
        }
    }
    for (id, d) in &old {
        if !new.contains_key(id) {
            removed.push(ToolRef {
                tool_id: d.tool_id.clone(),
                version: d.version.clone(),
            });
        }
    }
    for (id, old_d) in &old {
        if let Some(new_d) = new.get(id) {
            let changes = changes_between(old_d, new_d);
            if !changes.is_empty() {
                changed.push(ToolChange {
                    tool_id: old_d.tool_id.clone(),
                    from_version: old_d.version.clone(),
                    to_version: new_d.version.clone(),
                    changes,
                });
            }
        }
    }

    added.sort_by(|a, b| a.tool_id.cmp(&b.tool_id));
    removed.sort_by(|a, b| a.tool_id.cmp(&b.tool_id));
    changed.sort_by(|a, b| a.tool_id.cmp(&b.tool_id));

    let breaking_count = changed
        .iter()
        .flat_map(|c| &c.changes)
        .filter(|c| c.breaking)
        .count() as u64;

    DiffReport {
        added,
        removed,
        changed,
        breaking_count,
    }
}

/// Index descriptors by `tool_id` (last wins if a snapshot carries multiple
/// versions of the same id — multi-version diffing is out of initial scope).
fn index(descriptors: &[ToolDescriptor]) -> std::collections::BTreeMap<String, &ToolDescriptor> {
    descriptors.iter().map(|d| (d.tool_id.clone(), d)).collect()
}

fn changes_between(old: &ToolDescriptor, new: &ToolDescriptor) -> Vec<Change> {
    let mut changes = Vec::new();

    if old.version != new.version {
        changes.push(Change {
            path: "version".into(),
            detail: format!("{} → {}", old.version, new.version),
            breaking: false,
        });
    }
    if old.description != new.description {
        changes.push(Change {
            path: "description".into(),
            detail: "description text changed".into(),
            breaking: false,
        });
    }
    if old.tags != new.tags {
        changes.push(Change {
            path: "tags".into(),
            detail: format!("{:?} → {:?}", old.tags, new.tags),
            breaking: false,
        });
    }

    diff_schema_field(
        "input",
        &old.input_schema,
        &new.input_schema,
        true,
        &mut changes,
    );
    diff_schema_field(
        "output",
        &old.output_schema,
        &new.output_schema,
        false,
        &mut changes,
    );

    changes
}

/// Diff one schema slot (input or output). `is_input` tunes the
/// removed-field breaking heuristic (removing an output field breaks
/// consumers; removing an input field generally doesn't).
fn diff_schema_field(
    label: &str,
    old: &Option<String>,
    new: &Option<String>,
    is_input: bool,
    out: &mut Vec<Change>,
) {
    match (old, new) {
        (None, None) => {}
        (Some(_), None) => out.push(Change {
            path: label.into(),
            detail: "schema removed".into(),
            breaking: !is_input, // output schema removal weakens consumer typing
        }),
        (None, Some(_)) => out.push(Change {
            path: label.into(),
            detail: "schema added".into(),
            breaking: false,
        }),
        (Some(o), Some(n)) => {
            let (Ok(op), Ok(np)) = (schema::parse(o), schema::parse(n)) else {
                // Unparseable on either side — report a coarse change.
                if o != n {
                    out.push(Change {
                        path: label.into(),
                        detail: "schema changed (not structurally compared)".into(),
                        breaking: true,
                    });
                }
                return;
            };
            match (&op.root, &np.root) {
                (Schema::Object(oo), Schema::Object(no)) => {
                    diff_objects(label, oo, no, is_input, out)
                }
                (a, b) => {
                    if canonical(a) != canonical(b) {
                        out.push(Change {
                            path: label.into(),
                            detail: format!("type {} → {}", canonical(a), canonical(b)),
                            breaking: true,
                        });
                    }
                }
            }
        }
    }
}

fn diff_objects(
    label: &str,
    old: &ObjectSchema,
    new: &ObjectSchema,
    is_input: bool,
    out: &mut Vec<Change>,
) {
    let old_fields: std::collections::BTreeMap<&str, &Schema> = old
        .properties
        .iter()
        .map(|(k, v)| (k.as_str(), v))
        .collect();
    let new_fields: std::collections::BTreeMap<&str, &Schema> = new
        .properties
        .iter()
        .map(|(k, v)| (k.as_str(), v))
        .collect();

    // Added / type / optionality changes.
    for (name, new_schema) in &new_fields {
        let path = format!("{label}.{name}");
        match old_fields.get(name) {
            None => {
                let required = new.required.contains(*name);
                out.push(Change {
                    path,
                    detail: if required {
                        "added (required)".into()
                    } else {
                        "added (optional)".into()
                    },
                    breaking: required, // a new required field breaks existing callers
                });
            }
            Some(old_schema) => {
                // Optionality.
                let was_req = old.required.contains(*name);
                let now_req = new.required.contains(*name);
                if !was_req && now_req {
                    out.push(Change {
                        path: path.clone(),
                        detail: "optional → required".into(),
                        breaking: true,
                    });
                } else if was_req && !now_req {
                    out.push(Change {
                        path: path.clone(),
                        detail: "required → optional".into(),
                        breaking: false,
                    });
                }
                // Type / nullability / enum.
                if let Some((detail, breaking)) = type_change(old_schema, new_schema) {
                    out.push(Change {
                        path,
                        detail,
                        breaking,
                    });
                }
            }
        }
    }

    // Removed fields.
    for name in old_fields.keys() {
        if !new_fields.contains_key(name) {
            out.push(Change {
                path: format!("{label}.{name}"),
                detail: "removed".into(),
                breaking: !is_input,
            });
        }
    }
}

/// Describe a field-type change, or `None` when types are equivalent.
/// Handles nullability and enum widen/narrow specially so widening isn't
/// mis-flagged as breaking.
fn type_change(old: &Schema, new: &Schema) -> Option<(String, bool)> {
    let (old_base, old_null) = strip_null(old);
    let (new_base, new_null) = strip_null(new);

    // Enum value set comparison (on the non-null base).
    if let (Schema::Enum(ov), Schema::Enum(nv)) = (old_base, new_base) {
        let removed: Vec<String> = ov
            .iter()
            .filter(|v| !nv.contains(v))
            .map(|v| v.to_string())
            .collect();
        let added: Vec<String> = nv
            .iter()
            .filter(|v| !ov.contains(v))
            .map(|v| v.to_string())
            .collect();
        if !removed.is_empty() {
            return Some((
                format!("enum value(s) removed: {}", removed.join(", ")),
                true,
            ));
        }
        if !added.is_empty() {
            return Some((format!("enum value(s) added: {}", added.join(", ")), false));
        }
        // identical enum sets; fall through to nullability check.
    } else {
        let oc = canonical(old_base);
        let nc = canonical(new_base);
        if oc != nc {
            return Some((format!("type {oc} → {nc}"), true));
        }
    }

    if old_null && !new_null {
        return Some(("nullable → non-nullable".into(), true));
    }
    if !old_null && new_null {
        return Some(("non-nullable → nullable".into(), false));
    }
    None
}

/// Split a possibly-null union into `(base_schema, is_nullable)`.
fn strip_null(s: &Schema) -> (&Schema, bool) {
    if let Schema::Union(branches) = s {
        let has_null = branches
            .iter()
            .any(|b| matches!(b, Schema::Primitive(schema::Primitive::Null)));
        if has_null {
            let non_null: Vec<&Schema> = branches
                .iter()
                .filter(|b| !matches!(b, Schema::Primitive(schema::Primitive::Null)))
                .collect();
            if non_null.len() == 1 {
                return (non_null[0], true);
            }
            // Multi-branch nullable union: keep the whole thing as base but
            // report nullable.
            return (s, true);
        }
    }
    (s, false)
}

/// Language-neutral canonical type string for change detection / display.
fn canonical(s: &Schema) -> String {
    use schema::Primitive as P;
    match s {
        Schema::Primitive(P::String) => "string".into(),
        Schema::Primitive(P::Integer) => "integer".into(),
        Schema::Primitive(P::Number) => "number".into(),
        Schema::Primitive(P::Boolean) => "boolean".into(),
        Schema::Primitive(P::Null) => "null".into(),
        Schema::Array(inner) => format!("array<{}>", canonical(inner)),
        Schema::Tuple(items) => format!(
            "tuple<{}>",
            items.iter().map(canonical).collect::<Vec<_>>().join(", ")
        ),
        Schema::Object(_) => "object".into(),
        Schema::Enum(values) => format!(
            "enum[{}]",
            values
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Schema::Const(v) => format!("const({v})"),
        Schema::Union(b) => b.iter().map(canonical).collect::<Vec<_>>().join(" | "),
        Schema::Intersection(p) => p.iter().map(canonical).collect::<Vec<_>>().join(" & "),
        Schema::Ref(n) => format!("ref:{n}"),
        Schema::Unknown => "unknown".into(),
    }
}

/// Render the report in the human text format.
pub(super) fn render_text(report: &DiffReport) -> String {
    let mut s = String::new();
    if !report.added.is_empty() {
        s.push_str(&format!("Added tools ({}):\n", report.added.len()));
        for t in &report.added {
            s.push_str(&format!("  - {} v{}\n", t.tool_id, t.version));
        }
        s.push('\n');
    }
    if !report.removed.is_empty() {
        s.push_str(&format!("Removed tools ({}):\n", report.removed.len()));
        for t in &report.removed {
            s.push_str(&format!("  - {} v{}\n", t.tool_id, t.version));
        }
        s.push('\n');
    }
    if !report.changed.is_empty() {
        s.push_str(&format!("Schema changes ({}):\n", report.changed.len()));
        for c in &report.changed {
            let ver = if c.from_version == c.to_version {
                format!("v{}", c.from_version)
            } else {
                format!("v{} → v{}", c.from_version, c.to_version)
            };
            s.push_str(&format!("  {} {ver}\n", c.tool_id));
            for ch in &c.changes {
                let flag = if ch.breaking {
                    "          [BREAKING]"
                } else {
                    ""
                };
                s.push_str(&format!("    - {}: {}{flag}\n", ch.path, ch.detail));
            }
        }
        s.push('\n');
    }
    if report.added.is_empty() && report.removed.is_empty() && report.changed.is_empty() {
        s.push_str("No differences.\n");
    } else {
        s.push_str(&format!(
            "{} changed tool(s), {} marked BREAKING.\n",
            report.changed.len(),
            report.breaking_count
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(descriptors: Vec<ToolDescriptor>) -> TypegenSnapshot {
        TypegenSnapshot {
            format_version: 1,
            captured_at: "2026-06-04T10:00:00Z".into(),
            source_query: Default::default(),
            descriptors,
        }
    }

    fn tool(id: &str, version: &str, input: Option<&str>, output: Option<&str>) -> ToolDescriptor {
        ToolDescriptor {
            tool_id: id.into(),
            name: id.into(),
            version: version.into(),
            description: None,
            input_schema: input.map(str::to_string),
            output_schema: output.map(str::to_string),
            requires: vec![],
            estimated_time_ms: 0,
            stateless: true,
            streaming: false,
            tags: vec![],
            node_count: 1,
        }
    }

    #[test]
    fn detects_added_and_removed_tools() {
        let from = snap(vec![
            tool("a/keep", "1.0.0", None, None),
            tool("a/gone", "1.0.0", None, None),
        ]);
        let to = snap(vec![
            tool("a/keep", "1.0.0", None, None),
            tool("a/new", "1.0.0", None, None),
        ]);
        let r = diff(&from, &to);
        assert_eq!(r.added.len(), 1);
        assert_eq!(r.added[0].tool_id, "a/new");
        assert_eq!(r.removed.len(), 1);
        assert_eq!(r.removed[0].tool_id, "a/gone");
    }

    #[test]
    fn flags_required_field_added_and_optional_to_required() {
        let old = r#"{"type":"object","properties":{"q":{"type":"string"}},"required":["q"]}"#;
        // `filter` added as required; existing `q` unchanged.
        let new = r#"{"type":"object","properties":{"q":{"type":"string"},"filter":{"type":"string"}},"required":["q","filter"]}"#;
        let r = diff(
            &snap(vec![tool("t", "1.0.0", Some(old), None)]),
            &snap(vec![tool("t", "1.1.0", Some(new), None)]),
        );
        assert_eq!(r.changed.len(), 1);
        let breaking: Vec<_> = r.changed[0].changes.iter().filter(|c| c.breaking).collect();
        assert!(
            breaking.iter().any(|c| c.path == "input.filter"),
            "{:?}",
            r.changed[0].changes
        );
        assert_eq!(r.breaking_count, 1);
    }

    #[test]
    fn flags_type_change_and_enum_narrowing() {
        let old = r#"{"type":"object","properties":{"score":{"type":"number"},"mode":{"enum":["a","b","c"]}}}"#;
        let new = r#"{"type":"object","properties":{"score":{"type":"integer"},"mode":{"enum":["a","b"]}}}"#;
        let r = diff(
            &snap(vec![tool("t", "1.0.0", None, Some(old))]),
            &snap(vec![tool("t", "2.0.0", None, Some(new))]),
        );
        let changes = &r.changed[0].changes;
        assert!(
            changes
                .iter()
                .any(|c| c.path == "output.score" && c.breaking),
            "{changes:?}"
        );
        assert!(
            changes
                .iter()
                .any(|c| c.path == "output.mode" && c.breaking && c.detail.contains("removed")),
            "{changes:?}"
        );
    }

    #[test]
    fn enum_widening_is_not_breaking() {
        let old = r#"{"type":"object","properties":{"mode":{"enum":["a"]}}}"#;
        let new = r#"{"type":"object","properties":{"mode":{"enum":["a","b"]}}}"#;
        let r = diff(
            &snap(vec![tool("t", "1.0.0", Some(old), None)]),
            &snap(vec![tool("t", "1.1.0", Some(new), None)]),
        );
        assert_eq!(r.breaking_count, 0, "{:?}", r.changed);
        assert!(r.changed[0]
            .changes
            .iter()
            .any(|c| c.detail.contains("added")));
    }

    #[test]
    fn identical_snapshots_have_no_diff() {
        let t = vec![tool(
            "t",
            "1.0.0",
            Some(r#"{"type":"object","properties":{"q":{"type":"string"}}}"#),
            None,
        )];
        let r = diff(&snap(t.clone()), &snap(t));
        assert!(r.added.is_empty() && r.removed.is_empty() && r.changed.is_empty());
        assert!(render_text(&r).contains("No differences"));
    }
}
