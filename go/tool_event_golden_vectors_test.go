// Cross-language ToolEvent envelope round-trip fixture test (plan T-2).
//
// Loads `net/crates/net/tests/cross_lang_tool_formats/tool_event_vectors.json`
// and asserts that for each case the Go ToolEvent round-trips
// through encoding/json byte-equal to the wire shape. Matches the
// Rust / Node / Python verifiers.

package net

import (
	"encoding/json"
	"os"
	"path/filepath"
	"reflect"
	"runtime"
	"testing"
)

type fixtureEventCase struct {
	Name       string          `json:"name"`
	Wire       json.RawMessage `json:"wire"`
	IsTerminal bool            `json:"is_terminal"`
}

type fixtureEvents struct {
	Cases []fixtureEventCase `json:"cases"`
}

func eventFixturePath(t *testing.T) string {
	t.Helper()
	_, thisFile, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("runtime.Caller failed")
	}
	dir := filepath.Dir(thisFile)
	// go/ lives at the repo root; fixtures live at
	// net/crates/net/tests/cross_lang_tool_formats/.
	return filepath.Join(dir, "..", "net", "crates", "net", "tests", "cross_lang_tool_formats", "tool_event_vectors.json")
}

func loadEventFixture(t *testing.T) fixtureEvents {
	t.Helper()
	raw, err := os.ReadFile(eventFixturePath(t))
	if err != nil {
		t.Fatalf("read fixture: %v", err)
	}
	var f fixtureEvents
	if err := json.Unmarshal(raw, &f); err != nil {
		t.Fatalf("unmarshal fixture: %v", err)
	}
	return f
}

func TestToolEventRoundTripMatchesGoldenVectors(t *testing.T) {
	f := loadEventFixture(t)
	if len(f.Cases) == 0 {
		t.Fatal("no cases in fixture")
	}
	for _, c := range f.Cases {
		c := c
		t.Run(c.Name, func(t *testing.T) {
			// Deserialize wire → ToolEvent.
			var event ToolEvent
			if err := json.Unmarshal(c.Wire, &event); err != nil {
				t.Fatalf("unmarshal wire: %v", err)
			}

			// IsTerminal contract.
			if event.IsTerminal() != c.IsTerminal {
				t.Errorf("IsTerminal() = %v, want %v", event.IsTerminal(), c.IsTerminal)
			}

			// Re-serialize and deep-compare via parsed-JSON
			// normalization so int/float64 + map ordering don't
			// false-positive a mismatch.
			out, err := json.Marshal(event)
			if err != nil {
				t.Fatalf("marshal event: %v", err)
			}
			var got interface{}
			if err := json.Unmarshal(out, &got); err != nil {
				t.Fatalf("re-unmarshal: %v", err)
			}
			var want interface{}
			if err := json.Unmarshal(c.Wire, &want); err != nil {
				t.Fatalf("unmarshal wire (normalize): %v", err)
			}
			if !reflect.DeepEqual(got, want) {
				t.Errorf("round-tripped JSON differs from wire\n got:  %v\n want: %v", got, want)
			}
		})
	}
}
