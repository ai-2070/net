// Package net — capability announce / find_nodes surface.
//
// Mirrors the PyO3 / NAPI dict shape byte-for-byte so cross-binding
// fixtures round-trip. Capabilities cross as JSON; filters cross as
// JSON; node-id lists come back as JSON. Binary-only surfaces
// (entity ids, tokens) stay on `Identity` in `identity.go`.
//
// Tracks Stage G-2 of `docs/SDK_GO_PARITY_PLAN.md`.

package net

/*
#include "net.h"
#include <stdlib.h>
#include <string.h>
*/
import "C"

import (
	"encoding/json"
	"errors"
	"fmt"
	"unsafe"
)

// ErrCapability is returned when a capability-announcement dispatch
// fails in the core adapter (e.g. no peer connected, or the core
// rejected the payload).
var ErrCapability = errors.New("capability: dispatch failed")

func capabilityErrorFromCode(code C.int) error {
	if code == -128 {
		return ErrCapability
	}
	return identityErrorFromCode(code)
}

// ---------------------------------------------------------------------------
// Dict shapes — plain structs with JSON tags, matching PyO3 / NAPI
// POJOs. Every field is optional on the wire; zero-valued structs
// serialize to `{}` or `[]` which the Rust layer treats as "no
// restriction" / "no declaration."
// ---------------------------------------------------------------------------

// GPUInfo describes one GPU attached to an announcing node.
type GPUInfo struct {
	Vendor         string `json:"vendor,omitempty"` // nvidia | amd | intel | apple | qualcomm | unknown
	Model          string `json:"model,omitempty"`
	VRAMGB         uint32 `json:"vram_gb,omitempty"`
	ComputeUnits   uint32 `json:"compute_units,omitempty"`
	TensorCores    uint32 `json:"tensor_cores,omitempty"`
	FP16TFLOPSX10  uint32 `json:"fp16_tflops_x10,omitempty"`
}

// AcceleratorInfo describes one non-GPU accelerator (TPU / NPU / etc.).
type AcceleratorInfo struct {
	Kind      string `json:"kind,omitempty"` // tpu | npu | fpga | asic | dsp | unknown
	Model     string `json:"model,omitempty"`
	MemoryGB  uint32 `json:"memory_gb,omitempty"`
	TOPSX10   uint32 `json:"tops_x10,omitempty"`
}

// HardwareCaps is the hardware sub-section of a capability
// announcement.
type HardwareCaps struct {
	CPUCores       uint32            `json:"cpu_cores,omitempty"`
	CPUThreads     uint32            `json:"cpu_threads,omitempty"`
	MemoryGB       uint32            `json:"memory_gb,omitempty"`
	GPU            *GPUInfo          `json:"gpu,omitempty"`
	AdditionalGPUs []GPUInfo         `json:"additional_gpus,omitempty"`
	StorageGB      uint64            `json:"storage_gb,omitempty"`
	NetworkGbps    uint32            `json:"network_gbps,omitempty"`
	Accelerators   []AcceleratorInfo `json:"accelerators,omitempty"`
}

// SoftwareCaps is the software sub-section of a capability
// announcement. Pair lists match `[name, version]` tuples.
type SoftwareCaps struct {
	OS          string     `json:"os,omitempty"`
	OSVersion   string     `json:"os_version,omitempty"`
	Runtimes    [][]string `json:"runtimes,omitempty"`
	Frameworks  [][]string `json:"frameworks,omitempty"`
	CUDAVersion string     `json:"cuda_version,omitempty"`
	Drivers     [][]string `json:"drivers,omitempty"`
}

// ModelCaps describes one model loaded on the announcing node.
type ModelCaps struct {
	ModelID         string   `json:"model_id,omitempty"`
	Family          string   `json:"family,omitempty"`
	ParametersBx10  uint32   `json:"parameters_b_x10,omitempty"`
	ContextLength   uint32   `json:"context_length,omitempty"`
	Quantization    string   `json:"quantization,omitempty"`
	Modalities      []string `json:"modalities,omitempty"`
	TokensPerSec    uint32   `json:"tokens_per_sec,omitempty"`
	Loaded          bool     `json:"loaded,omitempty"`
}

// ToolCaps describes one tool the announcing node can execute.
type ToolCaps struct {
	ToolID           string   `json:"tool_id,omitempty"`
	Name             string   `json:"name,omitempty"`
	Version          string   `json:"version,omitempty"`
	InputSchema      string   `json:"input_schema,omitempty"`
	OutputSchema     string   `json:"output_schema,omitempty"`
	Requires         []string `json:"requires,omitempty"`
	EstimatedTimeMs  uint32   `json:"estimated_time_ms,omitempty"`
	Stateless        bool     `json:"stateless,omitempty"`
}

// CapabilityLimits is the resource-limits sub-section.
type CapabilityLimits struct {
	MaxConcurrentRequests uint32 `json:"max_concurrent_requests,omitempty"`
	MaxTokensPerRequest   uint32 `json:"max_tokens_per_request,omitempty"`
	RateLimitRpm          uint32 `json:"rate_limit_rpm,omitempty"`
	MaxBatchSize          uint32 `json:"max_batch_size,omitempty"`
	MaxInputBytes         uint32 `json:"max_input_bytes,omitempty"`
	MaxOutputBytes        uint32 `json:"max_output_bytes,omitempty"`
}

// CapabilitySet is the full announcement payload. Matches
// `CapabilitySet` on the Rust side one-for-one.
type CapabilitySet struct {
	Hardware *HardwareCaps     `json:"hardware,omitempty"`
	Software *SoftwareCaps     `json:"software,omitempty"`
	Models   []ModelCaps       `json:"models,omitempty"`
	Tools    []ToolCaps        `json:"tools,omitempty"`
	Tags     []string          `json:"tags,omitempty"`
	Limits   *CapabilityLimits `json:"limits,omitempty"`
}

// CapabilityFilter describes the subset of announcements that
// `FindNodes` should return. Empty filter matches every announcer.
type CapabilityFilter struct {
	RequireTags       []string `json:"require_tags,omitempty"`
	RequireModels     []string `json:"require_models,omitempty"`
	RequireTools      []string `json:"require_tools,omitempty"`
	MinMemoryGB       uint32   `json:"min_memory_gb,omitempty"`
	RequireGPU        bool     `json:"require_gpu,omitempty"`
	GPUVendor         string   `json:"gpu_vendor,omitempty"`
	MinVRAMGB         uint32   `json:"min_vram_gb,omitempty"`
	MinContextLength  uint32   `json:"min_context_length,omitempty"`
	RequireModalities []string `json:"require_modalities,omitempty"`
}

// ---------------------------------------------------------------------------
// MeshNode methods
// ---------------------------------------------------------------------------

// AnnounceCapabilities broadcasts `caps` to every directly-connected
// peer and self-indexes, so `FindNodes` on this same node matches
// when the filter is compatible. Multi-hop propagation is deferred.
func (m *MeshNode) AnnounceCapabilities(caps CapabilitySet) error {
	data, err := json.Marshal(caps)
	if err != nil {
		return fmt.Errorf("marshal caps: %w", err)
	}
	cJSON := C.CString(string(data))
	defer C.free(unsafe.Pointer(cJSON))

	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return ErrShuttingDown
	}
	code := C.net_mesh_announce_capabilities(m.handle, cJSON)
	return capabilityErrorFromCode(code)
}

// FindNodes queries the local capability index. Returns the node ids
// (u64) of every announcer whose latest announcement matches
// `filter`, including own node id on self-match.
func (m *MeshNode) FindNodes(filter CapabilityFilter) ([]uint64, error) {
	data, err := json.Marshal(filter)
	if err != nil {
		return nil, fmt.Errorf("marshal filter: %w", err)
	}
	cJSON := C.CString(string(data))
	defer C.free(unsafe.Pointer(cJSON))

	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return nil, ErrShuttingDown
	}
	var outJSON *C.char
	var outLen C.size_t
	code := C.net_mesh_find_nodes(m.handle, cJSON, &outJSON, &outLen)
	if err := capabilityErrorFromCode(code); err != nil {
		return nil, err
	}
	defer C.net_free_string(outJSON)
	raw := C.GoStringN(outJSON, C.int(outLen))
	var ids []uint64
	if err := json.Unmarshal([]byte(raw), &ids); err != nil {
		return nil, fmt.Errorf("parse find_nodes response: %w", err)
	}
	return ids, nil
}

// ScopeFilter narrows `FindNodesScoped` results by reserved
// `scope:*` tags on each node's `CapabilitySet`. Tagged-union by
// `Kind`; mirrors the NAPI / PyO3 shape so cross-binding fixtures
// round-trip.
//
// Recognized `Kind` values:
//   - `"any"` — every non-`SubnetLocal` node
//   - `"global_only"` — only untagged (Global) nodes
//   - `"same_subnet"` — caller's subnet only
//   - `"tenant"` — that tenant + Global; `Tenant` field required
//   - `"tenants"` — any of those + Global; `Tenants` field required
//   - `"region"` — that region + Global; `Region` field required
//   - `"regions"` — any of those + Global; `Regions` field required
//
// Unknown `Kind` values fall through to `"any"` defensively. Empty
// strings or empty lists also collapse to `"any"`.
type ScopeFilter struct {
	Kind    string   `json:"kind"`
	Tenant  string   `json:"tenant,omitempty"`
	Tenants []string `json:"tenants,omitempty"`
	Region  string   `json:"region,omitempty"`
	Regions []string `json:"regions,omitempty"`
}

// FindNodesScoped is the scoped variant of FindNodes. Filters
// candidates through `scope` (derived from each node's `scope:*`
// reserved tags) on top of the capability filter. Untagged nodes
// stay visible under most filters; nodes tagged `scope:subnet-local`
// only show up under `ScopeFilter{Kind: "same_subnet"}`.
//
// See `docs/SCOPED_CAPABILITIES_PLAN.md` for the full table.
func (m *MeshNode) FindNodesScoped(filter CapabilityFilter, scope ScopeFilter) ([]uint64, error) {
	filterJSON, err := json.Marshal(filter)
	if err != nil {
		return nil, fmt.Errorf("marshal filter: %w", err)
	}
	scopeJSON, err := json.Marshal(scope)
	if err != nil {
		return nil, fmt.Errorf("marshal scope: %w", err)
	}
	cFilter := C.CString(string(filterJSON))
	defer C.free(unsafe.Pointer(cFilter))
	cScope := C.CString(string(scopeJSON))
	defer C.free(unsafe.Pointer(cScope))

	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return nil, ErrShuttingDown
	}
	var outJSON *C.char
	var outLen C.size_t
	code := C.net_mesh_find_nodes_scoped(m.handle, cFilter, cScope, &outJSON, &outLen)
	if err := capabilityErrorFromCode(code); err != nil {
		return nil, err
	}
	defer C.net_free_string(outJSON)
	raw := C.GoStringN(outJSON, C.int(outLen))
	var ids []uint64
	if err := json.Unmarshal([]byte(raw), &ids); err != nil {
		return nil, fmt.Errorf("parse find_nodes_scoped response: %w", err)
	}
	return ids, nil
}

// CapabilityRequirement is a placement requirement: a base
// capability filter plus optional scoring weights. Higher weight
// (in [0.0, 1.0]) tips ties toward more memory / VRAM / faster
// inference / pre-loaded models. Weights are clamped on the Rust
// side; values outside the range are silently capped.
type CapabilityRequirement struct {
	Filter                CapabilityFilter `json:"filter"`
	PreferMoreMemory      float32          `json:"prefer_more_memory,omitempty"`
	PreferMoreVRAM        float32          `json:"prefer_more_vram,omitempty"`
	PreferFasterInference float32          `json:"prefer_faster_inference,omitempty"`
	PreferLoadedModels    float32          `json:"prefer_loaded_models,omitempty"`
}

// FindBestNode picks the highest-scoring node for a placement
// requirement. Returns `(nodeId, true, nil)` on hit,
// `(0, false, nil)` on no match, or `(_, _, err)` on parse / FFI
// failure. The boolean disambiguates "no match" from `nodeId == 0`,
// which is a valid id.
func (m *MeshNode) FindBestNode(req CapabilityRequirement) (uint64, bool, error) {
	data, err := json.Marshal(req)
	if err != nil {
		return 0, false, fmt.Errorf("marshal requirement: %w", err)
	}
	cJSON := C.CString(string(data))
	defer C.free(unsafe.Pointer(cJSON))

	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return 0, false, ErrShuttingDown
	}
	var outNodeID C.uint64_t
	var outHasMatch C.int
	code := C.net_mesh_find_best_node(m.handle, cJSON, &outNodeID, &outHasMatch)
	if err := capabilityErrorFromCode(code); err != nil {
		return 0, false, err
	}
	if outHasMatch == 0 {
		return 0, false, nil
	}
	return uint64(outNodeID), true, nil
}

// FindBestNodeScoped is the scoped variant of FindBestNode. Filters
// candidates through `scope` (same semantics as FindNodesScoped)
// before scoring; returns the highest-scoring node within the
// scope-filtered set.
func (m *MeshNode) FindBestNodeScoped(
	req CapabilityRequirement,
	scope ScopeFilter,
) (uint64, bool, error) {
	reqJSON, err := json.Marshal(req)
	if err != nil {
		return 0, false, fmt.Errorf("marshal requirement: %w", err)
	}
	scopeJSON, err := json.Marshal(scope)
	if err != nil {
		return 0, false, fmt.Errorf("marshal scope: %w", err)
	}
	cReq := C.CString(string(reqJSON))
	defer C.free(unsafe.Pointer(cReq))
	cScope := C.CString(string(scopeJSON))
	defer C.free(unsafe.Pointer(cScope))

	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return 0, false, ErrShuttingDown
	}
	var outNodeID C.uint64_t
	var outHasMatch C.int
	code := C.net_mesh_find_best_node_scoped(m.handle, cReq, cScope, &outNodeID, &outHasMatch)
	if err := capabilityErrorFromCode(code); err != nil {
		return 0, false, err
	}
	if outHasMatch == 0 {
		return 0, false, nil
	}
	return uint64(outNodeID), true, nil
}

// NormalizeGPUVendor maps a GPU vendor string to its canonical
// lowercase form (`nvidia`, `amd`, `intel`, `apple`, `qualcomm`,
// `unknown`). Matches the NAPI / PyO3 helper so every SDK produces
// an identical announcement payload.
func NormalizeGPUVendor(raw string) (string, error) {
	cRaw := C.CString(raw)
	defer C.free(unsafe.Pointer(cRaw))
	var out *C.char
	var outLen C.size_t
	code := C.net_normalize_gpu_vendor(cRaw, &out, &outLen)
	if err := identityErrorFromCode(code); err != nil {
		return "", err
	}
	defer C.net_free_string(out)
	return C.GoStringN(out, C.int(outLen)), nil
}
