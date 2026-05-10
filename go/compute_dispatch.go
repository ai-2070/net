// Compute dispatcher trampolines — Stage 6 sub-step 2.
//
// Rust (compute-ffi) calls into Go whenever a bridged daemon's
// `process` / `snapshot` / `restore` methods need to run. Go can't
// receive Rust callbacks directly through CGO, so we use the
// standard callback-table pattern: Go exposes four C-linkage
// functions via `//export`; Rust stores pointers to them in a
// `OnceLock<DispatcherFns>` via `net_compute_set_dispatcher`.
//
// Every Go daemon is registered in a process-wide `sync.Map`
// keyed by a monotonically-increasing `uint64` ID. Rust holds the
// ID (inside `GoBridge`) and hands it back to us on every
// callback; we look up the `MeshDaemon` and dispatch. When Rust
// drops the bridge, it invokes our `free` trampoline so we can
// release the map entry.
package net

/*
#include "net.h"
#include <stdlib.h>
#include <string.h>

// Prototypes for the bridge trampolines defined in
// `compute_dispatch_bridge.c`. Those functions have the exact
// `net_compute_*_fn` signatures (with `const uint8_t*` pointer
// params) that the dispatcher typedef expects; they thunk into
// the `//export`ed Go functions below which cgo generates with
// non-const pointer params.
extern int bridgeProcess(uint64_t daemon_id, uint64_t origin_hash, uint64_t sequence,
                         const uint8_t* payload, size_t payload_len,
                         net_compute_outputs_t* outputs);
extern int bridgeSnapshot(uint64_t daemon_id, uint8_t** out_ptr, size_t* out_len);
extern int bridgeRestore(uint64_t daemon_id, const uint8_t* state, size_t state_len);
extern void bridgeFree(uint64_t daemon_id);
extern int bridgeFactory(uint64_t runtime_id, const char* kind_ptr, size_t kind_len, uint64_t* out_daemon_id);
*/
import "C"

import (
	"sync"
	"sync/atomic"
	"unsafe"
)

// MeshDaemon is the interface a Go daemon implements. `Process`
// is required; `Snapshot` and `Restore` are optional — callers
// can type-assert to the matching single-method interfaces (below)
// to detect support.
type MeshDaemon interface {
	// Process handles one inbound event. Returns zero or more
	// output payloads — the runtime wraps each in a fresh causal
	// link. Must be synchronous: the Rust dispatcher blocks on
	// this call.
	Process(event CausalEvent) ([][]byte, error)
}

// DaemonSnapshotter is the optional snapshot interface. Daemons
// that don't implement this surface as stateless to the runtime.
type DaemonSnapshotter interface {
	// Snapshot returns the daemon's serialized state, or nil for a
	// stateless instant. An error falls back to "stateless" in the
	// core registry (with a stderr warning from the Rust bridge).
	Snapshot() ([]byte, error)
}

// DaemonRestorer is the optional restore interface. If a daemon
// registers via `SpawnFromSnapshot` (sub-step 3) but does not
// implement `DaemonRestorer`, the state bytes are silently ignored
// at the bridge layer — same semantics as the Node / Python
// bindings.
type DaemonRestorer interface {
	Restore(state []byte) error
}

// CausalEvent is the event delivered to a daemon's `Process`.
type CausalEvent struct {
	// OriginHash is the 64-bit hash of the emitting entity.
	OriginHash uint64
	// Sequence is the emitter's causal-chain sequence number.
	Sequence uint64
	// Payload is the opaque event body. Treat as borrowed — copy
	// if you need to retain it past the `Process` call.
	Payload []byte
}

// -------------------------------------------------------------------------
// Daemon registry — process-wide map of live MeshDaemon instances.
// -------------------------------------------------------------------------

// DaemonFactory is a zero-arg constructor that produces a fresh
// `MeshDaemon` per invocation. Registered via
// `DaemonRuntime.RegisterFactoryFunc`; the runtime's
// migration-target reconstruction path calls it whenever the SDK
// needs a new daemon instance for an inbound migration.
type DaemonFactory func() MeshDaemon

// factoryKey scopes a registered factory to a specific runtime so
// two `DaemonRuntime`s in the same process can register the same
// `kind` with different factories without colliding on the Go map.
// The runtime id comes from `net_compute_runtime_id` on the Rust
// side; 0 is reserved as a sentinel for "no runtime."
type factoryKey struct {
	runtimeID uint64
	kind      string
}

// factoryEntry pairs the registered `fn` with a monotonic epoch
// assigned by `swapFactoryFunc`. The epoch lets `restoreFactoryFunc`
// detect a concurrent successful registration for the same key and
// *not* clobber it with a stale rollback — without it, two
// interleaved RegisterFactoryFunc calls can corrupt the global map:
//
//	T1 swap(k, A)  — entry={A, 1}
//	T2 swap(k, B)  — entry={B, 2}   (succeeds native-side)
//	T1 native failed → restore(k, prev={}, existed=false)
//	                   …would delete entry={B, 2}  ← the bug
//
// With the epoch check, T1's restore sees `current.epoch == 2 != 1`
// and leaves the map alone. Only the thread that last successfully
// placed an entry can roll it back.
type factoryEntry struct {
	fn    DaemonFactory
	epoch uint64
}

// factoryEpoch is a process-wide monotonic counter minted on every
// `swapFactoryFunc`. Values start at 1; 0 is a "no epoch" sentinel.
var factoryEpoch atomic.Uint64

var (
	daemonsMu sync.RWMutex
	daemons   = make(map[uint64]MeshDaemon)
	nextID    atomic.Uint64

	// Factory funcs keyed by (runtime_id, kind). Populated by
	// `DaemonRuntime.RegisterFactoryFunc`; looked up by
	// `goComputeFactory` on migration-target reconstruction. The
	// runtime-id scoping means:
	//
	//   - Two runtimes registering the same kind with different
	//     factories stay independent.
	//   - Stale entries from a closed runtime can be purged in one
	//     sweep via `purgeFactoryFuncsForRuntime(runtimeID)`.
	factoryFuncsMu sync.RWMutex
	factoryFuncs   = make(map[factoryKey]factoryEntry)
)

// swapFactoryFunc stores `fn` under `(runtimeID, kind)` with a
// fresh epoch. Returns the prior entry (if any), the prior-existed
// bool, and our newly-minted epoch. Callers pass all three to
// `restoreFactoryFunc` if the native side later rejects the
// registration.
func swapFactoryFunc(runtimeID uint64, kind string, fn DaemonFactory) (prev factoryEntry, existed bool, ourEpoch uint64) {
	k := factoryKey{runtimeID: runtimeID, kind: kind}
	ourEpoch = factoryEpoch.Add(1)
	factoryFuncsMu.Lock()
	defer factoryFuncsMu.Unlock()
	prev, existed = factoryFuncs[k]
	factoryFuncs[k] = factoryEntry{fn: fn, epoch: ourEpoch}
	return
}

// restoreFactoryFunc undoes a `swapFactoryFunc` iff the current
// entry is still the one the caller wrote (matched by `ourEpoch`).
// If a concurrent thread successfully overwrote the entry in the
// meantime, leave it alone — that thread owns the slot now, and
// clobbering it with stale state would re-introduce the
// process-global overwrite bug at a smaller scale.
func restoreFactoryFunc(runtimeID uint64, kind string, ourEpoch uint64, prev factoryEntry, existed bool) {
	k := factoryKey{runtimeID: runtimeID, kind: kind}
	factoryFuncsMu.Lock()
	defer factoryFuncsMu.Unlock()
	current, present := factoryFuncs[k]
	if !present || current.epoch != ourEpoch {
		// A concurrent writer owns (or purged) this entry. Not
		// ours to touch.
		return
	}
	if existed {
		factoryFuncs[k] = prev
	} else {
		delete(factoryFuncs, k)
	}
}

// lookupFactoryFunc returns the factory for `(runtimeID, kind)` or nil.
func lookupFactoryFunc(runtimeID uint64, kind string) DaemonFactory {
	factoryFuncsMu.RLock()
	defer factoryFuncsMu.RUnlock()
	return factoryFuncs[factoryKey{runtimeID: runtimeID, kind: kind}].fn
}

// purgeFactoryFuncsForRuntime removes every factory registered
// against `runtimeID`. Called from `DaemonRuntime.Close()` so a
// closed runtime's factory callbacks don't leak into a future
// runtime that happens to reuse a freed runtime id (can't happen
// with a monotonic u64, but defensive) and don't pin captured
// Go closures longer than the runtime itself.
func purgeFactoryFuncsForRuntime(runtimeID uint64) {
	factoryFuncsMu.Lock()
	defer factoryFuncsMu.Unlock()
	for k := range factoryFuncs {
		if k.runtimeID == runtimeID {
			delete(factoryFuncs, k)
		}
	}
}

// registerDaemon stores `d` in the registry under a fresh uint64
// ID and returns the ID. Called by `DaemonRuntime.Spawn`.
func registerDaemon(d MeshDaemon) uint64 {
	id := nextID.Add(1)
	daemonsMu.Lock()
	daemons[id] = d
	daemonsMu.Unlock()
	return id
}

// lookupDaemon returns the MeshDaemon for `id`, or nil if gone.
func lookupDaemon(id uint64) MeshDaemon {
	daemonsMu.RLock()
	d := daemons[id]
	daemonsMu.RUnlock()
	return d
}

// unregisterDaemon drops the registry entry for `id`. Called by
// Rust's free callback on bridge drop.
func unregisterDaemon(id uint64) {
	daemonsMu.Lock()
	delete(daemons, id)
	daemonsMu.Unlock()
}

// -------------------------------------------------------------------------
// //export trampolines — called from Rust's GoBridge via the
// dispatcher registration below.
// -------------------------------------------------------------------------

//export goComputeProcess
func goComputeProcess(daemonID C.uint64_t, originHash C.uint64_t, sequence C.uint64_t,
	payloadPtr *C.uint8_t, payloadLen C.size_t, outputs *C.net_compute_outputs_t,
) C.int {
	d := lookupDaemon(uint64(daemonID))
	if d == nil {
		return -1
	}
	// Copy the payload — the Rust side owns the underlying `Bytes`
	// and may free it after this callback returns. Go slices
	// backed by Rust memory are unsafe to retain.
	var payload []byte
	if payloadLen > 0 {
		payload = C.GoBytes(unsafe.Pointer(payloadPtr), C.int(payloadLen))
	}
	outs, err := d.Process(CausalEvent{
		OriginHash: uint64(originHash),
		Sequence:   uint64(sequence),
		Payload:    payload,
	})
	if err != nil {
		return -1
	}
	for _, o := range outs {
		var ptr *C.uint8_t
		if len(o) > 0 {
			ptr = (*C.uint8_t)(unsafe.Pointer(&o[0]))
		}
		code := C.net_compute_outputs_push(outputs, ptr, C.size_t(len(o)))
		// Keep `o` alive until after the push call so the copy-in
		// is correct even if GC decides to move the slice backing.
		if code != C.NET_COMPUTE_OK {
			return -1
		}
	}
	// Prevent Go's escape analysis from collapsing `outs` before
	// the final push (belt-and-braces; the for-loop pins each o).
	if len(outs) > 0 {
		_ = outs[len(outs)-1]
	}
	return C.NET_COMPUTE_OK
}

//export goComputeSnapshot
func goComputeSnapshot(daemonID C.uint64_t, outPtr **C.uint8_t, outLen *C.size_t) C.int {
	d := lookupDaemon(uint64(daemonID))
	if d == nil {
		*outPtr = nil
		*outLen = 0
		return -1
	}
	snapper, ok := d.(DaemonSnapshotter)
	if !ok {
		// Daemon is stateless — not an error, just empty output.
		*outPtr = nil
		*outLen = 0
		return C.NET_COMPUTE_OK
	}
	state, err := snapper.Snapshot()
	if err != nil {
		*outPtr = nil
		*outLen = 0
		return -1
	}
	if len(state) == 0 {
		*outPtr = nil
		*outLen = 0
		return C.NET_COMPUTE_OK
	}
	// Copy into a C.malloc buffer so Rust can free via libc::free
	// (matches `net_compute_snapshot_bytes_free`'s contract).
	buf := C.malloc(C.size_t(len(state)))
	if buf == nil {
		*outPtr = nil
		*outLen = 0
		return -1
	}
	C.memcpy(buf, unsafe.Pointer(&state[0]), C.size_t(len(state)))
	*outPtr = (*C.uint8_t)(buf)
	*outLen = C.size_t(len(state))
	return C.NET_COMPUTE_OK
}

//export goComputeRestore
func goComputeRestore(daemonID C.uint64_t, statePtr *C.uint8_t, stateLen C.size_t) C.int {
	d := lookupDaemon(uint64(daemonID))
	if d == nil {
		return -1
	}
	restorer, ok := d.(DaemonRestorer)
	if !ok {
		// No restore method — silently succeed. Matches the
		// Node / Python semantics: absent `restore` = ignore state.
		return C.NET_COMPUTE_OK
	}
	var state []byte
	if stateLen > 0 {
		state = C.GoBytes(unsafe.Pointer(statePtr), C.int(stateLen))
	}
	if err := restorer.Restore(state); err != nil {
		return -1
	}
	return C.NET_COMPUTE_OK
}

//export goComputeFree
func goComputeFree(daemonID C.uint64_t) {
	unregisterDaemon(uint64(daemonID))
}

//export goComputeFactory
func goComputeFactory(runtimeID C.uint64_t, kindPtr *C.char, kindLen C.size_t, outDaemonID *C.uint64_t) C.int {
	if outDaemonID == nil {
		return -1
	}
	kind := ""
	if kindLen > 0 && kindPtr != nil {
		// `C.GoStringN` copies — safer than GoString which stops at
		// the first NUL.
		kind = C.GoStringN(kindPtr, C.int(kindLen))
	}
	fn := lookupFactoryFunc(uint64(runtimeID), kind)
	if fn == nil {
		return -1
	}
	// Call the user's factory. Recover from panics so a buggy
	// factory can't take down the whole tokio worker.
	var inst MeshDaemon
	func() {
		defer func() {
			if r := recover(); r != nil {
				inst = nil
			}
		}()
		inst = fn()
	}()
	if inst == nil {
		return -1
	}
	id := registerDaemon(inst)
	*outDaemonID = C.uint64_t(id)
	return C.NET_COMPUTE_OK
}

// -------------------------------------------------------------------------
// init — register the dispatcher with Rust.
// -------------------------------------------------------------------------

func init() {
	// OnceLock on the Rust side makes this idempotent: a second
	// call (e.g., from a test harness re-init) is a no-op.
	// Pass the C-side `bridge*` wrappers — they have the exact
	// `net_compute_*_fn` signatures (with `const uint8_t*` /
	// `const char*` parameters) and thunk into the `//export`ed Go
	// functions which cgo emits with non-const pointer parameters.
	code := C.net_compute_set_dispatcher(
		C.net_compute_process_fn(C.bridgeProcess),
		C.net_compute_snapshot_fn(C.bridgeSnapshot),
		C.net_compute_restore_fn(C.bridgeRestore),
		C.net_compute_free_fn(C.bridgeFree),
		C.net_compute_factory_fn(C.bridgeFactory),
	)
	if code != C.NET_COMPUTE_OK {
		// Panic is appropriate here — without the dispatcher,
		// nothing on the compute surface will work.
		panic("net: failed to install compute dispatcher")
	}
}
