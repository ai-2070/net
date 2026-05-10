/**
 * Capability axis schema + `validateCapabilities` — Phase 9a of
 * `CAPABILITY_SYSTEM_SDK_PLAN.md`.
 *
 * Mirrors the substrate's `AXIS_SCHEMA` const + the canonical
 * `validate_capabilities` validator. The wire shape of the
 * `ValidationReport` (lowercase `kind` discriminator, axis as
 * lowercase string, value type as lowercase string) is pinned by the
 * cross-binding fixture `tests/cross_lang_capability/capability_validation.json`.
 *
 * Source-of-truth for the schema is
 * `net/crates/net/docs/CAPABILITIES_SCHEMA.md`. The substrate's Rust
 * mirror in `behavior/schema.rs` and this TS mirror are
 * hand-maintained against the doc; CI guards each binding's
 * regenerated schema once the codegen tool from Phase 9a's plan
 * lands.
 *
 * @packageDocumentation
 */

import type {
  CapabilitySetWire,
  TaxonomyAxis,
} from './capability-enhancements';
import { tagFromString } from './capability-enhancements';

// ============================================================================
// Schema vocabulary
// ============================================================================

export type ValueType =
  | 'presence'
  | 'number'
  | 'string'
  | 'enumeration'
  | 'bool'
  | 'csv';

export interface KeyEntry {
  /** Full key under its axis, e.g. `cpu_cores`, `gpu.vendor`. */
  key: string;
  valueType: ValueType;
}

export type KeyShapeKind =
  | { kind: 'indexedCollection' }
  | { kind: 'keyedMap'; valueType: ValueType };

export interface KeyShape {
  /** Axis-relative prefix, e.g. `model.` (trailing `.` mandatory). */
  prefix: string;
  shape: KeyShapeKind;
  /** Per-element sub-keys for indexed collections; empty for keyed maps. */
  subKeys: KeyEntry[];
}

export interface AxisEntry {
  keys: KeyEntry[];
  shapes: KeyShape[];
}

export interface AxisSchema {
  hardware: AxisEntry;
  software: AxisEntry;
  devices: AxisEntry;
  dataforts: AxisEntry;
  metadataReserved: string[];
  metadataReservedPrefixes: string[];
}

// ============================================================================
// AXIS_SCHEMA — mirrors `behavior::schema::AXIS_SCHEMA`.
// ============================================================================

const HARDWARE_KEYS: KeyEntry[] = [
  { key: 'cpu_cores', valueType: 'number' },
  { key: 'cpu_threads', valueType: 'number' },
  { key: 'memory_mb', valueType: 'number' },
  { key: 'gpu', valueType: 'presence' },
  { key: 'gpu.vendor', valueType: 'enumeration' },
  { key: 'gpu.model', valueType: 'string' },
  { key: 'gpu.vram_mb', valueType: 'number' },
  { key: 'gpu.compute_units', valueType: 'number' },
  { key: 'gpu.tensor_cores', valueType: 'number' },
  { key: 'gpu.fp16_tflops_x10', valueType: 'number' },
  { key: 'storage_mb', valueType: 'number' },
  { key: 'network_mbps', valueType: 'number' },
  { key: 'limits.max_concurrent_requests', valueType: 'number' },
  { key: 'limits.max_tokens_per_request', valueType: 'number' },
  { key: 'limits.rate_limit_rpm', valueType: 'number' },
  { key: 'limits.max_batch_size', valueType: 'number' },
  { key: 'limits.max_input_bytes', valueType: 'number' },
  { key: 'limits.max_output_bytes', valueType: 'number' },
];

const SOFTWARE_KEYS: KeyEntry[] = [
  { key: 'os', valueType: 'string' },
  { key: 'os_version', valueType: 'string' },
  { key: 'cuda_version', valueType: 'string' },
];

const SOFTWARE_SHAPES: KeyShape[] = [
  {
    prefix: 'runtime.',
    shape: { kind: 'keyedMap', valueType: 'string' },
    subKeys: [],
  },
  {
    prefix: 'framework.',
    shape: { kind: 'keyedMap', valueType: 'string' },
    subKeys: [],
  },
  {
    prefix: 'driver.',
    shape: { kind: 'keyedMap', valueType: 'string' },
    subKeys: [],
  },
  {
    prefix: 'model.',
    shape: { kind: 'indexedCollection' },
    subKeys: [
      { key: 'id', valueType: 'string' },
      { key: 'family', valueType: 'string' },
      { key: 'parameters_b_x10', valueType: 'number' },
      { key: 'context_length', valueType: 'number' },
      { key: 'quantization', valueType: 'string' },
      { key: 'modalities', valueType: 'csv' },
      { key: 'tokens_per_sec', valueType: 'number' },
      { key: 'loaded', valueType: 'bool' },
    ],
  },
  {
    prefix: 'tool.',
    shape: { kind: 'indexedCollection' },
    subKeys: [
      { key: 'tool_id', valueType: 'string' },
      { key: 'name', valueType: 'string' },
      { key: 'version', valueType: 'string' },
      { key: 'requires', valueType: 'csv' },
      { key: 'estimated_time_ms', valueType: 'number' },
      { key: 'stateless', valueType: 'bool' },
    ],
  },
];

/** Reserved metadata keys (substrate-defined). */
export const METADATA_RESERVED_KEYS: readonly string[] = [
  'intent',
  'colocate-with',
  'colocate-with-strict',
  'priority',
  'owner',
];

/** Reserved metadata-key prefixes (`tool::<id>::input_schema` etc.). */
export const METADATA_RESERVED_PREFIXES: readonly string[] = ['tool::'];

/** Default soft cap for `metadata` total size. */
export const METADATA_SOFT_CAP_BYTES = 4 * 1024;

/** The canonical axis schema. Mirrors `behavior::schema::AXIS_SCHEMA`. */
export const AXIS_SCHEMA: AxisSchema = {
  hardware: { keys: HARDWARE_KEYS, shapes: [] },
  software: { keys: SOFTWARE_KEYS, shapes: SOFTWARE_SHAPES },
  devices: { keys: [], shapes: [] },
  dataforts: { keys: [], shapes: [] },
  metadataReserved: [...METADATA_RESERVED_KEYS],
  metadataReservedPrefixes: [...METADATA_RESERVED_PREFIXES],
};

// ============================================================================
// ValidationReport wire types
// ============================================================================

export type SchemaError =
  | {
      kind: 'unknown_axis';
      axis_prefix: string;
      tag: string;
    }
  | {
      kind: 'type_mismatch';
      axis: TaxonomyAxis;
      key: string;
      expected: ValueType;
      actual: string;
    }
  | {
      kind: 'index_malformed';
      axis: TaxonomyAxis;
      prefix: string;
      index: string;
      tag: string;
    };

export type ValidationWarning =
  | {
      kind: 'unknown_key';
      axis: TaxonomyAxis;
      key: string;
    }
  | {
      kind: 'metadata_oversize';
      soft_cap_bytes: number;
      actual_bytes: number;
    }
  | {
      kind: 'legacy_tag';
      tag: string;
    }
  // P2-H: mirror the substrate's CR-14 reserved-metadata warnings
  // (`schema.rs::ValidationWarning::MetadataReservedKey` /
  // `MetadataReservedPrefix`). Cross-binding parity restored.
  | {
      kind: 'metadata_reserved_key';
      key: string;
    }
  | {
      kind: 'metadata_reserved_prefix';
      key: string;
      prefix: string;
    };

export interface ValidationReport {
  errors: SchemaError[];
  warnings: ValidationWarning[];
}

// ============================================================================
// Validator
// ============================================================================

function axisEntry(schema: AxisSchema, axis: TaxonomyAxis): AxisEntry {
  switch (axis) {
    case 'hardware':
      return schema.hardware;
    case 'software':
      return schema.software;
    case 'devices':
      return schema.devices;
    case 'dataforts':
      return schema.dataforts;
  }
}

function checkValue(
  entry: KeyEntry,
  observedType: ValueType,
  observedValue: string | undefined,
  axis: TaxonomyAxis,
  errors: SchemaError[],
): void {
  if (entry.valueType === 'presence') {
    if (observedType !== 'presence') {
      errors.push({
        kind: 'type_mismatch',
        axis,
        key: entry.key,
        expected: 'presence',
        actual: observedValue ?? '',
      });
    }
    return;
  }
  if (observedValue === undefined) {
    errors.push({
      kind: 'type_mismatch',
      axis,
      key: entry.key,
      expected: entry.valueType,
      actual: '<no value>',
    });
    return;
  }
  let parses = false;
  switch (entry.valueType) {
    case 'number':
      // Substrate `Number` is unsigned (u64-only) — see CR-15 in the
      // capability-system-2 review and `schema.rs::ValueType::Number`.
      // Negative values surface as `TypeMismatch` errors on the
      // substrate side; mirror that here so client-side validation
      // doesn't flag a CapabilitySet as valid that the substrate
      // would later reject.
      parses = /^\d+$/.test(observedValue);
      break;
    case 'string':
    case 'enumeration':
    case 'csv':
      parses = observedValue.length > 0;
      break;
    case 'bool':
      parses = observedValue === 'true' || observedValue === 'false';
      break;
  }
  if (!parses) {
    errors.push({
      kind: 'type_mismatch',
      axis,
      key: entry.key,
      expected: entry.valueType,
      actual: observedValue,
    });
  }
}

function validateAxisKey(
  axis: TaxonomyAxis,
  key: string,
  observedType: ValueType,
  observedValue: string | undefined,
  schema: AxisSchema,
  errors: SchemaError[],
  warnings: ValidationWarning[],
  tagWire: string,
): void {
  const entry = axisEntry(schema, axis);
  const fixed = entry.keys.find((e) => e.key === key);
  if (fixed) {
    checkValue(fixed, observedType, observedValue, axis, errors);
    return;
  }
  for (const shape of entry.shapes) {
    if (!key.startsWith(shape.prefix)) continue;
    const rest = key.slice(shape.prefix.length);
    if (shape.shape.kind === 'indexedCollection') {
      const dot = rest.indexOf('.');
      if (dot < 0) continue;
      const idx = rest.slice(0, dot);
      const sub = rest.slice(dot + 1);
      // Q10: substrate parses the index as `u32`; strings of digits
      // longer than `u32::MAX` (e.g. `"4294967296"`, 2^32) are
      // accepted by the TS regex but rejected by the substrate as
      // `IndexMalformed`. Mirror the u32 range so client-side
      // validation doesn't silently pass payloads the substrate
      // would later reject.
      const idxNum = Number(idx);
      if (
        !/^\d+$/.test(idx) ||
        !Number.isInteger(idxNum) ||
        idxNum > 0xffff_ffff
      ) {
        errors.push({
          kind: 'index_malformed',
          axis,
          prefix: shape.prefix,
          index: idx,
          tag: tagWire,
        });
        return;
      }
      const subEntry = shape.subKeys.find((e) => e.key === sub);
      if (subEntry) {
        checkValue(subEntry, observedType, observedValue, axis, errors);
        return;
      }
      warnings.push({ kind: 'unknown_key', axis, key });
      return;
    }
    // KeyedMap: the rest IS the user-defined name.
    if (rest.length > 0) {
      const synth: KeyEntry = {
        key: shape.prefix,
        valueType: shape.shape.valueType,
      };
      checkValue(synth, observedType, observedValue, axis, errors);
      return;
    }
  }
  warnings.push({ kind: 'unknown_key', axis, key });
}

/**
 * Validate a wire-format capability set against a schema. Defaults to
 * the canonical {@link AXIS_SCHEMA}; pass a custom schema for
 * application-specific extensions.
 *
 * Mirrors the substrate's `validate_capabilities`. Output shape pinned
 * by `tests/cross_lang_capability/capability_validation.json`.
 */
export function validateCapabilities(
  caps: CapabilitySetWire,
  schema: AxisSchema = AXIS_SCHEMA,
): ValidationReport {
  const errors: SchemaError[] = [];
  const warnings: ValidationWarning[] = [];

  for (const wire of caps.tags) {
    const tag = tagFromString(wire);
    switch (tag.kind) {
      case 'axisPresent':
        validateAxisKey(
          tag.axis,
          tag.key,
          'presence',
          undefined,
          schema,
          errors,
          warnings,
          wire,
        );
        break;
      case 'axisValue':
        validateAxisKey(
          tag.axis,
          tag.key,
          'string',
          tag.value,
          schema,
          errors,
          warnings,
          wire,
        );
        break;
      case 'reserved':
        // Reserved-prefix tags pass through unchecked.
        break;
      case 'legacy':
        warnings.push({ kind: 'legacy_tag', tag: tag.raw });
        break;
    }
  }

  // P2-H: metadata-key reservation check. The schema declares
  // `metadataReserved` (exact-match) and `metadataReservedPrefixes`
  // (prefix-match) but pre-fix the validator never consulted them
  // — a user's `with_metadata("intent", …)` smuggling onto a
  // scheduler-reserved key emitted no warning. Mirrors the
  // substrate's CR-14 fix.
  for (const key of Object.keys(caps.metadata)) {
    if (schema.metadataReserved.includes(key)) {
      warnings.push({ kind: 'metadata_reserved_key', key });
      continue;
    }
    const prefix = schema.metadataReservedPrefixes.find((p) =>
      key.startsWith(p),
    );
    if (prefix !== undefined) {
      warnings.push({ kind: 'metadata_reserved_prefix', key, prefix });
    }
  }

  // Metadata soft-cap check.
  let metadataBytes = 0;
  for (const [k, v] of Object.entries(caps.metadata)) {
    metadataBytes += k.length + v.length;
  }
  if (metadataBytes > METADATA_SOFT_CAP_BYTES) {
    warnings.push({
      kind: 'metadata_oversize',
      soft_cap_bytes: METADATA_SOFT_CAP_BYTES,
      actual_bytes: metadataBytes,
    });
  }

  return { errors, warnings };
}

/** True iff there are zero errors and zero warnings. */
export function isReportClean(r: ValidationReport): boolean {
  return r.errors.length === 0 && r.warnings.length === 0;
}

/** True iff there are zero errors. Warnings are allowed. */
export function isReportValid(r: ValidationReport): boolean {
  return r.errors.length === 0;
}
