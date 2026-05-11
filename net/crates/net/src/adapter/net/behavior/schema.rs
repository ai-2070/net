//! Capability axis schemas — Phase 2 of `CAPABILITY_ENHANCEMENTS_PLAN.md`.
//!
//! This module declares the canonical axis schema (`AXIS_SCHEMA`) and ships
//! the runtime validator (`validate_capabilities`) that surfaces shape
//! violations on a `CapabilitySet`. The wire format is unchanged; this layer
//! is binding-local — it drives auto-completion + type-checking + diagnostic
//! validation without forcing peers to agree on a schema.
//!
//! The authoritative source-of-truth is
//! [`docs/CAPABILITIES_SCHEMA.md`](../../../../../docs/CAPABILITIES_SCHEMA.md).
//! `AXIS_SCHEMA` below mirrors the tables in that doc; the per-binding
//! generators (TS / Python / Go) read the same doc and produce equivalent
//! schemas in their host language. CI guards each binding's regenerated
//! schema against the canonical doc; this Rust schema is itself a
//! hand-maintained mirror, kept in agreement via the unit tests in this
//! module.
//!
//! Eternal-rule alignment (per `CAPABILITY_ENHANCEMENTS_PLAN.md`):
//!
//! - **Wire stays `tags + metadata`.** This module reads `CapabilitySet`,
//!   never writes to it.
//! - **All smarts local to callers.** Validation runs in-process; no peer
//!   coordination required.
//! - **Forward-compat preserved.** Unknown keys under known axes produce
//!   warnings, not errors; older / newer peers without identical schemas
//!   continue to interop because the substrate's tag round-trip is
//!   schema-agnostic.

use crate::adapter::net::behavior::capability::CapabilitySet;
use crate::adapter::net::behavior::tag::{Tag, TaxonomyAxis};

// =============================================================================
// Schema vocabulary
// =============================================================================

/// Value-type discriminator for an axis key. Maps to the `Type` column in
/// [`docs/CAPABILITIES_SCHEMA.md`](../../../../../docs/CAPABILITIES_SCHEMA.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueType {
    /// `<axis>.<key>` (no separator, no value).
    Presence,
    /// `<axis>.<key>=<integer>`.
    Number,
    /// `<axis>.<key>=<string>`.
    String,
    /// `<axis>.<key>=<value>` where value is one of a known set
    /// (`hardware.gpu.vendor=nvidia` etc.). The validator checks the
    /// general string shape; per-enum validation lives at the codec
    /// level.
    Enumeration,
    /// `<axis>.<key>=true` or `=false`.
    Bool,
    /// `<axis>.<key>=v1,v2,v3`.
    Csv,
}

/// Schema entry for a fixed key (top-level under an axis or a sub-key).
///
/// Keys with structural sub-paths (`hardware.gpu.*`,
/// `hardware.limits.*`) appear here as concrete sub-keys
/// (`hardware.gpu.vendor`, `hardware.limits.max_concurrent_requests`).
/// Indexed / keyed collections (`software.model.<i>.*`,
/// `software.runtime.<name>`) are matched via [`KeyShape`] patterns
/// rather than enumerated entries.
#[derive(Debug, Clone, Copy)]
pub struct KeyEntry {
    /// Full key under its axis, e.g. `cpu_cores`, `gpu.vendor`,
    /// `limits.max_concurrent_requests`.
    pub key: &'static str,
    /// Value type per `CAPABILITIES_SCHEMA.md`.
    pub value_type: ValueType,
}

/// Pattern matcher for indexed / keyed collections under an axis.
///
/// `software.model.<i>.<sub>` is described by a single `KeyShape`
/// with `prefix = "model."`, `kind = KeyShapeKind::IndexedCollection`,
/// and `sub_keys = &[("id", String), ("family", String), ...]`.
///
/// `software.runtime.<name>` is `prefix = "runtime."`,
/// `kind = KeyShapeKind::KeyedMap { value_type: String }`, no sub_keys.
#[derive(Debug, Clone, Copy)]
pub struct KeyShape {
    /// Axis-relative prefix, e.g. `model.` or `runtime.`. The trailing
    /// `.` is mandatory.
    pub prefix: &'static str,
    /// Pattern kind.
    pub kind: KeyShapeKind,
    /// For `IndexedCollection` patterns, the per-element sub-keys. Empty
    /// for `KeyedMap` patterns.
    pub sub_keys: &'static [KeyEntry],
}

/// Discriminator for [`KeyShape`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyShapeKind {
    /// `<axis>.<prefix><i>.<sub>=<v>` — numeric index. The validator
    /// confirms `<i>` parses as `u32`.
    IndexedCollection,
    /// `<axis>.<prefix><name>=<value>` — string key. The validator
    /// accepts any non-empty `<name>`. `value_type` is the value type
    /// of the map entries.
    KeyedMap {
        /// Value type for entries in the keyed map.
        value_type: ValueType,
    },
}

/// Schema for one axis. Entries enumerate the fixed keys; shapes
/// describe the indexed / keyed sub-namespaces.
#[derive(Debug, Clone, Copy)]
pub struct AxisEntry {
    /// Fixed-key entries (e.g. `cpu_cores`, `gpu.vendor`).
    pub keys: &'static [KeyEntry],
    /// Indexed / keyed sub-namespace patterns.
    pub shapes: &'static [KeyShape],
}

/// Top-level schema bundle — one entry per [`TaxonomyAxis`] plus
/// metadata-key reservations.
#[derive(Debug, Clone, Copy)]
pub struct AxisSchema {
    /// `hardware.*` keys.
    pub hardware: AxisEntry,
    /// `software.*` keys.
    pub software: AxisEntry,
    /// `devices.*` keys (currently empty; reserved for future
    /// substrate-defined keys).
    pub devices: AxisEntry,
    /// `dataforts.*` keys (currently empty; reserved).
    pub dataforts: AxisEntry,
    /// Reserved metadata keys (`intent`, `colocate-with`, etc.).
    pub metadata_reserved: &'static [&'static str],
    /// Reserved metadata-key prefixes (`tool::`, etc.). Keys
    /// matching any prefix here are treated as substrate-reserved
    /// (no warning), even if not explicitly enumerated.
    pub metadata_reserved_prefixes: &'static [&'static str],
}

// =============================================================================
// AXIS_SCHEMA — canonical const, mirroring `docs/CAPABILITIES_SCHEMA.md`.
// =============================================================================

/// Hardware axis fixed keys.
const HARDWARE_KEYS: &[KeyEntry] = &[
    KeyEntry {
        key: "cpu_cores",
        value_type: ValueType::Number,
    },
    KeyEntry {
        key: "cpu_threads",
        value_type: ValueType::Number,
    },
    KeyEntry {
        key: "memory_gb",
        value_type: ValueType::Number,
    },
    KeyEntry {
        key: "gpu",
        value_type: ValueType::Presence,
    },
    KeyEntry {
        key: "gpu.vendor",
        value_type: ValueType::Enumeration,
    },
    KeyEntry {
        key: "gpu.model",
        value_type: ValueType::String,
    },
    KeyEntry {
        key: "gpu.vram_gb",
        value_type: ValueType::Number,
    },
    KeyEntry {
        key: "gpu.compute_units",
        value_type: ValueType::Number,
    },
    KeyEntry {
        key: "gpu.tensor_cores",
        value_type: ValueType::Number,
    },
    KeyEntry {
        key: "gpu.fp16_tflops_x10",
        value_type: ValueType::Number,
    },
    KeyEntry {
        key: "storage_gb",
        value_type: ValueType::Number,
    },
    KeyEntry {
        key: "network_mbps",
        value_type: ValueType::Number,
    },
    KeyEntry {
        key: "limits.max_concurrent_requests",
        value_type: ValueType::Number,
    },
    KeyEntry {
        key: "limits.max_tokens_per_request",
        value_type: ValueType::Number,
    },
    KeyEntry {
        key: "limits.rate_limit_rpm",
        value_type: ValueType::Number,
    },
    KeyEntry {
        key: "limits.max_batch_size",
        value_type: ValueType::Number,
    },
    KeyEntry {
        key: "limits.max_input_bytes",
        value_type: ValueType::Number,
    },
    KeyEntry {
        key: "limits.max_output_bytes",
        value_type: ValueType::Number,
    },
];

/// Software axis fixed keys (excluding the indexed `model.<i>.*` /
/// `tool.<i>.*` and keyed `runtime.<n>` / `framework.<n>` /
/// `driver.<n>` collections — those live in [`SOFTWARE_SHAPES`]).
const SOFTWARE_KEYS: &[KeyEntry] = &[
    KeyEntry {
        key: "os",
        value_type: ValueType::String,
    },
    KeyEntry {
        key: "os_version",
        value_type: ValueType::String,
    },
    KeyEntry {
        key: "cuda_version",
        value_type: ValueType::String,
    },
];

/// Software axis indexed / keyed sub-namespaces.
const SOFTWARE_SHAPES: &[KeyShape] = &[
    KeyShape {
        prefix: "runtime.",
        kind: KeyShapeKind::KeyedMap {
            value_type: ValueType::String,
        },
        sub_keys: &[],
    },
    KeyShape {
        prefix: "framework.",
        kind: KeyShapeKind::KeyedMap {
            value_type: ValueType::String,
        },
        sub_keys: &[],
    },
    KeyShape {
        prefix: "driver.",
        kind: KeyShapeKind::KeyedMap {
            value_type: ValueType::String,
        },
        sub_keys: &[],
    },
    KeyShape {
        prefix: "model.",
        kind: KeyShapeKind::IndexedCollection,
        sub_keys: &[
            KeyEntry {
                key: "id",
                value_type: ValueType::String,
            },
            KeyEntry {
                key: "family",
                value_type: ValueType::String,
            },
            KeyEntry {
                key: "parameters_b_x10",
                value_type: ValueType::Number,
            },
            KeyEntry {
                key: "context_length",
                value_type: ValueType::Number,
            },
            KeyEntry {
                key: "quantization",
                value_type: ValueType::String,
            },
            KeyEntry {
                key: "modalities",
                value_type: ValueType::Csv,
            },
            KeyEntry {
                key: "tokens_per_sec",
                value_type: ValueType::Number,
            },
            KeyEntry {
                key: "loaded",
                value_type: ValueType::Bool,
            },
        ],
    },
    KeyShape {
        prefix: "tool.",
        kind: KeyShapeKind::IndexedCollection,
        sub_keys: &[
            KeyEntry {
                key: "tool_id",
                value_type: ValueType::String,
            },
            KeyEntry {
                key: "name",
                value_type: ValueType::String,
            },
            KeyEntry {
                key: "version",
                value_type: ValueType::String,
            },
            KeyEntry {
                key: "requires",
                value_type: ValueType::Csv,
            },
            KeyEntry {
                key: "estimated_time_ms",
                value_type: ValueType::Number,
            },
            KeyEntry {
                key: "stateless",
                value_type: ValueType::Bool,
            },
        ],
    },
];

/// Reserved metadata keys (substrate-defined; per `CAPABILITIES_SCHEMA.md`
/// "Metadata reserved keys").
const METADATA_RESERVED_KEYS: &[&str] = &[
    "intent",
    "colocate-with",
    "colocate-with-strict",
    "priority",
    "owner",
];

/// Reserved metadata-key prefixes (e.g. `tool::<id>::input_schema`,
/// `tool::<id>::output_schema`).
const METADATA_RESERVED_PREFIXES: &[&str] = &["tool::"];

/// The canonical axis schema. Mirrors
/// [`docs/CAPABILITIES_SCHEMA.md`](../../../../../docs/CAPABILITIES_SCHEMA.md).
pub const AXIS_SCHEMA: AxisSchema = AxisSchema {
    hardware: AxisEntry {
        keys: HARDWARE_KEYS,
        shapes: &[],
    },
    software: AxisEntry {
        keys: SOFTWARE_KEYS,
        shapes: SOFTWARE_SHAPES,
    },
    devices: AxisEntry {
        keys: &[],
        shapes: &[],
    },
    dataforts: AxisEntry {
        keys: &[],
        shapes: &[],
    },
    metadata_reserved: METADATA_RESERVED_KEYS,
    metadata_reserved_prefixes: METADATA_RESERVED_PREFIXES,
};

// =============================================================================
// Validation
// =============================================================================

/// Validation report for a [`CapabilitySet`] under an [`AxisSchema`].
///
/// See [`docs/CAPABILITIES_SCHEMA.md`](../../../../../docs/CAPABILITIES_SCHEMA.md)
/// "Validation behavior" for the contract.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ValidationReport {
    /// Schema violations the operator should fix.
    pub errors: Vec<SchemaError>,
    /// Forward-compat or hygiene observations.
    pub warnings: Vec<ValidationWarning>,
}

impl ValidationReport {
    /// True when there are zero errors and zero warnings.
    pub fn is_clean(&self) -> bool {
        self.errors.is_empty() && self.warnings.is_empty()
    }

    /// True when there are zero errors. Warnings are allowed.
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Schema violation. Ordered loosely from "syntactic" to "semantic".
#[derive(Debug, Clone, PartialEq)]
pub enum SchemaError {
    /// A tag uses an axis prefix the schema doesn't recognize. Only
    /// fires for shapes that LOOK axis-prefixed but use an unknown
    /// taxonomy axis (e.g. typo `compute.gpu`). Untyped legacy tags
    /// like `nat:full-cone` ride through as forward-compat — they're
    /// not flagged here.
    UnknownAxis {
        /// The unknown axis prefix (string before the first `.`).
        axis_prefix: String,
        /// The full tag wire form.
        tag: String,
    },
    /// A known-axis, known-key tag has a value that doesn't parse as
    /// the expected `ValueType` (e.g. `hardware.memory_gb=lots`).
    TypeMismatch {
        /// Axis the key belongs to.
        axis: TaxonomyAxis,
        /// Full key path under the axis.
        key: String,
        /// Expected value type per the schema.
        expected: ValueType,
        /// The raw value string that failed to parse.
        actual: String,
    },
    /// Indexed-collection sub-key whose numeric index didn't parse
    /// (`software.model.bogus.id=foo` — `bogus` isn't a `u32`).
    IndexMalformed {
        /// Axis the shape belongs to.
        axis: TaxonomyAxis,
        /// Shape prefix (`model.` etc.).
        prefix: String,
        /// The non-numeric index segment.
        index: String,
        /// Full tag wire form.
        tag: String,
    },
}

/// Forward-compat / hygiene observation. Consumers that want strict
/// validation can promote warnings to errors at their layer; the
/// substrate validator always emits warnings, never errors, for these.
#[derive(Debug, Clone, PartialEq)]
pub enum ValidationWarning {
    /// A known-axis tag whose key isn't enumerated in the schema.
    /// Forward-compat: a future binding may emit a key this binding
    /// doesn't yet know.
    UnknownKey {
        /// Axis the unknown key belongs to.
        axis: TaxonomyAxis,
        /// Key path under the axis.
        key: String,
    },
    /// Total `metadata` size exceeds the soft cap. Consumed by
    /// telemetry counters per `CAPABILITY_SYSTEM_PLAN.md` Locked
    /// decision 2.
    MetadataOversize {
        /// Soft cap (4 KB by default).
        soft_cap_bytes: usize,
        /// Actual total bytes across all metadata key+value pairs.
        actual_bytes: usize,
    },
    /// A `Tag::Legacy` (untyped) tag. Future major versions may
    /// deprecate; surfaced here for operator visibility.
    LegacyTag {
        /// The untyped tag value.
        tag: String,
    },
    /// CR-14: a metadata key collides with a substrate-reserved key
    /// (`intent`, `colocate-with`, …). User-emitted writes through
    /// `with_metadata` should route around these — collision can
    /// override scheduler-internal hints.
    MetadataReservedKey {
        /// The reserved metadata key the user wrote into.
        key: String,
    },
    /// CR-14: a metadata key matches a substrate-reserved prefix
    /// (`tool::`, …). Same hazard shape as `MetadataReservedKey`
    /// but matched by prefix rather than exact name.
    MetadataReservedPrefix {
        /// The full key (with the reserved prefix).
        key: String,
        /// The reserved prefix that matched.
        prefix: String,
    },
}

/// Default soft cap for `CapabilitySet::metadata` total size, per
/// `CAPABILITY_SYSTEM_PLAN.md` Locked decision 2.
pub const METADATA_SOFT_CAP_BYTES: usize = 4 * 1024;

/// Validate a [`CapabilitySet`] against the canonical [`AXIS_SCHEMA`].
///
/// Convenience wrapper over [`validate_capabilities_against`] using
/// `AXIS_SCHEMA`. Most callers want this entry point.
pub fn validate_capabilities(caps: &CapabilitySet) -> ValidationReport {
    validate_capabilities_against(caps, &AXIS_SCHEMA)
}

/// Validate a [`CapabilitySet`] against a custom [`AxisSchema`].
///
/// The custom-schema entry point lets applications layer their own
/// schema extensions on top of the substrate's canonical one (e.g.
/// adding application-specific `devices.*` keys). The substrate
/// itself always validates against `AXIS_SCHEMA`.
pub fn validate_capabilities_against(
    caps: &CapabilitySet,
    schema: &AxisSchema,
) -> ValidationReport {
    let mut report = ValidationReport::default();

    for tag in &caps.tags {
        validate_tag(tag, schema, &mut report);
    }

    // CR-14: metadata-key reservation check. The schema declares
    // `metadata_reserved` (exact-match) and `metadata_reserved_prefixes`
    // (prefix-match) but the validator never consulted them — a
    // user's `with_metadata("intent", …)` smuggling onto a
    // scheduler-reserved key emitted no warning. Walk both.
    for key in caps.metadata.keys() {
        if schema.metadata_reserved.contains(&key.as_str()) {
            report
                .warnings
                .push(ValidationWarning::MetadataReservedKey { key: key.clone() });
            continue;
        }
        if let Some(prefix) = schema
            .metadata_reserved_prefixes
            .iter()
            .find(|p| key.starts_with(*p))
        {
            report
                .warnings
                .push(ValidationWarning::MetadataReservedPrefix {
                    key: key.clone(),
                    prefix: (*prefix).to_string(),
                });
        }
    }

    // Metadata size cap (soft only here; hard cap belongs to the
    // emit path, not this diagnostic validator).
    let metadata_bytes: usize = caps.metadata.iter().map(|(k, v)| k.len() + v.len()).sum();
    if metadata_bytes > METADATA_SOFT_CAP_BYTES {
        report.warnings.push(ValidationWarning::MetadataOversize {
            soft_cap_bytes: METADATA_SOFT_CAP_BYTES,
            actual_bytes: metadata_bytes,
        });
    }

    report
}

/// Inspect a single tag, appending any errors / warnings to `report`.
fn validate_tag(tag: &Tag, schema: &AxisSchema, report: &mut ValidationReport) {
    match tag {
        Tag::AxisPresent { axis, key } => {
            validate_axis_key(*axis, key, ValueType::Presence, None, schema, report, tag);
        }
        Tag::AxisValue {
            axis, key, value, ..
        } => {
            // Values aren't typed in `Tag::AxisValue` — they're string
            // payloads. We pass the value through to the type-mismatch
            // check below.
            validate_axis_key(
                *axis,
                key,
                ValueType::String,
                Some(value),
                schema,
                report,
                tag,
            );
        }
        Tag::Reserved { .. } => {
            // Reserved-prefix tags pass through unchecked; their
            // body shape is application-defined and the substrate
            // doesn't constrain it beyond the prefix recognition.
        }
        Tag::Legacy(s) => {
            report
                .warnings
                .push(ValidationWarning::LegacyTag { tag: s.clone() });
        }
    }
}

/// Match a single axis-prefixed key against the schema's keys and
/// shapes for that axis. Push errors / warnings as appropriate.
fn validate_axis_key(
    axis: TaxonomyAxis,
    key: &str,
    observed_type: ValueType,
    observed_value: Option<&str>,
    schema: &AxisSchema,
    report: &mut ValidationReport,
    tag: &Tag,
) {
    let axis_entry = match axis {
        TaxonomyAxis::Hardware => &schema.hardware,
        TaxonomyAxis::Software => &schema.software,
        TaxonomyAxis::Devices => &schema.devices,
        TaxonomyAxis::Dataforts => &schema.dataforts,
    };

    // Try fixed-key match first.
    if let Some(entry) = axis_entry.keys.iter().find(|e| e.key == key) {
        check_value(entry, observed_type, observed_value, axis, report);
        return;
    }

    // Try shape patterns.
    for shape in axis_entry.shapes {
        if let Some(rest) = key.strip_prefix(shape.prefix) {
            match shape.kind {
                KeyShapeKind::IndexedCollection => {
                    if let Some((idx, sub)) = rest.split_once('.') {
                        if idx.parse::<u32>().is_err() {
                            report.errors.push(SchemaError::IndexMalformed {
                                axis,
                                prefix: shape.prefix.to_string(),
                                index: idx.to_string(),
                                tag: tag.to_string(),
                            });
                            return;
                        }
                        if let Some(sub_entry) = shape.sub_keys.iter().find(|e| e.key == sub) {
                            check_value(sub_entry, observed_type, observed_value, axis, report);
                            return;
                        }
                        // Known shape, unknown sub-key: forward-compat warning.
                        report.warnings.push(ValidationWarning::UnknownKey {
                            axis,
                            key: key.to_string(),
                        });
                        return;
                    }
                    // No `.<sub>` part; doesn't match the indexed
                    // shape. Fall through to UnknownKey below.
                }
                KeyShapeKind::KeyedMap { value_type } => {
                    if !rest.is_empty() {
                        // KeyedMap: the `rest` IS the user-defined name.
                        // We type-check the value against `value_type`.
                        let synth = KeyEntry {
                            key: shape.prefix,
                            value_type,
                        };
                        check_value(&synth, observed_type, observed_value, axis, report);
                        return;
                    }
                }
            }
        }
    }

    // No fixed-key, no shape match: forward-compat unknown key.
    report.warnings.push(ValidationWarning::UnknownKey {
        axis,
        key: key.to_string(),
    });
}

/// Type-check the observed value against the entry's expected type.
fn check_value(
    entry: &KeyEntry,
    observed_type: ValueType,
    observed_value: Option<&str>,
    axis: TaxonomyAxis,
    report: &mut ValidationReport,
) {
    // Presence keys must be observed as presence (no value).
    if entry.value_type == ValueType::Presence {
        if observed_type != ValueType::Presence {
            report.errors.push(SchemaError::TypeMismatch {
                axis,
                key: entry.key.to_string(),
                expected: ValueType::Presence,
                actual: observed_value.unwrap_or("").to_string(),
            });
        }
        return;
    }

    // Non-presence keys must carry a value.
    let Some(value) = observed_value else {
        report.errors.push(SchemaError::TypeMismatch {
            axis,
            key: entry.key.to_string(),
            expected: entry.value_type,
            actual: "<no value>".to_string(),
        });
        return;
    };

    // Type-specific value check.
    let parses = match entry.value_type {
        ValueType::Presence => unreachable!("handled above"),
        // CR-15: Number is unsigned. The previous `i64` fallback
        // admitted negatives for keys like `hardware.memory_gb`,
        // `gpu.vram_gb`, `cpu_cores`, `limits.max_concurrent_requests`
        // — none of which can be negative semantically. The schema
        // doesn't currently model signed numerics; if one is ever
        // needed, split into `Int` / `UInt` rather than reintroducing
        // the fallback.
        ValueType::Number => value.parse::<u64>().is_ok(),
        // Strings, enums, csv all pass any non-empty string. Enum-
        // value validation lives at the codec level (e.g.
        // `GpuVendor::from(...)` falls back to `Unknown`).
        ValueType::String | ValueType::Enumeration | ValueType::Csv => !value.is_empty(),
        ValueType::Bool => matches!(value, "true" | "false"),
    };

    if !parses {
        report.errors.push(SchemaError::TypeMismatch {
            axis,
            key: entry.key.to_string(),
            expected: entry.value_type,
            actual: value.to_string(),
        });
    }
}

/// Detect tags whose wire-form looks axis-prefixed but uses an
/// unknown taxonomy axis. Currently rolled into `Tag::parse`'s
/// fallback to `Tag::Legacy`; retained as a placeholder so future
/// tightening (e.g. flagging `compute.gpu` as a typo of `hardware.gpu`)
/// has a hook to land in.
#[allow(dead_code)]
fn detect_unknown_axis_typo(_legacy: &str) -> Option<String> {
    None
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::net::behavior::capability::{
        GpuInfo, GpuVendor, HardwareCapabilities, Modality, ModelCapability, SoftwareCapabilities,
    };
    use crate::adapter::net::behavior::tag::AxisSeparator;
    use std::collections::HashSet;

    #[test]
    fn axis_schema_const_covers_every_documented_hardware_key() {
        // Pin: every `hardware.*` key listed in CAPABILITIES_SCHEMA.md
        // appears in `HARDWARE_KEYS`. Detect drift between the doc
        // and the const at build time.
        let keys: HashSet<&str> = HARDWARE_KEYS.iter().map(|e| e.key).collect();
        let expected: HashSet<&str> = [
            "cpu_cores",
            "cpu_threads",
            "memory_gb",
            "gpu",
            "gpu.vendor",
            "gpu.model",
            "gpu.vram_gb",
            "gpu.compute_units",
            "gpu.tensor_cores",
            "gpu.fp16_tflops_x10",
            "storage_gb",
            "network_mbps",
            "limits.max_concurrent_requests",
            "limits.max_tokens_per_request",
            "limits.rate_limit_rpm",
            "limits.max_batch_size",
            "limits.max_input_bytes",
            "limits.max_output_bytes",
        ]
        .into_iter()
        .collect();
        assert_eq!(keys, expected);
    }

    #[test]
    fn axis_schema_const_covers_every_documented_software_shape() {
        let prefixes: HashSet<&str> = SOFTWARE_SHAPES.iter().map(|s| s.prefix).collect();
        let expected: HashSet<&str> = ["runtime.", "framework.", "driver.", "model.", "tool."]
            .into_iter()
            .collect();
        assert_eq!(prefixes, expected);
    }

    #[test]
    fn validate_default_capability_set_is_clean() {
        let caps = CapabilitySet::default();
        let report = validate_capabilities(&caps);
        assert!(report.is_clean(), "report not clean: {report:?}");
    }

    #[test]
    fn validate_well_formed_capability_set_is_clean() {
        let gpu = GpuInfo::new(GpuVendor::Nvidia, "h100", 80);
        let hw = HardwareCapabilities::new()
            .with_cpu(16, 32)
            .with_memory(64)
            .with_gpu(gpu);
        let sw = SoftwareCapabilities::new()
            .with_os("linux", "6.5")
            .add_runtime("python", "3.11")
            .add_framework("pytorch", "2.1");
        let model = ModelCapability::new("llama-3.1-70b", "llama")
            .with_parameters(70.0)
            .with_context_length(128000)
            .add_modality(Modality::Text)
            .with_loaded(true);
        let caps = CapabilitySet::new()
            .with_hardware(hw)
            .with_software(sw)
            .add_model(model)
            .with_metadata("intent", "ml-training");
        let report = validate_capabilities(&caps);
        assert!(
            report.errors.is_empty(),
            "errors on a well-formed set: {:?}",
            report.errors
        );
    }

    #[test]
    fn validate_unknown_key_under_known_axis_is_warning_not_error() {
        // Forward-compat: a future binding emits `hardware.future_key`;
        // an older binding sees it as an UnknownKey warning, not an
        // error. Ride-through.
        let caps = CapabilitySet::new().add_tag("hardware.future_key=42");
        let report = validate_capabilities(&caps);
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
        assert!(
            report
                .warnings
                .iter()
                .any(|w| matches!(w, ValidationWarning::UnknownKey { axis, key }
                    if *axis == TaxonomyAxis::Hardware && key == "future_key")),
            "missing UnknownKey warning: {:?}",
            report.warnings,
        );
    }

    #[test]
    fn validate_legacy_untyped_tag_is_warning_not_error() {
        let caps = CapabilitySet::new().add_tag("nat:full-cone");
        let report = validate_capabilities(&caps);
        assert!(report.errors.is_empty());
        assert!(
            report.warnings.iter().any(|w| matches!(w,
                ValidationWarning::LegacyTag { tag } if tag == "nat:full-cone"
            )),
            "missing LegacyTag warning: {:?}",
            report.warnings,
        );
    }

    #[test]
    fn validate_indexed_collection_unknown_subkey_is_warning() {
        // `software.model.0.future_field=v` — known indexed shape
        // (`model.<i>.*`), unknown sub-key. Forward-compat warning.
        let caps = CapabilitySet::new().add_tag("software.model.0.future_field=v");
        let report = validate_capabilities(&caps);
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
        assert!(
            report.warnings.iter().any(|w| matches!(w,
                ValidationWarning::UnknownKey { axis, key }
                    if *axis == TaxonomyAxis::Software && key == "model.0.future_field"
            )),
            "missing UnknownKey warning: {:?}",
            report.warnings,
        );
    }

    #[test]
    fn validate_indexed_collection_non_numeric_index_is_error() {
        // `software.model.bogus.id=foo` — non-numeric index segment
        // is a hard error (the substrate's tag_codec drops these
        // silently; the validator surfaces them so operators see
        // the typo).
        let caps = CapabilitySet::new().add_tag("software.model.bogus.id=foo");
        let report = validate_capabilities(&caps);
        assert!(
            report.errors.iter().any(|e| matches!(e,
                SchemaError::IndexMalformed { axis, prefix, index, .. }
                    if *axis == TaxonomyAxis::Software
                       && prefix == "model."
                       && index == "bogus"
            )),
            "missing IndexMalformed error: {:?}",
            report.errors,
        );
    }

    #[test]
    fn validate_metadata_oversize_is_warning() {
        let mut caps = CapabilitySet::new();
        // Push past the 4 KB soft cap with one big value.
        caps.metadata.insert("big".into(), "x".repeat(8 * 1024));
        let report = validate_capabilities(&caps);
        assert!(report.errors.is_empty());
        assert!(
            report.warnings.iter().any(|w| matches!(w,
                ValidationWarning::MetadataOversize { actual_bytes, .. }
                    if *actual_bytes > METADATA_SOFT_CAP_BYTES
            )),
            "missing MetadataOversize warning: {:?}",
            report.warnings,
        );
    }

    /// CR-14: validator emits MetadataReservedKey for every
    /// metadata key colliding with a schema-reserved exact name.
    /// Pre-CR-14 the schema declared the reserved-keys list but
    /// the validator never checked it — `with_metadata("intent",
    /// …)` warned on nothing.
    #[test]
    fn validate_metadata_reserved_key_is_warning() {
        let mut caps = CapabilitySet::new();
        caps.metadata
            .insert("intent".to_string(), "scheduler-hint".to_string());
        caps.metadata.insert("benign".to_string(), "ok".to_string());
        let report = validate_capabilities(&caps);
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
        assert!(
            report.warnings.iter().any(|w| matches!(w,
                ValidationWarning::MetadataReservedKey { key } if key == "intent"
            )),
            "missing MetadataReservedKey warning: {:?}",
            report.warnings,
        );
        // Benign key produces no warning.
        assert!(
            !report.warnings.iter().any(|w| matches!(w,
                ValidationWarning::MetadataReservedKey { key } if key == "benign"
            )),
            "benign key wrongly flagged: {:?}",
            report.warnings,
        );
    }

    /// CR-14: prefix-matched reservations (`tool::*`).
    #[test]
    fn validate_metadata_reserved_prefix_is_warning() {
        let mut caps = CapabilitySet::new();
        caps.metadata
            .insert("tool::evil::input_schema".to_string(), "spoof".to_string());
        let report = validate_capabilities(&caps);
        assert!(report.errors.is_empty());
        assert!(
            report.warnings.iter().any(|w| matches!(w,
                ValidationWarning::MetadataReservedPrefix { key, prefix }
                    if key == "tool::evil::input_schema" && prefix == "tool::"
            )),
            "missing MetadataReservedPrefix warning: {:?}",
            report.warnings,
        );
    }

    /// CR-15: `Number` no longer accepts negative integers. The
    /// previous `i64` fallback let `-1` slip in for unsigned-only
    /// keys (`memory_gb`, `vram_gb`, `cpu_cores`, etc.).
    #[test]
    fn validate_number_rejects_negative_values() {
        let mut caps = CapabilitySet::new();
        caps.tags.insert(Tag::AxisValue {
            axis: TaxonomyAxis::Hardware,
            key: "memory_gb".to_string(),
            value: "-1".to_string(),
            separator: AxisSeparator::Eq,
        });
        let report = validate_capabilities(&caps);
        assert!(
            report.errors.iter().any(|e| matches!(e,
                SchemaError::TypeMismatch { axis, key, expected, actual }
                    if *axis == TaxonomyAxis::Hardware
                       && key == "memory_gb"
                       && *expected == ValueType::Number
                       && actual == "-1"
            )),
            "negative value should fail Number validation: {:?}",
            report.errors,
        );
    }

    #[test]
    fn validate_keyed_map_accepts_arbitrary_runtime_name() {
        // `software.runtime.<name>=<version>` — `<name>` is
        // user-defined; the schema accepts any non-empty `<name>`.
        let caps = CapabilitySet::new()
            .with_software(SoftwareCapabilities::new().add_runtime("custom-runtime", "1.0"));
        let report = validate_capabilities(&caps);
        assert!(report.is_valid(), "errors: {:?}", report.errors);
    }

    #[test]
    fn validate_report_is_valid_allows_warnings() {
        let mut report = ValidationReport::default();
        report
            .warnings
            .push(ValidationWarning::LegacyTag { tag: "x".into() });
        assert!(report.is_valid());
        assert!(!report.is_clean());
    }

    #[test]
    fn validate_report_is_valid_rejects_errors() {
        let mut report = ValidationReport::default();
        report.errors.push(SchemaError::UnknownAxis {
            axis_prefix: "compute".into(),
            tag: "compute.gpu".into(),
        });
        assert!(!report.is_valid());
        assert!(!report.is_clean());
    }

    #[test]
    fn validate_metadata_reserved_keys_pinned() {
        // Pin: every metadata key documented in CAPABILITIES_SCHEMA.md
        // "Metadata reserved keys" appears in
        // `METADATA_RESERVED_KEYS`. Drift detector.
        let pinned: HashSet<&str> = METADATA_RESERVED_KEYS.iter().copied().collect();
        let expected: HashSet<&str> = [
            "intent",
            "colocate-with",
            "colocate-with-strict",
            "priority",
            "owner",
        ]
        .into_iter()
        .collect();
        assert_eq!(pinned, expected);
    }

    #[test]
    fn validate_metadata_reserved_prefixes_pinned() {
        let pinned: HashSet<&str> = METADATA_RESERVED_PREFIXES.iter().copied().collect();
        let expected: HashSet<&str> = ["tool::"].into_iter().collect();
        assert_eq!(pinned, expected);
    }
}
