// Package net — aggregator-registry + fold-query RPC client
// surface.
//
// Wraps the C ABI exported by `net::ffi::aggregator` (compiled
// into the main `libnet` cdylib alongside the netdb / cortex
// FFI symbols). The full reference implementation with
// docstrings + every documented edge case lives at
// `net/crates/net/bindings/go/net/aggregator.go`; this file is
// the consumer-side trim built against the same C symbols.
//
// # Build prerequisite
//
//	cargo build --release --features net
//	# libnet.{so,dylib} ends up in net/crates/net/target/release/
//
// `#cgo LDFLAGS` below points at that directory relative to
// SRCDIR; the consumer go.mod sits next to it so the relative
// path is stable.

package net

/*
#cgo LDFLAGS: -L${SRCDIR}/../net/crates/net/target/release -lnet
#include <stdint.h>
#include <stdlib.h>

// Forward-declared opaque handles from `libnet`.
typedef struct net_registry_client_handle_t net_registry_client_handle_t;
typedef struct net_fold_query_client_handle_t net_fold_query_client_handle_t;

// Error-kind discriminants — locked across SDKs.
#define NET_REGISTRY_OK                       0
#define NET_REGISTRY_ERR_TRANSPORT            1
#define NET_REGISTRY_ERR_CODEC                2
#define NET_REGISTRY_ERR_UNKNOWN_TEMPLATE     3
#define NET_REGISTRY_ERR_DUPLICATE_GROUP_NAME 4
#define NET_REGISTRY_ERR_SPAWN_REJECTED       5
#define NET_REGISTRY_ERR_SPAWN_NOT_SUPPORTED  6
#define NET_REGISTRY_ERR_UNKNOWN_KIND         7
#define NET_REGISTRY_ERR_INVALID_ARGS         99

// RegistryClient.
extern net_registry_client_handle_t* net_registry_client_new(void* mesh_handle);
extern void net_registry_client_free(net_registry_client_handle_t* handle);
extern void net_registry_client_set_deadline(
    net_registry_client_handle_t* handle, uint64_t millis);
extern char* net_registry_client_list(
    net_registry_client_handle_t* handle,
    uint64_t target_node_id,
    int* out_error_kind);
extern char* net_registry_client_spawn(
    net_registry_client_handle_t* handle,
    uint64_t target_node_id,
    const char* template_name,
    const char* group_name,
    uint8_t replica_count,
    int* out_error_kind);
extern int net_registry_client_unregister(
    net_registry_client_handle_t* handle,
    uint64_t target_node_id,
    const char* group_name,
    int* out_error_kind);
extern const char* net_registry_last_error_detail(net_registry_client_handle_t* handle);

// FoldQueryClient.
extern net_fold_query_client_handle_t* net_fold_query_client_new(void* mesh_handle);
extern void net_fold_query_client_free(net_fold_query_client_handle_t* handle);
extern void net_fold_query_client_set_ttl(
    net_fold_query_client_handle_t* handle, uint64_t millis);
extern void net_fold_query_client_set_deadline(
    net_fold_query_client_handle_t* handle, uint64_t millis);
extern char* net_fold_query_client_query_latest(
    net_fold_query_client_handle_t* handle,
    uint64_t target_node_id,
    uint16_t kind,
    int* out_error_kind);
extern char* net_fold_query_client_query_summarize_now(
    net_fold_query_client_handle_t* handle,
    uint64_t target_node_id,
    uint16_t kind,
    int* out_error_kind);
extern void net_fold_query_client_invalidate_cache(net_fold_query_client_handle_t* handle);
extern void net_fold_query_client_invalidate_target(
    net_fold_query_client_handle_t* handle, uint64_t target_node_id);
extern const char* net_fold_query_last_error_detail(net_fold_query_client_handle_t* handle);

// Free strings returned by the *_list / *_spawn / *_query_* ops.
// Same symbol the netdb / memories surfaces use.
extern void net_free_string(char* s);
*/
import "C"

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"runtime"
	"sync"
	"time"
	"unsafe"
)

// =====================================================================
// Error types
// =====================================================================

// RegistryErrorKind is the stable kebab-case discriminator a
// `*RegistryClientError` carries in its `Kind` field.
type RegistryErrorKind string

const (
	RegistryErrKindTransport          RegistryErrorKind = "transport"
	RegistryErrKindCodec              RegistryErrorKind = "codec"
	RegistryErrKindUnknownTemplate    RegistryErrorKind = "unknown-template"
	RegistryErrKindDuplicateGroupName RegistryErrorKind = "duplicate-group-name"
	RegistryErrKindSpawnRejected      RegistryErrorKind = "spawn-rejected"
	RegistryErrKindSpawnNotSupported  RegistryErrorKind = "spawn-not-supported"
	RegistryErrKindInvalidArgs        RegistryErrorKind = "invalid-args"
)

// RegistryClientError is the typed failure from any registry op.
// Retrieve via `errors.As(err, &reg)`.
type RegistryClientError struct {
	Kind   RegistryErrorKind
	Detail string
}

func (e *RegistryClientError) Error() string {
	if e.Detail == "" {
		return "agg:" + string(e.Kind)
	}
	return fmt.Sprintf("agg:%s: %s", e.Kind, e.Detail)
}

// FoldQueryErrorKind is the discriminator for fold-query op failures.
type FoldQueryErrorKind string

const (
	FoldQueryErrKindTransport   FoldQueryErrorKind = "transport"
	FoldQueryErrKindCodec       FoldQueryErrorKind = "codec"
	FoldQueryErrKindUnknownKind FoldQueryErrorKind = "unknown-kind"
	FoldQueryErrKindInvalidArgs FoldQueryErrorKind = "invalid-args"
)

// FoldQueryClientError is the typed failure from any fold-query op.
type FoldQueryClientError struct {
	Kind   FoldQueryErrorKind
	Detail string
}

func (e *FoldQueryClientError) Error() string {
	if e.Detail == "" {
		return "agg:" + string(e.Kind)
	}
	return fmt.Sprintf("agg:%s: %s", e.Kind, e.Detail)
}

// ErrAggregatorHandleClosed is returned by any client op invoked
// after Close() has freed the underlying handle.
var ErrAggregatorHandleClosed = errors.New("net: aggregator client handle is closed")

func registryErrFromKind(kind C.int, detail string) *RegistryClientError {
	var k RegistryErrorKind
	switch kind {
	case C.NET_REGISTRY_ERR_TRANSPORT:
		k = RegistryErrKindTransport
	case C.NET_REGISTRY_ERR_CODEC:
		k = RegistryErrKindCodec
	case C.NET_REGISTRY_ERR_UNKNOWN_TEMPLATE:
		k = RegistryErrKindUnknownTemplate
	case C.NET_REGISTRY_ERR_DUPLICATE_GROUP_NAME:
		k = RegistryErrKindDuplicateGroupName
	case C.NET_REGISTRY_ERR_SPAWN_REJECTED:
		k = RegistryErrKindSpawnRejected
	case C.NET_REGISTRY_ERR_SPAWN_NOT_SUPPORTED:
		k = RegistryErrKindSpawnNotSupported
	case C.NET_REGISTRY_ERR_INVALID_ARGS:
		k = RegistryErrKindInvalidArgs
	default:
		k = RegistryErrorKind(fmt.Sprintf("unknown-%d", int(kind)))
	}
	return &RegistryClientError{Kind: k, Detail: detail}
}

func foldQueryErrFromKind(kind C.int, detail string) *FoldQueryClientError {
	var k FoldQueryErrorKind
	switch kind {
	case C.NET_REGISTRY_ERR_TRANSPORT:
		k = FoldQueryErrKindTransport
	case C.NET_REGISTRY_ERR_CODEC:
		k = FoldQueryErrKindCodec
	case C.NET_REGISTRY_ERR_UNKNOWN_KIND:
		k = FoldQueryErrKindUnknownKind
	case C.NET_REGISTRY_ERR_INVALID_ARGS:
		k = FoldQueryErrKindInvalidArgs
	default:
		k = FoldQueryErrorKind(fmt.Sprintf("unknown-%d", int(kind)))
	}
	return &FoldQueryClientError{Kind: k, Detail: detail}
}

// =====================================================================
// Wire-shape structs — locked across the Node / Python / Go SDKs.
// =====================================================================

// RegistryReplicaRow is one replica inside a RegistryGroupSummary.
type RegistryReplicaRow struct {
	Generation      uint64  `json:"generation"`
	Healthy         bool    `json:"healthy"`
	Diagnostic      *string `json:"diagnostic"`
	PlacementNodeID *uint64 `json:"placement_node_id"`
}

// RegistryGroupSummary mirrors the SDK's substrate-side type.
type RegistryGroupSummary struct {
	Name         string               `json:"name"`
	GroupSeedHex string               `json:"group_seed_hex"`
	Replicas     []RegistryReplicaRow `json:"replicas"`
}

// SummaryBucket is one (name, count) pair inside a SummaryAnnouncement.
type SummaryBucket struct {
	Name  string `json:"name"`
	Count uint64 `json:"count"`
}

// SummaryAnnouncement mirrors the substrate-side type. `FoldKind`
// is the numeric `FoldKind::KIND_ID`; `SourceSubnet` is
// dotted-notation ("3.7") or "global".
type SummaryAnnouncement struct {
	FoldKind     uint32          `json:"fold_kind"`
	SourceSubnet string          `json:"source_subnet"`
	Generation   uint64          `json:"generation"`
	Buckets      []SummaryBucket `json:"buckets"`
}

// =====================================================================
// RegistryClient
// =====================================================================

// RegistryClient is a typed client for the `aggregator.registry`
// RPC service. Construct via NewRegistryClient against a live
// mesh handle.
type RegistryClient struct {
	mu     sync.RWMutex
	handle *C.net_registry_client_handle_t
}

// NewRegistryClient builds a RegistryClient bound to `meshHandle`
// (the opaque pointer produced by the main libnet's net_mesh_new).
func NewRegistryClient(meshHandle unsafe.Pointer) (*RegistryClient, error) {
	if meshHandle == nil {
		return nil, &RegistryClientError{
			Kind:   RegistryErrKindInvalidArgs,
			Detail: "mesh handle is nil",
		}
	}
	h := C.net_registry_client_new(meshHandle)
	if h == nil {
		return nil, &RegistryClientError{
			Kind:   RegistryErrKindInvalidArgs,
			Detail: "net_registry_client_new returned NULL",
		}
	}
	c := &RegistryClient{handle: h}
	runtime.SetFinalizer(c, func(c *RegistryClient) { _ = c.Close() })
	return c, nil
}

// Close releases the underlying handle. Idempotent.
func (c *RegistryClient) Close() error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.handle == nil {
		return nil
	}
	C.net_registry_client_free(c.handle)
	c.handle = nil
	runtime.SetFinalizer(c, nil)
	return nil
}

// WithDeadline sets the per-call deadline. Pass 0 to reset.
func (c *RegistryClient) WithDeadline(d time.Duration) *RegistryClient {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.handle != nil {
		C.net_registry_client_set_deadline(c.handle, C.uint64_t(d/time.Millisecond))
	}
	return c
}

// List returns the groups registered on `targetNodeID`.
func (c *RegistryClient) List(ctx context.Context, targetNodeID uint64) ([]RegistryGroupSummary, error) {
	c.mu.RLock()
	defer c.mu.RUnlock()
	if c.handle == nil {
		return nil, ErrAggregatorHandleClosed
	}
	c.honorContextDeadline(ctx)
	var errKind C.int
	jsonPtr := C.net_registry_client_list(c.handle, C.uint64_t(targetNodeID), &errKind)
	if jsonPtr == nil {
		return nil, registryErrFromKind(errKind, c.lastErrorDetail())
	}
	defer C.net_free_string(jsonPtr)
	var out []RegistryGroupSummary
	if err := json.Unmarshal([]byte(C.GoString(jsonPtr)), &out); err != nil {
		return nil, &RegistryClientError{
			Kind:   RegistryErrKindCodec,
			Detail: fmt.Sprintf("decode list response: %v", err),
		}
	}
	return out, nil
}

// Spawn deploys a new group via a daemon-side template.
func (c *RegistryClient) Spawn(
	ctx context.Context,
	targetNodeID uint64,
	templateName, groupName string,
	replicaCount uint8,
) (RegistryGroupSummary, error) {
	c.mu.RLock()
	defer c.mu.RUnlock()
	if c.handle == nil {
		return RegistryGroupSummary{}, ErrAggregatorHandleClosed
	}
	c.honorContextDeadline(ctx)
	cTemplate := C.CString(templateName)
	defer C.free(unsafe.Pointer(cTemplate))
	cGroup := C.CString(groupName)
	defer C.free(unsafe.Pointer(cGroup))
	var errKind C.int
	jsonPtr := C.net_registry_client_spawn(
		c.handle,
		C.uint64_t(targetNodeID),
		cTemplate,
		cGroup,
		C.uint8_t(replicaCount),
		&errKind,
	)
	if jsonPtr == nil {
		return RegistryGroupSummary{}, registryErrFromKind(errKind, c.lastErrorDetail())
	}
	defer C.net_free_string(jsonPtr)
	var out RegistryGroupSummary
	if err := json.Unmarshal([]byte(C.GoString(jsonPtr)), &out); err != nil {
		return RegistryGroupSummary{}, &RegistryClientError{
			Kind:   RegistryErrKindCodec,
			Detail: fmt.Sprintf("decode spawn response: %v", err),
		}
	}
	return out, nil
}

// Unregister tears down a registered group by name. Returns
// (true, nil) when the group existed and was stopped, (false, nil)
// when no group matched, or an error on transport / codec failure.
func (c *RegistryClient) Unregister(
	ctx context.Context,
	targetNodeID uint64,
	groupName string,
) (bool, error) {
	c.mu.RLock()
	defer c.mu.RUnlock()
	if c.handle == nil {
		return false, ErrAggregatorHandleClosed
	}
	c.honorContextDeadline(ctx)
	cGroup := C.CString(groupName)
	defer C.free(unsafe.Pointer(cGroup))
	var errKind C.int
	result := C.net_registry_client_unregister(
		c.handle,
		C.uint64_t(targetNodeID),
		cGroup,
		&errKind,
	)
	switch result {
	case 1:
		return true, nil
	case 0:
		return false, nil
	default:
		return false, registryErrFromKind(errKind, c.lastErrorDetail())
	}
}

func (c *RegistryClient) honorContextDeadline(ctx context.Context) {
	if ctx == nil {
		return
	}
	if deadline, ok := ctx.Deadline(); ok {
		remaining := time.Until(deadline)
		if remaining <= 0 {
			return
		}
		C.net_registry_client_set_deadline(
			c.handle,
			C.uint64_t(remaining/time.Millisecond),
		)
	}
}

func (c *RegistryClient) lastErrorDetail() string {
	if c.handle == nil {
		return ""
	}
	ptr := C.net_registry_last_error_detail(c.handle)
	if ptr == nil {
		return ""
	}
	return C.GoString(ptr)
}

// =====================================================================
// FoldQueryClient
// =====================================================================

// FoldQueryClient is a typed client for the `fold.query` RPC
// service with a configurable TTL cache.
type FoldQueryClient struct {
	mu     sync.RWMutex
	handle *C.net_fold_query_client_handle_t
}

// NewFoldQueryClient builds a FoldQueryClient bound to `meshHandle`.
func NewFoldQueryClient(meshHandle unsafe.Pointer) (*FoldQueryClient, error) {
	if meshHandle == nil {
		return nil, &FoldQueryClientError{
			Kind:   FoldQueryErrKindInvalidArgs,
			Detail: "mesh handle is nil",
		}
	}
	h := C.net_fold_query_client_new(meshHandle)
	if h == nil {
		return nil, &FoldQueryClientError{
			Kind:   FoldQueryErrKindInvalidArgs,
			Detail: "net_fold_query_client_new returned NULL",
		}
	}
	c := &FoldQueryClient{handle: h}
	runtime.SetFinalizer(c, func(c *FoldQueryClient) { _ = c.Close() })
	return c, nil
}

// Close releases the underlying handle.
func (c *FoldQueryClient) Close() error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.handle == nil {
		return nil
	}
	C.net_fold_query_client_free(c.handle)
	c.handle = nil
	runtime.SetFinalizer(c, nil)
	return nil
}

// WithTTL sets the cache TTL. Pass 0 to disable caching.
func (c *FoldQueryClient) WithTTL(d time.Duration) *FoldQueryClient {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.handle != nil {
		C.net_fold_query_client_set_ttl(c.handle, C.uint64_t(d/time.Millisecond))
	}
	return c
}

// WithDeadline sets the per-call deadline.
func (c *FoldQueryClient) WithDeadline(d time.Duration) *FoldQueryClient {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.handle != nil {
		C.net_fold_query_client_set_deadline(c.handle, C.uint64_t(d/time.Millisecond))
	}
	return c
}

// QueryLatest returns the aggregator's latest cached summaries.
func (c *FoldQueryClient) QueryLatest(
	ctx context.Context,
	targetNodeID uint64,
	kind uint16,
) ([]SummaryAnnouncement, error) {
	c.mu.RLock()
	defer c.mu.RUnlock()
	if c.handle == nil {
		return nil, ErrAggregatorHandleClosed
	}
	c.honorContextDeadline(ctx)
	var errKind C.int
	jsonPtr := C.net_fold_query_client_query_latest(
		c.handle,
		C.uint64_t(targetNodeID),
		C.uint16_t(kind),
		&errKind,
	)
	if jsonPtr == nil {
		return nil, foldQueryErrFromKind(errKind, c.lastErrorDetail())
	}
	defer C.net_free_string(jsonPtr)
	var out []SummaryAnnouncement
	if err := json.Unmarshal([]byte(C.GoString(jsonPtr)), &out); err != nil {
		return nil, &FoldQueryClientError{
			Kind:   FoldQueryErrKindCodec,
			Detail: fmt.Sprintf("decode query_latest response: %v", err),
		}
	}
	return out, nil
}

// QuerySummarizeNow forces a fresh SummarizeNow — never cached.
func (c *FoldQueryClient) QuerySummarizeNow(
	ctx context.Context,
	targetNodeID uint64,
	kind uint16,
) ([]SummaryAnnouncement, error) {
	c.mu.RLock()
	defer c.mu.RUnlock()
	if c.handle == nil {
		return nil, ErrAggregatorHandleClosed
	}
	c.honorContextDeadline(ctx)
	var errKind C.int
	jsonPtr := C.net_fold_query_client_query_summarize_now(
		c.handle,
		C.uint64_t(targetNodeID),
		C.uint16_t(kind),
		&errKind,
	)
	if jsonPtr == nil {
		return nil, foldQueryErrFromKind(errKind, c.lastErrorDetail())
	}
	defer C.net_free_string(jsonPtr)
	var out []SummaryAnnouncement
	if err := json.Unmarshal([]byte(C.GoString(jsonPtr)), &out); err != nil {
		return nil, &FoldQueryClientError{
			Kind:   FoldQueryErrKindCodec,
			Detail: fmt.Sprintf("decode query_summarize_now response: %v", err),
		}
	}
	return out, nil
}

// InvalidateCache drops every cached entry.
func (c *FoldQueryClient) InvalidateCache() {
	c.mu.RLock()
	defer c.mu.RUnlock()
	if c.handle != nil {
		C.net_fold_query_client_invalidate_cache(c.handle)
	}
}

// InvalidateTarget drops only entries matching `targetNodeID`.
func (c *FoldQueryClient) InvalidateTarget(targetNodeID uint64) {
	c.mu.RLock()
	defer c.mu.RUnlock()
	if c.handle != nil {
		C.net_fold_query_client_invalidate_target(c.handle, C.uint64_t(targetNodeID))
	}
}

func (c *FoldQueryClient) honorContextDeadline(ctx context.Context) {
	if ctx == nil {
		return
	}
	if deadline, ok := ctx.Deadline(); ok {
		remaining := time.Until(deadline)
		if remaining <= 0 {
			return
		}
		C.net_fold_query_client_set_deadline(
			c.handle,
			C.uint64_t(remaining/time.Millisecond),
		)
	}
}

func (c *FoldQueryClient) lastErrorDetail() string {
	if c.handle == nil {
		return ""
	}
	ptr := C.net_fold_query_last_error_detail(c.handle)
	if ptr == nil {
		return ""
	}
	return C.GoString(ptr)
}
