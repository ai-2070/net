// NetDb — Go wrapper over the C ABI exported by `net::ffi::cortex`
// (the NetDb half).
//
// NetDb composes Tasks + Memories over a single Redex behind one
// handle. Snapshot bytes round-trip with the Rust, napi, and PyO3
// surfaces — a bundle captured here restores in any other binding,
// and vice versa.
//
// # Surface scope
//
//   - OpenNetDb(redex, config) -> *NetDb
//   - OpenNetDbFromSnapshot(redex, config, bundle) -> *NetDb
//   - (*NetDb).Tasks() -> *TasksAdapter      (independent Arc clone)
//   - (*NetDb).Memories() -> *MemoriesAdapter (independent Arc clone)
//   - (*NetDb).Snapshot() -> []byte
//   - (*NetDb).Close() / (*NetDb).Free()
//
// # Adapter lifetime
//
// `net_netdb_tasks` / `net_netdb_memories` hand out independent
// Arc-cloned adapter handles — closing or freeing them does NOT
// affect the NetDb, and the NetDb itself can be freed before its
// adapter clones. See `net_cortex.h:135-138` for the substrate
// contract.

package net

/*
#include <stdint.h>
#include <stdlib.h>

typedef struct RedexHandle RedexHandle;
typedef struct TasksAdapterHandle TasksAdapterHandle;
typedef struct MemoriesAdapterHandle MemoriesAdapterHandle;
typedef struct NetDbHandle NetDbHandle;

extern int net_netdb_open(
    RedexHandle* redex,
    const char* config_json,
    NetDbHandle** out_handle
);
extern int net_netdb_open_from_snapshot(
    RedexHandle* redex,
    const char* config_json,
    const uint8_t* bundle,
    size_t bundle_len,
    NetDbHandle** out_handle
);
extern int net_netdb_snapshot(
    NetDbHandle* handle,
    uint8_t** out_bytes,
    size_t* out_len
);
extern void net_netdb_free_bundle(uint8_t* bytes, size_t len);
extern int net_netdb_tasks(
    NetDbHandle* handle,
    TasksAdapterHandle** out_handle
);
extern int net_netdb_memories(
    NetDbHandle* handle,
    MemoriesAdapterHandle** out_handle
);
extern int net_netdb_close(NetDbHandle* handle);
extern void net_netdb_free(NetDbHandle* handle);
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

// =====================================================================
// Errors
// =====================================================================

// ErrNetDb is the umbrella error for any NetDb failure. Use
// `errors.Is(err, ErrNetDb)` to detect any NetDb failure regardless of
// the underlying kind.
var ErrNetDb = errors.New("netdb")

// =====================================================================
// Config
// =====================================================================

// NetDbConfig is the open config consumed by `OpenNetDb` /
// `OpenNetDbFromSnapshot`. Fields map directly to the FFI's JSON shape
// at `net_cortex.h:128-130`.
type NetDbConfig struct {
	// OriginHash stamped on every EventMeta by bundled adapters.
	OriginHash uint64 `json:"origin_hash"`
	// Persistent routes every enabled model's RedEX file through the
	// parent Redex's persistent directory. Requires the owning Redex
	// to have been constructed via `NewRedexWithPersistentDir`.
	Persistent bool `json:"persistent,omitempty"`
	// WithTasks enables the tasks model.
	WithTasks bool `json:"with_tasks,omitempty"`
	// WithMemories enables the memories model.
	WithMemories bool `json:"with_memories,omitempty"`
}

// =====================================================================
// Handle
// =====================================================================

// NetDb wraps the C `*NetDbHandle`. Adapter accessors (`Tasks` /
// `Memories`) return independent handles whose lifetimes are decoupled
// from the NetDb itself.
type NetDb struct {
	mu     sync.RWMutex
	handle *C.NetDbHandle
	cfg    NetDbConfig
}

// OpenNetDb opens a fresh bundle. Failure-atomic: if the second adapter
// fails to open, the first is closed before returning `ErrNetDb`.
//
// Same lifecycle pattern as the rest of the Go binding: a runtime
// finalizer is installed; pair every Open with a `defer db.Free()`
// for deterministic cleanup.
func OpenNetDb(redex *Redex, cfg NetDbConfig) (*NetDb, error) {
	if redex == nil {
		return nil, fmt.Errorf("%w: nil redex", ErrNetDb)
	}
	rh := redex.Handle()
	if rh == nil {
		return nil, fmt.Errorf("%w: redex closed", ErrNetDb)
	}
	cfgJSON, err := json.Marshal(cfg)
	if err != nil {
		return nil, fmt.Errorf("%w: marshal config: %v", ErrNetDb, err)
	}
	cCfg := C.CString(string(cfgJSON))
	defer C.free(unsafe.Pointer(cCfg))

	var out *C.NetDbHandle
	rc := C.net_netdb_open((*C.RedexHandle)(rh), cCfg, &out)
	if rc != 0 {
		return nil, fmt.Errorf("%w: open failed (rc=%d)", ErrNetDb, int(rc))
	}
	db := &NetDb{handle: out, cfg: cfg}
	runtime.SetFinalizer(db, func(db *NetDb) { db.Free() })
	return db, nil
}

// OpenNetDbFromSnapshot restores a bundle from a postcard-encoded
// `NetDbSnapshot`. Each enabled model is restored from its bundle
// entry when present, else opened from scratch. Pass `nil` or an empty
// bundle to open everything from scratch (equivalent to `OpenNetDb`).
func OpenNetDbFromSnapshot(redex *Redex, cfg NetDbConfig, bundle []byte) (*NetDb, error) {
	if redex == nil {
		return nil, fmt.Errorf("%w: nil redex", ErrNetDb)
	}
	rh := redex.Handle()
	if rh == nil {
		return nil, fmt.Errorf("%w: redex closed", ErrNetDb)
	}
	cfgJSON, err := json.Marshal(cfg)
	if err != nil {
		return nil, fmt.Errorf("%w: marshal config: %v", ErrNetDb, err)
	}
	cCfg := C.CString(string(cfgJSON))
	defer C.free(unsafe.Pointer(cCfg))

	var bundlePtr *C.uint8_t
	if len(bundle) > 0 {
		bundlePtr = (*C.uint8_t)(unsafe.Pointer(&bundle[0]))
	}

	var out *C.NetDbHandle
	rc := C.net_netdb_open_from_snapshot(
		(*C.RedexHandle)(rh),
		cCfg,
		bundlePtr,
		C.size_t(len(bundle)),
		&out,
	)
	// Keep `bundle` alive across the cgo call so the GC can't move it.
	runtime.KeepAlive(bundle)
	if rc != 0 {
		return nil, fmt.Errorf("%w: open_from_snapshot failed (rc=%d)", ErrNetDb, int(rc))
	}
	db := &NetDb{handle: out, cfg: cfg}
	runtime.SetFinalizer(db, func(db *NetDb) { db.Free() })
	return db, nil
}

// Tasks returns an independent TasksAdapter handle bound to the same
// origin_hash the NetDb was opened with. The returned adapter has its
// own finalizer + lifecycle — closing or freeing it does NOT affect
// the NetDb (per the `net_cortex.h:135-138` contract).
//
// Returns `ErrNetDb` when the tasks model wasn't enabled at open time.
func (db *NetDb) Tasks() (*TasksAdapter, error) {
	db.mu.RLock()
	defer db.mu.RUnlock()
	if db.handle == nil {
		return nil, fmt.Errorf("%w: handle closed", ErrNetDb)
	}
	var out *C.TasksAdapterHandle
	rc := C.net_netdb_tasks(db.handle, &out)
	if rc != 0 {
		return nil, fmt.Errorf("%w: tasks rc=%d", ErrNetDb, int(rc))
	}
	return newTasksAdapterFromRaw(unsafe.Pointer(out), db.cfg.OriginHash), nil
}

// Memories returns an independent MemoriesAdapter handle bound to the
// NetDb's origin_hash. Same lifetime semantics as `Tasks`.
func (db *NetDb) Memories() (*MemoriesAdapter, error) {
	db.mu.RLock()
	defer db.mu.RUnlock()
	if db.handle == nil {
		return nil, fmt.Errorf("%w: handle closed", ErrNetDb)
	}
	var out *C.MemoriesAdapterHandle
	rc := C.net_netdb_memories(db.handle, &out)
	if rc != 0 {
		return nil, fmt.Errorf("%w: memories rc=%d", ErrNetDb, int(rc))
	}
	return newMemoriesAdapterFromRaw(unsafe.Pointer(out), db.cfg.OriginHash), nil
}

// Snapshot captures a postcard-encoded `NetDbSnapshot` bundle. The
// returned slice is a Go-owned copy — the substrate buffer is freed
// before this call returns.
func (db *NetDb) Snapshot() ([]byte, error) {
	db.mu.RLock()
	defer db.mu.RUnlock()
	if db.handle == nil {
		return nil, fmt.Errorf("%w: handle closed", ErrNetDb)
	}
	var bytes *C.uint8_t
	var n C.size_t
	rc := C.net_netdb_snapshot(db.handle, &bytes, &n)
	if rc != 0 {
		return nil, fmt.Errorf("%w: snapshot rc=%d", ErrNetDb, int(rc))
	}
	if n == 0 || bytes == nil {
		return nil, nil
	}
	defer C.net_netdb_free_bundle(bytes, n)
	return C.GoBytes(unsafe.Pointer(bytes), C.int(n)), nil
}

// Close stops every enabled adapter on the NetDb. The underlying RedEX
// files stay open on the parent Redex; adapter handles returned from
// `Tasks` / `Memories` continue to function independently. Idempotent.
func (db *NetDb) Close() error {
	db.mu.RLock()
	if db.handle == nil {
		db.mu.RUnlock()
		return nil
	}
	rc := C.net_netdb_close(db.handle)
	db.mu.RUnlock()
	if rc != 0 {
		return fmt.Errorf("%w: close rc=%d", ErrNetDb, int(rc))
	}
	return nil
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
