// Pure-function tests for the Go tool layer.
//
// Covers DescriptorFor, AddToolCapabilitiesToAnnounce, and all
// four provider format translators (both directions).
//
// Live-mesh tests (RegisterTool + CallTool round-trip) are deferred
// until we have a Go-side mesh-pair harness. The cross-language
// byte-equality fixtures pinned by T-1 will eventually feed this
// file alongside the Rust formats module + the Node TS tool.test
// + the Python test_tool — same golden vectors, four languages.

package net

import (
	"encoding/json"
	"errors"
	"testing"
)

func sampleDescriptor(t *testing.T) ToolDescriptor {
	t.Helper()
	desc, err := DescriptorFor(ToolOptions{
		Name:        "web_search",
		Description: "Search the web.",
		InputSchema: map[string]interface{}{
			"type": "object",
			"properties": map[string]interface{}{
				"query": map[string]interface{}{"type": "string"},
			},
			"required": []string{"query"},
		},
	})
	if err != nil {
		t.Fatalf("DescriptorFor: %v", err)
	}
	return desc
}

// ---------------------------------------------------------------------------
// DescriptorFor
// ---------------------------------------------------------------------------

func TestDescriptorForDefaults(t *testing.T) {
	desc, err := DescriptorFor(ToolOptions{Name: "x"})
	if err != nil {
		t.Fatalf("DescriptorFor: %v", err)
	}
	if desc.ToolID != "x" {
		t.Errorf("ToolID = %q, want %q", desc.ToolID, "x")
	}
	if desc.Version != "1.0.0" {
		t.Errorf("Version = %q, want %q", desc.Version, "1.0.0")
	}
	if !desc.Stateless {
		t.Error("Stateless should default to true")
	}
	if desc.Streaming {
		t.Error("Streaming should default to false")
	}
	if desc.EstimatedTimeMs != 0 {
		t.Errorf("EstimatedTimeMs = %d, want 0", desc.EstimatedTimeMs)
	}
	if desc.InputSchema != "" {
		t.Errorf("InputSchema should default to empty string")
	}
}

func TestDescriptorForSerializesSchemas(t *testing.T) {
	desc := sampleDescriptor(t)
	if desc.InputSchema == "" {
		t.Fatal("InputSchema should be populated")
	}
	var parsed map[string]interface{}
	if err := json.Unmarshal([]byte(desc.InputSchema), &parsed); err != nil {
		t.Fatalf("InputSchema unmarshal: %v", err)
	}
	props, ok := parsed["properties"].(map[string]interface{})
	if !ok {
		t.Fatalf("properties missing: %v", parsed)
	}
	if _, ok := props["query"]; !ok {
		t.Errorf("properties.query missing")
	}
}

// ---------------------------------------------------------------------------
// IsTerminal
// ---------------------------------------------------------------------------

func TestToolEventIsTerminal(t *testing.T) {
	if !(ToolEvent{Type: ToolEventResult}).IsTerminal() {
		t.Error("result must be terminal")
	}
	if !(ToolEvent{Type: ToolEventError}).IsTerminal() {
		t.Error("error must be terminal")
	}
	if (ToolEvent{Type: ToolEventStart}).IsTerminal() {
		t.Error("start must not be terminal")
	}
	if (ToolEvent{Type: ToolEventDelta}).IsTerminal() {
		t.Error("delta must not be terminal")
	}
}

// ---------------------------------------------------------------------------
// AddToolCapabilitiesToAnnounce
// ---------------------------------------------------------------------------

func TestAddToolCapabilitiesMergesTagAndMetadata(t *testing.T) {
	desc := sampleDescriptor(t)
	desc.Description = "Search the web."
	desc.Tags = []string{"web", "research"}
	caps := AddToolCapabilitiesToAnnounce(CapabilitySetWire{}, []ToolDescriptor{desc})
	found := false
	for _, tag := range caps.Tags {
		if tag == "ai-tool:web_search" {
			found = true
		}
	}
	if !found {
		t.Errorf("ai-tool:web_search tag missing from %v", caps.Tags)
	}
	if caps.Metadata["tool::web_search::description"] != "Search the web." {
		t.Errorf("description metadata missing: %v", caps.Metadata)
	}
	if caps.Metadata["tool::web_search::tags"] != "web,research" {
		t.Errorf("tags metadata wrong: %q", caps.Metadata["tool::web_search::tags"])
	}
	if caps.Metadata["tool::web_search::input_schema"] == "" {
		t.Error("input_schema metadata missing")
	}
}

func TestAddToolCapabilitiesDedupesTags(t *testing.T) {
	desc := sampleDescriptor(t)
	caps := AddToolCapabilitiesToAnnounce(
		CapabilitySetWire{Tags: []string{"region.eu", "ai-tool:web_search"}},
		[]ToolDescriptor{desc},
	)
	count := 0
	for _, tag := range caps.Tags {
		if tag == "ai-tool:web_search" {
			count++
		}
	}
	if count != 1 {
		t.Errorf("ai-tool:web_search count = %d, want 1", count)
	}
}

func TestAddToolCapabilitiesNoOpOnEmpty(t *testing.T) {
	caps := AddToolCapabilitiesToAnnounce(
		CapabilitySetWire{Tags: []string{"x"}},
		nil,
	)
	if len(caps.Tags) != 1 || caps.Tags[0] != "x" {
		t.Errorf("Tags should be unchanged: %v", caps.Tags)
	}
}

// ---------------------------------------------------------------------------
// OpenAI format
// ---------------------------------------------------------------------------

func TestOpenAIToolEnvelopeAndStrict(t *testing.T) {
	tool := ToOpenAITool(sampleDescriptor(t))
	if tool["type"] != "function" {
		t.Errorf("type = %v, want \"function\"", tool["type"])
	}
	fn := tool["function"].(map[string]interface{})
	if fn["name"] != "web_search" {
		t.Errorf("name = %v", fn["name"])
	}
	if fn["strict"] != true {
		t.Errorf("strict should be true when schema present")
	}
	params := fn["parameters"].(map[string]interface{})
	if params["type"] != "object" {
		t.Errorf("parameters.type = %v", params["type"])
	}
}

func TestOpenAILowerToolCall(t *testing.T) {
	spec, err := LowerOpenAIToolCall(map[string]interface{}{
		"id":   "call_abc",
		"type": "function",
		"function": map[string]interface{}{
			"name":      "web_search",
			"arguments": `{"query":"mesh"}`,
		},
	})
	if err != nil {
		t.Fatalf("LowerOpenAIToolCall: %v", err)
	}
	if spec.Name != "web_search" {
		t.Errorf("Name = %q", spec.Name)
	}
	if spec.ArgumentsJSON != `{"query":"mesh"}` {
		t.Errorf("ArgumentsJSON = %q", spec.ArgumentsJSON)
	}
	if spec.ProviderCallID == nil || *spec.ProviderCallID != "call_abc" {
		t.Errorf("ProviderCallID = %v", spec.ProviderCallID)
	}
}

func TestOpenAILowerToolCallRejectsMalformedArguments(t *testing.T) {
	_, err := LowerOpenAIToolCall(map[string]interface{}{
		"function": map[string]interface{}{"name": "x", "arguments": "not valid json {"},
	})
	if err == nil {
		t.Fatal("expected error")
	}
	if !errors.Is(err, ErrToolCallParse) {
		t.Errorf("expected ErrToolCallParse, got %v", err)
	}
}

// ---------------------------------------------------------------------------
// Anthropic format
// ---------------------------------------------------------------------------

func TestAnthropicToolSnakeCaseInputSchema(t *testing.T) {
	tool := ToAnthropicTool(sampleDescriptor(t))
	if tool["name"] != "web_search" {
		t.Errorf("name = %v", tool["name"])
	}
	if _, ok := tool["input_schema"]; !ok {
		t.Error("input_schema (snake_case) missing")
	}
	if _, ok := tool["strict"]; ok {
		t.Error("Anthropic should have no tool-level `strict`")
	}
}

func TestAnthropicLowerToolUse(t *testing.T) {
	spec, err := LowerAnthropicToolUse(map[string]interface{}{
		"type":  "tool_use",
		"id":    "toolu_xyz",
		"name":  "web_search",
		"input": map[string]interface{}{"query": "mesh", "max_results": 5},
	})
	if err != nil {
		t.Fatalf("LowerAnthropicToolUse: %v", err)
	}
	if spec.Name != "web_search" {
		t.Errorf("Name = %q", spec.Name)
	}
	var parsed map[string]interface{}
	if err := json.Unmarshal([]byte(spec.ArgumentsJSON), &parsed); err != nil {
		t.Fatalf("ArgumentsJSON unmarshal: %v", err)
	}
	if parsed["query"] != "mesh" {
		t.Errorf("query = %v", parsed["query"])
	}
	if spec.ProviderCallID == nil || *spec.ProviderCallID != "toolu_xyz" {
		t.Errorf("ProviderCallID = %v", spec.ProviderCallID)
	}
}

// ---------------------------------------------------------------------------
// MCP format
// ---------------------------------------------------------------------------

func TestMCPToolCamelCase(t *testing.T) {
	tool := ToMCPTool(sampleDescriptor(t))
	if _, ok := tool["inputSchema"]; !ok {
		t.Error("inputSchema (camelCase) missing")
	}
}

func TestMCPLowerToolsCallNoProviderCallID(t *testing.T) {
	spec, err := LowerMCPToolsCall(map[string]interface{}{
		"name":      "web_search",
		"arguments": map[string]interface{}{"query": "mesh"},
	})
	if err != nil {
		t.Fatalf("LowerMCPToolsCall: %v", err)
	}
	if spec.ProviderCallID != nil {
		t.Error("MCP tools/call has no provider call id at this layer")
	}
}

// ---------------------------------------------------------------------------
// Gemini format
// ---------------------------------------------------------------------------

func TestGeminiFunctionDeclarationParametersField(t *testing.T) {
	decl := ToGeminiFunctionDeclaration(sampleDescriptor(t))
	if _, ok := decl["parameters"]; !ok {
		t.Error("parameters missing")
	}
}

func TestGeminiLowerFunctionCallArgs(t *testing.T) {
	spec, err := LowerGeminiFunctionCall(map[string]interface{}{
		"name": "web_search",
		"args": map[string]interface{}{"query": "mesh"},
	})
	if err != nil {
		t.Fatalf("LowerGeminiFunctionCall: %v", err)
	}
	var parsed map[string]interface{}
	if err := json.Unmarshal([]byte(spec.ArgumentsJSON), &parsed); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	if parsed["query"] != "mesh" {
		t.Errorf("query = %v", parsed["query"])
	}
	if spec.ProviderCallID != nil {
		t.Error("Gemini has no call id")
	}
}

// ---------------------------------------------------------------------------
// Empty-schema fallback
// ---------------------------------------------------------------------------

func TestEmptySchemaFallback(t *testing.T) {
	desc, err := DescriptorFor(ToolOptions{Name: "no_args", Description: "Bare."})
	if err != nil {
		t.Fatalf("DescriptorFor: %v", err)
	}
	// OpenAI: strict=false + empty-object parameters
	openai := ToOpenAITool(desc)
	fn := openai["function"].(map[string]interface{})
	if fn["strict"] != false {
		t.Errorf("strict should be false when schema missing")
	}
	params := fn["parameters"].(map[string]interface{})
	if params["type"] != "object" {
		t.Errorf("parameters fallback should be empty object")
	}
	// Anthropic
	anth := ToAnthropicTool(desc)["input_schema"].(map[string]interface{})
	if anth["type"] != "object" {
		t.Errorf("anthropic input_schema fallback wrong")
	}
	// MCP
	mcp := ToMCPTool(desc)["inputSchema"].(map[string]interface{})
	if mcp["type"] != "object" {
		t.Errorf("mcp inputSchema fallback wrong")
	}
	// Gemini
	gem := ToGeminiFunctionDeclaration(desc)["parameters"].(map[string]interface{})
	if gem["type"] != "object" {
		t.Errorf("gemini parameters fallback wrong")
	}
}

// ---------------------------------------------------------------------------
// diffToolIndex — WatchTools diffing logic (pure function; no CGO needed).
// ---------------------------------------------------------------------------

func mkDesc(toolID, version string, nodeCount uint32) ToolDescriptor {
	return ToolDescriptor{
		ToolID:    toolID,
		Name:      toolID,
		Version:   version,
		NodeCount: nodeCount,
	}
}

func TestDiffToolIndexAdded(t *testing.T) {
	prev := indexDescriptors(nil)
	next := indexDescriptors([]ToolDescriptor{mkDesc("web_search", "1.0.0", 1)})
	changes := diffToolIndex(prev, next)
	if len(changes) != 1 {
		t.Fatalf("expected 1 change, got %d", len(changes))
	}
	c := changes[0]
	if c.Type != "added" || c.Descriptor.ToolID != "web_search" || c.Descriptor.NodeCount != 1 {
		t.Errorf("added shape wrong: %#v", c)
	}
}

func TestDiffToolIndexRemoved(t *testing.T) {
	prev := indexDescriptors([]ToolDescriptor{mkDesc("temp", "1.0.0", 1)})
	next := indexDescriptors(nil)
	changes := diffToolIndex(prev, next)
	if len(changes) != 1 {
		t.Fatalf("expected 1 change, got %d", len(changes))
	}
	c := changes[0]
	if c.Type != "removed" || c.Descriptor.ToolID != "temp" {
		t.Errorf("removed shape wrong: %#v", c)
	}
}

func TestDiffToolIndexNodeCountChanged(t *testing.T) {
	prev := indexDescriptors([]ToolDescriptor{mkDesc("shared", "1.0.0", 1)})
	next := indexDescriptors([]ToolDescriptor{mkDesc("shared", "1.0.0", 3)})
	changes := diffToolIndex(prev, next)
	if len(changes) != 1 {
		t.Fatalf("expected 1 change, got %d", len(changes))
	}
	c := changes[0]
	if c.Type != "node_count_changed" || c.PrevNodeCount != 1 || c.Descriptor.NodeCount != 3 {
		t.Errorf("node_count_changed shape wrong: %#v", c)
	}
}

func TestDiffToolIndexNoChangeSameNodeCount(t *testing.T) {
	prev := indexDescriptors([]ToolDescriptor{mkDesc("stable", "1.0.0", 2)})
	next := indexDescriptors([]ToolDescriptor{mkDesc("stable", "1.0.0", 2)})
	changes := diffToolIndex(prev, next)
	if len(changes) != 0 {
		t.Errorf("expected no changes for identical state, got %#v", changes)
	}
}

func TestDiffToolIndexVersionsAreDistinctKeys(t *testing.T) {
	// Same tool_id, two versions — diff sees them as separate slots.
	prev := indexDescriptors([]ToolDescriptor{mkDesc("svc", "1.0.0", 1)})
	next := indexDescriptors([]ToolDescriptor{
		mkDesc("svc", "1.0.0", 1),
		mkDesc("svc", "2.0.0", 1),
	})
	changes := diffToolIndex(prev, next)
	if len(changes) != 1 {
		t.Fatalf("expected 1 change (the v2 addition), got %d: %#v", len(changes), changes)
	}
	if changes[0].Type != "added" || changes[0].Descriptor.Version != "2.0.0" {
		t.Errorf("expected v2 addition, got %#v", changes[0])
	}
}

func TestDiffToolIndexDeterministicOrdering(t *testing.T) {
	// Adds are emitted in (tool_id, version) order, then removes.
	prev := indexDescriptors([]ToolDescriptor{mkDesc("z_old", "1.0.0", 1)})
	next := indexDescriptors([]ToolDescriptor{
		mkDesc("b_new", "1.0.0", 1),
		mkDesc("a_new", "1.0.0", 1),
	})
	changes := diffToolIndex(prev, next)
	if len(changes) != 3 {
		t.Fatalf("expected 3 changes, got %d: %#v", len(changes), changes)
	}
	// Added group sorts a_new before b_new; remove group comes last.
	if changes[0].Descriptor.ToolID != "a_new" || changes[0].Type != "added" {
		t.Errorf("first change should be added/a_new: %#v", changes[0])
	}
	if changes[1].Descriptor.ToolID != "b_new" || changes[1].Type != "added" {
		t.Errorf("second change should be added/b_new: %#v", changes[1])
	}
	if changes[2].Descriptor.ToolID != "z_old" || changes[2].Type != "removed" {
		t.Errorf("third change should be removed/z_old: %#v", changes[2])
	}
}

// TestToolServeHandleCloseRemovesGlobalRegistryEntry pins C-3 / E-9:
// closing the last serve handle for a given rpc must remove the
// process-global registry entry AND clear the fetch handle. Without
// this, long-lived processes that recycle rpcs leak one entry +
// one fetch handler per cycle.
func TestToolServeHandleCloseRemovesGlobalRegistryEntry(t *testing.T) {
	// Use a non-nil rpc pointer as a map key. We don't deref it.
	rpc := &TypedMeshRpc{}

	// Pre-build an entry holding one descriptor. fetchHandle is nil
	// so Close()'s `if fetch != nil { fetch.Close() }` skips the
	// CGO call we can't service in a unit test.
	entry := &toolRegistryEntry{
		descriptors: map[string]ToolDescriptor{
			"web_search": mkDesc("web_search", "1.0.0", 1),
		},
		fetchHandle: nil,
	}
	toolRegistriesMu.Lock()
	toolRegistries[rpc] = entry
	toolRegistriesMu.Unlock()
	t.Cleanup(func() {
		toolRegistriesMu.Lock()
		delete(toolRegistries, rpc)
		toolRegistriesMu.Unlock()
	})

	handle := &ToolServeHandle{
		Descriptor: mkDesc("web_search", "1.0.0", 1),
		inner:      nil,
		registry:   entry,
		rpc:        rpc,
	}
	handle.Close()

	toolRegistriesMu.Lock()
	_, exists := toolRegistries[rpc]
	toolRegistriesMu.Unlock()
	if exists {
		t.Error("registry entry leaked after last Close")
	}
}

// TestToolServeHandleCloseKeepsEntryWhenOthersActive pins that
// closing ONE of multiple serve handles must NOT drop the entry
// (would silently kill the fetch handler for the surviving tools).
func TestToolServeHandleCloseKeepsEntryWhenOthersActive(t *testing.T) {
	rpc := &TypedMeshRpc{}
	entry := &toolRegistryEntry{
		descriptors: map[string]ToolDescriptor{
			"web_search": mkDesc("web_search", "1.0.0", 1),
			"summarize":  mkDesc("summarize", "1.0.0", 1),
		},
		fetchHandle: nil,
	}
	toolRegistriesMu.Lock()
	toolRegistries[rpc] = entry
	toolRegistriesMu.Unlock()
	t.Cleanup(func() {
		toolRegistriesMu.Lock()
		delete(toolRegistries, rpc)
		toolRegistriesMu.Unlock()
	})

	h1 := &ToolServeHandle{
		Descriptor: mkDesc("web_search", "1.0.0", 1),
		registry:   entry,
		rpc:        rpc,
	}
	h1.Close()

	toolRegistriesMu.Lock()
	_, exists := toolRegistries[rpc]
	toolRegistriesMu.Unlock()
	if !exists {
		t.Error("entry dropped while other serve handle still active")
	}
	if len(entry.descriptors) != 1 || entry.descriptors["summarize"].ToolID != "summarize" {
		t.Errorf("surviving descriptor wrong: %#v", entry.descriptors)
	}
}

// TestToolListChangeJSONWireShape pins the JSON encoding of each
// ToolListChange variant against the canonical Rust/Node/Python
// wire shape: {type, descriptor, prev_node_count?}. Regression
// guard for the historical Go-side divergence
// ({tool_id, version, tool, old_count, new_count}).
func TestToolListChangeJSONWireShape(t *testing.T) {
	desc := mkDesc("web_search", "1.0.0", 2)
	cases := []struct {
		name     string
		change   ToolListChange
		wantKeys []string // keys that MUST appear in the JSON
		bannedKeys []string // keys that MUST NOT appear
	}{
		{
			name:       "added",
			change:     ToolListChange{Type: "added", Descriptor: desc},
			wantKeys:   []string{"type", "descriptor"},
			bannedKeys: []string{"tool_id", "version", "tool", "old_count", "new_count"},
		},
		{
			name:       "removed",
			change:     ToolListChange{Type: "removed", Descriptor: desc},
			wantKeys:   []string{"type", "descriptor"},
			bannedKeys: []string{"tool_id", "version", "tool", "old_count", "new_count"},
		},
		{
			name:       "node_count_changed",
			change:     ToolListChange{Type: "node_count_changed", Descriptor: desc, PrevNodeCount: 1},
			wantKeys:   []string{"type", "descriptor", "prev_node_count"},
			bannedKeys: []string{"tool_id", "version", "tool", "old_count", "new_count"},
		},
	}
	for _, c := range cases {
		c := c
		t.Run(c.name, func(t *testing.T) {
			body, err := json.Marshal(c.change)
			if err != nil {
				t.Fatalf("marshal: %v", err)
			}
			var parsed map[string]interface{}
			if err := json.Unmarshal(body, &parsed); err != nil {
				t.Fatalf("unmarshal: %v", err)
			}
			for _, k := range c.wantKeys {
				if _, ok := parsed[k]; !ok {
					t.Errorf("missing key %q in %s: %s", k, c.name, string(body))
				}
			}
			for _, k := range c.bannedKeys {
				if _, ok := parsed[k]; ok {
					t.Errorf("banned key %q present in %s: %s", k, c.name, string(body))
				}
			}
			// `descriptor` must be the embedded ToolDescriptor (object),
			// not a flattened string id.
			d, ok := parsed["descriptor"].(map[string]interface{})
			if !ok {
				t.Fatalf("descriptor field is not an object: %v", parsed["descriptor"])
			}
			if d["tool_id"] != "web_search" {
				t.Errorf("descriptor.tool_id = %v, want %q", d["tool_id"], "web_search")
			}
		})
	}
}
