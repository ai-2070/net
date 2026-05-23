// Package net — capability-aggregation surface for the C ABI
// exported by `bindings/go/compute-ffi`. Phase 6c of
// `MULTIFOLD_PHASE_6C_CAPACITY_AGGREGATION.md`.
//
// Two operations wrap `Fold::aggregate` and `Fold::capacity_ranking`
// on the local `CapabilityFold`:
//
//   - `CapabilityAggregate(mesh, matcher, groupBy, agg) []AggregateRow`
//     — bucket-and-count primitive.
//   - `CapabilityCapacityRanking(mesh, query, rttMap) []CapacityRow`
//     — state-broken-down materialized view with latency gate,
//     optional summed capacity, sort, truncate.
//
// # Wire shape
//
// Both calls cross the C ABI as JSON strings. The Rust core derives
// serde Serialize/Deserialize on every aggregation type with the
// cross-binding-pinned wire format
// (`serde_shapes_match_cross_binding_wire_format` in the core);
// matching Go structs use JSON tags that map onto the Rust shape.
//
// For RTT, the C ABI accepts a JSON-encoded `[{node_id, rtt_ms}, ...]`
// array — operators typically have the RTT map cached from the
// proximity graph anyway, and the materialized-map shape avoids the
// CGo-callback round-trip cost (per the plan's "Risk: rtt_lookup
// closure called once per candidate" rationale).
package net

/*
#include <stdint.h>
#include <stdlib.h>

// Opaque mesh-arc handle from `net_mesh_arc_clone` (see redex.go for
// the canonical forward-declaration pattern).
typedef struct ArcMeshNode ArcMeshNode;

// Imported FFI surface from `net-compute-ffi`. Both functions return
// a heap-allocated UTF-8 string the caller frees with
// `net_compute_free_cstring`. cgo preambles are per-file, so the
// free-fn declaration is repeated here even though other files in
// the package also declare it.
extern char* net_capability_aggregate(
    const ArcMeshNode* mesh_arc,
    const char* matcher_json,
    const char* group_by_json,
    const char* aggregation_json
);
extern char* net_capability_capacity_ranking(
    const ArcMeshNode* mesh_arc,
    const char* query_json,
    const char* rtt_map_json
);
extern void net_compute_free_cstring(char* s);
*/
import "C"

import (
	"encoding/json"
	"fmt"
	"unsafe"
)

// =====================================================================
// Types — discriminated unions encoded as JSON tagged unions
// =====================================================================

// TaxonomyAxis matches the Rust core's `TaxonomyAxis` enum.
type TaxonomyAxis string

const (
	AxisHardware  TaxonomyAxis = "hardware"
	AxisSoftware  TaxonomyAxis = "software"
	AxisDevices   TaxonomyAxis = "devices"
	AxisDataforts TaxonomyAxis = "dataforts"
)

// TagMatcher is a discriminated union picking which entries the
// aggregation walks. Construct via the package's `Match*` helpers.
// Marshals to the JSON shape `{"kind": "<variant>", ...}`.
type TagMatcher struct {
	// `Kind` discriminator — one of "exact", "prefix", "axis",
	// "axis_key", "regex", "version_range".
	Kind string `json:"kind"`
	// `Value` is set for Exact, Prefix.
	Value string `json:"value,omitempty"`
	// `Axis` is set for Axis, AxisKey.
	Axis TaxonomyAxis `json:"axis,omitempty"`
	// `Key` is set for AxisKey.
	Key string `json:"key,omitempty"`
	// `Pattern` is set for Regex.
	Pattern string `json:"pattern,omitempty"`
	// `AxisKey` is set for VersionRange.
	AxisKey string `json:"axis_key,omitempty"`
	// `Min` / `Max` are set for VersionRange.
	Min *string `json:"min,omitempty"`
	Max *string `json:"max,omitempty"`
}

// MatchExact returns a matcher for the literal canonical tag.
func MatchExact(value string) TagMatcher {
	return TagMatcher{Kind: "exact", Value: value}
}

// MatchPrefix returns a matcher for any tag starting with `value`.
func MatchPrefix(value string) TagMatcher {
	return TagMatcher{Kind: "prefix", Value: value}
}

// MatchAxis returns a matcher for any tag in `axis`.
func MatchAxis(axis TaxonomyAxis) TagMatcher {
	return TagMatcher{Kind: "axis", Axis: axis}
}

// MatchAxisKey returns a matcher for tags with `(axis, key)` regardless
// of value.
func MatchAxisKey(axis TaxonomyAxis, key string) TagMatcher {
	return TagMatcher{Kind: "axis_key", Axis: axis, Key: key}
}

// MatchRegex returns a matcher for canonical-form regex.
func MatchRegex(pattern string) TagMatcher {
	return TagMatcher{Kind: "regex", Pattern: pattern}
}

// MatchVersionRange returns a matcher for semver ranges against an
// `AxisValue` tag's value. Pass `nil` for `min`/`max` to leave that
// bound unconstrained.
func MatchVersionRange(axisKey string, min, max *string) TagMatcher {
	return TagMatcher{Kind: "version_range", AxisKey: axisKey, Min: min, Max: max}
}

// GroupBy is a discriminated union of bucket-derivation strategies.
// Marshals to `{"kind": "<variant>", ...}`.
type GroupBy struct {
	Kind   string       `json:"kind"`
	Prefix string       `json:"prefix,omitempty"`
	Axis   TaxonomyAxis `json:"axis,omitempty"`
	Key    string       `json:"key,omitempty"`
}

func GroupByClass() GroupBy     { return GroupBy{Kind: "class"} }
func GroupByState() GroupBy     { return GroupBy{Kind: "state"} }
func GroupByRegion() GroupBy    { return GroupBy{Kind: "region"} }
func GroupByPublisher() GroupBy { return GroupBy{Kind: "publisher"} }

// GroupByTagStem buckets by the dotted segment after `prefix`.
func GroupByTagStem(prefix string) GroupBy {
	return GroupBy{Kind: "tag_stem", Prefix: prefix}
}

// GroupByTagValue buckets by the value of an `AxisValue` tag matching
// `(axis, key)`.
func GroupByTagValue(axis TaxonomyAxis, key string) GroupBy {
	return GroupBy{Kind: "tag_value", Axis: axis, Key: key}
}

// Aggregation is a discriminated union of per-bucket reductions.
// Marshals to `{"kind": "<variant>", ...}`.
type Aggregation struct {
	Kind    string       `json:"kind"`
	Axis    TaxonomyAxis `json:"axis,omitempty"`
	Key     string       `json:"key,omitempty"`
	AxisKey string       `json:"axis_key,omitempty"`
}

func AggCount() Aggregation              { return Aggregation{Kind: "count"} }
func AggDistinctPublishers() Aggregation { return Aggregation{Kind: "distinct_publishers"} }

// AggDistinctValues counts unique `(axis, key)` values per bucket.
func AggDistinctValues(axis TaxonomyAxis, key string) Aggregation {
	return Aggregation{Kind: "distinct_values", Axis: axis, Key: key}
}

// AggSumNumericTag sums parseable u64 values of `<axis_key>=<n>` tags.
func AggSumNumericTag(axisKey string) Aggregation {
	return Aggregation{Kind: "sum_numeric_tag", AxisKey: axisKey}
}

// AggMinNumericTag / AggMaxNumericTag track the min/max of parseable
// numeric tag values per bucket.
func AggMinNumericTag(axisKey string) Aggregation {
	return Aggregation{Kind: "min_numeric_tag", AxisKey: axisKey}
}
func AggMaxNumericTag(axisKey string) Aggregation {
	return Aggregation{Kind: "max_numeric_tag", AxisKey: axisKey}
}

// CapacityQuery composes a matcher + groupBy + optional RTT filter +
// optional summed-capacity axis into a single
// `CapabilityCapacityRanking` call.
type CapacityQuery struct {
	// `Matcher` may be nil — walks every entry when absent.
	Matcher *TagMatcher `json:"matcher,omitempty"`
	GroupBy GroupBy     `json:"group_by"`
	// `MaxRTTMs` may be nil — disables the RTT filter regardless of
	// the `rttMap` argument.
	MaxRTTMs *uint32 `json:"max_rtt_ms,omitempty"`
	// `SumAxisKey` may be empty — disables the summed_capacity column.
	SumAxisKey string `json:"sum_axis_key,omitempty"`
	// `Limit` top-N buckets by `available` descending. 0 = no truncate.
	Limit int `json:"limit"`
}

// AggregateRow is one row of a `CapabilityAggregate` result.
type AggregateRow struct {
	Bucket string `json:"bucket"`
	Value  uint64 `json:"value"`
}

// CapacityRow is one row of a `CapabilityCapacityRanking` result.
type CapacityRow struct {
	Bucket string `json:"bucket"`
	Idle   uint64 `json:"idle"`
	Busy   uint64 `json:"busy"`
	// `Reserved` is the number of `Reserved`-state entries.
	Reserved uint64 `json:"reserved"`
	// `Available = Idle + Busy + Reserved`. Faulty excluded upstream.
	Available uint64 `json:"available"`
	// `SummedCapacity` is non-nil when `Query.SumAxisKey` was set.
	SummedCapacity *uint64 `json:"summed_capacity,omitempty"`
}

// RttEntry is one entry of the materialized RTT map passed to
// `CapabilityCapacityRanking`. JSON-encoded as
// `[{"node_id": <u64>, "rtt_ms": <u32>}, ...]`.
type RttEntry struct {
	NodeID uint64 `json:"node_id"`
	RttMs  uint32 `json:"rtt_ms"`
}

// =====================================================================
// Public API
// =====================================================================

// CapabilityAggregate runs `Fold::aggregate` against the publisher's
// capability fold and returns a list of bucketed rows sorted lex by
// bucket key. `matcher == nil` walks every entry.
//
// `meshArc` must be a pointer obtained from `net_mesh_arc_clone`.
func CapabilityAggregate(
	meshArc MeshArcPtr,
	matcher *TagMatcher,
	groupBy GroupBy,
	agg Aggregation,
) ([]AggregateRow, error) {
	var matcherCStr *C.char
	if matcher != nil {
		b, err := json.Marshal(matcher)
		if err != nil {
			return nil, fmt.Errorf("marshal matcher: %w", err)
		}
		matcherCStr = C.CString(string(b))
		defer C.free(unsafe.Pointer(matcherCStr))
	}
	gbBytes, err := json.Marshal(groupBy)
	if err != nil {
		return nil, fmt.Errorf("marshal groupBy: %w", err)
	}
	aggBytes, err := json.Marshal(agg)
	if err != nil {
		return nil, fmt.Errorf("marshal aggregation: %w", err)
	}
	gbCStr := C.CString(string(gbBytes))
	defer C.free(unsafe.Pointer(gbCStr))
	aggCStr := C.CString(string(aggBytes))
	defer C.free(unsafe.Pointer(aggCStr))

	out := C.net_capability_aggregate(
		(*C.ArcMeshNode)(unsafe.Pointer(meshArc)),
		matcherCStr,
		gbCStr,
		aggCStr,
	)
	if out == nil {
		return nil, fmt.Errorf("net_capability_aggregate returned NULL " +
			"(mesh handle invalid or JSON parse failure)")
	}
	defer C.net_compute_free_cstring(out)

	js := C.GoString(out)
	var rows []AggregateRow
	if err := json.Unmarshal([]byte(js), &rows); err != nil {
		return nil, fmt.Errorf("unmarshal rows: %w", err)
	}
	return rows, nil
}

// CapabilityCapacityRanking runs `Fold::capacity_ranking` and returns
// the per-bucket state breakdown + optional summed capacity. Rows are
// sorted by `Available` descending, ties broken on bucket asc.
//
// `rttMap` may be nil/empty — disables the RTT filter regardless of
// `query.MaxRTTMs`.
func CapabilityCapacityRanking(
	meshArc MeshArcPtr,
	query CapacityQuery,
	rttMap map[uint64]uint32,
) ([]CapacityRow, error) {
	qBytes, err := json.Marshal(query)
	if err != nil {
		return nil, fmt.Errorf("marshal query: %w", err)
	}
	qCStr := C.CString(string(qBytes))
	defer C.free(unsafe.Pointer(qCStr))

	var rttCStr *C.char
	if len(rttMap) > 0 {
		entries := make([]RttEntry, 0, len(rttMap))
		for id, rtt := range rttMap {
			entries = append(entries, RttEntry{NodeID: id, RttMs: rtt})
		}
		rttBytes, err := json.Marshal(entries)
		if err != nil {
			return nil, fmt.Errorf("marshal rttMap: %w", err)
		}
		rttCStr = C.CString(string(rttBytes))
		defer C.free(unsafe.Pointer(rttCStr))
	}

	out := C.net_capability_capacity_ranking(
		(*C.ArcMeshNode)(unsafe.Pointer(meshArc)),
		qCStr,
		rttCStr,
	)
	if out == nil {
		return nil, fmt.Errorf("net_capability_capacity_ranking returned NULL " +
			"(mesh handle invalid or JSON parse failure)")
	}
	defer C.net_compute_free_cstring(out)

	js := C.GoString(out)
	var rows []CapacityRow
	if err := json.Unmarshal([]byte(js), &rows); err != nil {
		return nil, fmt.Errorf("unmarshal rows: %w", err)
	}
	return rows, nil
}
