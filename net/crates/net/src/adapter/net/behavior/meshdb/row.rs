//! Row-schema decoding — opens the `ResultRow.payload` byte
//! soup to the operators that need to look inside (Filter
//! predicates, numeric aggregates, payload-keyed joins).
//!
//! # Decoding contract
//!
//! Phase E-2 ships a single decoder: **JSON objects**. A
//! payload that parses cleanly as `serde_json::Value::Object`
//! exposes its leaf scalars (strings, numbers, booleans) under
//! flattened dotted paths. Anything else is opaque — predicates
//! that reference payload fields against a non-JSON or
//! non-object payload simply don't match (the predicate
//! evaluates as if the field were absent).
//!
//! This is deliberately minimal. Phase E-2 picks JSON because
//! it's the lingua-franca for event payloads in the named
//! consumer workloads (Hermes telemetry, Deck metrics) and it
//! avoids inventing a schema layer ahead of an actual user.
//! Richer decoders (CBOR, protobuf, postcard with a registered
//! schema) can be added later as additional decoder variants
//! without breaking the existing JSON surface.
//!
//! # Synthetic tag view
//!
//! The Filter executor wraps every [`ResultRow`] in a
//! synthetic [`crate::adapter::net::behavior::CapabilitySet`]-
//! shaped view so it can reuse the existing
//! [`crate::adapter::net::behavior::predicate::PredicateWire`]
//! evaluation machinery. Synthetic tags follow the convention:
//!
//! - `dataforts.origin = <16-hex>` (always present)
//! - `dataforts.seq = <decimal>` (always present)
//! - `dataforts.<flat-json-path> = <scalar-as-string>` for
//!   every leaf scalar in a JSON-object payload. Nested objects
//!   flatten with `.` separators (e.g. `dataforts.a.b.c`).
//!   Arrays flatten with `.[i]` (Phase E-3 territory; not yet
//!   surfaced).

use std::collections::BTreeMap;

use super::query::ResultRow;
use crate::adapter::net::behavior::tag::{AxisSeparator, Tag, TaxonomyAxis};

/// Build the synthetic per-row view consumed by the Filter
/// executor: a `Vec<Tag>` of synthetic axis-value tags plus a
/// `BTreeMap<String, String>` mirroring the same data on the
/// metadata side.
///
/// Mirroring is intentional: predicate clauses that key off
/// metadata (`MetadataEquals`, numeric-on-metadata) and those
/// that key off tags both succeed against the same per-row
/// fact.
pub fn synthetic_row_view(row: &ResultRow) -> (Vec<Tag>, BTreeMap<String, String>) {
    let mut tags: Vec<Tag> = Vec::new();
    let mut metadata: BTreeMap<String, String> = BTreeMap::new();

    let origin_str = format!("{:016x}", row.origin);
    let seq_str = row.seq.0.to_string();
    push_field(&mut tags, &mut metadata, "origin", &origin_str);
    push_field(&mut tags, &mut metadata, "seq", &seq_str);

    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&row.payload) {
        flatten_json("", &value, &mut tags, &mut metadata);
    }

    (tags, metadata)
}

/// Push a single (key, value) pair as both a synthetic tag
/// (`dataforts.<key>=<value>`) and a metadata entry.
fn push_field(tags: &mut Vec<Tag>, metadata: &mut BTreeMap<String, String>, key: &str, value: &str) {
    tags.push(Tag::AxisValue {
        axis: TaxonomyAxis::Dataforts,
        key: key.to_string(),
        value: value.to_string(),
        separator: AxisSeparator::Eq,
    });
    metadata.insert(key.to_string(), value.to_string());
}

/// Recursively flatten a JSON value into dotted-path leaves.
/// `prefix` is the accumulated path; leaves are pushed as
/// individual fields.
fn flatten_json(
    prefix: &str,
    value: &serde_json::Value,
    tags: &mut Vec<Tag>,
    metadata: &mut BTreeMap<String, String>,
) {
    use serde_json::Value::*;
    match value {
        Object(map) => {
            for (k, v) in map {
                let next = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten_json(&next, v, tags, metadata);
            }
        }
        // Arrays are skipped in Phase E-2. Wiring per-index
        // tags here is straightforward (`<prefix>.[i]`) but
        // adds a lot of predicate-surface ambiguity (how do
        // you say "any element"?) so deferred until a
        // consumer asks for it.
        Array(_) => {}
        String(s) => {
            if !prefix.is_empty() {
                push_field(tags, metadata, prefix, s);
            }
        }
        Number(n) => {
            if !prefix.is_empty() {
                push_field(tags, metadata, prefix, &n.to_string());
            }
        }
        Bool(b) => {
            if !prefix.is_empty() {
                push_field(tags, metadata, prefix, &b.to_string());
            }
        }
        Null => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::query::SeqNum;

    fn row_with_payload(origin: u64, seq: u64, payload: &str) -> ResultRow {
        ResultRow {
            origin,
            seq: SeqNum(seq),
            payload: payload.as_bytes().to_vec(),
        }
    }

    fn tag_value(tags: &[Tag], key: &str) -> Option<String> {
        tags.iter().find_map(|t| match t {
            Tag::AxisValue {
                axis: TaxonomyAxis::Dataforts,
                key: k,
                value,
                ..
            } if k == key => Some(value.clone()),
            _ => None,
        })
    }

    #[test]
    fn origin_and_seq_are_always_synthesized() {
        let row = row_with_payload(0xABCD_EF01_2345_6789, 42, "");
        let (tags, metadata) = synthetic_row_view(&row);
        assert_eq!(tag_value(&tags, "origin"), Some("abcdef0123456789".to_string()));
        assert_eq!(tag_value(&tags, "seq"), Some("42".to_string()));
        assert_eq!(metadata.get("origin"), Some(&"abcdef0123456789".to_string()));
        assert_eq!(metadata.get("seq"), Some(&"42".to_string()));
    }

    #[test]
    fn non_json_payload_is_opaque_and_does_not_panic() {
        let row = row_with_payload(0x1, 0, "this is not json");
        let (tags, _) = synthetic_row_view(&row);
        // Only origin + seq tags exist.
        assert!(tag_value(&tags, "origin").is_some());
        assert!(tag_value(&tags, "seq").is_some());
        assert_eq!(tags.len(), 2);
    }

    #[test]
    fn flat_json_object_flattens_top_level_scalars() {
        let row = row_with_payload(
            0x1,
            0,
            r#"{"severity":"high","count":3,"ok":true,"unused":null}"#,
        );
        let (tags, metadata) = synthetic_row_view(&row);
        assert_eq!(tag_value(&tags, "severity"), Some("high".to_string()));
        assert_eq!(tag_value(&tags, "count"), Some("3".to_string()));
        assert_eq!(tag_value(&tags, "ok"), Some("true".to_string()));
        // Nulls are skipped.
        assert!(tag_value(&tags, "unused").is_none());
        assert_eq!(metadata.get("count"), Some(&"3".to_string()));
    }

    #[test]
    fn nested_json_flattens_with_dotted_paths() {
        let row = row_with_payload(
            0x1,
            0,
            r#"{"a":{"b":{"c":"deep"}},"flat":1}"#,
        );
        let (tags, _) = synthetic_row_view(&row);
        assert_eq!(tag_value(&tags, "a.b.c"), Some("deep".to_string()));
        assert_eq!(tag_value(&tags, "flat"), Some("1".to_string()));
    }

    #[test]
    fn arrays_are_skipped_in_phase_e2() {
        let row = row_with_payload(
            0x1,
            0,
            r#"{"items":["x","y","z"],"name":"keep"}"#,
        );
        let (tags, _) = synthetic_row_view(&row);
        // Array body absent; sibling scalar present.
        assert_eq!(tag_value(&tags, "name"), Some("keep".to_string()));
        assert!(tags.iter().all(|t| !matches!(t, Tag::AxisValue { key, .. } if key.starts_with("items"))));
    }

    #[test]
    fn non_object_json_root_falls_back_to_intrinsic_only() {
        // A JSON array at the top level isn't an object —
        // skipped per the Phase E-2 contract.
        let row = row_with_payload(0x1, 0, r#"["a","b","c"]"#);
        let (tags, _) = synthetic_row_view(&row);
        assert_eq!(tags.len(), 2); // origin + seq only
    }
}
