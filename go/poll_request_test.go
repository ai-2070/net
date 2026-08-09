// The poll request Go sends must carry everything the caller asked for.
//
// Go hard-coded limit + cursor, which happened to be the only shape the
// C parser read — while `net_poll` documented an `ordering` field. A
// caller asking for cross-shard ordering got an unordered response and
// a success code, on both sides of the boundary. These pin the request
// bytes; they are pure Go and run without the native library.

package net

import (
	"encoding/json"
	"strings"
	"testing"
)

// decode strips the trailing NUL and parses what Go would hand to C.
func decode(t *testing.T, opts PollOptions) map[string]any {
	t.Helper()
	buf := buildPollRequest(opts)
	if buf[len(buf)-1] != 0 {
		t.Fatal("request must be null-terminated for the C boundary")
	}
	var got map[string]any
	if err := json.Unmarshal(buf[:len(buf)-1], &got); err != nil {
		t.Fatalf("request is not valid JSON: %v\n%s", err, buf[:len(buf)-1])
	}
	return got
}

func TestBuildPollRequest_MinimalShapeUnchanged(t *testing.T) {
	// The Poll(limit, cursor) path must still produce exactly what it
	// did before PollWith existed — no new keys, so an older core that
	// rejects unknown keys keeps working.
	got := decode(t, PollOptions{Limit: 100})
	if len(got) != 1 || got["limit"] != float64(100) {
		t.Fatalf("want only {\"limit\":100}, got %v", got)
	}

	got = decode(t, PollOptions{Limit: 10, Cursor: "abc"})
	if len(got) != 2 || got["cursor"] != "abc" {
		t.Fatalf("want limit+cursor only, got %v", got)
	}
}

func TestBuildPollRequest_CarriesOrdering(t *testing.T) {
	got := decode(t, PollOptions{Limit: 10, Ordering: "insertion_ts"})
	if got["ordering"] != "insertion_ts" {
		t.Fatalf("ordering did not reach the request: %v", got)
	}
}

func TestBuildPollRequest_CarriesFilterAsAnObject(t *testing.T) {
	// The C parser expects a nested object, not a JSON string.
	got := decode(t, PollOptions{
		Limit:  10,
		Filter: `{"path":"kind","value":"lidar"}`,
	})
	filter, ok := got["filter"].(map[string]any)
	if !ok {
		t.Fatalf("filter must embed as an object, got %T: %v", got["filter"], got)
	}
	if filter["path"] != "kind" {
		t.Fatalf("filter contents lost: %v", filter)
	}
}

func TestBuildPollRequest_CarriesShards(t *testing.T) {
	got := decode(t, PollOptions{Limit: 10, Shards: []uint16{0, 2, 3}})
	shards, ok := got["shards"].([]any)
	if !ok || len(shards) != 3 {
		t.Fatalf("shards did not reach the request: %v", got)
	}
	if shards[0] != float64(0) || shards[2] != float64(3) {
		t.Fatalf("shard ids wrong: %v", shards)
	}
}

func TestBuildPollRequest_AllFieldsTogether(t *testing.T) {
	got := decode(t, PollOptions{
		Limit:    5,
		Cursor:   "c1",
		Ordering: "none",
		Filter:   `{"path":"a","value":"b"}`,
		Shards:   []uint16{1},
	})
	for _, key := range []string{"limit", "cursor", "ordering", "filter", "shards"} {
		if _, ok := got[key]; !ok {
			t.Errorf("key %q missing from %v", key, got)
		}
	}
}

func TestPollOptions_RejectsUnknownOrdering(t *testing.T) {
	// Matches the C parser, which now refuses these rather than
	// defaulting to "none".
	for _, bad := range []string{
		"InsertionTs", // the spelling the old C docs used
		"insertionTs",
		"timestamp",
		"NONE",
		" none",
	} {
		opts := PollOptions{Limit: 10, Ordering: bad}
		err := opts.validate()
		if err == nil {
			t.Fatalf("ordering %q must be rejected", bad)
		}
		if !strings.Contains(err.Error(), bad) {
			t.Errorf("error for %q should quote the value, got %v", bad, err)
		}
	}
}

func TestPollOptions_EmptyOrderingIsTheDefault(t *testing.T) {
	if err := (&PollOptions{Limit: 10}).validate(); err != nil {
		t.Fatalf("unset ordering must be allowed, got %v", err)
	}
	got := decode(t, PollOptions{Limit: 10})
	if _, present := got["ordering"]; present {
		t.Fatalf("unset ordering must be omitted, got %v", got)
	}
}

// A malformed Filter must be refused by name, before the cgo call.
//
// Filter is spliced into the request body verbatim — it has to be, so
// the C parser receives a nested object rather than a string. That
// makes it the one PollOptions field where a bad value corrupts the
// whole request rather than just itself: `{"path":` yields malformed
// JSON, and `"lidar"` or `[1,2]` yield well-formed JSON the parser then
// refuses. Both came back as a bare InvalidJson naming nothing, while
// Ordering next to it already refused its bad values by name.
func TestPollOptions_RejectsMalformedFilter(t *testing.T) {
	for _, bad := range []string{
		`{"path":`,                 // truncated — corrupts the request
		`{"path":"kind",}`,         // trailing comma
		`"lidar"`,                  // a scalar, not an object
		`[{"path":"kind"}]`,        // an array, not an object
		`42`,                       // a number
		`null`,                     // unmarshals to a nil map, no error
		`{"path":"kind"} trailing`, // junk after the object
	} {
		opts := PollOptions{Limit: 10, Filter: bad}
		err := opts.validate()
		if err == nil {
			t.Fatalf("filter %q must be rejected", bad)
		}
		// The message has to name the field and the value — the point
		// of checking here rather than letting C answer.
		if !strings.Contains(err.Error(), "filter") {
			t.Fatalf("error must name the filter, got %v", err)
		}
	}
}

func TestPollOptions_AcceptsWellFormedFilter(t *testing.T) {
	for _, ok := range []string{
		`{}`,
		`{"path":"kind","value":"lidar"}`,
		`{"and":[{"path":"a","value":1},{"path":"b","value":2}]}`,
	} {
		opts := PollOptions{Limit: 10, Filter: ok}
		if err := opts.validate(); err != nil {
			t.Fatalf("filter %q must be accepted, got %v", ok, err)
		}
	}

	// And an accepted filter still lands in the request as a nested
	// object, not a string.
	got := decode(t, PollOptions{Limit: 10, Filter: `{"path":"kind","value":"lidar"}`})
	filter, isObject := got["filter"].(map[string]any)
	if !isObject {
		t.Fatalf("filter must be embedded as an object, got %T", got["filter"])
	}
	if filter["path"] != "kind" {
		t.Fatalf("filter contents must survive, got %v", filter)
	}
}
