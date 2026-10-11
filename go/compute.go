// Compute surface — MeshDaemon + migration. Stage 6 of
// SDK_COMPUTE_SURFACE_PLAN.md.
//
// Sub-step 1 covers lifecycle: a Go caller can build a
// DaemonRuntime against an existing MeshNode, transition it to
// Ready, register a placeholder kind, and shut it down. Event
// dispatch, spawn, snapshot/restore, and migration land in
// sub-steps 2-4.
package net

/*
#cgo LDFLAGS: -L${SRCDIR}/../net/crates/net/target/release -lnet
#include "net.h"
#include <stdlib.h>
*/
import "C"

import (
	"errors"
	"fmt"
	"runtime"
	"sync"
	"sync/atomic"
	"unsafe"
)

// DaemonError is the base error type the compute surface surfaces.
// All errors from this package carry messages prefixed with
// "daemon:" so callers can dispatch programmatically if desired;
// matches the convention used by the Node and Python bindings.
type DaemonError struct {
	// Message is the free-form detail surfaced by the Rust layer
	// (everything after the "daemon: " prefix).
	Message string
}

// Error implements the built-in error interface.
func (e *DaemonError) Error() string {
	return "daemon: " + e.Message
}

// DuplicateKindError is returned by RegisterFactory when the same
// kind is registered twice.
type DuplicateKindError struct {
	Kind string
}

// Error implements the built-in error interface.
func (e *DuplicateKindError) Error() string {
	return fmt.Sprintf("daemon: factory for kind '%s' is already registered", e.Kind)
}

// ErrRuntimeShutDown is returned by DaemonRuntime methods called
// after Shutdown / Close.
var ErrRuntimeShutDown = errors.New("daemon: runtime handle freed")

// DaemonRuntime is the Go handle to the compute runtime. Construct
// via NewDaemonRuntime(mesh); each runtime shares the given
// MeshNode's socket + handshake table (no second socket).
//
// Lifecycle: NewDaemonRuntime → Start → (spawn/deliver/... — future
// sub-steps) → Shutdown. Close() is an idempotent alias for
// Shutdown() + freeing the native handle, safe to call from a
// finalizer.
type DaemonRuntime struct {
	handle *C.net_compute_runtime_t
	// runtimeID is the monotonic id returned by
	// `net_compute_runtime_id`. Captured at construction and
	// used as the first component of every `factoryKey` so two
	// runtimes in the same process that register the same
	// `kind` don't collide on the Go factory map. Purged via
	// `purgeFactoryFuncsForRuntime` in `Close()` so a closed
	// runtime's callbacks don't leak.
	runtimeID uint64
	mu        sync.RWMutex
	// pins counts native calls running on a pinned handle WITHOUT rt.mu
	// held (see pin) — the group spawns, which call the user's factory
	// synchronously. pendingFree holds the handle a Close detached while a
	// pin was live; the last unpin frees it. Both are guarded by mu
	// (pins is atomic so pin can bump it under the READ lock).
	pins        atomic.Int64
	pendingFree *C.net_compute_runtime_t
}

// pin returns the live handle with its lifetime extended past rt.mu: the
// caller runs a native call on it with NO lock held and MUST call the
// returned unpin afterwards. Returns (nil, nil) on a closed runtime.
//
// Group spawns invoke the user's factory synchronously from inside the
// native call. Holding rt.mu's read lock there deadlocked a factory that
// called rt.Close() (Close waits for the write lock the spawn's own read
// lock blocks), and a factory calling any rt method while another
// goroutine's Close was queued (code review 2026-10-11). A Close that
// lands while a pin is live detaches the handle at once — every later
// method sees a closed runtime — and leaves the free to the last unpin, so
// the native spawn never runs on a freed runtime.
func (rt *DaemonRuntime) pin() (*C.net_compute_runtime_t, func()) {
	rt.mu.RLock()
	h := rt.handle
	if h != nil {
		rt.pins.Add(1)
	}
	rt.mu.RUnlock()
	if h == nil {
		return nil, nil
	}
	return h, func() {
		if rt.pins.Add(-1) != 0 {
			return
		}
		rt.mu.Lock()
		defer rt.mu.Unlock()
		if rt.pendingFree != nil && rt.pins.Load() == 0 {
			rt.freeDetached(rt.pendingFree)
			rt.pendingFree = nil
		}
	}
}

// freeDetached frees a handle Close already detached and purges the
// runtime's factories. Called with rt.mu held for writing.
func (rt *DaemonRuntime) freeDetached(h *C.net_compute_runtime_t) {
	C.net_compute_runtime_free(h)
	// Drop every factory registered against this runtime's id. Safe to
	// call with `runtimeID == 0` (a never-initialized struct, e.g. from a
	// test that zero-values the type): the purge is a no-op in that case.
	// Deferred with the free so an in-flight pinned spawn still resolves
	// the factories it needs.
	if rt.runtimeID != 0 {
		purgeFactoryFuncsForRuntime(rt.runtimeID)
	}
}

// NewDaemonRuntime builds a DaemonRuntime bound to the given
// MeshNode. The returned runtime shares the MeshNode's Arc — the
// mesh stays alive as long as the runtime holds its reference.
//
// Shutting down the runtime does NOT shut down the MeshNode;
// callers own the mesh lifecycle separately.
func NewDaemonRuntime(mesh *MeshNode) (*DaemonRuntime, error) {
	if mesh == nil {
		return nil, &DaemonError{Message: "mesh is nil"}
	}

	// Clone the Arcs with the mesh handle held live (withHandleForChild:
	// the mesh's read lock, so a concurrent Shutdown cannot free the
	// handle between the load and the clones). Once cloned, the Arcs keep
	// the node alive by refcount, so nothing is held past the clones.
	//
	// withHandleForChild also refuses once mesh.Shutdown has BEGUN, not
	// only once it has freed the handle: a runtime built beside the
	// in-progress native shutdown was bound to a stopped node (code review
	// 2026-10-11).
	//
	// These pointers are consumed by net_compute_runtime_new on success —
	// we only need to free them on the error path.
	var nodeArc *C.net_compute_mesh_arc_t
	var ccArc *C.net_compute_cc_arc_t
	if !mesh.withHandleForChild(func(h unsafe.Pointer) {
		nodeArc = C.net_mesh_arc_clone((*C.net_meshnode_t)(h))
		if nodeArc != nil {
			ccArc = C.net_mesh_channel_configs_arc_clone((*C.net_meshnode_t)(h))
		}
	}) {
		return nil, &DaemonError{Message: "mesh has been closed"}
	}
	if nodeArc == nil {
		return nil, &DaemonError{Message: "failed to clone mesh Arc"}
	}
	if ccArc == nil {
		C.net_mesh_arc_free(nodeArc)
		return nil, &DaemonError{Message: "failed to clone channel configs Arc"}
	}

	handle := C.net_compute_runtime_new(nodeArc, ccArc)
	if handle == nil {
		// The constructor consumes the Arcs on success only; a NULL
		// return (including a panic caught mid-construction in an
		// unwind build) leaves both intact, so they are ours to free.
		C.net_mesh_arc_free(nodeArc)
		C.net_mesh_channel_configs_arc_free(ccArc)
		return nil, &DaemonError{Message: "failed to build runtime"}
	}
	rt := &DaemonRuntime{
		handle:    handle,
		runtimeID: uint64(C.net_compute_runtime_id(handle)),
	}
	runtime.SetFinalizer(rt, (*DaemonRuntime).Close)
	return rt, nil
}

// Start transitions the runtime to Ready and installs the migration
// subprotocol handler on the underlying mesh. Idempotent on an
// already-ready runtime; returns an error if the runtime has been
// shut down.
func (rt *DaemonRuntime) Start() error {
	rt.mu.RLock()
	defer rt.mu.RUnlock()
	if rt.handle == nil {
		return ErrRuntimeShutDown
	}
	var errOut *C.char
	code := C.net_compute_runtime_start(rt.handle, &errOut)
	return computeErr(code, errOut)
}

// Shutdown tears down the runtime (drains daemons, clears factory
// registrations, uninstalls the migration handler) but leaves the
// underlying MeshNode running. Idempotent; calling on an
// already-shut-down handle returns ErrRuntimeShutDown.
//
// After Shutdown, the native handle is still allocated — call
// Close to release it, or let the finalizer do so.
func (rt *DaemonRuntime) Shutdown() error {
	rt.mu.RLock()
	defer rt.mu.RUnlock()
	if rt.handle == nil {
		return ErrRuntimeShutDown
	}
	var errOut *C.char
	code := C.net_compute_runtime_shutdown(rt.handle, &errOut)
	return computeErr(code, errOut)
}

// IsReady reports whether the runtime has transitioned to Ready
// and not yet begun shutting down. Returns false for handles that
// have been Close()d.
func (rt *DaemonRuntime) IsReady() bool {
	rt.mu.RLock()
	defer rt.mu.RUnlock()
	if rt.handle == nil {
		return false
	}
	return C.net_compute_runtime_is_ready(rt.handle) == 1
}

// DaemonCount returns the number of daemons currently registered.
// Returns 0 for Close()d runtimes.
func (rt *DaemonRuntime) DaemonCount() int64 {
	rt.mu.RLock()
	defer rt.mu.RUnlock()
	if rt.handle == nil {
		return 0
	}
	n := int64(C.net_compute_runtime_daemon_count(rt.handle))
	if n < 0 {
		return 0
	}
	return n
}

// RegisterFactoryFunc registers a kind with a real Go factory
// function. Enables both local spawn AND migration-target
// reconstruction — an inbound migration for this kind will
// invoke `factory()` on the target to build a fresh daemon, then
// Restore is called with the snapshot state. This is what lets a
// stateful daemon migrate to a Go node and continue running.
//
// Use this instead of [`RegisterFactory`] unless you have a
// reason to opt out of migration support for a kind (e.g., spawn-
// only local daemons).
//
// Duplicate kind registration returns `*DuplicateKindError`.
func (rt *DaemonRuntime) RegisterFactoryFunc(kind string, factory DaemonFactory) error {
	rt.mu.RLock()
	defer rt.mu.RUnlock()
	if rt.handle == nil {
		return ErrRuntimeShutDown
	}
	if factory == nil {
		return &DaemonError{Message: "factory is nil"}
	}
	// Swap-with-restore. Mutate the Go map first so the
	// trampoline has the entry by the time the native register
	// call returns OK (a migration can land and fire the factory
	// the instant `net_compute_register_factory_with_func`
	// returns). If the native call rejects, restore the prior
	// value — a failed registration must not leave a stale entry
	// bound to the kind. The Go map is process-global because
	// the trampoline is keyed on `kind` with no runtime
	// discriminator; per-runtime duplicate-kind rejection is the
	// Rust layer's job (it returns `DUPLICATE_KIND` from the FFI
	// when a given DaemonRuntime already has `kind` registered).
	// A kind this runtime already holds is refused HERE, before the swap:
	// swapping first briefly installed the factory Rust was about to reject,
	// and a migration landing in that window was built with it (SDK audit
	// 2026-10-11). Rust's DUPLICATE_KIND below still covers a kind
	// registered through RegisterFactory.
	//
	// Check, swap and native call run with the (runtime, kind) key held,
	// so a concurrent registration of the same kind cannot slip its
	// factory in between and briefly displace the winner's (code review
	// 2026-10-11). Lookups from the trampoline never take this lock.
	release := lockFactoryRegistration(rt.runtimeID, kind)
	defer release()
	if lookupFactoryFunc(rt.runtimeID, kind) != nil {
		return &DuplicateKindError{Kind: kind}
	}
	prev, existed, ourEpoch := swapFactoryFunc(rt.runtimeID, kind, factory)

	kindBytes := []byte(kind)
	var ptr *C.char
	if len(kindBytes) > 0 {
		ptr = (*C.char)(unsafe.Pointer(&kindBytes[0]))
	}
	code := C.net_compute_register_factory_with_func(rt.handle, ptr, C.size_t(len(kindBytes)))
	runtime.KeepAlive(kindBytes)
	switch code {
	case C.NET_COMPUTE_OK:
		return nil
	case C.NET_COMPUTE_ERR_DUPLICATE_KIND:
		// Rust says this runtime already had the kind. Restore
		// the prior Go-side factory so a subsequent migration
		// into the original runtime still resolves correctly.
		// Epoch-gated so a concurrent successful registration
		// doesn't get clobbered by this rollback.
		restoreFactoryFunc(rt.runtimeID, kind, ourEpoch, prev, existed)
		return &DuplicateKindError{Kind: kind}
	case C.NET_COMPUTE_ERR_CALL_FAILED:
		// The Rust side maps `DaemonError::ShuttingDown` /
		// `NotReady` to this code. Callers racing a concurrent
		// `Shutdown` should see the typed shutdown sentinel, not
		// a generic CALL_FAILED message.
		restoreFactoryFunc(rt.runtimeID, kind, ourEpoch, prev, existed)
		return ErrRuntimeShutDown
	case C.NET_COMPUTE_ERR_NULL:
		restoreFactoryFunc(rt.runtimeID, kind, ourEpoch, prev, existed)
		return &DaemonError{Message: "register_factory_with_func: null argument"}
	default:
		restoreFactoryFunc(rt.runtimeID, kind, ourEpoch, prev, existed)
		return &DaemonError{Message: fmt.Sprintf("register_factory_with_func: unexpected code %d", code)}
	}
}

// RegisterFactory registers a kind without a Go factory func.
// Enables `Spawn`; inbound migrations to this node for this kind
// run as a no-op (`NoopBridge`). Use `RegisterFactoryFunc` when
// you want migrated-in daemons to run user code.
//
// Second registration of the same kind returns a
// `*DuplicateKindError`.
func (rt *DaemonRuntime) RegisterFactory(kind string) error {
	rt.mu.RLock()
	defer rt.mu.RUnlock()
	if rt.handle == nil {
		return ErrRuntimeShutDown
	}
	// C.CString allocates; avoid it for short-lived args by using
	// the byte-slice + size_t pattern that the Rust side prefers.
	kindBytes := []byte(kind)
	var ptr *C.char
	if len(kindBytes) > 0 {
		ptr = (*C.char)(unsafe.Pointer(&kindBytes[0]))
	}
	code := C.net_compute_register_factory(rt.handle, ptr, C.size_t(len(kindBytes)))
	// Keep `kindBytes` alive until after the call returns — Go's
	// escape analysis would otherwise free the slice backing
	// store if the closure below referenced only `ptr`.
	runtime.KeepAlive(kindBytes)
	switch code {
	case C.NET_COMPUTE_OK:
		return nil
	case C.NET_COMPUTE_ERR_DUPLICATE_KIND:
		return &DuplicateKindError{Kind: kind}
	case C.NET_COMPUTE_ERR_CALL_FAILED:
		// The Rust side maps `DaemonError::ShuttingDown` /
		// `NotReady` to this code. See `RegisterFactoryFunc` for
		// the same branch.
		return ErrRuntimeShutDown
	case C.NET_COMPUTE_ERR_NULL:
		return &DaemonError{Message: "register_factory: null argument"}
	default:
		return &DaemonError{Message: fmt.Sprintf("register_factory: unexpected code %d", code)}
	}
}

// Close releases the native handle. Idempotent — a second call is
// a no-op. If Shutdown has not been called, Close attempts it
// best-effort (discarding the error) before freeing.
//
// Also purges every factory the runtime had registered in the
// process-global factory map. Without this, closed runtimes would
// leak their Go-closure factories indefinitely — the map retains
// the closure references and the closures may capture substantial
// state (JS / Python bridges, configs, etc.).
func (rt *DaemonRuntime) Close() {
	rt.mu.Lock()
	defer rt.mu.Unlock()
	if rt.handle == nil {
		return
	}
	h := rt.handle
	rt.handle = nil
	var errOut *C.char
	_ = C.net_compute_runtime_shutdown(h, &errOut)
	if errOut != nil {
		C.net_compute_free_cstring(errOut)
	}
	if rt.pins.Load() > 0 {
		// A pinned native call (a group spawn — possibly the very one
		// whose factory is calling Close) still runs on h: the last
		// unpin frees it. See pin.
		rt.pendingFree = h
	} else {
		rt.freeDetached(h)
	}
	runtime.SetFinalizer(rt, nil)
}

// DaemonHandle is returned by Spawn. Identifies a running daemon
// by its 64-bit origin_hash. Cloning the Go struct shares the
// native pointer — the finalizer runs on the last reference.
//
// Pre-2026-05-11 the origin_hash was uint32, truncating the upper
// 32 bits of the canonical u64 value the substrate emits. The Go
// header was widened to match.
type DaemonHandle struct {
	handle     *C.net_compute_daemon_handle_t
	originHash uint64
	entityID   [32]byte
	mu         sync.RWMutex
}

// OriginHash returns the daemon's stable 64-bit origin_hash.
func (h *DaemonHandle) OriginHash() uint64 {
	return h.originHash
}

// EntityID returns the daemon's full 32-byte ed25519 public key.
// The returned slice is a fresh copy — safe to retain.
func (h *DaemonHandle) EntityID() []byte {
	out := make([]byte, 32)
	copy(out, h.entityID[:])
	return out
}

// Close releases the native daemon handle. Does NOT stop the
// daemon — call DaemonRuntime.Stop(h.OriginHash()) first.
// Idempotent.
func (h *DaemonHandle) Close() {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.handle == nil {
		return
	}
	C.net_compute_daemon_handle_free(h.handle)
	h.handle = nil
	runtime.SetFinalizer(h, nil)
}

// DaemonHostConfig configures per-daemon host behavior. Zero values
// take the runtime defaults (0 = manual snapshots only, ≥0 log
// entries with a sensible default cap).
type DaemonHostConfig struct {
	// AutoSnapshotInterval is the event count between automatic
	// snapshots. 0 disables auto-snapshot.
	AutoSnapshotInterval uint64
	// MaxLogEntries caps the event log before forcing a snapshot.
	// 0 takes the default.
	MaxLogEntries uint32
}

// Spawn creates a new daemon of `kind` under the given identity.
// `daemon` is the Go implementation of `MeshDaemon`; the runtime
// registers it in a process-wide map keyed by a fresh uint64 ID
// that Rust hands back on every `process` / `snapshot` / `restore`
// callback.
//
// `kind` must be a string registered via RegisterFactory (sub-step
// 1 only enforces uniqueness; sub-step 2 lets any string be used
// at spawn time since the bridge is a caller-supplied instance).
//
// Passing a nil `cfg` is equivalent to a zero-value DaemonHostConfig.
func (rt *DaemonRuntime) Spawn(
	kind string,
	identity *Identity,
	daemon MeshDaemon,
	cfg *DaemonHostConfig,
) (*DaemonHandle, error) {
	rt.mu.RLock()
	defer rt.mu.RUnlock()
	if rt.handle == nil {
		return nil, ErrRuntimeShutDown
	}
	if daemon == nil {
		return nil, &DaemonError{Message: "daemon is nil"}
	}
	if identity == nil {
		return nil, &DaemonError{Message: "identity is nil"}
	}

	seed, err := identity.ToSeed()
	if err != nil {
		return nil, &DaemonError{Message: "failed to read identity seed: " + err.Error()}
	}
	if len(seed) != 32 {
		return nil, &DaemonError{Message: "identity seed must be 32 bytes"}
	}

	// Register on the Go side BEFORE calling Rust. If spawn fails,
	// we unregister — otherwise Rust owns the entry via the
	// free callback on bridge drop.
	daemonID := registerDaemon(daemon)

	kindBytes := []byte(kind)
	var kindPtr *C.char
	if len(kindBytes) > 0 {
		kindPtr = (*C.char)(unsafe.Pointer(&kindBytes[0]))
	}

	var autoSnap C.uint64_t
	var maxLog C.uint32_t
	if cfg != nil {
		autoSnap = C.uint64_t(cfg.AutoSnapshotInterval)
		maxLog = C.uint32_t(cfg.MaxLogEntries)
	}

	var nativeHandle *C.net_compute_daemon_handle_t
	var errOut *C.char
	code := C.net_compute_spawn(
		rt.handle,
		kindPtr,
		C.size_t(len(kindBytes)),
		(*C.uint8_t)(unsafe.Pointer(&seed[0])),
		C.uint64_t(daemonID),
		autoSnap,
		maxLog,
		&nativeHandle,
		&errOut,
	)
	runtime.KeepAlive(kindBytes)
	runtime.KeepAlive(seed)

	if code != C.NET_COMPUTE_OK {
		// Rust didn't take ownership — roll back the Go-side
		// registration so the daemon doesn't leak.
		unregisterDaemon(daemonID)
		return nil, computeErr(code, errOut)
	}

	var entityID [32]byte
	_ = C.net_compute_daemon_handle_entity_id(nativeHandle, (*C.uint8_t)(unsafe.Pointer(&entityID[0])))
	originHash := uint64(C.net_compute_daemon_handle_origin_hash(nativeHandle))

	h := &DaemonHandle{
		handle:     nativeHandle,
		originHash: originHash,
		entityID:   entityID,
	}
	runtime.SetFinalizer(h, (*DaemonHandle).Close)
	return h, nil
}

// Stop removes the daemon identified by originHash from the
// runtime's registry. Idempotent during ShuttingDown.
func (rt *DaemonRuntime) Stop(originHash uint64) error {
	rt.mu.RLock()
	defer rt.mu.RUnlock()
	if rt.handle == nil {
		return ErrRuntimeShutDown
	}
	var errOut *C.char
	code := C.net_compute_runtime_stop(rt.handle, C.uint64_t(originHash), &errOut)
	return computeErr(code, errOut)
}

// Snapshot takes a snapshot of a running daemon. Returns the
// serialized state bytes, or nil for a stateless daemon (one that
// doesn't implement DaemonSnapshotter, or whose Snapshot returned
// nil).
//
// The returned bytes round-trip through SpawnFromSnapshot; callers
// treat the slice as opaque.
func (rt *DaemonRuntime) Snapshot(originHash uint64) ([]byte, error) {
	rt.mu.RLock()
	defer rt.mu.RUnlock()
	if rt.handle == nil {
		return nil, ErrRuntimeShutDown
	}
	var outputs *C.net_compute_outputs_t
	var errOut *C.char
	code := C.net_compute_runtime_snapshot(rt.handle, C.uint64_t(originHash), &outputs, &errOut)
	if code != C.NET_COMPUTE_OK {
		return nil, computeErr(code, errOut)
	}
	defer C.net_compute_outputs_free(outputs)
	n := int(C.net_compute_outputs_len(outputs))
	if n == 0 {
		return nil, nil
	}
	var ptr *C.uint8_t
	var length C.size_t
	if C.net_compute_outputs_at(outputs, C.size_t(0), &ptr, &length) != C.NET_COMPUTE_OK {
		return nil, &DaemonError{Message: "snapshot: failed to read bytes"}
	}
	snapshot, okLen := goBytesChecked(ptr, length)
	if !okLen {
		return nil, &DaemonError{
			Message: "snapshot: daemon state is too large to represent as a Go slice",
		}
	}
	return snapshot, nil
}

// SpawnFromSnapshot creates a new daemon of `kind` seeded from a
// previously-taken snapshot. `snapshotBytes` must be the exact
// buffer returned by a prior Snapshot call; the core validates the
// wire format and rejects corruption before touching any state.
//
// The daemon instance supplied by `daemon` provides the initial
// fresh state; its Restore method (if any) is invoked with
// snapshotBytes' inner state payload before Process fires.
func (rt *DaemonRuntime) SpawnFromSnapshot(
	kind string,
	identity *Identity,
	snapshotBytes []byte,
	daemon MeshDaemon,
	cfg *DaemonHostConfig,
) (*DaemonHandle, error) {
	rt.mu.RLock()
	defer rt.mu.RUnlock()
	if rt.handle == nil {
		return nil, ErrRuntimeShutDown
	}
	if daemon == nil {
		return nil, &DaemonError{Message: "daemon is nil"}
	}
	if identity == nil {
		return nil, &DaemonError{Message: "identity is nil"}
	}

	seed, err := identity.ToSeed()
	if err != nil {
		return nil, &DaemonError{Message: "failed to read identity seed: " + err.Error()}
	}
	if len(seed) != 32 {
		return nil, &DaemonError{Message: "identity seed must be 32 bytes"}
	}

	daemonID := registerDaemon(daemon)

	kindBytes := []byte(kind)
	var kindPtr *C.char
	if len(kindBytes) > 0 {
		kindPtr = (*C.char)(unsafe.Pointer(&kindBytes[0]))
	}
	var snapPtr *C.uint8_t
	if len(snapshotBytes) > 0 {
		snapPtr = (*C.uint8_t)(unsafe.Pointer(&snapshotBytes[0]))
	}

	var autoSnap C.uint64_t
	var maxLog C.uint32_t
	if cfg != nil {
		autoSnap = C.uint64_t(cfg.AutoSnapshotInterval)
		maxLog = C.uint32_t(cfg.MaxLogEntries)
	}

	var nativeHandle *C.net_compute_daemon_handle_t
	var errOut *C.char
	code := C.net_compute_spawn_from_snapshot(
		rt.handle,
		kindPtr,
		C.size_t(len(kindBytes)),
		(*C.uint8_t)(unsafe.Pointer(&seed[0])),
		snapPtr,
		C.size_t(len(snapshotBytes)),
		C.uint64_t(daemonID),
		autoSnap,
		maxLog,
		&nativeHandle,
		&errOut,
	)
	runtime.KeepAlive(kindBytes)
	runtime.KeepAlive(seed)
	runtime.KeepAlive(snapshotBytes)

	if code != C.NET_COMPUTE_OK {
		unregisterDaemon(daemonID)
		return nil, computeErr(code, errOut)
	}

	var entityID [32]byte
	_ = C.net_compute_daemon_handle_entity_id(nativeHandle, (*C.uint8_t)(unsafe.Pointer(&entityID[0])))
	originHash := uint64(C.net_compute_daemon_handle_origin_hash(nativeHandle))

	h := &DaemonHandle{
		handle:     nativeHandle,
		originHash: originHash,
		entityID:   entityID,
	}
	runtime.SetFinalizer(h, (*DaemonHandle).Close)
	return h, nil
}

// Deliver drives one causal event through the daemon at
// originHash. Returns the daemon's output payloads (each a fresh
// []byte).
func (rt *DaemonRuntime) Deliver(originHash uint64, event CausalEvent) ([][]byte, error) {
	rt.mu.RLock()
	defer rt.mu.RUnlock()
	if rt.handle == nil {
		return nil, ErrRuntimeShutDown
	}

	var payloadPtr *C.uint8_t
	if len(event.Payload) > 0 {
		payloadPtr = (*C.uint8_t)(unsafe.Pointer(&event.Payload[0]))
	}

	var outputs *C.net_compute_outputs_t
	var errOut *C.char
	code := C.net_compute_runtime_deliver(
		rt.handle,
		C.uint64_t(originHash),
		C.uint64_t(event.OriginHash),
		C.uint64_t(event.Sequence),
		payloadPtr,
		C.size_t(len(event.Payload)),
		&outputs,
		&errOut,
	)
	runtime.KeepAlive(event.Payload)
	if code != C.NET_COMPUTE_OK {
		return nil, computeErr(code, errOut)
	}
	defer C.net_compute_outputs_free(outputs)

	n := int(C.net_compute_outputs_len(outputs))
	out := make([][]byte, n)
	for i := 0; i < n; i++ {
		var ptr *C.uint8_t
		var length C.size_t
		if C.net_compute_outputs_at(outputs, C.size_t(i), &ptr, &length) != C.NET_COMPUTE_OK {
			return nil, &DaemonError{Message: "deliver: failed to read output"}
		}
		// Copy — outputs pointer lifetime is tied to the outputs
		// vec which we're about to free.
		copied, okLen := goBytesChecked(ptr, length)
		if !okLen {
			return nil, &DaemonError{
				Message: "deliver: daemon output is too large to represent as a Go slice",
			}
		}
		out[i] = copied
	}
	return out, nil
}

// computeErr turns a compute-ffi return code + optional err_out
// CString into a Go error. OK returns nil; all other codes return
// a *DaemonError, with Message populated from err_out when set.
func computeErr(code C.int, errOut *C.char) error {
	if code == C.NET_COMPUTE_OK {
		if errOut != nil {
			// Shouldn't happen on success, but be defensive.
			C.net_compute_free_cstring(errOut)
		}
		return nil
	}
	msg := ""
	if errOut != nil {
		msg = C.GoString(errOut)
		C.net_compute_free_cstring(errOut)
	}
	if msg == "" {
		msg = fmt.Sprintf("compute call failed (code %d)", code)
	}
	return &DaemonError{Message: msg}
}
