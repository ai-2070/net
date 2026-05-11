// Capability axis schema + ValidateCapabilities — Phase 9a of
// `CAPABILITY_SYSTEM_SDK_PLAN.md`.
//
// Mirrors the substrate's `AXIS_SCHEMA` const + the canonical
// `validate_capabilities` validator. The wire shape of the
// `ValidationReport` (lowercase `kind` discriminator, axis as
// lowercase string, value type as lowercase string) is pinned by the
// cross-binding fixture
// `tests/cross_lang_capability/capability_validation.json`.
//
// Source-of-truth for the schema is
// `net/crates/net/docs/CAPABILITIES_SCHEMA.md`. This Go reference
// implementation is hand-maintained against the doc; the codegen
// guard from Phase 9a will land alongside the production go.mod.

package net

import (
	"strconv"
	"strings"
)

// ============================================================================
// Schema vocabulary
// ============================================================================

// ValueType discriminates the value shape of an axis key. Matches the
// canonical wire string ("presence", "number", "string", "enumeration",
// "bool", "csv").
type ValueType string

const (
	ValueTypePresence    ValueType = "presence"
	ValueTypeNumber      ValueType = "number"
	ValueTypeString      ValueType = "string"
	ValueTypeEnumeration ValueType = "enumeration"
	ValueTypeBool        ValueType = "bool"
	ValueTypeCsv         ValueType = "csv"
)

// SchemaKeyEntry describes a fixed key under an axis.
type SchemaKeyEntry struct {
	Key       string
	ValueType ValueType
}

// SchemaShapeKind discriminates KeyShape.
type SchemaShapeKind uint8

const (
	SchemaShapeIndexedCollection SchemaShapeKind = iota
	SchemaShapeKeyedMap
)

// SchemaKeyShape describes an indexed / keyed sub-namespace under an
// axis. For indexed collections, SubKeys enumerates the per-element
// sub-keys; for keyed maps, KeyedValueType is the value type of map
// entries.
type SchemaKeyShape struct {
	Prefix         string
	Kind           SchemaShapeKind
	SubKeys        []SchemaKeyEntry
	KeyedValueType ValueType
}

// AxisEntry holds the fixed keys + shape patterns for one axis.
type SchemaAxisEntry struct {
	Keys   []SchemaKeyEntry
	Shapes []SchemaKeyShape
}

// AxisSchema is the top-level schema bundle.
type AxisSchema struct {
	Hardware                 SchemaAxisEntry
	Software                 SchemaAxisEntry
	Devices                  SchemaAxisEntry
	Dataforts                SchemaAxisEntry
	MetadataReserved         []string
	MetadataReservedPrefixes []string
}

// ============================================================================
// AxisSchemaCanonical — mirrors `behavior::schema::AXIS_SCHEMA`.
// ============================================================================

var hardwareKeys = []SchemaKeyEntry{
	{"cpu_cores", ValueTypeNumber},
	{"cpu_threads", ValueTypeNumber},
	{"memory_gb", ValueTypeNumber},
	{"gpu", ValueTypePresence},
	{"gpu.vendor", ValueTypeEnumeration},
	{"gpu.model", ValueTypeString},
	{"gpu.vram_gb", ValueTypeNumber},
	{"gpu.compute_units", ValueTypeNumber},
	{"gpu.tensor_cores", ValueTypeNumber},
	{"gpu.fp16_tflops_x10", ValueTypeNumber},
	{"storage_gb", ValueTypeNumber},
	{"network_gbps", ValueTypeNumber},
	{"limits.max_concurrent_requests", ValueTypeNumber},
	{"limits.max_tokens_per_request", ValueTypeNumber},
	{"limits.rate_limit_rpm", ValueTypeNumber},
	{"limits.max_batch_size", ValueTypeNumber},
	{"limits.max_input_bytes", ValueTypeNumber},
	{"limits.max_output_bytes", ValueTypeNumber},
}

var softwareKeys = []SchemaKeyEntry{
	{"os", ValueTypeString},
	{"os_version", ValueTypeString},
	{"cuda_version", ValueTypeString},
}

var softwareShapes = []SchemaKeyShape{
	{Prefix: "runtime.", Kind: SchemaShapeKeyedMap, KeyedValueType: ValueTypeString},
	{Prefix: "framework.", Kind: SchemaShapeKeyedMap, KeyedValueType: ValueTypeString},
	{Prefix: "driver.", Kind: SchemaShapeKeyedMap, KeyedValueType: ValueTypeString},
	{
		Prefix: "model.",
		Kind:   SchemaShapeIndexedCollection,
		SubKeys: []SchemaKeyEntry{
			{"id", ValueTypeString},
			{"family", ValueTypeString},
			{"parameters_b_x10", ValueTypeNumber},
			{"context_length", ValueTypeNumber},
			{"quantization", ValueTypeString},
			{"modalities", ValueTypeCsv},
			{"tokens_per_sec", ValueTypeNumber},
			{"loaded", ValueTypeBool},
		},
	},
	{
		Prefix: "tool.",
		Kind:   SchemaShapeIndexedCollection,
		SubKeys: []SchemaKeyEntry{
			{"tool_id", ValueTypeString},
			{"name", ValueTypeString},
			{"version", ValueTypeString},
			{"requires", ValueTypeCsv},
			{"estimated_time_ms", ValueTypeNumber},
			{"stateless", ValueTypeBool},
		},
	},
}

// MetadataReservedKeys lists substrate-defined reserved metadata
// keys.
var MetadataReservedKeys = []string{
	"intent",
	"colocate-with",
	"colocate-with-strict",
	"priority",
	"owner",
}

// MetadataReservedPrefixes lists reserved metadata-key prefixes.
var MetadataReservedPrefixes = []string{"tool::"}

// MetadataSoftCapBytes is the default soft cap for `metadata` total
// size.
const MetadataSoftCapBytes = 4 * 1024

// AxisSchemaCanonical mirrors `behavior::schema::AXIS_SCHEMA`.
var AxisSchemaCanonical = AxisSchema{
	Hardware:                 SchemaAxisEntry{Keys: hardwareKeys, Shapes: nil},
	Software:                 SchemaAxisEntry{Keys: softwareKeys, Shapes: softwareShapes},
	Devices:                  SchemaAxisEntry{},
	Dataforts:                SchemaAxisEntry{},
	MetadataReserved:         MetadataReservedKeys,
	MetadataReservedPrefixes: MetadataReservedPrefixes,
}

// ============================================================================
// ValidationReport wire types
// ============================================================================

// SchemaError is the wire-format schema-violation record. Only one
// of the optional groups is populated per kind; JSON-omitempty on
// the unused fields produces the canonical wire form.
type SchemaError struct {
	Kind       string    `json:"kind"`
	AxisPrefix string    `json:"axis_prefix,omitempty"`
	Tag        string    `json:"tag,omitempty"`
	Axis       string    `json:"axis,omitempty"`
	Key        string    `json:"key,omitempty"`
	Expected   ValueType `json:"expected,omitempty"`
	Actual     string    `json:"actual,omitempty"`
	Prefix     string    `json:"prefix,omitempty"`
	Index      string    `json:"index,omitempty"`
}

// ValidationWarning is the wire-format forward-compat / hygiene
// record.
type ValidationWarning struct {
	Kind         string `json:"kind"`
	Axis         string `json:"axis,omitempty"`
	Key          string `json:"key,omitempty"`
	SoftCapBytes int    `json:"soft_cap_bytes,omitempty"`
	ActualBytes  int    `json:"actual_bytes,omitempty"`
	Tag          string `json:"tag,omitempty"`
	// Q11: mirror substrate CR-14 reserved-metadata warnings.
	// Wire shape matches `src/ffi/schema.rs::validation_warning_to_wire`
	// — `metadata_reserved_prefix` carries both `key` (full key
	// the user wrote) AND `prefix` (the reserved prefix that
	// matched). The `key` field is reused; `Prefix` is the new
	// JSON-tagged field below.
	Prefix string `json:"prefix,omitempty"`
}

// ValidationReport is the validator's output.
type ValidationReport struct {
	Errors   []SchemaError       `json:"errors"`
	Warnings []ValidationWarning `json:"warnings"`
}

// IsClean returns true iff there are zero errors and zero warnings.
func (r ValidationReport) IsClean() bool {
	return len(r.Errors) == 0 && len(r.Warnings) == 0
}

// IsValid returns true iff there are zero errors. Warnings are allowed.
func (r ValidationReport) IsValid() bool {
	return len(r.Errors) == 0
}

// ============================================================================
// Validator
// ============================================================================

func axisEntry(schema *AxisSchema, axis TaxonomyAxis) *SchemaAxisEntry {
	switch axis {
	case AxisHardware:
		return &schema.Hardware
	case AxisSoftware:
		return &schema.Software
	case AxisDevices:
		return &schema.Devices
	case AxisDataforts:
		return &schema.Dataforts
	}
	return nil
}

func isAllDigits(s string) bool {
	if s == "" {
		return false
	}
	for _, c := range s {
		if c < '0' || c > '9' {
			return false
		}
	}
	return true
}

// N-5: substrate `ValueType::Number` is unsigned u64. Rust uses
// `value.parse::<u64>()` (schema.rs:704), which accepts `+1` and
// rejects negatives, non-ASCII digits, and values exceeding
// `u64::MAX`. Python locks the same accepted-set via the
// `_U64_LITERAL = re.compile(r"^\+?[0-9]+$")` regex plus an
// explicit ceiling check (R4 / capability_schema.py:332,385).
//
// The pre-fix Go shape used `isIntegerLiteral`, which admitted a
// leading `-` AND any number of digits — `software.model.0.context_length=-1`
// or `=18446744073709551616` would validate clean in Go and error
// in Rust/Python. Mirror Python: ASCII digits with optional leading
// `+`, then `strconv.ParseUint` for the range check.
func isU64Literal(s string) bool {
	if s == "" {
		return false
	}
	body := s
	if body[0] == '+' {
		body = body[1:]
	}
	if !isAllDigits(body) {
		return false
	}
	if _, err := strconv.ParseUint(body, 10, 64); err != nil {
		return false
	}
	return true
}

// N-5: substrate `schema.rs:616` parses the indexed-collection
// position as `idx.parse::<u32>()`. Pre-fix `isAllDigits` admitted
// `99999999999999` which overflows u32. Mirror Rust strictly.
func isU32Literal(s string) bool {
	if s == "" {
		return false
	}
	body := s
	if body[0] == '+' {
		body = body[1:]
	}
	if !isAllDigits(body) {
		return false
	}
	if _, err := strconv.ParseUint(body, 10, 32); err != nil {
		return false
	}
	return true
}

func checkValue(
	entry SchemaKeyEntry,
	observedType ValueType,
	observedValue *string,
	axis TaxonomyAxis,
	errors *[]SchemaError,
) {
	if entry.ValueType == ValueTypePresence {
		if observedType != ValueTypePresence {
			actual := ""
			if observedValue != nil {
				actual = *observedValue
			}
			*errors = append(*errors, SchemaError{
				Kind:     "type_mismatch",
				Axis:     string(axis),
				Key:      entry.Key,
				Expected: ValueTypePresence,
				Actual:   actual,
			})
		}
		return
	}
	if observedValue == nil {
		*errors = append(*errors, SchemaError{
			Kind:     "type_mismatch",
			Axis:     string(axis),
			Key:      entry.Key,
			Expected: entry.ValueType,
			Actual:   "<no value>",
		})
		return
	}
	v := *observedValue
	parses := false
	switch entry.ValueType {
	case ValueTypeNumber:
		parses = isU64Literal(v)
	case ValueTypeString, ValueTypeEnumeration, ValueTypeCsv:
		parses = v != ""
	case ValueTypeBool:
		parses = v == "true" || v == "false"
	}
	if !parses {
		*errors = append(*errors, SchemaError{
			Kind:     "type_mismatch",
			Axis:     string(axis),
			Key:      entry.Key,
			Expected: entry.ValueType,
			Actual:   v,
		})
	}
}

func validateAxisKey(
	axis TaxonomyAxis,
	key string,
	observedType ValueType,
	observedValue *string,
	schema *AxisSchema,
	errors *[]SchemaError,
	warnings *[]ValidationWarning,
	tagWire string,
) {
	entry := axisEntry(schema, axis)
	if entry == nil {
		return
	}
	for _, e := range entry.Keys {
		if e.Key == key {
			checkValue(e, observedType, observedValue, axis, errors)
			return
		}
	}
	for _, shape := range entry.Shapes {
		if !strings.HasPrefix(key, shape.Prefix) {
			continue
		}
		rest := key[len(shape.Prefix):]
		switch shape.Kind {
		case SchemaShapeIndexedCollection:
			dot := strings.IndexByte(rest, '.')
			if dot < 0 {
				continue
			}
			idx := rest[:dot]
			sub := rest[dot+1:]
			if !isU32Literal(idx) {
				*errors = append(*errors, SchemaError{
					Kind:   "index_malformed",
					Axis:   string(axis),
					Prefix: shape.Prefix,
					Index:  idx,
					Tag:    tagWire,
				})
				return
			}
			for _, sk := range shape.SubKeys {
				if sk.Key == sub {
					checkValue(sk, observedType, observedValue, axis, errors)
					return
				}
			}
			*warnings = append(*warnings, ValidationWarning{
				Kind: "unknown_key",
				Axis: string(axis),
				Key:  key,
			})
			return
		case SchemaShapeKeyedMap:
			if rest != "" {
				synth := SchemaKeyEntry{Key: shape.Prefix, ValueType: shape.KeyedValueType}
				checkValue(synth, observedType, observedValue, axis, errors)
				return
			}
		}
	}
	*warnings = append(*warnings, ValidationWarning{
		Kind: "unknown_key",
		Axis: string(axis),
		Key:  key,
	})
}

// ValidateCapabilities runs the canonical validator against the
// canonical AxisSchemaCanonical.
func ValidateCapabilities(caps CapabilitySetWire) ValidationReport {
	return ValidateCapabilitiesAgainst(caps, &AxisSchemaCanonical)
}

// ValidateCapabilitiesAgainst runs the validator against a custom schema.
func ValidateCapabilitiesAgainst(
	caps CapabilitySetWire,
	schema *AxisSchema,
) ValidationReport {
	// R3: nil schema → fall back to the canonical schema. The
	// validateAxisKey / metadata-reservation walks below all
	// dereference `schema`; passing nil would panic. Coercing
	// to the canonical schema matches what `ValidateCapabilities`
	// (the public entry point with no schema arg) does and gives
	// callers who pass nil the same shape as the no-arg variant.
	if schema == nil {
		schema = &AxisSchemaCanonical
	}
	errors := []SchemaError{}
	warnings := []ValidationWarning{}

	for _, wire := range caps.Tags {
		tag, err := TagFromString(wire)
		if err != nil {
			continue
		}
		switch tag.Kind {
		case TagKindAxisPresent:
			validateAxisKey(
				tag.Axis, tag.Key, ValueTypePresence, nil,
				schema, &errors, &warnings, wire,
			)
		case TagKindAxisValue:
			v := tag.Value
			validateAxisKey(
				tag.Axis, tag.Key, ValueTypeString, &v,
				schema, &errors, &warnings, wire,
			)
		case TagKindReserved:
			// Reserved-prefix tags pass through unchecked.
		case TagKindLegacy:
			warnings = append(warnings, ValidationWarning{
				Kind: "legacy_tag",
				Tag:  tag.Raw,
			})
		}
	}

	// Q11: metadata-key reservation check. Mirror substrate CR-14
	// (and TS P2-H, Py P2-L). The schema declares
	// `MetadataReserved` (exact-match) and `MetadataReservedPrefixes`
	// (prefix-match) but pre-fix the Go validator never consulted
	// either list — a user's `WithMetadata("intent", …)` smuggling
	// onto a scheduler-reserved key emitted no warning client-side
	// even though Rust / TS / Python now flag it.
	for k := range caps.Metadata {
		matched := false
		for _, reserved := range schema.MetadataReserved {
			if k == reserved {
				warnings = append(warnings, ValidationWarning{
					Kind: "metadata_reserved_key",
					Key:  k,
				})
				matched = true
				break
			}
		}
		if matched {
			continue
		}
		for _, prefix := range schema.MetadataReservedPrefixes {
			if strings.HasPrefix(k, prefix) {
				warnings = append(warnings, ValidationWarning{
					Kind:   "metadata_reserved_prefix",
					Key:    k,
					Prefix: prefix,
				})
				break
			}
		}
	}

	metadataBytes := 0
	for k, v := range caps.Metadata {
		metadataBytes += len(k) + len(v)
	}
	if metadataBytes > MetadataSoftCapBytes {
		warnings = append(warnings, ValidationWarning{
			Kind:         "metadata_oversize",
			SoftCapBytes: MetadataSoftCapBytes,
			ActualBytes:  metadataBytes,
		})
	}

	return ValidationReport{Errors: errors, Warnings: warnings}
}
