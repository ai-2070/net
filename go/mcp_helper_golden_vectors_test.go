// Cross-language MCP bridge helper-parity fixture test
// (`MCP_BRIDGE_SDK_PLAN.md` P1-P3 conformance).
//
// Loads `net/crates/net/tests/cross_lang_mcp/helper_vectors.json` — the
// canonical fixture the Rust source-of-truth verifier
// (`adapters/mcp/tests/helper_golden_vectors.rs`) validates — and asserts
// the Go net.Classify / net.LowerTool wrappers produce the same results. A
// failure here means the Go binding drifted from the core.

package net

import (
	"encoding/json"
	"os"
	"path/filepath"
	"reflect"
	"runtime"
	"testing"
)

func mcpFixturePath(t *testing.T) string {
	t.Helper()
	_, thisFile, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("runtime.Caller failed")
	}
	dir := filepath.Dir(thisFile)
	// The published go/ module is at the repo root; fixtures are at
	// ../net/crates/net/tests/cross_lang_mcp/.
	return filepath.Join(dir, "..", "net", "crates", "net", "tests", "cross_lang_mcp", "helper_vectors.json")
}

type mcpClassifyCase struct {
	Name               string            `json:"name"`
	Program            string            `json:"program"`
	Args               []string          `json:"args"`
	Envs               map[string]string `json:"envs"`
	CredentialOverride *string           `json:"credential_override"`
	Force              bool              `json:"force"`
	ExpectedStatus     string            `json:"expected_status"`
}

type mcpLowerCase struct {
	Name             string          `json:"name"`
	Tool             json.RawMessage `json:"tool"`
	ServerVersion    string          `json:"server_version"`
	CredentialStatus string          `json:"credential_status"`
	Substitutability string          `json:"substitutability"`
	Expected         json.RawMessage `json:"expected"`
}

type mcpHelperFixture struct {
	Classify []mcpClassifyCase `json:"classify"`
	Lower    []mcpLowerCase    `json:"lower"`
}

func loadMcpHelperFixture(t *testing.T) mcpHelperFixture {
	t.Helper()
	raw, err := os.ReadFile(mcpFixturePath(t))
	if err != nil {
		t.Fatalf("read fixture: %v", err)
	}
	var f mcpHelperFixture
	if err := json.Unmarshal(raw, &f); err != nil {
		t.Fatalf("parse fixture: %v", err)
	}
	return f
}

func TestClassifyGoldenVectors(t *testing.T) {
	for _, c := range loadMcpHelperFixture(t).Classify {
		override := ""
		if c.CredentialOverride != nil {
			override = *c.CredentialOverride
		}
		got, err := Classify(c.Program, c.Args, c.Envs, override, c.Force)
		if err != nil {
			t.Fatalf("[%s] Classify error: %v", c.Name, err)
		}
		if got != c.ExpectedStatus {
			t.Errorf("[%s] classify = %q, want %q", c.Name, got, c.ExpectedStatus)
		}
	}
}

// normalizeToInterface reshapes a lowered DTO into the fixture's comparison
// shape — the descriptor's input_schema / output_schema JSON strings become
// parsed *_object values — then round-trips through JSON to interface{} so
// the comparison is by value (all numbers become float64 on both sides).
func normalizeLowered(t *testing.T, lowered LoweredTool) interface{} {
	t.Helper()
	var desc map[string]interface{}
	if err := json.Unmarshal(lowered.Descriptor, &desc); err != nil {
		t.Fatalf("descriptor not JSON: %v", err)
	}
	parseSchema := func(key string) interface{} {
		if s, ok := desc[key].(string); ok && s != "" {
			var obj interface{}
			if err := json.Unmarshal([]byte(s), &obj); err != nil {
				t.Fatalf("%s not JSON: %v", key, err)
			}
			return obj
		}
		return nil
	}
	desc["input_schema_object"] = parseSchema("input_schema")
	desc["output_schema_object"] = parseSchema("output_schema")
	delete(desc, "input_schema")
	delete(desc, "output_schema")

	got := map[string]interface{}{
		"tool_id":         lowered.ToolID,
		"mcp_name":        lowered.McpName,
		"bridge_metadata": lowered.BridgeMetadata,
		"descriptor":      desc,
	}
	blob, err := json.Marshal(got)
	if err != nil {
		t.Fatalf("re-marshal got: %v", err)
	}
	var norm interface{}
	if err := json.Unmarshal(blob, &norm); err != nil {
		t.Fatalf("normalize got: %v", err)
	}
	return norm
}

func TestLowerGoldenVectors(t *testing.T) {
	for _, c := range loadMcpHelperFixture(t).Lower {
		lowered, err := LowerTool(string(c.Tool), c.ServerVersion, c.CredentialStatus, c.Substitutability)
		if err != nil {
			t.Fatalf("[%s] LowerTool error: %v", c.Name, err)
		}
		got := normalizeLowered(t, lowered)

		var want interface{}
		if err := json.Unmarshal(c.Expected, &want); err != nil {
			t.Fatalf("[%s] parse expected: %v", c.Name, err)
		}
		if !reflect.DeepEqual(got, want) {
			gotJSON, _ := json.MarshalIndent(got, "", "  ")
			wantJSON, _ := json.MarshalIndent(want, "", "  ")
			t.Errorf("[%s] lower DTO mismatch\n got: %s\nwant: %s", c.Name, gotJSON, wantJSON)
		}
	}
}
