// JSON-encoder pins for the Phase 6c capability-aggregation surface.
//
// The Rust core's `serde_json::to_string` produces specific byte
// sequences pinned by `serde_shapes_match_cross_binding_wire_format`
// in the Rust capability_aggregation tests. The Go encoder in this
// file must produce the same JSON for the C ABI to deserialize
// correctly. This file pins the Go side; both sides move together
// when the wire shape changes.
//
// Pure marshaling — no cgo round-trip, runs as part of plain
// `go test ./...` without a built `libnet`.

package net

import (
	"encoding/json"
	"testing"
)

// ──────────────────────────────────────────────────────────────────
// TagMatcher
// ──────────────────────────────────────────────────────────────────

func TestMatcherEncodings(t *testing.T) {
	cases := []struct {
		name string
		m    TagMatcher
		want string
	}{
		{
			"Exact",
			MatchExact("software.python=3.11"),
			`{"kind":"exact","value":"software.python=3.11"}`,
		},
		{
			"Prefix",
			MatchPrefix("hardware.gpu"),
			`{"kind":"prefix","value":"hardware.gpu"}`,
		},
		{
			"Axis",
			MatchAxis(AxisHardware),
			`{"kind":"axis","axis":"hardware"}`,
		},
		{
			"AxisKey",
			MatchAxisKey(AxisHardware, "gpu.count"),
			`{"kind":"axis_key","axis":"hardware","key":"gpu.count"}`,
		},
		{
			"Regex",
			MatchRegex("^a$"),
			`{"kind":"regex","pattern":"^a$"}`,
		},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			b, err := json.Marshal(c.m)
			if err != nil {
				t.Fatalf("marshal: %v", err)
			}
			if got := string(b); got != c.want {
				t.Errorf("got %s, want %s", got, c.want)
			}
		})
	}
}

func TestMatcherVersionRangeMinOnly(t *testing.T) {
	min := "3.10.0"
	b, _ := json.Marshal(MatchVersionRange("software.python", &min, nil))
	if got, want := string(b),
		`{"kind":"version_range","axis_key":"software.python","min":"3.10.0"}`; got != want {
		t.Errorf("got %s, want %s", got, want)
	}
}

// ──────────────────────────────────────────────────────────────────
// GroupBy
// ──────────────────────────────────────────────────────────────────

func TestGroupByEncodings(t *testing.T) {
	cases := []struct {
		name string
		gb   GroupBy
		want string
	}{
		{"Class", GroupByClass(), `{"kind":"class"}`},
		{"State", GroupByState(), `{"kind":"state"}`},
		{"Region", GroupByRegion(), `{"kind":"region"}`},
		{"Publisher", GroupByPublisher(), `{"kind":"publisher"}`},
		{
			"TagStem",
			GroupByTagStem("hardware.gpu"),
			`{"kind":"tag_stem","prefix":"hardware.gpu"}`,
		},
		{
			"TagValue",
			GroupByTagValue(AxisSoftware, "python"),
			`{"kind":"tag_value","axis":"software","key":"python"}`,
		},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			b, _ := json.Marshal(c.gb)
			if got := string(b); got != c.want {
				t.Errorf("got %s, want %s", got, c.want)
			}
		})
	}
}

// ──────────────────────────────────────────────────────────────────
// Aggregation
// ──────────────────────────────────────────────────────────────────

func TestAggregationEncodings(t *testing.T) {
	cases := []struct {
		name string
		agg  Aggregation
		want string
	}{
		{"Count", AggCount(), `{"kind":"count"}`},
		{
			"DistinctPublishers",
			AggDistinctPublishers(),
			`{"kind":"distinct_publishers"}`,
		},
		{
			"DistinctValues",
			AggDistinctValues(AxisSoftware, "python"),
			`{"kind":"distinct_values","axis":"software","key":"python"}`,
		},
		{
			"SumNumericTag",
			AggSumNumericTag("hardware.gpu.count"),
			`{"kind":"sum_numeric_tag","axis_key":"hardware.gpu.count"}`,
		},
		{
			"MinNumericTag",
			AggMinNumericTag("hardware.gpu.count"),
			`{"kind":"min_numeric_tag","axis_key":"hardware.gpu.count"}`,
		},
		{
			"MaxNumericTag",
			AggMaxNumericTag("hardware.gpu.count"),
			`{"kind":"max_numeric_tag","axis_key":"hardware.gpu.count"}`,
		},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			b, _ := json.Marshal(c.agg)
			if got := string(b); got != c.want {
				t.Errorf("got %s, want %s", got, c.want)
			}
		})
	}
}

// ──────────────────────────────────────────────────────────────────
// CapacityQuery — round-trip + nil-field handling
// ──────────────────────────────────────────────────────────────────

func TestCapacityQueryRoundTripsRustWireShape(t *testing.T) {
	matcher := MatchPrefix("hardware.gpu")
	maxRtt := uint32(50)
	q := CapacityQuery{
		Matcher:    &matcher,
		GroupBy:    GroupByTagStem("hardware.gpu"),
		MaxRTTMs:   &maxRtt,
		SumAxisKey: "hardware.gpu.count",
		Limit:      5,
	}
	b, err := json.Marshal(q)
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}
	var got map[string]any
	if err := json.Unmarshal(b, &got); err != nil {
		t.Fatalf("re-parse: %v", err)
	}
	if got["limit"].(float64) != 5 {
		t.Errorf("limit: got %v", got["limit"])
	}
	if got["max_rtt_ms"].(float64) != 50 {
		t.Errorf("max_rtt_ms: got %v", got["max_rtt_ms"])
	}
	if got["sum_axis_key"].(string) != "hardware.gpu.count" {
		t.Errorf("sum_axis_key: got %v", got["sum_axis_key"])
	}
	m := got["matcher"].(map[string]any)
	if m["kind"].(string) != "prefix" || m["value"].(string) != "hardware.gpu" {
		t.Errorf("matcher: got %v", m)
	}
	g := got["group_by"].(map[string]any)
	if g["kind"].(string) != "tag_stem" || g["prefix"].(string) != "hardware.gpu" {
		t.Errorf("group_by: got %v", g)
	}
}

func TestCapacityQueryOmitsAbsentOptionalFields(t *testing.T) {
	q := CapacityQuery{GroupBy: GroupByRegion(), Limit: 0}
	b, _ := json.Marshal(q)
	var got map[string]any
	if err := json.Unmarshal(b, &got); err != nil {
		t.Fatalf("re-parse: %v", err)
	}
	// Optional fields are omitted (not nulled) per Go's omitempty
	// convention. The Rust core's CapacityQuery has serde defaults
	// for these — `Option<TagMatcher>` defaults None, `max_rtt_ms`
	// defaults None — so omission is equivalent to null at the
	// Rust boundary.
	if _, ok := got["matcher"]; ok {
		t.Errorf("matcher should be omitted; got %v", got["matcher"])
	}
	if _, ok := got["max_rtt_ms"]; ok {
		t.Errorf("max_rtt_ms should be omitted")
	}
	if got["limit"].(float64) != 0 {
		t.Errorf("limit: got %v", got["limit"])
	}
}
