// Migration surface for the compute runtime — Stage 6 sub-step 4.
//
// Mirrors the NAPI / PyO3 migration APIs: source-side
// `StartMigration` / `StartMigrationWith` return a `MigrationHandle`
// whose `Wait` / `WaitWithTimeout` / `Cancel` methods drive the
// orchestrator state machine; target-side `ExpectMigration` /
// `RegisterMigrationTargetIdentity` pre-register the factory so
// inbound snapshots can restore.
//
// Typed migration failures surface via `*MigrationError` (a
// subtype of `*DaemonError`) with a stable `Kind` field parsed
// from the Rust-emitted `migration: <kind>[: detail]` prefix.
package net

/*
#include "net.h"
#include <stdlib.h>
*/
import "C"

import (
	"errors"
	"fmt"
	"runtime"
	"strings"
	"sync"
	"time"
	"unsafe"
)

// MigrationErrorKind is the stable discriminator parsed from a
// migration failure message. Matches the kind vocabulary used by
// the Node + Python bindings so callers can write portable
// dispatch logic.
type MigrationErrorKind string

const (
	MigrationErrKindNotReady                MigrationErrorKind = "not-ready"
	MigrationErrKindFactoryNotFound         MigrationErrorKind = "factory-not-found"
	MigrationErrKindComputeNotSupported     MigrationErrorKind = "compute-not-supported"
	MigrationErrKindStateFailed             MigrationErrorKind = "state-failed"
	MigrationErrKindAlreadyMigrating        MigrationErrorKind = "already-migrating"
	MigrationErrKindIdentityTransportFailed MigrationErrorKind = "identity-transport-failed"
	MigrationErrKindNotReadyTimeout         MigrationErrorKind = "not-ready-timeout"
	MigrationErrKindDaemonNotFound          MigrationErrorKind = "daemon-not-found"
	MigrationErrKindTargetUnavailable       MigrationErrorKind = "target-unavailable"
	MigrationErrKindWrongPhase              MigrationErrorKind = "wrong-phase"
	MigrationErrKindSnapshotTooLarge        MigrationErrorKind = "snapshot-too-large"
	MigrationErrKindUnknown                 MigrationErrorKind = "unknown"
)

// MigrationError is the typed failure returned by migration
// methods. Embeds *DaemonError so callers who catch the broader
// type still match; `errors.As(err, &*MigrationError)` lifts the
// discriminator fields.
type MigrationError struct {
	*DaemonError
	// Kind is the stable machine-readable failure tag.
	Kind MigrationErrorKind
	// Detail is the optional free-form detail that follows the
	// kind (e.g., the `msg` in `state-failed: <msg>`). Empty for
	// tag-only variants like `not-ready`.
	Detail string
}

// Error implements the built-in error interface.
func (e *MigrationError) Error() string {
	if e.DaemonError != nil {
		return e.DaemonError.Error()
	}
	return "migration error"
}

// Unwrap lets `errors.Is` / `errors.As` walk to the underlying
// *DaemonError.
func (e *MigrationError) Unwrap() error {
	return e.DaemonError
}

// parseMigrationError tries to lift a DaemonError into a typed
// MigrationError by parsing its "daemon: migration: <kind>[: detail]"
// prefix. Returns nil if the message doesn't match that shape.
func parseMigrationError(d *DaemonError) *MigrationError {
	if d == nil {
		return nil
	}
	// DaemonError.Message is already stripped of the "daemon: "
	// prefix — it starts with "migration: ..." for typed errors.
	const prefix = "migration:"
	msg := d.Message
	if !strings.HasPrefix(msg, prefix) {
		return nil
	}
	body := strings.TrimSpace(msg[len(prefix):])
	kind := body
	var detail string
	if i := strings.Index(body, ":"); i != -1 {
		kind = strings.TrimSpace(body[:i])
		detail = strings.TrimSpace(body[i+1:])
	}
	k := MigrationErrorKind(kind)
	switch k {
	case MigrationErrKindNotReady,
		MigrationErrKindFactoryNotFound,
		MigrationErrKindComputeNotSupported,
		MigrationErrKindStateFailed,
		MigrationErrKindAlreadyMigrating,
		MigrationErrKindIdentityTransportFailed,
		MigrationErrKindNotReadyTimeout,
		MigrationErrKindDaemonNotFound,
		MigrationErrKindTargetUnavailable,
		MigrationErrKindWrongPhase,
		MigrationErrKindSnapshotTooLarge:
		// Known tag.
	default:
		k = MigrationErrKindUnknown
	}
	return &MigrationError{
		DaemonError: d,
		Kind:        k,
		Detail:      detail,
	}
}

// migrationErr mirrors `computeErr` but lifts known migration
// bodies into the typed `*MigrationError`. Non-migration errors
// fall through to `*DaemonError`.
func migrationErr(code C.int, errOut *C.char) error {
	err := computeErr(code, errOut)
	if err == nil {
		return nil
	}
	var de *DaemonError
	if errors.As(err, &de) {
		if me := parseMigrationError(de); me != nil {
			return me
		}
	}
	return err
}

// MigrationOptions configures StartMigrationWith. Zero values
// take the SDK defaults (TransportIdentity=true,
// RetryNotReadyMs=30_000).
type MigrationOptions struct {
	// TransportIdentity: seal the daemon's ed25519 seed in the
	// snapshot envelope so the target keeps signing capability.
	// Defaults to true.
	TransportIdentity bool
	// RetryNotReadyMs: backoff budget in milliseconds on target
	// `NotReady`. 0 disables retry; otherwise the source
	// re-initiates up to this total elapsed time.
	RetryNotReadyMs uint64

	// defaultsSet is flipped when the caller constructed the
	// options via `DefaultMigrationOptions`. Lets us distinguish
	// "user explicitly set fields to zero" from "struct was just
	// `MigrationOptions{}`" at the call site.
	defaultsSet bool
}

// DefaultMigrationOptions returns the SDK-recommended defaults —
// identity transport on, 30-second NotReady retry budget.
func DefaultMigrationOptions() MigrationOptions {
	return MigrationOptions{
		TransportIdentity: true,
		RetryNotReadyMs:   30_000,
		defaultsSet:       true,
	}
}

// MigrationHandle tracks an in-flight migration. Dropping the
// handle does NOT cancel the migration — the orchestrator keeps
// driving it to completion. Call Close when done.
type MigrationHandle struct {
	handle     *C.net_compute_migration_handle_t
	originHash uint64
	sourceNode uint64
	targetNode uint64
	mu         sync.RWMutex
}

// OriginHash returns the 64-bit origin_hash of the migrating daemon.
func (h *MigrationHandle) OriginHash() uint64 { return h.originHash }

// SourceNode returns the source node ID.
func (h *MigrationHandle) SourceNode() uint64 { return h.sourceNode }

// TargetNode returns the target node ID.
func (h *MigrationHandle) TargetNode() uint64 { return h.targetNode }

// Phase reports the current migration phase, or the empty string
// once the orchestrator has cleaned up the record (terminal
// success OR abort — callers distinguish by remembering the last
// non-empty phase).
func (h *MigrationHandle) Phase() string {
	h.mu.RLock()
	defer h.mu.RUnlock()
	if h.handle == nil {
		return ""
	}
	cstr := C.net_compute_migration_handle_phase(h.handle)
	if cstr == nil {
		return ""
	}
	defer C.net_compute_free_cstring(cstr)
	return C.GoString(cstr)
}

// Wait blocks until the migration reaches a terminal state.
// Returns nil on `complete`; returns a *MigrationError on
// abort/failure.
func (h *MigrationHandle) Wait() error {
	h.mu.RLock()
	defer h.mu.RUnlock()
	if h.handle == nil {
		return ErrRuntimeShutDown
	}
	var errOut *C.char
	code := C.net_compute_migration_handle_wait(h.handle, &errOut)
	return migrationErr(code, errOut)
}

// clampTimeoutToU64Ms converts a `time.Duration` to the u64
// milliseconds the Rust FFI expects, clamping non-positive
// durations to zero. Extracted as a pure helper so the clamp is
// unit-testable without spinning up a real migration handle — a
// nil-handle test would short-circuit via the `handle == nil`
// guard in `WaitWithTimeout` and never exercise this branch.
//
// Rationale: `time.Duration.Milliseconds()` returns `int64`.
// A negative value cast straight to `uint64` wraps to a huge
// number — `-1 ns` becomes roughly 584 million years — which
// would turn a past-deadline call into effectively infinite
// blocking. Clamping to 0 forces the FFI to "check once and
// return."
func clampTimeoutToU64Ms(d time.Duration) uint64 {
	ms := d.Milliseconds()
	if ms < 0 {
		return 0
	}
	return uint64(ms)
}

// WaitWithTimeout is like Wait but aborts the migration on
// timeout and returns a *MigrationError describing the stall.
//
// A zero or negative timeout aborts immediately — see
// [`clampTimeoutToU64Ms`] for why.
func (h *MigrationHandle) WaitWithTimeout(timeout time.Duration) error {
	h.mu.RLock()
	defer h.mu.RUnlock()
	if h.handle == nil {
		return ErrRuntimeShutDown
	}
	var errOut *C.char
	code := C.net_compute_migration_handle_wait_with_timeout(
		h.handle,
		C.uint64_t(clampTimeoutToU64Ms(timeout)),
		&errOut,
	)
	return migrationErr(code, errOut)
}

// Cancel requests cancellation. Best-effort; past `cutover` the
// routing flip is irreversible.
func (h *MigrationHandle) Cancel() error {
	h.mu.RLock()
	defer h.mu.RUnlock()
	if h.handle == nil {
		return ErrRuntimeShutDown
	}
	var errOut *C.char
	code := C.net_compute_migration_handle_cancel(h.handle, &errOut)
	return migrationErr(code, errOut)
}

// Phases returns a receive-only channel that yields each distinct
// phase transition as the orchestrator drives the migration, and
// closes when the record is cleaned up. Polls at 50 ms — matching
// the Rust SDK's `wait()` cadence.
//
// Call Phases right after the handle is returned; if you Wait()
// first and then ask for Phases, the orchestrator record may
// already be gone and the channel closes immediately.
func (h *MigrationHandle) Phases() <-chan string {
	ch := make(chan string, 8)
	go func() {
		defer close(ch)
		var last string
		for {
			cur := h.Phase()
			if cur == "" {
				// Terminal — orchestrator cleared the record.
				return
			}
			if cur != last {
				// Best-effort send; drop on full channel to avoid
				// blocking the poller if the caller stops reading.
				select {
				case ch <- cur:
				default:
				}
				last = cur
			}
			time.Sleep(50 * time.Millisecond)
		}
	}()
	return ch
}

// Close releases the native migration handle. Does NOT cancel
// the migration. Idempotent.
func (h *MigrationHandle) Close() {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.handle == nil {
		return
	}
	C.net_compute_migration_handle_free(h.handle)
	h.handle = nil
	runtime.SetFinalizer(h, nil)
}

// -------------------------------------------------------------------------
// DaemonRuntime migration methods
// -------------------------------------------------------------------------

// StartMigration initiates a migration for the daemon at
// `originHash` from `sourceNode` to `targetNode` using the default
// options (identity transport on, 30 s NotReady retry). Returns
// a *MigrationHandle whose `Wait()` resolves on terminal state.
func (rt *DaemonRuntime) StartMigration(
	originHash uint64,
	sourceNode uint64,
	targetNode uint64,
) (*MigrationHandle, error) {
	return rt.StartMigrationWith(originHash, sourceNode, targetNode, DefaultMigrationOptions())
}

// StartMigrationWith is the options-taking variant. Set
// `opts.TransportIdentity = false` for identity-envelope-free
// migrations, or tune `opts.RetryNotReadyMs`.
func (rt *DaemonRuntime) StartMigrationWith(
	originHash uint64,
	sourceNode uint64,
	targetNode uint64,
	opts MigrationOptions,
) (*MigrationHandle, error) {
	rt.mu.RLock()
	defer rt.mu.RUnlock()
	if rt.handle == nil {
		return nil, ErrRuntimeShutDown
	}

	var transport C.uint8_t
	if opts.TransportIdentity {
		transport = 1
	}

	var nativeHandle *C.net_compute_migration_handle_t
	var errOut *C.char
	code := C.net_compute_start_migration(
		rt.handle,
		C.uint64_t(originHash),
		C.uint64_t(sourceNode),
		C.uint64_t(targetNode),
		transport,
		C.uint64_t(opts.RetryNotReadyMs),
		&nativeHandle,
		&errOut,
	)
	if code != C.NET_COMPUTE_OK {
		return nil, migrationErr(code, errOut)
	}

	h := &MigrationHandle{
		handle:     nativeHandle,
		originHash: uint64(C.net_compute_migration_handle_origin_hash(nativeHandle)),
		sourceNode: uint64(C.net_compute_migration_handle_source_node(nativeHandle)),
		targetNode: uint64(C.net_compute_migration_handle_target_node(nativeHandle)),
	}
	runtime.SetFinalizer(h, (*MigrationHandle).Close)
	return h, nil
}

// ExpectMigration declares on this node that a migration will land
// here for `originHash` of `kind`. Registers a placeholder
// factory; identity comes from the snapshot's envelope.
func (rt *DaemonRuntime) ExpectMigration(
	kind string,
	originHash uint64,
	cfg *DaemonHostConfig,
) error {
	rt.mu.RLock()
	defer rt.mu.RUnlock()
	if rt.handle == nil {
		return ErrRuntimeShutDown
	}
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
	var errOut *C.char
	code := C.net_compute_expect_migration(
		rt.handle,
		kindPtr,
		C.size_t(len(kindBytes)),
		C.uint64_t(originHash),
		autoSnap,
		maxLog,
		&errOut,
	)
	runtime.KeepAlive(kindBytes)
	return migrationErr(code, errOut)
}

// RegisterMigrationTargetIdentity pre-registers a target-side
// identity for a migration that will arrive WITHOUT an identity
// envelope (source used TransportIdentity=false). For the
// envelope-transport case, use ExpectMigration.
func (rt *DaemonRuntime) RegisterMigrationTargetIdentity(
	kind string,
	identity *Identity,
	cfg *DaemonHostConfig,
) error {
	rt.mu.RLock()
	defer rt.mu.RUnlock()
	if rt.handle == nil {
		return ErrRuntimeShutDown
	}
	if identity == nil {
		return &DaemonError{Message: "identity is nil"}
	}
	seed, err := identity.ToSeed()
	if err != nil {
		return &DaemonError{Message: "failed to read identity seed: " + err.Error()}
	}
	if len(seed) != 32 {
		return &DaemonError{Message: "identity seed must be 32 bytes"}
	}

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

	var errOut *C.char
	code := C.net_compute_register_migration_target_identity(
		rt.handle,
		kindPtr,
		C.size_t(len(kindBytes)),
		(*C.uint8_t)(unsafe.Pointer(&seed[0])),
		autoSnap,
		maxLog,
		&errOut,
	)
	runtime.KeepAlive(kindBytes)
	runtime.KeepAlive(seed)
	return migrationErr(code, errOut)
}

// MigrationPhase queries the orchestrator's current migration
// phase for `originHash`. Returns "" if no migration is in flight.
func (rt *DaemonRuntime) MigrationPhase(originHash uint64) string {
	rt.mu.RLock()
	defer rt.mu.RUnlock()
	if rt.handle == nil {
		return ""
	}
	cstr := C.net_compute_migration_phase(rt.handle, C.uint64_t(originHash))
	if cstr == nil {
		return ""
	}
	defer C.net_compute_free_cstring(cstr)
	return C.GoString(cstr)
}

// ensure fmt is actually used somewhere in the package — the
// import is defensive because Error() formatters may land here
// later (e.g., wrap-with-context helpers).
var _ = fmt.Sprintf