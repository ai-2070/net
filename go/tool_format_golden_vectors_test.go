// Cross-language tool-format compatibility fixture test (plan T-1).
//
// Loads `net/crates/net/tests/cross_lang_tool_formats/golden_vectors.json`
// — the canonical fixture pinning byte-equality across all four
// tool-format translators (Rust / Node TS / Python / Go). Failure of
// any case here signals cross-binding wire-format drift.
//
// Matches the Rust verifier at
// `sdk/tests/tool_format_golden_vectors.rs`, the Node TS verifier at
// `bindings/node/test/tool_format_golden_vectors.test.ts`, and the
// Python verifier at
// `bindings/python/tests/test_tool_format_golden_vectors.py`.

package net

import (
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"reflect"
	"runtime"
	"testing"
)

// fixturePath returns the absolute path to the golden-vector JSON,
// resolving relative to this test file. The published go/ module
// lives at the repo root, so the fixtures are at
// ../net/crates/net/tests/cross_lang_tool_formats/.
func fixturePath(t *testing.T) string {
	t.Helper()
	_, thisFile, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("runtime.Caller failed")
	}
	dir := filepath.Dir(thisFile)
	return filepath.Join(dir, "..", "net", "crates", "net", "tests", "cross_lang_tool_formats", "golden_vectors.json")
}

type fixtureDescriptorCase struct {
	Name  string `json:"name"`
	Input struct {
		ToolID             string                 `json:"tool_id"`
		Name               string                 `json:"name"`
		Version            string                 `json:"version"`
		Description        *string                `json:"description"`
		InputSchemaObject  map[string]interface{} `json:"input_schema_object"`
		OutputSchemaObject map[string]interface{} `json:"output_schema_object"`
		Requires           []string               `json:"requires"`
		EstimatedTimeMs    uint32                 `json:"estimated_time_ms"`
		Stateless          bool                   `json:"stateless"`
		Streaming          bool                   `json:"streaming"`
		Tags               []string               `json:"tags"`
		NodeCount          uint32                 `json:"node_count"`
	} `json:"input"`
	LoweredOpenAI    map[string]interface{} `json:"lowered_openai"`
	LoweredAnthropic map[string]interface{} `json:"lowered_anthropic"`
	LoweredMCP       map[string]interface{} `json:"lowered_mcp"`
	LoweredGemini    map[string]interface{} `json:"lowered_gemini"`
}

type fixtureLowerCase struct {
	Name         string                 `json:"name"`
	ReplyJSON    map[string]interface{} `json:"reply_json"`
	ExpectedSpec struct {
		Name            string      `json:"name"`
		ArgumentsJSON   *string     `json:"arguments_json,omitempty"`
		ArgumentsParsed interface{} `json:"arguments_parsed,omitempty"`
		ProviderCallID  *string     `json:"provider_call_id,omitempty"`
	} `json:"expected_spec"`
}

type fixtureErrorCase struct {
	Name      string                 `json:"name"`
	Provider  string                 `json:"provider"`
	ReplyJSON map[string]interface{} `json:"reply_json"`
}

type fixture struct {
	Descriptors         []fixtureDescriptorCase `json:"descriptors"`
	LowerOpenAICases    []fixtureLowerCase      `json:"lower_openai_cases"`
	LowerAnthropicCases []fixtureLowerCase      `json:"lower_anthropic_cases"`
	LowerMCPCases       []fixtureLowerCase      `json:"lower_mcp_cases"`
	LowerGeminiCases    []fixtureLowerCase      `json:"lower_gemini_cases"`
	ErrorCases          []fixtureErrorCase      `json:"error_cases"`
}

func loadFixture(t *testing.T) fixture {
	t.Helper()
	raw, err := os.ReadFile(fixturePath(t))
	if err != nil {
		t.Fatalf("read fixture: %v", err)
	}
	var f fixture
	if err := json.Unmarshal(raw, &f); err != nil {
		t.Fatalf("unmarshal fixture: %v", err)
	}
	return f
}

func descriptorFromFixture(t *testing.T, in fixtureDescriptorCase) ToolDescriptor {
	t.Helper()
	desc := ToolDescriptor{
		ToolID:          in.Input.ToolID,
		Name:            in.Input.Name,
		Version:         in.Input.Version,
		Requires:        in.Input.Requires,
		EstimatedTimeMs: in.Input.EstimatedTimeMs,
		Stateless:       in.Input.Stateless,
		Streaming:       in.Input.Streaming,
		Tags:            in.Input.Tags,
		NodeCount:       in.Input.NodeCount,
	}
	if in.Input.Description != nil {
		desc.Description = *in.Input.Description
	}
	if in.Input.InputSchemaObject != nil {
		b, err := json.Marshal(in.Input.InputSchemaObject)
		if err != nil {
			t.Fatalf("marshal input_schema: %v", err)
		}
		desc.InputSchema = string(b)
	}
	if in.Input.OutputSchemaObject != nil {
		b, err := json.Marshal(in.Input.OutputSchemaObject)
		if err != nil {
			t.Fatalf("marshal output_schema: %v", err)
		}
		desc.OutputSchema = string(b)
	}
	if desc.Requires == nil {
		desc.Requires = []string{}
	}
	if desc.Tags == nil {
		desc.Tags = []string{}
	}
	return desc
}

// normalizeJSON round-trips a value through json.Marshal/Unmarshal
// so reflect.DeepEqual compares semantically (Go's map and the
// fixture's parsed map have identical shape; this normalizes int
// vs float64 differences and nested map types).
func normalizeJSON(t *testing.T, v interface{}) interface{} {
	t.Helper()
	b, err := json.Marshal(v)
	if err != nil {
		t.Fatalf("marshal for normalize: %v", err)
	}
	var out interface{}
	if err := json.Unmarshal(b, &out); err != nil {
		t.Fatalf("unmarshal for normalize: %v", err)
	}
	return out
}

func TestDescriptorLoweringsMatchGoldenVectors(t *testing.T) {
	f := loadFixture(t)
	for _, c := range f.Descriptors {
		c := c
		t.Run(c.Name, func(t *testing.T) {
			desc := descriptorFromFixture(t, c)
			got := normalizeJSON(t, ToOpenAITool(desc))
			want := normalizeJSON(t, c.LoweredOpenAI)
			if !reflect.DeepEqual(got, want) {
				t.Errorf("openai lowering mismatch\n got:  %v\n want: %v", got, want)
			}
			got = normalizeJSON(t, ToAnthropicTool(desc))
			want = normalizeJSON(t, c.LoweredAnthropic)
			if !reflect.DeepEqual(got, want) {
				t.Errorf("anthropic lowering mismatch\n got:  %v\n want: %v", got, want)
			}
			got = normalizeJSON(t, ToMCPTool(desc))
			want = normalizeJSON(t, c.LoweredMCP)
			if !reflect.DeepEqual(got, want) {
				t.Errorf("mcp lowering mismatch\n got:  %v\n want: %v", got, want)
			}
			got = normalizeJSON(t, ToGeminiFunctionDeclaration(desc))
			want = normalizeJSON(t, c.LoweredGemini)
			if !reflect.DeepEqual(got, want) {
				t.Errorf("gemini lowering mismatch\n got:  %v\n want: %v", got, want)
			}
		})
	}
}

func assertLowerSpec(t *testing.T, caseName string, got ToolCallSpec, expected fixtureLowerCase) {
	t.Helper()
	if got.Name != expected.ExpectedSpec.Name {
		t.Errorf("case %q: name = %q, want %q", caseName, got.Name, expected.ExpectedSpec.Name)
	}
	if expected.ExpectedSpec.ArgumentsJSON != nil {
		if got.ArgumentsJSON != *expected.ExpectedSpec.ArgumentsJSON {
			t.Errorf("case %q: arguments_json = %q, want %q",
				caseName, got.ArgumentsJSON, *expected.ExpectedSpec.ArgumentsJSON)
		}
	}
	if expected.ExpectedSpec.ArgumentsParsed != nil {
		var parsed interface{}
		if err := json.Unmarshal([]byte(got.ArgumentsJSON), &parsed); err != nil {
			t.Errorf("case %q: arguments_json doesn't parse: %v", caseName, err)
		} else if !reflect.DeepEqual(normalizeJSON(t, parsed), normalizeJSON(t, expected.ExpectedSpec.ArgumentsParsed)) {
			t.Errorf("case %q: arguments_parsed mismatch\n got:  %v\n want: %v",
				caseName, parsed, expected.ExpectedSpec.ArgumentsParsed)
		}
	}
	if expected.ExpectedSpec.ProviderCallID == nil {
		if got.ProviderCallID != nil {
			t.Errorf("case %q: provider_call_id should be unset, got %q", caseName, *got.ProviderCallID)
		}
	} else {
		if got.ProviderCallID == nil || *got.ProviderCallID != *expected.ExpectedSpec.ProviderCallID {
			t.Errorf("case %q: provider_call_id = %v, want %q",
				caseName, got.ProviderCallID, *expected.ExpectedSpec.ProviderCallID)
		}
	}
}

func TestLowerOpenAIMatchesGoldenVectors(t *testing.T) {
	f := loadFixture(t)
	for _, c := range f.LowerOpenAICases {
		c := c
		t.Run(c.Name, func(t *testing.T) {
			spec, err := LowerOpenAIToolCall(c.ReplyJSON)
			if err != nil {
				t.Fatalf("LowerOpenAIToolCall: %v", err)
			}
			assertLowerSpec(t, c.Name, spec, c)
		})
	}
}

func TestLowerAnthropicMatchesGoldenVectors(t *testing.T) {
	f := loadFixture(t)
	for _, c := range f.LowerAnthropicCases {
		c := c
		t.Run(c.Name, func(t *testing.T) {
			spec, err := LowerAnthropicToolUse(c.ReplyJSON)
			if err != nil {
				t.Fatalf("LowerAnthropicToolUse: %v", err)
			}
			assertLowerSpec(t, c.Name, spec, c)
		})
	}
}

func TestLowerMCPMatchesGoldenVectors(t *testing.T) {
	f := loadFixture(t)
	for _, c := range f.LowerMCPCases {
		c := c
		t.Run(c.Name, func(t *testing.T) {
			spec, err := LowerMCPToolsCall(c.ReplyJSON)
			if err != nil {
				t.Fatalf("LowerMCPToolsCall: %v", err)
			}
			assertLowerSpec(t, c.Name, spec, c)
		})
	}
}

func TestLowerGeminiMatchesGoldenVectors(t *testing.T) {
	f := loadFixture(t)
	for _, c := range f.LowerGeminiCases {
		c := c
		t.Run(c.Name, func(t *testing.T) {
			spec, err := LowerGeminiFunctionCall(c.ReplyJSON)
			if err != nil {
				t.Fatalf("LowerGeminiFunctionCall: %v", err)
			}
			assertLowerSpec(t, c.Name, spec, c)
		})
	}
}

func TestErrorCasesAllReject(t *testing.T) {
	f := loadFixture(t)
	for _, c := range f.ErrorCases {
		c := c
		t.Run(c.Name, func(t *testing.T) {
			var err error
			switch c.Provider {
			case "openai":
				_, err = LowerOpenAIToolCall(c.ReplyJSON)
			case "anthropic":
				_, err = LowerAnthropicToolUse(c.ReplyJSON)
			case "mcp":
				_, err = LowerMCPToolsCall(c.ReplyJSON)
			case "gemini":
				_, err = LowerGeminiFunctionCall(c.ReplyJSON)
			default:
				t.Fatalf("unknown provider %q", c.Provider)
			}
			if err == nil {
				t.Fatalf("expected parse error")
			}
			if !errors.Is(err, ErrToolCallParse) {
				t.Errorf("expected ErrToolCallParse, got %v", err)
			}
		})
	}
}
