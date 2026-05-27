// Go layer for AI tool calling on net.
//
// Wraps the existing TypedMeshRpc Go surface (TypedServe /
// TypedCallService) with the RegisterTool / CallTool ergonomic
// helpers + format translators that lower ToolDescriptor instances
// to OpenAI / Anthropic / MCP / Gemini tool shapes and parse
// provider tool-call replies back into nRPC dispatches.
//
// This is the Wave 3 / D-1 starting point. v1 covers unary
// register + invoke + format conversion. Streaming and discovery
// follow once the underlying CGO surface exposes them.
//
// Plan: see
// `crates/net/docs/plans/NRPC_AI_TOOL_CALLING_AND_AGENT_DX.md`,
// slices D-1 / D-2. Mirror of the Rust SDK's `net_sdk::tool` +
// `net_sdk::tool::formats`, the Node TS `tool.ts`, and the Python
// `net.tool` modules. Cross-language tests (T-1) will pin byte
// equality across all four implementations.

package net

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"sync"
)

// =============================================================================
// Wire types — mirror of the Rust `ToolDescriptor` + `ToolEvent`.
// =============================================================================

// ToolDescriptor is the discovery shape for an AI tool, as
// advertised on the capability fold. One row per (ToolID, Version);
// NodeCount is filled by the aggregating walk (list_tools once it
// lands on the CGO surface).
//
// Wire-compatible 1:1 with the Rust substrate's `ToolDescriptor`,
// the Node TS `ToolDescriptor`, and the Python `ToolDescriptor`
// dataclass.
//
// Schemas are stored as JSON-encoded strings (matching the wire
// shape); use `json.Unmarshal([]byte(desc.InputSchema), &obj)` to
// get the parsed object for lowering into a provider tool
// definition.
type ToolDescriptor struct {
	ToolID          string   `json:"tool_id"`
	Name            string   `json:"name"`
	Version         string   `json:"version"`
	Description     string   `json:"description,omitempty"`
	InputSchema     string   `json:"input_schema,omitempty"`
	OutputSchema    string   `json:"output_schema,omitempty"`
	Requires        []string `json:"requires"`
	EstimatedTimeMs uint32   `json:"estimated_time_ms"`
	Stateless       bool     `json:"stateless"`
	Streaming       bool     `json:"streaming"`
	Tags            []string `json:"tags"`
	NodeCount       uint32   `json:"node_count"`
}

// ToolEventType discriminates ToolEvent variants. The wire JSON
// uses the `type` field; ToolEvent's MarshalJSON / UnmarshalJSON
// keeps the tagged-union shape symmetric with the other languages.
type ToolEventType string

const (
	ToolEventStart    ToolEventType = "start"
	ToolEventProgress ToolEventType = "progress"
	ToolEventDelta    ToolEventType = "delta"
	ToolEventResult   ToolEventType = "result"
	ToolEventError    ToolEventType = "error"
)

// ToolEvent is one envelope on a streaming tool. Wire-compatible
// 1:1 with the Rust `ToolEvent` enum: JSON-tagged with `type`,
// snake_case variants.
//
// Every stream ends with exactly one terminal event (Result or
// Error). Handlers that forget emit a synthesized
// `{type: "error", code: "missing_terminal", …}` from the Rust
// SDK's streaming wrapper.
type ToolEvent struct {
	Type ToolEventType `json:"type"`

	// Variant-specific fields — only the ones relevant to Type are
	// populated. Unmarshal sets the rest to zero values.

	// Start
	ToolID   string          `json:"tool_id,omitempty"`
	CallID   uint64          `json:"call_id,omitempty"`
	Metadata json.RawMessage `json:"metadata,omitempty"`

	// Progress
	Pct     *float32 `json:"pct,omitempty"`
	Message string   `json:"message,omitempty"`

	// Delta / Result
	Data json.RawMessage `json:"data,omitempty"`

	// Error
	Code    string          `json:"code,omitempty"`
	ErrMsg  string          `json:"-"` // Filled from `message` via UnmarshalJSON for Error variant.
	Details json.RawMessage `json:"details,omitempty"`
}

// IsTerminal returns true if the event closes the stream
// (Result or Error variant).
func (e ToolEvent) IsTerminal() bool {
	return e.Type == ToolEventResult || e.Type == ToolEventError
}

// =============================================================================
// Descriptor construction
// =============================================================================

// ToolOptions is the input to DescriptorFor / RegisterTool. Mirror
// of the Rust ToolMetadataBuilder, the Node TS ToolOptions, and the
// Python descriptor_for kwargs.
type ToolOptions struct {
	// Name is the nRPC service name + tool identifier. Required.
	Name string
	// Description is human-readable. Strongly recommended.
	Description string
	// Version defaults to "1.0.0".
	Version string
	// InputSchema is the JSON Schema for the request, as a Go
	// object (will be json.Marshal-ed to a string for storage).
	// Pass `nil` to omit.
	InputSchema interface{}
	// OutputSchema mirrors InputSchema.
	OutputSchema interface{}
	// Requires lists capability dependencies.
	Requires []string
	// EstimatedTimeMs is a soft latency hint (ms).
	EstimatedTimeMs uint32
	// Stateless defaults to true.
	Stateless *bool
	// Tags is a free-form list (e.g. ["web", "research"]).
	Tags []string
}

// DescriptorFor builds a ToolDescriptor from a ToolOptions literal.
// Schemas are json.Marshal-ed to strings; on encode failure (for
// non-serializable types) the field is left empty and the error is
// returned.
func DescriptorFor(opts ToolOptions) (ToolDescriptor, error) {
	desc := ToolDescriptor{
		ToolID:          opts.Name,
		Name:            opts.Name,
		Version:         opts.Version,
		Description:     opts.Description,
		Requires:        append([]string(nil), opts.Requires...),
		EstimatedTimeMs: opts.EstimatedTimeMs,
		Stateless:       true,
		Streaming:       false,
		Tags:            append([]string(nil), opts.Tags...),
		NodeCount:       0,
	}
	if desc.Version == "" {
		desc.Version = "1.0.0"
	}
	if opts.Stateless != nil {
		desc.Stateless = *opts.Stateless
	}
	if desc.Requires == nil {
		desc.Requires = []string{}
	}
	if desc.Tags == nil {
		desc.Tags = []string{}
	}
	if opts.InputSchema != nil {
		b, err := json.Marshal(opts.InputSchema)
		if err != nil {
			return desc, fmt.Errorf("descriptorFor: marshal input schema: %w", err)
		}
		desc.InputSchema = string(b)
	}
	if opts.OutputSchema != nil {
		b, err := json.Marshal(opts.OutputSchema)
		if err != nil {
			return desc, fmt.Errorf("descriptorFor: marshal output schema: %w", err)
		}
		desc.OutputSchema = string(b)
	}
	return desc, nil
}

// =============================================================================
// Register / invoke
// =============================================================================

// ToolServeHandle is returned by RegisterTool. Call Close() to
// deregister the underlying nRPC handler + remove the descriptor
// from the per-rpc registry that backs tool.metadata.fetch.
// Idempotent; second Close() is a no-op. Mirror of the Rust
// ToolServeHandle's Drop semantics.
type ToolServeHandle struct {
	Descriptor ToolDescriptor
	inner      *ServeHandle
	registry   *toolRegistryEntry
	closed     bool
}

// Close deregisters the handler. Idempotent.
func (h *ToolServeHandle) Close() {
	if h.closed {
		return
	}
	h.closed = true
	if h.registry != nil {
		h.registry.mu.Lock()
		delete(h.registry.descriptors, h.Descriptor.ToolID)
		h.registry.mu.Unlock()
	}
	if h.inner != nil {
		h.inner.Close()
	}
}

// toolRegistryEntry holds the per-rpc descriptor map + the lazy-
// installed tool.metadata.fetch handle. One entry per
// *TypedMeshRpc, looked up in `toolRegistries` by pointer.
type toolRegistryEntry struct {
	mu          sync.Mutex
	descriptors map[string]ToolDescriptor
	fetchHandle *ServeHandle
}

var (
	toolRegistriesMu sync.Mutex
	toolRegistries   = map[*TypedMeshRpc]*toolRegistryEntry{}
)

// ensureFetchInstalled returns (or creates) the per-rpc registry
// entry. On first call against a given rpc, registers the
// `tool.metadata.fetch` nRPC service handler backed by the
// per-rpc descriptor map. Subsequent calls reuse the same handler.
// Mirrors the Rust SDK's `ensure_tool_metadata_fetch_installed`.
func ensureFetchInstalled(rpc *TypedMeshRpc) *toolRegistryEntry {
	toolRegistriesMu.Lock()
	defer toolRegistriesMu.Unlock()
	if entry, ok := toolRegistries[rpc]; ok {
		return entry
	}
	entry := &toolRegistryEntry{
		descriptors: make(map[string]ToolDescriptor),
	}
	// Install the fetch handler. Captures `entry` by reference so
	// later RegisterTool calls flow descriptors into the same map
	// the handler reads from.
	type fetchReq struct {
		Name string `json:"name"`
	}
	handle, err := TypedServe[fetchReq, ToolMetadataResponse](
		rpc,
		TOOL_METADATA_FETCH_SERVICE,
		func(req fetchReq) (ToolMetadataResponse, error) {
			entry.mu.Lock()
			desc, ok := entry.descriptors[req.Name]
			entry.mu.Unlock()
			if !ok {
				return ToolMetadataResponse{Type: "not_found", Name: req.Name}, nil
			}
			d := desc
			return ToolMetadataResponse{Type: "found", Descriptor: &d}, nil
		},
	)
	if err == nil {
		entry.fetchHandle = handle
	}
	// If install fails (service name already taken — unlikely),
	// leave fetchHandle nil; subsequent RegisterTool calls won't
	// retry but the per-rpc registry still tracks descriptors for
	// future close() bookkeeping.
	toolRegistries[rpc] = entry
	return entry
}

// RegisterTool registers a typed handler as an AI tool against
// `rpc`. The handler is registered as an nRPC service at
// `descriptor.ToolID` with JSON codec.
//
// Atomically:
//
//  1. Inserts the descriptor into a per-rpc local registry keyed
//     on ToolID. The next FetchToolMetadata call against this host
//     resolves the descriptor by name.
//  2. Registers the typed handler at ToolID with JSON codec.
//  3. On the FIRST RegisterTool call against this rpc, lazy-
//     installs the tool.metadata.fetch nRPC service handler so
//     remote agents can pull the full descriptor for any
//     registered tool. Subsequent calls reuse the same fetch
//     handler. Mirrors the Rust / Node TS / Python pattern.
//
// The caller is still responsible for announcing the tool to
// peers — use AddToolCapabilitiesToAnnounce on the
// CapabilitySetWire passed to the mesh's announce surface.
//
// On handle.Close(): removes the descriptor from the per-rpc
// registry and unregisters the user handler. The lazy
// tool.metadata.fetch service stays installed for the rpc's
// lifetime (harmless when empty — returns NotFound for every
// request).
func RegisterTool[Req, Resp any](
	rpc *TypedMeshRpc,
	descriptor ToolDescriptor,
	handler TypedHandler[Req, Resp],
) (*ToolServeHandle, error) {
	inner, err := TypedServe[Req, Resp](rpc, descriptor.ToolID, handler)
	if err != nil {
		return nil, err
	}
	entry := ensureFetchInstalled(rpc)
	entry.mu.Lock()
	entry.descriptors[descriptor.ToolID] = descriptor
	entry.mu.Unlock()
	return &ToolServeHandle{Descriptor: descriptor, inner: inner, registry: entry}, nil
}

// TOOL_METADATA_FETCH_SERVICE is the nRPC service name for the
// on-demand tool-descriptor pull. The substrate auto-installs
// the server-side handler on the host's first serve_tool call.
const TOOL_METADATA_FETCH_SERVICE = "tool.metadata.fetch"

// ToolMetadataResponse is the wire shape of a
// `tool.metadata.fetch` reply. JSON-tagged on `type`, snake_case:
//
//   - `{"type": "found", "descriptor": {...}}` — host has a
//     serve_tool registration for the requested name.
//   - `{"type": "not_found", "name": "..."}` — host doesn't
//     currently serve this tool.
//
// Pinned by the substrate's `cortex::tool::ToolMetadataResponse`
// enum. Use Type to discriminate; Descriptor is populated only on
// "found".
type ToolMetadataResponse struct {
	Type       string          `json:"type"`
	Descriptor *ToolDescriptor `json:"descriptor,omitempty"`
	Name       string          `json:"name,omitempty"`
}

// FetchToolMetadata pulls a tool's full descriptor from a specific
// host by calling the auto-installed `tool.metadata.fetch` nRPC
// service. Useful when the local fold's entry dropped the schema
// (size-budget-exceeded) and the agent needs the full
// input/output schemas for strict-mode provider lowering.
//
// Mirror of `mesh.call_typed(host, TOOL_METADATA_FETCH_SERVICE,
// {name: tool_id})` in the Rust SDK.
func FetchToolMetadata(
	ctx context.Context,
	rpc *TypedMeshRpc,
	hostNodeID uint64,
	toolID string,
) (ToolMetadataResponse, error) {
	type req struct {
		Name string `json:"name"`
	}
	return TypedCall[req, ToolMetadataResponse](
		ctx,
		rpc,
		hostNodeID,
		TOOL_METADATA_FETCH_SERVICE,
		req{Name: toolID},
	)
}

// CallTool dispatches a capability-routed unary tool invocation
// via TypedCallService. JSON codec is hardwired — every AI
// provider (OpenAI, Anthropic, Gemini, MCP) consumes JSON for tool
// input/output.
//
// Returns the decoded response, or an error (NoRoute if no host
// advertises `nrpc:<toolID>`, bubbled handler errors otherwise).
func CallTool[Req, Resp any](
	ctx context.Context,
	rpc *TypedMeshRpc,
	toolID string,
	request Req,
) (Resp, error) {
	return TypedCallService[Req, Resp](ctx, rpc, toolID, request)
}

// CallToolStreaming opens a capability-routed streaming tool
// invocation. Returns a `*ToolEventStream` — drain via `Recv()`
// until ok=false (clean EOF or a terminal ToolEvent).
//
// The wrapper synthesizes a terminal
// `{Type: ToolEventError, Code: "missing_terminal", ...}` event
// if the stream ends without a Result / Error envelope — matches
// the Rust SDK's serve_tool_streaming contract and the T-2
// cross-language fixture.
//
// Cancel mid-stream via the `ctx` argument — the underlying
// RpcStream's watcher closes the stream and emits CANCEL on the
// wire.
func CallToolStreaming[Req any](
	ctx context.Context,
	rpc *TypedMeshRpc,
	toolID string,
	request Req,
) (*ToolEventStream, error) {
	stream, err := TypedCallServiceStreaming[Req, ToolEvent](
		ctx, rpc, toolID, request, StreamOptions{},
	)
	if err != nil {
		return nil, err
	}
	return &ToolEventStream{inner: stream}, nil
}

// ToolEventStream wraps a TypedRpcStream[ToolEvent] and tracks
// whether a terminal envelope was observed. On clean EOF without
// a terminal, the next Recv() synthesizes a `missing_terminal`
// error event before returning ok=false.
type ToolEventStream struct {
	inner       *TypedRpcStream[ToolEvent]
	sawTerminal bool
	synthesized bool
}

// Recv pulls the next ToolEvent. Returns (event, true, nil) for
// each envelope until the stream ends. Once the stream is
// exhausted, returns (zero, false, nil) AFTER one synthesized
// `missing_terminal` event if no terminal was observed.
func (s *ToolEventStream) Recv() (ToolEvent, bool, error) {
	event, ok, err := s.inner.Recv()
	if err != nil {
		return ToolEvent{}, false, err
	}
	if ok {
		if event.IsTerminal() {
			s.sawTerminal = true
		}
		return event, true, nil
	}
	if !s.sawTerminal && !s.synthesized {
		s.synthesized = true
		return ToolEvent{
			Type:   ToolEventError,
			Code:   "missing_terminal",
			ErrMsg: "tool stream ended without a terminal result or error envelope",
		}, true, nil
	}
	return ToolEvent{}, false, nil
}

// Close drops the stream and emits CANCEL on the wire. Idempotent.
func (s *ToolEventStream) Close() {
	s.inner.Close()
}

// CallID returns the server-assigned call id — useful for trace
// correlation.
func (s *ToolEventStream) CallID() uint64 {
	return s.inner.CallID()
}

// AddToolCapabilitiesToAnnounce merges tool descriptors into a
// CapabilitySetWire so the next announce carries:
//
//   - `ai-tool:<tool_id>` tag — peer fold's tag-prefix lookup hits.
//   - The `tool::<id>::input_schema` / `output_schema` metadata
//     keys for fold-side hydration.
//
// Returns the same wire object for chaining. v1 convenience; once
// the CGO surface exposes tool_registry, this becomes optional.
func AddToolCapabilitiesToAnnounce(
	caps CapabilitySetWire,
	descriptors []ToolDescriptor,
) CapabilitySetWire {
	if len(descriptors) == 0 {
		return caps
	}
	// Dedupe tags.
	tagSet := make(map[string]struct{}, len(caps.Tags))
	for _, t := range caps.Tags {
		tagSet[t] = struct{}{}
	}
	if caps.Metadata == nil {
		caps.Metadata = make(map[string]string)
	}
	for _, desc := range descriptors {
		tag := "ai-tool:" + desc.ToolID
		if _, ok := tagSet[tag]; !ok {
			tagSet[tag] = struct{}{}
			caps.Tags = append(caps.Tags, tag)
		}
		// Mirror schemas into metadata under the substrate's
		// canonical key shape (matches
		// ToolCapability::input_schema_metadata_key in Rust).
		if desc.InputSchema != "" {
			caps.Metadata["tool::"+desc.ToolID+"::input_schema"] = desc.InputSchema
		}
		if desc.OutputSchema != "" {
			caps.Metadata["tool::"+desc.ToolID+"::output_schema"] = desc.OutputSchema
		}
		if desc.Description != "" {
			caps.Metadata["tool::"+desc.ToolID+"::description"] = desc.Description
		}
		if desc.Streaming {
			caps.Metadata["tool::"+desc.ToolID+"::streaming"] = "1"
		}
		if len(desc.Tags) > 0 {
			// Comma-joined (substrate convention).
			joined := ""
			for i, t := range desc.Tags {
				if i > 0 {
					joined += ","
				}
				joined += t
			}
			caps.Metadata["tool::"+desc.ToolID+"::tags"] = joined
		}
	}
	return caps
}

// =============================================================================
// Format translators — mirror of `net_sdk::tool::formats`
// =============================================================================

// ToolCallSpec is the canonical hand-off between an LLM-provider
// adapter and CallTool. ArgumentsJSON is a string so the boundary
// is provider-agnostic (OpenAI's arguments arrive as a string
// anyway; Anthropic/MCP/Gemini's parsed objects re-serialize once).
type ToolCallSpec struct {
	Name              string
	ArgumentsJSON     string
	ProviderCallID    string
	HasProviderCallID bool
}

// ToolCallParseError is returned when a provider's tool-call reply
// doesn't match its spec.
var ErrToolCallParse = errors.New("net.tool: provider tool-call reply parse error")

func parseError(msg string) error {
	return fmt.Errorf("%w: %s", ErrToolCallParse, msg)
}

// inputSchemaValue parses desc.InputSchema into a Go value, falling
// back to `{"type":"object","properties":{}}` if missing/malformed.
// Providers' strict-mode validators reject null parameter schemas.
func inputSchemaValue(desc ToolDescriptor) interface{} {
	if desc.InputSchema == "" {
		return map[string]interface{}{"type": "object", "properties": map[string]interface{}{}}
	}
	var v interface{}
	if err := json.Unmarshal([]byte(desc.InputSchema), &v); err != nil {
		return map[string]interface{}{"type": "object", "properties": map[string]interface{}{}}
	}
	return v
}

// ---- OpenAI ----

// ToOpenAITool lowers a descriptor to an OpenAI tool definition:
//
//	{type: "function", function: {name, description, parameters, strict}}
//
// `strict` is true when the descriptor carried an InputSchema (i.e.
// publishable on the fold).
func ToOpenAITool(desc ToolDescriptor) map[string]interface{} {
	return map[string]interface{}{
		"type": "function",
		"function": map[string]interface{}{
			"name":        desc.ToolID,
			"description": desc.Description,
			"parameters":  inputSchemaValue(desc),
			"strict":      desc.InputSchema != "",
		},
	}
}

// LowerOpenAIToolCall parses one OpenAI `tool_calls[]` entry into
// a ToolCallSpec. OpenAI's `function.arguments` is a JSON-encoded
// STRING; validates the string parses up front to fail fast.
func LowerOpenAIToolCall(call map[string]interface{}) (ToolCallSpec, error) {
	spec := ToolCallSpec{}
	fn, ok := call["function"].(map[string]interface{})
	if !ok {
		return spec, parseError("tool-call reply missing field `function`")
	}
	name, ok := fn["name"].(string)
	if !ok {
		return spec, parseError("tool-call reply field `function.name` must be a string")
	}
	args, ok := fn["arguments"].(string)
	if !ok {
		return spec, parseError("tool-call reply field `function.arguments` must be a JSON-encoded string")
	}
	var probe interface{}
	if err := json.Unmarshal([]byte(args), &probe); err != nil {
		return spec, parseError(fmt.Sprintf("tool-call arguments were not valid JSON: %v", err))
	}
	spec.Name = name
	spec.ArgumentsJSON = args
	if id, ok := call["id"].(string); ok {
		spec.ProviderCallID = id
		spec.HasProviderCallID = true
	}
	return spec, nil
}

// ---- Anthropic ----

// ToAnthropicTool lowers a descriptor to an Anthropic tool
// definition: {name, description, input_schema}. Anthropic has no
// tool-level `strict` flag.
func ToAnthropicTool(desc ToolDescriptor) map[string]interface{} {
	return map[string]interface{}{
		"name":         desc.ToolID,
		"description":  desc.Description,
		"input_schema": inputSchemaValue(desc),
	}
}

// LowerAnthropicToolUse parses one Anthropic `tool_use` content
// block. `input` is already a parsed object; re-serializes once to
// preserve the `ArgumentsJSON: string` invariant.
func LowerAnthropicToolUse(block map[string]interface{}) (ToolCallSpec, error) {
	spec := ToolCallSpec{}
	name, ok := block["name"].(string)
	if !ok {
		return spec, parseError("tool_use block field `name` must be a string")
	}
	input, exists := block["input"]
	if !exists {
		return spec, parseError("tool_use block missing field `input`")
	}
	b, err := json.Marshal(input)
	if err != nil {
		return spec, parseError(fmt.Sprintf("tool_use input re-serialize failed: %v", err))
	}
	spec.Name = name
	spec.ArgumentsJSON = string(b)
	if id, ok := block["id"].(string); ok {
		spec.ProviderCallID = id
		spec.HasProviderCallID = true
	}
	return spec, nil
}

// ---- MCP ----

// ToMCPTool lowers a descriptor to an MCP tool definition:
// {name, description, inputSchema} (camelCase).
func ToMCPTool(desc ToolDescriptor) map[string]interface{} {
	return map[string]interface{}{
		"name":        desc.ToolID,
		"description": desc.Description,
		"inputSchema": inputSchemaValue(desc),
	}
}

// LowerMCPToolsCall parses an MCP `tools/call` request's `params`
// into a ToolCallSpec. ProviderCallID is left unset — MCP's
// JSON-RPC `id` lives one envelope layer up.
func LowerMCPToolsCall(params map[string]interface{}) (ToolCallSpec, error) {
	spec := ToolCallSpec{}
	name, ok := params["name"].(string)
	if !ok {
		return spec, parseError("tools/call params field `name` must be a string")
	}
	args, exists := params["arguments"]
	if !exists {
		return spec, parseError("tools/call params missing field `arguments`")
	}
	b, err := json.Marshal(args)
	if err != nil {
		return spec, parseError(fmt.Sprintf("tools/call arguments re-serialize failed: %v", err))
	}
	spec.Name = name
	spec.ArgumentsJSON = string(b)
	return spec, nil
}

// ---- Gemini ----

// ToGeminiFunctionDeclaration lowers a descriptor to one Gemini
// FunctionDeclaration: {name, description, parameters}. Caller
// wraps these into the outer
// `tools: [{ function_declarations: [...] }]` array.
func ToGeminiFunctionDeclaration(desc ToolDescriptor) map[string]interface{} {
	return map[string]interface{}{
		"name":        desc.ToolID,
		"description": desc.Description,
		"parameters":  inputSchemaValue(desc),
	}
}

// LowerGeminiFunctionCall parses one Gemini `functionCall` part.
// Gemini has no per-call id; ProviderCallID is left unset.
func LowerGeminiFunctionCall(call map[string]interface{}) (ToolCallSpec, error) {
	spec := ToolCallSpec{}
	name, ok := call["name"].(string)
	if !ok {
		return spec, parseError("functionCall field `name` must be a string")
	}
	args, exists := call["args"]
	if !exists {
		return spec, parseError("functionCall missing field `args`")
	}
	b, err := json.Marshal(args)
	if err != nil {
		return spec, parseError(fmt.Sprintf("functionCall args re-serialize failed: %v", err))
	}
	spec.Name = name
	spec.ArgumentsJSON = string(b)
	return spec, nil
}
