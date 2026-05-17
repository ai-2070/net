// Package net — NetDb bindings.
//
// NetDb composes Tasks + Memories over a single Redex behind one Go
// handle. Snapshot bytes round-trip with the Rust, napi, and PyO3
// surfaces — a bundle captured here restores in any other binding,
// and vice versa.
//
// See `example/netdb/main.go` for an end-to-end walkthrough.

package net

/*
#include "net.h"
#include <stdlib.h>
*/
import "C"

import (
	"encoding/json"
	"fmt"
	"runtime"
	"sync"
	"unsafe"
)

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

// NetDbConfig is the open config consumed by `OpenNetDb` /
// `OpenNetDbFromSnapshot`. Fields map directly to the FFI's JSON shape.
type NetDbConfig struct {
	// OriginHash stamped on every EventMeta by bundled adapters.
	OriginHash uint64 `json:"origin_hash"`
	// Persistent routes every enabled model's RedEX file through the
	// parent Redex's persistent directory. Requires `NewRedex` to have
	// been called with a non-empty directory.
	Persistent bool `json:"persistent,omitempty"`
	// WithTasks includes the tasks model.
	WithTasks bool `json:"with_tasks,omitempty"`
	// WithMemories includes the memories model.
	WithMemories bool `json:"with_memories,omitempty"`
}

// ---------------------------------------------------------------------------
// NetDb handle
// ---------------------------------------------------------------------------

// NetDb is a bundle of CortEX adapters opened against one Redex.
// Adapter accessors return independent handles whose lifetimes are
// decoupled from the NetDb itself.
type NetDb struct {
	mu     sync.RWMutex
	handle *C.net_netdb_t
}

// OpenNetDb opens a fresh bundle. Failure-atomic: if the second
// adapter fails to open, the first is closed before returning
// `ErrNetDb`.
func OpenNetDb(redex *Redex, cfg NetDbConfig) (*NetDb, error) {
	if redex == nil {
		return nil, fmt.Errorf("%w: nil redex", ErrNetDb)
	}
	cfgJSON, err := json.Marshal(cfg)
	if err != nil {
		return nil, fmt.Errorf("marshal NetDbConfig: %w", err)
	}
	cCfg := C.CString(string(cfgJSON))
	defer C.free(unsafe.Pointer(cCfg))

	redex.mu.RLock()
	defer redex.mu.RUnlock()
	if redex.handle == nil {
		return nil, ErrShuttingDown
	}
	var out *C.net_netdb_t
	code := C.net_netdb_open(redex.handle, cCfg, &out)
	if errFromCode := cortexErrorFromCode(code); errFromCode != nil {
		return nil, errFromCode
	}
	db := &NetDb{handle: out}
	runtime.SetFinalizer(db, (*NetDb).Free)
	return db, nil
}

// OpenNetDbFromSnapshot restores a bundle from a postcard-encoded
// snapshot. Each enabled model is restored from its bundle entry
// when present, else opened from scratch. Pass `nil` or an empty
// bundle to open everything from scratch (equivalent to `OpenNetDb`).
func OpenNetDbFromSnapshot(redex *Redex, cfg NetDbConfig, bundle []byte) (*NetDb, error) {
	if redex == nil {
		return nil, fmt.Errorf("%w: nil redex", ErrNetDb)
	}
	cfgJSON, err := json.Marshal(cfg)
	if err != nil {
		return nil, fmt.Errorf("marshal NetDbConfig: %w", err)
	}
	cCfg := C.CString(string(cfgJSON))
	defer C.free(unsafe.Pointer(cCfg))

	var bundlePtr *C.uint8_t
	if len(bundle) > 0 {
		bundlePtr = (*C.uint8_t)(unsafe.Pointer(&bundle[0]))
	}

	redex.mu.RLock()
	defer redex.mu.RUnlock()
	if redex.handle == nil {
		return nil, ErrShuttingDown
	}
	var out *C.net_netdb_t
	code := C.net_netdb_open_from_snapshot(
		redex.handle,
		cCfg,
		bundlePtr,
		C.size_t(len(bundle)),
		&out,
	)
	if errFromCode := cortexErrorFromCode(code); errFromCode != nil {
		return nil, errFromCode
	}
	db := &NetDb{handle: out}
	runtime.SetFinalizer(db, (*NetDb).Free)
	return db, nil
}

// Tasks returns an independent TasksAdapter handle. The returned
// adapter has its own finalizer + lifecycle — closing or freeing it
// does NOT affect the NetDb. Returns `ErrNetDb` if the tasks model
// wasn't enabled at open time.
func (db *NetDb) Tasks() (*TasksAdapter, error) {
	db.mu.RLock()
	defer db.mu.RUnlock()
	if db.handle == nil {
		return nil, ErrShuttingDown
	}
	var out *C.net_tasks_adapter_t
	code := C.net_netdb_tasks(db.handle, &out)
	if err := cortexErrorFromCode(code); err != nil {
		return nil, err
	}
	t := &TasksAdapter{handle: out}
	runtime.SetFinalizer(t, (*TasksAdapter).free)
	return t, nil
}

// Memories returns an independent MemoriesAdapter handle. Same
// lifetime semantics as `Tasks`.
func (db *NetDb) Memories() (*MemoriesAdapter, error) {
	db.mu.RLock()
	defer db.mu.RUnlock()
	if db.handle == nil {
		return nil, ErrShuttingDown
	}
	var out *C.net_memories_adapter_t
	code := C.net_netdb_memories(db.handle, &out)
	if err := cortexErrorFromCode(code); err != nil {
		return nil, err
	}
	m := &MemoriesAdapter{handle: out}
	runtime.SetFinalizer(m, (*MemoriesAdapter).free)
	return m, nil
}

// Snapshot captures a postcard-encoded `NetDbSnapshot` bundle. The
// returned byte slice is a Go-owned copy of the substrate buffer
// (the substrate buffer is freed before returning).
func (db *NetDb) Snapshot() ([]byte, error) {
	db.mu.RLock()
	defer db.mu.RUnlock()
	if db.handle == nil {
		return nil, ErrShuttingDown
	}
	var bytes *C.uint8_t
	var n C.size_t
	code := C.net_netdb_snapshot(db.handle, &bytes, &n)
	if err := cortexErrorFromCode(code); err != nil {
		return nil, err
	}
	if n == 0 || bytes == nil {
		return nil, nil
	}
	// Defensively free the substrate-side buffer regardless of how
	// we exit — Go-side copy below survives.
	defer C.net_netdb_free_bundle(bytes, n)
	out := C.GoBytes(unsafe.Pointer(bytes), C.int(n))
	return out, nil
}

// Close closes every enabled adapter on the NetDb. The underlying
// RedEX files stay open on the parent Redex. Idempotent.
func (db *NetDb) Close() error {
	db.mu.RLock()
	if db.handle == nil {
		db.mu.RUnlock()
		return nil
	}
	code := C.net_netdb_close(db.handle)
	db.mu.RUnlock()
	return cortexErrorFromCode(code)
}

// Free releases the underlying handle. Idempotent. Adapter handles
// returned from `Tasks` / `Memories` survive — they hold their own
// Arc clone of the underlying adapter.
func (db *NetDb) Free() {
	db.mu.Lock()
	defer db.mu.Unlock()
	if db.handle != nil {
		C.net_netdb_free(db.handle)
		db.handle = nil
		runtime.SetFinalizer(db, nil)
	}
}
