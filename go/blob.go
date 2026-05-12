// Package net — Dataforts blob storage bindings.
//
// Wraps the `net::ffi::blob` C FFI for the v0.2 substrate-owned
// `MeshBlobAdapter` + the v0.3 active overflow extension.
//
// The adapter is Rust-backed (chunks live in Redex as
// content-addressed `RedexFile`s); Go callers get a thin
// wrapper over the opaque `MeshBlobAdapterHandle*` pointer.
// Mirrors the Python + Node bindings.
//
// # State persistence
//
// Chunk bytes persist on disk via the underlying `Redex` (so
// `Persistent: true` round-trips across process restart);
// refcount + metrics state is per-process.
//
// # Overflow opt-in
//
// Disabled by default. To turn on:
//
//	cfg := &OverflowConfig{
//	    Enabled:          true,
//	    HighWaterRatio:   0.85,
//	    LowWaterRatio:    0.70,
//	    MaxPushesPerTick: 16,
//	    Scope:            "mesh",
//	    TickIntervalMs:   30_000,
//	}
//	adapter, err := NewMeshBlobAdapter(redex, "go-prod", &MeshBlobAdapterOpts{
//	    Persistent: true,
//	    Overflow:   cfg,
//	})

package net

/*
#include <stdint.h>
#include <stdlib.h>

typedef struct MeshBlobAdapterHandle MeshBlobAdapterHandle;
typedef struct RedexHandle RedexHandle;

// v0.2 substrate-owned blob CAS — basic CRUD.
extern MeshBlobAdapterHandle* net_mesh_blob_adapter_new(
    RedexHandle* redex,
    const char* adapter_id,
    int persistent,
    const char* overflow_json
);
extern void net_mesh_blob_adapter_free(MeshBlobAdapterHandle* handle);
extern int net_mesh_blob_adapter_store(
    const MeshBlobAdapterHandle* handle,
    const uint8_t* blob_ref_bytes,
    size_t blob_ref_len,
    const uint8_t* data,
    size_t data_len
);
extern int net_mesh_blob_adapter_fetch(
    const MeshBlobAdapterHandle* handle,
    const uint8_t* blob_ref_bytes,
    size_t blob_ref_len,
    uint8_t** out_data,
    size_t* out_len
);
extern int net_mesh_blob_adapter_exists(
    const MeshBlobAdapterHandle* handle,
    const uint8_t* blob_ref_bytes,
    size_t blob_ref_len,
    int* out_exists
);
extern char* net_mesh_blob_adapter_prometheus_text(const MeshBlobAdapterHandle* handle);
extern void net_blob_free_buffer(uint8_t* ptr, size_t len);

// v0.3 active overflow.
extern int net_mesh_blob_adapter_overflow_enabled(const MeshBlobAdapterHandle* handle);
extern int net_mesh_blob_adapter_overflow_active(const MeshBlobAdapterHandle* handle);
extern char* net_mesh_blob_adapter_overflow_config(const MeshBlobAdapterHandle* handle);
extern int net_mesh_blob_adapter_set_overflow_enabled(
    const MeshBlobAdapterHandle* handle,
    int enabled
);
extern int net_mesh_blob_adapter_set_overflow_config(
    const MeshBlobAdapterHandle* handle,
    const char* config_json
);
*/
import "C"

import (
	"encoding/json"
	"errors"
	"fmt"
	"runtime"
	"sync"
	"unsafe"
)

// ErrBlob is the umbrella error for any failure surfaced by the
// `net_mesh_blob_*` FFI. Use `errors.Is(err, ErrBlob)` to match
// any blob-related failure regardless of the underlying typed kind.
var ErrBlob = errors.New("blob")

// ErrBlobClosed is returned when an op runs against an already-
// closed adapter handle.
var ErrBlobClosed = fmt.Errorf("%w: adapter handle already closed", ErrBlob)

// ErrBlobInvalidConfig wraps the FFI's `InvalidJson` for the
// overflow-config parser path. Surfaces operator typos at the
// boundary (unknown scope, malformed JSON).
var ErrBlobInvalidConfig = fmt.Errorf("%w: invalid overflow config", ErrBlob)

// OverflowConfig mirrors the typed Rust + Python config shape.
//
// Pass to `NewMeshBlobAdapter` at construction or to
// `(*MeshBlobAdapter).SetOverflowConfig` at runtime.
type OverflowConfig struct {
	// Master switch. `false` (default) keeps the adapter on the
	// v0.2 pull-only posture — no `dataforts.blob.overflow`
	// capability tag advertised, no inbound `OverflowPush`
	// accepted.
	Enabled bool `json:"enabled"`

	// Disk usage ratio at or above which the overflow tick
	// fires. Default `0.85` when omitted (handled inside the
	// Rust default).
	HighWaterRatio float64 `json:"high_water_ratio,omitempty"`

	// Disk usage ratio at or below which the controller
	// re-enters the inactive state. Hysteresis band between
	// `LowWaterRatio` and `HighWaterRatio`. Default `0.70`.
	LowWaterRatio float64 `json:"low_water_ratio,omitempty"`

	// Per-tick push budget. Each push opens a chunk channel
	// with replication armed. Default `16`.
	MaxPushesPerTick uint64 `json:"max_pushes_per_tick,omitempty"`

	// Topology scope: one of `"node"`, `"zone"`, `"region"`,
	// `"mesh"`. Default `"mesh"`.
	Scope string `json:"scope,omitempty"`

	// Tick cadence in milliseconds. Default `30000`.
	TickIntervalMs uint64 `json:"tick_interval_ms,omitempty"`
}

// MeshBlobAdapterOpts is the optional bag for
// `NewMeshBlobAdapter`. Zero-value is "in-memory, no overflow"
// — matches v0.2 pull-only behavior.
type MeshBlobAdapterOpts struct {
	// Opt every per-chunk file into disk persistence. Requires
	// the underlying `Redex` to have been constructed with a
	// `persistent_dir` (i.e. via `NewRedexWithPersistentDir`).
	Persistent bool

	// Initial overflow configuration. Pass `nil` for the v0.2
	// posture (disabled); pass `&OverflowConfig{Enabled: true}`
	// to opt in at defaults; pass a fully-populated struct to
	// tune thresholds at construction.
	Overflow *OverflowConfig
}

// MeshBlobAdapter wraps `*MeshBlobAdapterHandle`. Cheap to
// share via the Go runtime; methods take an internal lock
// around `Close()` to serialize FFI `_free` against any
// concurrent in-flight op.
type MeshBlobAdapter struct {
	mu     sync.Mutex
	handle *C.MeshBlobAdapterHandle
}

// NewMeshBlobAdapter constructs a substrate-owned blob adapter
// against `redex`. `adapterID` surfaces in the Prometheus body's
// `adapter=...` label.
//
// The adapter is feature-gated server-side on
// `dataforts,netdb,redex-disk`; if the runtime was built
// without those features, the FFI returns null and this
// constructor returns `ErrBlob`.
//
// Like every Go binding handle in this crate, a finalizer is
// installed but should NOT be relied upon — pair every
// constructor with `defer adapter.Close()`.
func NewMeshBlobAdapter(redex *Redex, adapterID string, opts *MeshBlobAdapterOpts) (*MeshBlobAdapter, error) {
	if redex == nil || redex.handle == nil {
		return nil, fmt.Errorf("%w: redex handle is nil", ErrBlob)
	}
	persistent := C.int(0)
	overflowJSON := (*C.char)(nil)
	if opts != nil {
		if opts.Persistent {
			persistent = 1
		}
		if opts.Overflow != nil {
			body, err := json.Marshal(opts.Overflow)
			if err != nil {
				return nil, fmt.Errorf("%w: %v", ErrBlobInvalidConfig, err)
			}
			overflowJSON = C.CString(string(body))
			defer C.free(unsafe.Pointer(overflowJSON))
		}
	}
	cID := C.CString(adapterID)
	defer C.free(unsafe.Pointer(cID))
	h := C.net_mesh_blob_adapter_new(redex.handle, cID, persistent, overflowJSON)
	if h == nil {
		return nil, fmt.Errorf("%w: substrate returned null (check feature gates: dataforts,netdb,redex-disk)", ErrBlob)
	}
	a := &MeshBlobAdapter{handle: h}
	runtime.SetFinalizer(a, func(a *MeshBlobAdapter) { _ = a.Close() })
	return a, nil
}

// Close releases the underlying handle. Idempotent.
func (a *MeshBlobAdapter) Close() error {
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.handle == nil {
		return nil
	}
	C.net_mesh_blob_adapter_free(a.handle)
	a.handle = nil
	runtime.SetFinalizer(a, nil)
	return nil
}

// Store `data` under the content address declared by
// `blobRefBytes` (a previously-encoded `BlobRef` wire blob).
// The substrate BLAKE3-verifies + raises a typed error on
// mismatch.
func (a *MeshBlobAdapter) Store(blobRefBytes, data []byte) error {
	a.mu.Lock()
	handle := a.handle
	a.mu.Unlock()
	if handle == nil {
		return ErrBlobClosed
	}
	var refPtr *C.uint8_t
	if len(blobRefBytes) > 0 {
		refPtr = (*C.uint8_t)(unsafe.Pointer(&blobRefBytes[0]))
	}
	var dataPtr *C.uint8_t
	if len(data) > 0 {
		dataPtr = (*C.uint8_t)(unsafe.Pointer(&data[0]))
	}
	rc := C.net_mesh_blob_adapter_store(
		handle,
		refPtr, C.size_t(len(blobRefBytes)),
		dataPtr, C.size_t(len(data)),
	)
	if rc != 0 {
		return fmt.Errorf("%w: store failed with rc=%d", ErrBlob, int(rc))
	}
	return nil
}

// Fetch returns the content-addressed bytes for `blobRefBytes`.
func (a *MeshBlobAdapter) Fetch(blobRefBytes []byte) ([]byte, error) {
	a.mu.Lock()
	handle := a.handle
	a.mu.Unlock()
	if handle == nil {
		return nil, ErrBlobClosed
	}
	var refPtr *C.uint8_t
	if len(blobRefBytes) > 0 {
		refPtr = (*C.uint8_t)(unsafe.Pointer(&blobRefBytes[0]))
	}
	var outPtr *C.uint8_t
	var outLen C.size_t
	rc := C.net_mesh_blob_adapter_fetch(
		handle,
		refPtr, C.size_t(len(blobRefBytes)),
		&outPtr, &outLen,
	)
	if rc != 0 {
		return nil, fmt.Errorf("%w: fetch failed with rc=%d", ErrBlob, int(rc))
	}
	defer C.net_blob_free_buffer(outPtr, outLen)
	body := C.GoBytes(unsafe.Pointer(outPtr), C.int(outLen))
	return body, nil
}

// Exists probes local presence — returns `true` when every
// chunk of `blobRefBytes` is locally reachable.
func (a *MeshBlobAdapter) Exists(blobRefBytes []byte) (bool, error) {
	a.mu.Lock()
	handle := a.handle
	a.mu.Unlock()
	if handle == nil {
		return false, ErrBlobClosed
	}
	var refPtr *C.uint8_t
	if len(blobRefBytes) > 0 {
		refPtr = (*C.uint8_t)(unsafe.Pointer(&blobRefBytes[0]))
	}
	var present C.int
	rc := C.net_mesh_blob_adapter_exists(
		handle,
		refPtr, C.size_t(len(blobRefBytes)),
		&present,
	)
	if rc != 0 {
		return false, fmt.Errorf("%w: exists failed with rc=%d", ErrBlob, int(rc))
	}
	return present != 0, nil
}

// PrometheusText renders the adapter's Prometheus body
// (includes the v0.2 counter family + the v0.3 overflow
// counter family if active).
func (a *MeshBlobAdapter) PrometheusText() (string, error) {
	a.mu.Lock()
	handle := a.handle
	a.mu.Unlock()
	if handle == nil {
		return "", ErrBlobClosed
	}
	body := C.net_mesh_blob_adapter_prometheus_text(handle)
	if body == nil {
		return "", fmt.Errorf("%w: prometheus_text returned null", ErrBlob)
	}
	defer C.net_free_string(body)
	return C.GoString(body), nil
}

// OverflowEnabled — `true` iff the adapter is currently
// advertising `dataforts.blob.overflow`.
func (a *MeshBlobAdapter) OverflowEnabled() (bool, error) {
	a.mu.Lock()
	handle := a.handle
	a.mu.Unlock()
	if handle == nil {
		return false, ErrBlobClosed
	}
	rc := C.net_mesh_blob_adapter_overflow_enabled(handle)
	if rc < 0 {
		return false, fmt.Errorf("%w: overflow_enabled rc=%d", ErrBlob, int(rc))
	}
	return rc == 1, nil
}

// OverflowActive — `true` iff the most recent overflow tick
// observed disk at or above the high-water threshold.
func (a *MeshBlobAdapter) OverflowActive() (bool, error) {
	a.mu.Lock()
	handle := a.handle
	a.mu.Unlock()
	if handle == nil {
		return false, ErrBlobClosed
	}
	rc := C.net_mesh_blob_adapter_overflow_active(handle)
	if rc < 0 {
		return false, fmt.Errorf("%w: overflow_active rc=%d", ErrBlob, int(rc))
	}
	return rc == 1, nil
}

// OverflowConfig snapshots the current overflow configuration.
func (a *MeshBlobAdapter) OverflowConfig() (*OverflowConfig, error) {
	a.mu.Lock()
	handle := a.handle
	a.mu.Unlock()
	if handle == nil {
		return nil, ErrBlobClosed
	}
	body := C.net_mesh_blob_adapter_overflow_config(handle)
	if body == nil {
		return nil, fmt.Errorf("%w: overflow_config returned null", ErrBlob)
	}
	defer C.net_free_string(body)
	jsonStr := C.GoString(body)
	var cfg OverflowConfig
	if err := json.Unmarshal([]byte(jsonStr), &cfg); err != nil {
		return nil, fmt.Errorf("%w: parse overflow_config JSON: %v", ErrBlob, err)
	}
	return &cfg, nil
}

// SetOverflowEnabled flips the master switch at runtime.
func (a *MeshBlobAdapter) SetOverflowEnabled(enabled bool) error {
	a.mu.Lock()
	handle := a.handle
	a.mu.Unlock()
	if handle == nil {
		return ErrBlobClosed
	}
	b := C.int(0)
	if enabled {
		b = 1
	}
	rc := C.net_mesh_blob_adapter_set_overflow_enabled(handle, b)
	if rc != 0 {
		return fmt.Errorf("%w: set_overflow_enabled rc=%d", ErrBlob, int(rc))
	}
	return nil
}

// SetOverflowConfig replaces the entire overflow configuration
// in one call.
func (a *MeshBlobAdapter) SetOverflowConfig(cfg *OverflowConfig) error {
	if cfg == nil {
		return fmt.Errorf("%w: config is nil", ErrBlobInvalidConfig)
	}
	a.mu.Lock()
	handle := a.handle
	a.mu.Unlock()
	if handle == nil {
		return ErrBlobClosed
	}
	body, err := json.Marshal(cfg)
	if err != nil {
		return fmt.Errorf("%w: marshal: %v", ErrBlobInvalidConfig, err)
	}
	cBody := C.CString(string(body))
	defer C.free(unsafe.Pointer(cBody))
	rc := C.net_mesh_blob_adapter_set_overflow_config(handle, cBody)
	if rc != 0 {
		return fmt.Errorf("%w: set_overflow_config rc=%d", ErrBlob, int(rc))
	}
	return nil
}
