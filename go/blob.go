// Package net — Dataforts blob storage bindings.
//
// Wraps the `net::ffi::blob` C FFI for the v0.2 substrate-owned
// `MeshBlobAdapter` + the v0.3 active overflow extension.
//
// The adapter is Rust-backed (chunks live in Redex as
// content-addressed `RedexFile`s); Go callers get a thin
// wrapper over the opaque `net_mesh_blob_adapter_t*` pointer.
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
#include "net.h"
#include <stdint.h>
#include <stdlib.h>

// `net_blob_free_buffer` lives in include/net.h's blob section; the
// MeshBlobAdapter wire surface that this file binds against is also
// declared in net.h (added alongside this binding). All symbols
// resolve at runtime via the libnet cdylib; consumers building
// without the `dataforts + netdb + redex-disk` feature triple get
// the `blob_stubs` fallbacks that return NET_ERR_FEATURE_NOT_BUILT.
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

// MeshBlobAdapter wraps `*net_mesh_blob_adapter_t`. Cheap to
// share via the Go runtime; methods hold a read lock across the
// FFI call and Close takes the write lock, so a concurrent _free
// (explicit Close or the GC finalizer) can never race an in-flight
// op into a use-after-free.
type MeshBlobAdapter struct {
	mu     sync.RWMutex
	handle *C.net_mesh_blob_adapter_t
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

// withReadHandle runs fn under the read lock with a validated, non-nil
// native handle, keeping the adapter alive across the cgo call. Returns
// false (without calling fn) when the handle has been closed. Every FFI
// deref of `handle` goes through here so Close's `_free` (which takes
// the write lock) cannot race a live op into a use-after-free.
func (a *MeshBlobAdapter) withReadHandle(fn func(handle *C.net_mesh_blob_adapter_t)) bool {
	a.mu.RLock()
	defer a.mu.RUnlock()
	if a.handle == nil {
		return false
	}
	fn(a.handle)
	runtime.KeepAlive(a)
	return true
}

// Store `data` under the content address declared by
// `blobRefBytes` (a previously-encoded `BlobRef` wire blob).
// The substrate BLAKE3-verifies + raises a typed error on
// mismatch.
func (a *MeshBlobAdapter) Store(blobRefBytes, data []byte) error {
	var refPtr *C.uint8_t
	if len(blobRefBytes) > 0 {
		refPtr = (*C.uint8_t)(unsafe.Pointer(&blobRefBytes[0]))
	}
	var dataPtr *C.uint8_t
	if len(data) > 0 {
		dataPtr = (*C.uint8_t)(unsafe.Pointer(&data[0]))
	}
	var rc C.int
	if !a.withReadHandle(func(handle *C.net_mesh_blob_adapter_t) {
		rc = C.net_mesh_blob_adapter_store(
			handle,
			refPtr, C.size_t(len(blobRefBytes)),
			dataPtr, C.size_t(len(data)),
		)
	}) {
		return ErrBlobClosed
	}
	if rc != 0 {
		return fmt.Errorf("%w: store failed with rc=%d", ErrBlob, int(rc))
	}
	return nil
}

// Publish computes the content address for `data` (BLAKE3), stores
// it through this adapter under `uri`, and returns the *encoded*
// BlobRef — the mint a producer needs. `Store` requires an
// already-encoded ref, so before this existed a Go producer had no
// way to create one; a consumer could only fetch a blob something
// else had published.
//
// The URI's scheme must be one the adapter accepts (`mesh:` for a
// substrate `MeshBlobAdapter`).
func (a *MeshBlobAdapter) Publish(uri string, data []byte) ([]byte, error) {
	cURI := C.CString(uri)
	defer C.free(unsafe.Pointer(cURI))
	var dataPtr *C.uint8_t
	if len(data) > 0 {
		dataPtr = (*C.uint8_t)(unsafe.Pointer(&data[0]))
	}
	var outRef *C.uint8_t
	var outLen C.size_t
	var rc C.int
	if !a.withReadHandle(func(handle *C.net_mesh_blob_adapter_t) {
		rc = C.net_mesh_blob_adapter_publish(
			handle,
			(*C.uint8_t)(unsafe.Pointer(cURI)), C.size_t(len(uri)),
			dataPtr, C.size_t(len(data)),
			&outRef, &outLen,
		)
	}) {
		return nil, ErrBlobClosed
	}
	if rc != 0 {
		return nil, fmt.Errorf("%w: publish failed with rc=%d", ErrBlob, int(rc))
	}
	defer C.net_blob_free_buffer(outRef, outLen)
	encoded := C.GoBytes(unsafe.Pointer(outRef), C.int(outLen))
	return encoded, nil
}

// Fetch returns the content-addressed bytes for `blobRefBytes`.
func (a *MeshBlobAdapter) Fetch(blobRefBytes []byte) ([]byte, error) {
	var refPtr *C.uint8_t
	if len(blobRefBytes) > 0 {
		refPtr = (*C.uint8_t)(unsafe.Pointer(&blobRefBytes[0]))
	}
	var outPtr *C.uint8_t
	var outLen C.size_t
	var rc C.int
	if !a.withReadHandle(func(handle *C.net_mesh_blob_adapter_t) {
		rc = C.net_mesh_blob_adapter_fetch(
			handle,
			refPtr, C.size_t(len(blobRefBytes)),
			&outPtr, &outLen,
		)
	}) {
		return nil, ErrBlobClosed
	}
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
	var refPtr *C.uint8_t
	if len(blobRefBytes) > 0 {
		refPtr = (*C.uint8_t)(unsafe.Pointer(&blobRefBytes[0]))
	}
	var present C.int
	var rc C.int
	if !a.withReadHandle(func(handle *C.net_mesh_blob_adapter_t) {
		rc = C.net_mesh_blob_adapter_exists(
			handle,
			refPtr, C.size_t(len(blobRefBytes)),
			&present,
		)
	}) {
		return false, ErrBlobClosed
	}
	if rc != 0 {
		return false, fmt.Errorf("%w: exists failed with rc=%d", ErrBlob, int(rc))
	}
	return present != 0, nil
}

// PrometheusText renders the adapter's Prometheus body
// (includes the v0.2 counter family + the v0.3 overflow
// counter family if active).
func (a *MeshBlobAdapter) PrometheusText() (string, error) {
	var body *C.char
	if !a.withReadHandle(func(handle *C.net_mesh_blob_adapter_t) {
		body = C.net_mesh_blob_adapter_prometheus_text(handle)
	}) {
		return "", ErrBlobClosed
	}
	if body == nil {
		return "", fmt.Errorf("%w: prometheus_text returned null", ErrBlob)
	}
	defer C.net_free_string(body)
	return C.GoString(body), nil
}

// OverflowEnabled — `true` iff the adapter is currently
// advertising `dataforts.blob.overflow`.
func (a *MeshBlobAdapter) OverflowEnabled() (bool, error) {
	var rc C.int
	if !a.withReadHandle(func(handle *C.net_mesh_blob_adapter_t) {
		rc = C.net_mesh_blob_adapter_overflow_enabled(handle)
	}) {
		return false, ErrBlobClosed
	}
	if rc < 0 {
		return false, fmt.Errorf("%w: overflow_enabled rc=%d", ErrBlob, int(rc))
	}
	return rc == 1, nil
}

// OverflowActive — `true` iff the most recent overflow tick
// observed disk at or above the high-water threshold.
func (a *MeshBlobAdapter) OverflowActive() (bool, error) {
	var rc C.int
	if !a.withReadHandle(func(handle *C.net_mesh_blob_adapter_t) {
		rc = C.net_mesh_blob_adapter_overflow_active(handle)
	}) {
		return false, ErrBlobClosed
	}
	if rc < 0 {
		return false, fmt.Errorf("%w: overflow_active rc=%d", ErrBlob, int(rc))
	}
	return rc == 1, nil
}

// OverflowConfig snapshots the current overflow configuration.
func (a *MeshBlobAdapter) OverflowConfig() (*OverflowConfig, error) {
	var body *C.char
	if !a.withReadHandle(func(handle *C.net_mesh_blob_adapter_t) {
		body = C.net_mesh_blob_adapter_overflow_config(handle)
	}) {
		return nil, ErrBlobClosed
	}
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
	b := C.int(0)
	if enabled {
		b = 1
	}
	var rc C.int
	if !a.withReadHandle(func(handle *C.net_mesh_blob_adapter_t) {
		rc = C.net_mesh_blob_adapter_set_overflow_enabled(handle, b)
	}) {
		return ErrBlobClosed
	}
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
	body, err := json.Marshal(cfg)
	if err != nil {
		return fmt.Errorf("%w: marshal: %v", ErrBlobInvalidConfig, err)
	}
	cBody := C.CString(string(body))
	defer C.free(unsafe.Pointer(cBody))
	var rc C.int
	if !a.withReadHandle(func(handle *C.net_mesh_blob_adapter_t) {
		rc = C.net_mesh_blob_adapter_set_overflow_config(handle, cBody)
	}) {
		return ErrBlobClosed
	}
	if rc != 0 {
		return fmt.Errorf("%w: set_overflow_config rc=%d", ErrBlob, int(rc))
	}
	return nil
}

// ---------------------------------------------------------------------------
// Content addressing + mesh transfer
// ---------------------------------------------------------------------------

// ErrTransfer is the umbrella error for failures surfaced by the
// dataforts transport FFI (`net_serve_blob_transfer` / `net_fetch_blob`).
var ErrTransfer = errors.New("blob transfer")

// The transfer failures a caller branches on. Everything else in the
// range is wrapped with ErrTransfer and the FFI code.
var (
	// ErrTransferNotFound - the holder did not have the content.
	ErrTransferNotFound = fmt.Errorf("%w: holder lacked the content", ErrTransfer)
	// ErrTransferHashMismatch - the bytes did not hash to the address.
	ErrTransferHashMismatch = fmt.Errorf("%w: bytes did not hash to the address", ErrTransfer)
	// ErrTransferEngineNotInstalled - ServeBlobTransfer was never called
	// on this node.
	ErrTransferEngineNotInstalled = fmt.Errorf("%w: engine not installed on this node", ErrTransfer)
	// ErrTransferInvalidArgument - bad hash length, oversize, etc.
	ErrTransferInvalidArgument = fmt.Errorf("%w: invalid argument", ErrTransfer)
	// ErrTransferBackend - some other substrate transfer failure.
	ErrTransferBackend = fmt.Errorf("%w: backend failure", ErrTransfer)
)

func transferErrorFromCode(code C.int) error {
	switch code {
	case 0:
		return nil
	case -200:
		return ErrTransferNotFound
	case -201:
		return ErrTransferHashMismatch
	case -202:
		return fmt.Errorf("%w: no connected peer served it", ErrTransfer)
	case -203:
		return fmt.Errorf("%w: cancelled", ErrTransfer)
	case -204:
		return fmt.Errorf("%w: null pointer", ErrTransfer)
	case -205:
		return fmt.Errorf("%w: node is shutting down", ErrTransfer)
	case -206:
		return ErrTransferEngineNotInstalled
	case -207:
		return ErrTransferBackend
	case -208:
		return fmt.Errorf("%w: panic at the FFI boundary", ErrTransfer)
	case -209:
		return ErrTransferInvalidArgument
	default:
		return fmt.Errorf("%w: unknown code %d", ErrTransfer, int(code))
	}
}

// BlobRefHash copies the 32-byte BLAKE3 content hash out of an encoded
// BlobRef. The transport fetch addresses a blob by its raw hash, so this
// is how a producer names what it just published.
func BlobRefHash(encoded []byte) ([32]byte, error) {
	var out [32]byte
	if len(encoded) == 0 {
		return out, fmt.Errorf("%w: encoded ref is empty", ErrTransferInvalidArgument)
	}
	rc := C.net_blob_ref_hash(
		(*C.uint8_t)(unsafe.Pointer(&encoded[0])),
		C.size_t(len(encoded)),
		(*C.uint8_t)(unsafe.Pointer(&out[0])),
	)
	runtime.KeepAlive(encoded)
	if err := transferErrorFromCode(rc); err != nil {
		return out, err
	}
	return out, nil
}

// ServeBlobTransfer installs the blob-transfer engine on this node.
//
// Call once per node before serving OR fetching — a fetch needs it just
// as much as a serve does, and without it the FFI answers
// NET_ERR_TRANSFER_ENGINE_NOT_INSTALLED.
func (m *MeshNode) ServeBlobTransfer(adapter *MeshBlobAdapter) error {
	if adapter == nil {
		return fmt.Errorf("%w: adapter is nil", ErrTransferInvalidArgument)
	}
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return ErrShuttingDown
	}
	var rc C.int
	if !adapter.withReadHandle(func(handle *C.net_mesh_blob_adapter_t) {
		rc = C.net_serve_blob_transfer(m.handle, handle)
	}) {
		return ErrBlobClosed
	}
	return transferErrorFromCode(rc)
}

// FetchBlob pulls the content addressed by the 32-byte BLAKE3 `hash`
// from the known holder `holderID`.
//
// This is the cross-node half of the blob surface:
// MeshBlobAdapter.Fetch reads only what this node already holds.
func (m *MeshNode) FetchBlob(holderID uint64, hash []byte) ([]byte, error) {
	// Exact, not a lower bound. The C side reads 32 bytes from the
	// pointer, so an over-long slice silently fetches whatever its
	// first 32 bytes address — a prefix of the caller's input naming
	// a different object, with no error anywhere.
	if len(hash) != 32 {
		return nil, fmt.Errorf(
			"%w: hash must be 32 bytes, got %d", ErrTransferInvalidArgument, len(hash))
	}
	var out *C.uint8_t
	var outLen C.size_t
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return nil, ErrShuttingDown
	}
	rc := C.net_fetch_blob(
		m.handle,
		C.uint64_t(holderID),
		(*C.uint8_t)(unsafe.Pointer(&hash[0])),
		&out, &outLen,
	)
	runtime.KeepAlive(hash)
	if err := transferErrorFromCode(rc); err != nil {
		return nil, err
	}
	defer C.net_transport_free_buffer(out, outLen)
	return C.GoBytes(unsafe.Pointer(out), C.int(outLen)), nil
}
