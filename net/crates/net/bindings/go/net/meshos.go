// Package net — MeshOS daemon-author SDK consumer wrapper for the
// C ABI exported by `net::ffi::meshos` (compiled as
// `libnet_meshos`).
//
// # Scope (slice 1b)
//
// Full daemon-author surface: lifecycle, control receive, log
// emission, graceful shutdown, and the cgo `//export` trampoline
// bridge that lets Go consumers supply real daemon callbacks
// (process / snapshot / restore / onControl / health /
// saturation) against the cdylib's vtable.
//
// `RegisterDaemon` registers an internal no-op daemon for
// lifecycle-only consumers. `RegisterDaemonWithCallbacks`
// (slice 1b) accepts a [MeshOsDaemon] interface implementation;
// the wrapper allocates a `cgo.Handle` that the //export
// trampolines dispatch through, so each Go closure runs on a
// tokio worker thread without copying state across the FFI on
// every call.
//
// # Memory model
//
// Every Rust object that crosses the FFI is wrapped in a
// `runtime.SetFinalizer`–protected Go handle. Manual `.Free()`
// methods are exposed for callers that want deterministic
// teardown.
//
// # Error model
//
// FFI functions return `c_int` status codes:
//
//   - 0 (`NET_MESHOS_OK`)            — success.
//   - -1 (`NET_MESHOS_ERR_NULL`)     — null handle.
//   - -2 (`NET_MESHOS_ERR_CALL_FAILED`) — substrate-side failure.
//   - -3 (`NET_MESHOS_ERR_INVALID_ARG`) — null pointer / bad input.
//   - -4 (`NET_MESHOS_ERR_ALREADY_SHUTDOWN`) — handle / sdk already
//     consumed by shutdown.
//
// Detail flows through a per-thread last-error pair populated by
// the FFI on every non-OK status. `MeshOsSdkError` wraps both the
// `kind` discriminator (e.g. `"register_failed"`, `"queue_full"`,
// `"already_shutdown"`) and the human-readable message; the
// `errors.Is(err, ErrMeshOs*)` sentinel routing keeps working.
package net

/*
#include <stdint.h>
#include <stdlib.h>

// Forward-declared opaque handles from `libnet_meshos`.
typedef struct NetMeshOsSdk NetMeshOsSdk;
typedef struct NetMeshOsHandle NetMeshOsHandle;

// Status codes.
#define NET_MESHOS_OK 0
#define NET_MESHOS_ERR_NULL -1
#define NET_MESHOS_ERR_CALL_FAILED -2
#define NET_MESHOS_ERR_INVALID_ARG -3
#define NET_MESHOS_ERR_ALREADY_SHUTDOWN -4

// DaemonControl kinds.
#define NET_MESHOS_CONTROL_NONE 0
#define NET_MESHOS_CONTROL_SHUTDOWN 1
#define NET_MESHOS_CONTROL_DRAIN_START 2
#define NET_MESHOS_CONTROL_DRAIN_FINISH 3
#define NET_MESHOS_CONTROL_BACKPRESSURE_ON 4
#define NET_MESHOS_CONTROL_BACKPRESSURE_OFF 5
#define NET_MESHOS_CONTROL_UNKNOWN 99

// LogLevel constants.
#define NET_MESHOS_LOG_TRACE 0
#define NET_MESHOS_LOG_DEBUG 1
#define NET_MESHOS_LOG_INFO 2
#define NET_MESHOS_LOG_WARN 3
#define NET_MESHOS_LOG_ERROR 4

typedef struct {
    int kind;
    uint64_t grace_period_ms;
    float level;
} NetMeshOsDaemonControl;

// SDK lifecycle.
extern int net_meshos_sdk_start(
    uint64_t this_node,
    uint64_t tick_interval_ms,
    size_t event_queue_capacity,
    size_t action_queue_capacity,
    size_t control_capacity,
    NetMeshOsSdk** out
);
extern void net_meshos_sdk_free(NetMeshOsSdk* sdk);
extern int net_meshos_sdk_shutdown(NetMeshOsSdk* sdk);
extern uint64_t net_meshos_sdk_dropped_control_events(NetMeshOsSdk* sdk);

// Daemon registration.
extern int net_meshos_register_daemon(
    NetMeshOsSdk* sdk,
    const char* name_ptr,
    size_t name_len,
    const uint8_t* seed_ptr,
    NetMeshOsHandle** out
);
extern void net_meshos_handle_free(NetMeshOsHandle* handle);
extern uint64_t net_meshos_handle_daemon_id(const NetMeshOsHandle* handle);
extern const char* net_meshos_handle_daemon_name(const NetMeshOsHandle* handle);

// Control receive.
extern int net_meshos_try_next_control(
    NetMeshOsHandle* handle,
    NetMeshOsDaemonControl* out
);
extern int net_meshos_next_control(
    NetMeshOsHandle* handle,
    uint64_t timeout_ms,
    NetMeshOsDaemonControl* out
);

// Log emission.
extern int net_meshos_publish_log(
    NetMeshOsHandle* handle,
    int level,
    const char* message_ptr,
    size_t message_len
);

// Graceful shutdown.
extern int net_meshos_graceful_shutdown(
    NetMeshOsHandle* handle,
    uint64_t grace_ms
);

// Last-error trio.
extern const char* net_meshos_last_error_message(void);
extern const char* net_meshos_last_error_kind(void);
extern void net_meshos_clear_last_error(void);

// Slice 1b — vtable bridge. The Go-side `//export` trampoline
// surface lands in slice 1c; for now these are declared so the
// cdylib's exported symbols are documented at the Go binding
// boundary.
#define NET_MESHOS_HEALTH_HEALTHY 0
#define NET_MESHOS_HEALTH_DEGRADED 1
#define NET_MESHOS_HEALTH_UNHEALTHY 2

typedef struct NetMeshOsProcessEmitCtx NetMeshOsProcessEmitCtx;
typedef struct NetMeshOsSnapshotEmitCtx NetMeshOsSnapshotEmitCtx;

extern void net_meshos_process_emit(
    NetMeshOsProcessEmitCtx* ctx,
    const uint8_t* payload_ptr,
    size_t payload_len
);
extern void net_meshos_snapshot_emit(
    NetMeshOsSnapshotEmitCtx* ctx,
    const uint8_t* payload_ptr,
    size_t payload_len
);

// Vtable struct mirroring the cdylib's
// `NetMeshOsDaemonVtable`. Each field is a function pointer
// the consumer fills in. The Go wrapper builds this with
// pointers to //export'd Go trampolines below.
typedef int (*NetMeshOsProcessFn)(
    void* user_ctx,
    NetMeshOsProcessEmitCtx* emit_ctx,
    uint64_t origin_hash,
    uint64_t sequence,
    const uint8_t* payload_ptr,
    size_t payload_len
);
typedef void (*NetMeshOsSnapshotFn)(
    void* user_ctx,
    NetMeshOsSnapshotEmitCtx* emit_ctx
);
typedef int (*NetMeshOsRestoreFn)(
    void* user_ctx,
    const uint8_t* payload_ptr,
    size_t payload_len
);
typedef void (*NetMeshOsOnControlFn)(
    void* user_ctx,
    int kind,
    uint64_t grace_period_ms,
    float level
);
typedef int (*NetMeshOsHealthFn)(void* user_ctx);
typedef float (*NetMeshOsSaturationFn)(void* user_ctx);

typedef struct {
    NetMeshOsProcessFn process;
    NetMeshOsSnapshotFn snapshot;
    NetMeshOsRestoreFn restore;
    NetMeshOsOnControlFn on_control;
    NetMeshOsHealthFn health;
    NetMeshOsSaturationFn saturation;
} NetMeshOsDaemonVtable;

extern int net_meshos_register_daemon_with_vtable(
    NetMeshOsSdk* sdk,
    const char* name_ptr,
    size_t name_len,
    const uint8_t* seed_ptr,
    const NetMeshOsDaemonVtable* vtable_ptr,
    void* user_ctx,
    NetMeshOsHandle** out
);

// Forward declarations of the //export'd Go trampolines (cgo
// emits these into `_cgo_export.h`; we duplicate the
// declarations here so the static-inline builder below can
// take their addresses).
extern int goMeshOsProcessTrampoline(
    void* user_ctx,
    NetMeshOsProcessEmitCtx* emit_ctx,
    uint64_t origin_hash,
    uint64_t sequence,
    const uint8_t* payload_ptr,
    size_t payload_len
);
extern void goMeshOsSnapshotTrampoline(
    void* user_ctx,
    NetMeshOsSnapshotEmitCtx* emit_ctx
);
extern int goMeshOsRestoreTrampoline(
    void* user_ctx,
    const uint8_t* payload_ptr,
    size_t payload_len
);
extern void goMeshOsOnControlTrampoline(
    void* user_ctx,
    int kind,
    uint64_t grace_period_ms,
    float level
);
extern int goMeshOsHealthTrampoline(void* user_ctx);
extern float goMeshOsSaturationTrampoline(void* user_ctx);

// Populate a vtable with pointers to the //export'd Go
// trampolines. The cdylib copies the vtable on registration,
// so this returned-by-value form is safe.
static inline NetMeshOsDaemonVtable goMeshOsBuildVtable(void) {
    NetMeshOsDaemonVtable vt;
    vt.process    = goMeshOsProcessTrampoline;
    vt.snapshot   = goMeshOsSnapshotTrampoline;
    vt.restore    = goMeshOsRestoreTrampoline;
    vt.on_control = goMeshOsOnControlTrampoline;
    vt.health     = goMeshOsHealthTrampoline;
    vt.saturation = goMeshOsSaturationTrampoline;
    return vt;
}

// Slice 2 — metadata + capability advertisement.
extern char* net_meshos_metadata(const NetMeshOsHandle* handle);
extern char* net_meshos_refresh_metadata(NetMeshOsHandle* handle);
extern void net_meshos_free_string(char* s);
extern int net_meshos_publish_capabilities(
    NetMeshOsHandle* handle,
    const char* tags_json_ptr,
    size_t tags_json_len
);
*/
import "C"

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"runtime"
	"runtime/cgo"
	"sync"
	"time"
	"unsafe"
)

// =====================================================================
// Errors
// =====================================================================

// ErrMeshOs is the root discriminator for MeshOS-side errors. Wrap
// concrete errors with `fmt.Errorf("...: %w", ErrMeshOs)` so callers
// can route via `errors.Is(err, ErrMeshOs)`.
var ErrMeshOs = errors.New("meshos")

// ErrMeshOsInvalidArg covers null-pointer / out-of-range inputs
// that the FFI rejects synchronously.
var ErrMeshOsInvalidArg = errors.New("meshos: invalid argument")

// ErrMeshOsAlreadyShutdown covers the case where the SDK or handle
// was already consumed by `Shutdown` / `GracefulShutdown`.
var ErrMeshOsAlreadyShutdown = errors.New("meshos: already shutdown")

// ErrMeshOsCallFailed covers substrate-side failures (register
// rejected, queue full, runtime shutdown failed, etc.). The
// concrete `kind` is on the `MeshOsSdkError`.
var ErrMeshOsCallFailed = errors.New("meshos: call failed")

// MeshOsSdkError carries the substrate's structured error envelope.
// `Kind` is the cross-binding discriminator (one of the
// `<<meshos-sdk-kind:KIND>>` values — `"register_failed"`,
// `"queue_full"`, `"already_shutdown"`, `"invalid_log_level"`,
// etc.). `Sentinel` exposes the broad bucket for `errors.Is`
// routing.
type MeshOsSdkError struct {
	Sentinel error  // ErrMeshOsInvalidArg | ErrMeshOsAlreadyShutdown | ErrMeshOsCallFailed
	Kind     string // Substrate kind discriminator; empty when not reported
	Message  string // Human-readable detail; empty when not reported
}

func (e *MeshOsSdkError) Error() string {
	if e == nil {
		return "<nil meshos error>"
	}
	switch {
	case e.Kind != "" && e.Message != "":
		return fmt.Sprintf("%s (kind=%s): %s", e.Sentinel.Error(), e.Kind, e.Message)
	case e.Kind != "":
		return fmt.Sprintf("%s (kind=%s)", e.Sentinel.Error(), e.Kind)
	case e.Message != "":
		return fmt.Sprintf("%s: %s", e.Sentinel.Error(), e.Message)
	default:
		return e.Sentinel.Error()
	}
}

func (e *MeshOsSdkError) Unwrap() error {
	if e == nil {
		return nil
	}
	return e.Sentinel
}

// wrapMeshOsError reads the per-thread last-error pair from the
// FFI and pairs it with a sentinel. Always returns a non-nil
// `*MeshOsSdkError`. Clears the thread-local state after reading.
func wrapMeshOsError(sentinel error) *MeshOsSdkError {
	err := &MeshOsSdkError{Sentinel: sentinel}
	if msgPtr := C.net_meshos_last_error_message(); msgPtr != nil {
		err.Message = C.GoString(msgPtr)
	}
	if kindPtr := C.net_meshos_last_error_kind(); kindPtr != nil {
		err.Kind = C.GoString(kindPtr)
	}
	C.net_meshos_clear_last_error()
	return err
}

func statusToError(status C.int) error {
	switch status {
	case C.NET_MESHOS_OK:
		return nil
	case C.NET_MESHOS_ERR_NULL, C.NET_MESHOS_ERR_INVALID_ARG:
		return wrapMeshOsError(ErrMeshOsInvalidArg)
	case C.NET_MESHOS_ERR_ALREADY_SHUTDOWN:
		return wrapMeshOsError(ErrMeshOsAlreadyShutdown)
	default:
		return wrapMeshOsError(ErrMeshOsCallFailed)
	}
}

// =====================================================================
// Daemon control event — cross-binding wire form
// =====================================================================

// DaemonControlKind discriminates the variant carried by a
// `MeshOsDaemonControl`. The integer values are stable across the
// FFI (see `NET_MESHOS_CONTROL_*` constants in the cdylib).
type DaemonControlKind int

const (
	// ControlNone — no event present (returned by TryNextControl
	// on an empty channel, or NextControl on timeout / shutdown).
	ControlNone DaemonControlKind = 0
	// ControlShutdown — graceful shutdown request with a
	// `GracePeriodMs` budget.
	ControlShutdown DaemonControlKind = 1
	// ControlDrainStart — stop accepting new work; in-flight work
	// continues until `GracePeriodMs` elapses or DrainFinish arrives.
	ControlDrainStart DaemonControlKind = 2
	// ControlDrainFinish — drain done; exit immediately.
	ControlDrainFinish DaemonControlKind = 3
	// ControlBackpressureOn — cluster-wide backpressure asserted;
	// `Level ∈ [0.0, 1.0]`.
	ControlBackpressureOn DaemonControlKind = 4
	// ControlBackpressureOff — backpressure cleared.
	ControlBackpressureOff DaemonControlKind = 5
	// ControlUnknown — fallback for substrate-side variants the
	// binding hasn't been rebuilt against.
	ControlUnknown DaemonControlKind = 99
)

// MeshOsDaemonControl is the Go-side projection of the substrate's
// `DaemonControl` enum. `GracePeriodMs` is valid for Shutdown /
// DrainStart; `Level` is valid for BackpressureOn.
type MeshOsDaemonControl struct {
	Kind          DaemonControlKind
	GracePeriodMs uint64
	Level         float32
}

func controlFromC(c C.NetMeshOsDaemonControl) MeshOsDaemonControl {
	return MeshOsDaemonControl{
		Kind:          DaemonControlKind(c.kind),
		GracePeriodMs: uint64(c.grace_period_ms),
		Level:         float32(c.level),
	}
}

// =====================================================================
// LogLevel
// =====================================================================

// MeshOsLogLevel is the cross-binding log level. Values match the
// FFI's `NET_MESHOS_LOG_*` constants.
type MeshOsLogLevel int

const (
	LogTrace MeshOsLogLevel = MeshOsLogLevel(C.NET_MESHOS_LOG_TRACE)
	LogDebug MeshOsLogLevel = MeshOsLogLevel(C.NET_MESHOS_LOG_DEBUG)
	LogInfo  MeshOsLogLevel = MeshOsLogLevel(C.NET_MESHOS_LOG_INFO)
	LogWarn  MeshOsLogLevel = MeshOsLogLevel(C.NET_MESHOS_LOG_WARN)
	LogError MeshOsLogLevel = MeshOsLogLevel(C.NET_MESHOS_LOG_ERROR)
)

// =====================================================================
// SDK
// =====================================================================

// MeshOsConfig mirrors the substrate's `MeshOsConfig` knobs. All
// fields are optional — zero values pick the substrate default.
type MeshOsConfig struct {
	// ThisNode is the substrate's node identifier. 0 → default 0.
	ThisNode uint64
	// TickIntervalMs is the supervisor reconcile cadence in ms.
	// 0 → substrate default 500ms.
	TickIntervalMs uint64
	// EventQueueCapacity sizes the internal event-source mpsc.
	// 0 → substrate default 1024.
	EventQueueCapacity int
	// ActionQueueCapacity sizes the action-executor mpsc.
	// 0 → substrate default 1024.
	ActionQueueCapacity int
	// ControlCapacity sizes the per-daemon control channel.
	// 0 → substrate default 8.
	ControlCapacity int
}

// MeshOsDaemonSdk is the Go-side handle for the daemon-author SDK.
// Construct via `StartMeshOsDaemonSdk`; tear down via `Shutdown`.
type MeshOsDaemonSdk struct {
	ptr *C.NetMeshOsSdk
}

// StartMeshOsDaemonSdk starts the MeshOS SDK with the substrate's
// `LoggingDispatcher`. Returns a handle the caller must `Shutdown`
// (or rely on the finalizer to `_free`).
func StartMeshOsDaemonSdk(cfg MeshOsConfig) (*MeshOsDaemonSdk, error) {
	var raw *C.NetMeshOsSdk
	status := C.net_meshos_sdk_start(
		C.uint64_t(cfg.ThisNode),
		C.uint64_t(cfg.TickIntervalMs),
		C.size_t(cfg.EventQueueCapacity),
		C.size_t(cfg.ActionQueueCapacity),
		C.size_t(cfg.ControlCapacity),
		&raw,
	)
	if err := statusToError(status); err != nil {
		return nil, err
	}
	sdk := &MeshOsDaemonSdk{ptr: raw}
	runtime.SetFinalizer(sdk, func(s *MeshOsDaemonSdk) { s.Free() })
	return sdk, nil
}

// DroppedControlEvents — diagnostic counter for control events
// the router dropped across every registered daemon because the
// daemon's channel was full.
func (s *MeshOsDaemonSdk) DroppedControlEvents() uint64 {
	if s == nil || s.ptr == nil {
		return 0
	}
	return uint64(C.net_meshos_sdk_dropped_control_events(s.ptr))
}

// RegisterDaemon registers a daemon under the supplied 32-byte
// ed25519 seed.
//
// **Slice 1a caveat:** the substrate-side daemon is an internal
// no-op (`process` returns no outputs, `snapshot` returns None,
// etc.). User-supplied Go callbacks land in slice 1b via the cgo
// `//export` trampoline bridge. The supervisor lifecycle works
// end-to-end today — control events, log emission, graceful
// shutdown — just not user-driven event processing.
func (s *MeshOsDaemonSdk) RegisterDaemon(name string, seed []byte) (*MeshOsDaemonHandle, error) {
	if s == nil || s.ptr == nil {
		return nil, wrapMeshOsError(ErrMeshOsInvalidArg)
	}
	if len(seed) != 32 {
		return nil, fmt.Errorf("%w: seed must be 32 bytes, got %d", ErrMeshOsInvalidArg, len(seed))
	}
	var raw *C.NetMeshOsHandle
	var namePtr *C.char
	var nameLen C.size_t
	if len(name) > 0 {
		nameBytes := []byte(name)
		namePtr = (*C.char)(unsafe.Pointer(&nameBytes[0]))
		nameLen = C.size_t(len(nameBytes))
	}
	seedPtr := (*C.uint8_t)(unsafe.Pointer(&seed[0]))
	status := C.net_meshos_register_daemon(s.ptr, namePtr, nameLen, seedPtr, &raw)
	if err := statusToError(status); err != nil {
		return nil, err
	}
	h := &MeshOsDaemonHandle{ptr: raw}
	runtime.SetFinalizer(h, func(h *MeshOsDaemonHandle) { h.Free() })
	return h, nil
}

// Shutdown drives a clean shutdown of the wrapped runtime. Consumes
// the SDK — subsequent ops return `ErrMeshOsAlreadyShutdown`.
// Caller still must `Free` (or rely on the finalizer).
func (s *MeshOsDaemonSdk) Shutdown() error {
	if s == nil || s.ptr == nil {
		return ErrMeshOsInvalidArg
	}
	return statusToError(C.net_meshos_sdk_shutdown(s.ptr))
}

// Free releases the SDK handle. Idempotent. After Free the SDK is
// no longer usable; subsequent method calls return an error.
func (s *MeshOsDaemonSdk) Free() {
	if s == nil || s.ptr == nil {
		return
	}
	C.net_meshos_sdk_free(s.ptr)
	s.ptr = nil
	runtime.SetFinalizer(s, nil)
}

// =====================================================================
// Handle
// =====================================================================

// MeshOsDaemonHandle is the Go-side handle for a registered daemon.
type MeshOsDaemonHandle struct {
	ptr *C.NetMeshOsHandle
	// daemonHandle is the `cgo.Handle` allocated by
	// `RegisterDaemonWithCallbacks` for the //export trampoline
	// dispatch. Zero for handles produced by `RegisterDaemon`
	// (which registers an internal no-op daemon and needs no
	// Go-side dispatch). Released on `Free` / finalizer.
	daemonHandle cgo.Handle
	// freeOnce serialises `Free` against itself and against the
	// runtime finalizer. Without it concurrent callers could
	// double-close `pumpStop`, double-free the FFI handle, or
	// race the finalizer-vs-explicit `Free` window.
	freeOnce sync.Once
	// controlOnce gates the per-handle ControlEvents pump
	// goroutine. The first `ControlEvents(ctx)` call spawns it;
	// subsequent calls return the same channel.
	controlOnce sync.Once
	// controlChan is closed when the pump goroutine exits.
	controlChan chan MeshOsDaemonControl
	// pumpStop is closed by `Free` to signal the pump goroutine
	// to exit even when the caller never cancelled the ctx they
	// passed to `ControlEvents`. Without this, `Free` would
	// release the underlying `ptr` while the pump was still
	// polling on it (use-after-free against a freed handle).
	pumpStop chan struct{}
	// pumpDone is closed by `pumpControlEvents` on exit. `Free`
	// waits on it before the C-free so a pump blocked inside
	// `NextControl` finishes against the live handle. Without
	// this wait, closing `pumpStop` only narrowed the race to
	// the in-flight FFI call (≤50ms window).
	pumpDone chan struct{}
}

// DaemonID returns the substrate identifier (the keypair's origin
// hash). Stable across the handle's lifetime, including after
// shutdown.
func (h *MeshOsDaemonHandle) DaemonID() uint64 {
	if h == nil || h.ptr == nil {
		return 0
	}
	return uint64(C.net_meshos_handle_daemon_id(h.ptr))
}

// DaemonName returns the daemon's name at registration. Empty
// string when the handle is nil.
func (h *MeshOsDaemonHandle) DaemonName() string {
	if h == nil || h.ptr == nil {
		return ""
	}
	if cstr := C.net_meshos_handle_daemon_name(h.ptr); cstr != nil {
		return C.GoString(cstr)
	}
	return ""
}

// TryNextControl returns the next control event without blocking,
// or `ControlNone` if the channel is empty. Errors on
// already-shutdown handles.
func (h *MeshOsDaemonHandle) TryNextControl() (MeshOsDaemonControl, error) {
	if h == nil || h.ptr == nil {
		return MeshOsDaemonControl{}, ErrMeshOsInvalidArg
	}
	var out C.NetMeshOsDaemonControl
	if err := statusToError(C.net_meshos_try_next_control(h.ptr, &out)); err != nil {
		return MeshOsDaemonControl{}, err
	}
	return controlFromC(out), nil
}

// NextControl blocks until the next control event arrives, the
// runtime shuts down, or `timeoutMs` elapses. Pass 0 for an
// unbounded wait. Returns `Kind == ControlNone` on timeout /
// shutdown.
func (h *MeshOsDaemonHandle) NextControl(timeoutMs uint64) (MeshOsDaemonControl, error) {
	if h == nil || h.ptr == nil {
		return MeshOsDaemonControl{}, ErrMeshOsInvalidArg
	}
	var out C.NetMeshOsDaemonControl
	if err := statusToError(C.net_meshos_next_control(h.ptr, C.uint64_t(timeoutMs), &out)); err != nil {
		return MeshOsDaemonControl{}, err
	}
	return controlFromC(out), nil
}

// PublishLog publishes a log line tagged with this daemon's id.
// Non-blocking — surfaces `MeshOsSdkError(kind: "queue_full" |
// "loop_closed")` when the substrate's log ring is saturated.
func (h *MeshOsDaemonHandle) PublishLog(level MeshOsLogLevel, message string) error {
	if h == nil || h.ptr == nil {
		return ErrMeshOsInvalidArg
	}
	var msgPtr *C.char
	var msgLen C.size_t
	if len(message) > 0 {
		msgBytes := []byte(message)
		msgPtr = (*C.char)(unsafe.Pointer(&msgBytes[0]))
		msgLen = C.size_t(len(msgBytes))
	}
	return statusToError(C.net_meshos_publish_log(h.ptr, C.int(level), msgPtr, msgLen))
}

// GracefulShutdown drives a graceful shutdown on the daemon. Sends
// `Shutdown { gracePeriodMs }` on the control channel, parks for
// `graceMs`, then unregisters. Pass 0 for the substrate default
// (5 seconds).
//
// Consumes the inner handle — subsequent calls return
// `ErrMeshOsAlreadyShutdown`. Caller still must `Free` (or rely
// on the finalizer).
func (h *MeshOsDaemonHandle) GracefulShutdown(graceMs uint64) error {
	if h == nil || h.ptr == nil {
		return ErrMeshOsInvalidArg
	}
	return statusToError(C.net_meshos_graceful_shutdown(h.ptr, C.uint64_t(graceMs)))
}

// Free releases the handle. Safe to call concurrently and
// repeatedly — `sync.Once` collapses every call past the first
// (including the runtime finalizer) into a no-op.
//
// Stops the `ControlEvents` pump goroutine (if one was started)
// before the underlying FFI handle is freed — otherwise the pump
// would race the free and either deref a freed pointer or surface
// a confusing `invalid_argument` to whichever goroutine was
// ranging over the channel. After closing `pumpStop`, `Free`
// waits on `pumpDone` so an in-flight `NextControl(50)` returns
// before the C handle disappears under it. The pump's own
// deferred `close(h.controlChan)` runs after it returns from
// `pumpControlEvents`, so the consumer's `range` loop terminates
// cleanly.
func (h *MeshOsDaemonHandle) Free() {
	if h == nil {
		return
	}
	h.freeOnce.Do(func() {
		if h.ptr == nil {
			return
		}
		if h.pumpStop != nil {
			close(h.pumpStop)
			if h.pumpDone != nil {
				<-h.pumpDone
			}
		}
		C.net_meshos_handle_free(h.ptr)
		h.ptr = nil
		if h.daemonHandle != 0 {
			h.daemonHandle.Delete()
			h.daemonHandle = 0
		}
		runtime.SetFinalizer(h, nil)
	})
}

// =====================================================================
// Slice 1b — MeshOsDaemon interface + cgo //export trampolines
// =====================================================================

// MeshOsCausalEvent is the value handed to a daemon's `Process`
// callback. Mirrors the substrate's `CausalEvent` projection.
type MeshOsCausalEvent struct {
	OriginHash uint64
	Sequence   uint64
	// Payload is a fresh copy of the substrate's event bytes —
	// safe to retain past the callback return.
	Payload []byte
}

// MeshOsDaemonHealthKind discriminates a daemon's reported
// health. Matches the substrate's `DaemonHealth` shape with the
// reason elided.
type MeshOsDaemonHealthKind int

const (
	HealthHealthy   MeshOsDaemonHealthKind = MeshOsDaemonHealthKind(C.NET_MESHOS_HEALTH_HEALTHY)
	HealthDegraded  MeshOsDaemonHealthKind = MeshOsDaemonHealthKind(C.NET_MESHOS_HEALTH_DEGRADED)
	HealthUnhealthy MeshOsDaemonHealthKind = MeshOsDaemonHealthKind(C.NET_MESHOS_HEALTH_UNHEALTHY)
)

// MeshOsDaemon is the Go-side daemon contract. Embed
// [MeshOsDefaultDaemon] to inherit no-op defaults for every
// method except `Name` + `Process`.
//
// **Threading.** Callbacks fire from tokio worker threads — the
// cgo bridge dispatches through a `cgo.Handle` looked up by the
// runtime, so consumer state is shared across threads. Protect
// any shared state with `sync.Mutex` / atomics as needed. Hot
// loops still pay the cgo crossing cost; the substrate's
// `process()` runs once per event, which is the dominant path.
type MeshOsDaemon interface {
	// Name returns the daemon's substrate-side name. Stable
	// across the daemon's lifetime; the bridge resolves it
	// once at registration.
	Name() string
	// Process handles one inbound causal event and returns zero
	// or more output payloads. Non-nil error surfaces as
	// `ProcessFailed` on the substrate side.
	Process(event MeshOsCausalEvent) ([][]byte, error)
	// Snapshot returns the daemon's serialized state, or
	// `(nil, false)` for stateless daemons.
	Snapshot() ([]byte, bool)
	// Restore re-seeds the daemon's state from a snapshot.
	// Non-nil error surfaces as `RestoreFailed`.
	Restore(state []byte) error
	// OnControl fires for every supervisor control event
	// routed to this daemon. Return-fast — the supervisor's
	// reconcile loop blocks on this call.
	OnControl(event MeshOsDaemonControl)
	// Health reports the daemon's current health.
	Health() MeshOsDaemonHealthKind
	// Saturation reports a value in `[0.0, 1.0]` summarizing
	// how loaded the daemon is.
	Saturation() float32
}

// MeshOsDefaultDaemon is a zero-method base every MeshOsDaemon
// implementation can embed to inherit no-op defaults for the
// optional methods. Override `Name` and `Process`; the rest
// fall through to safe substrate defaults.
type MeshOsDefaultDaemon struct{}

func (MeshOsDefaultDaemon) Snapshot() ([]byte, bool)        { return nil, false }
func (MeshOsDefaultDaemon) Restore(_ []byte) error          { return nil }
func (MeshOsDefaultDaemon) OnControl(_ MeshOsDaemonControl) {}
func (MeshOsDefaultDaemon) Health() MeshOsDaemonHealthKind  { return HealthHealthy }
func (MeshOsDefaultDaemon) Saturation() float32             { return 0 }

// RegisterDaemonWithCallbacks registers a user-supplied
// [MeshOsDaemon] under the given 32-byte ed25519 seed. The
// wrapper allocates a `cgo.Handle` for the daemon, builds the
// vtable with pointers to //export'd trampolines, and hands
// ownership of the handle to the resulting
// [MeshOsDaemonHandle] — when the handle is freed (or
// gracefully shut down + freed), the cgo.Handle is released.
//
// Returns `ErrMeshOsInvalidArg` if the seed isn't 32 bytes or
// the daemon is nil.
func (s *MeshOsDaemonSdk) RegisterDaemonWithCallbacks(daemon MeshOsDaemon, seed []byte) (*MeshOsDaemonHandle, error) {
	if s == nil || s.ptr == nil {
		return nil, wrapMeshOsError(ErrMeshOsInvalidArg)
	}
	if daemon == nil {
		return nil, fmt.Errorf("%w: daemon is nil", ErrMeshOsInvalidArg)
	}
	if len(seed) != 32 {
		return nil, fmt.Errorf("%w: seed must be 32 bytes, got %d", ErrMeshOsInvalidArg, len(seed))
	}
	name := daemon.Name()
	if name == "" {
		return nil, fmt.Errorf("%w: daemon Name() returned empty string", ErrMeshOsInvalidArg)
	}

	cgoHandle := cgo.NewHandle(daemon)
	vt := C.goMeshOsBuildVtable()

	var raw *C.NetMeshOsHandle
	var namePtr *C.char
	var nameLen C.size_t
	if len(name) > 0 {
		nameBytes := []byte(name)
		namePtr = (*C.char)(unsafe.Pointer(&nameBytes[0]))
		nameLen = C.size_t(len(nameBytes))
	}
	seedPtr := (*C.uint8_t)(unsafe.Pointer(&seed[0]))
	// `cgo.Handle` is documented as a `uintptr` under the hood;
	// round-tripping it through `unsafe.Pointer(uintptr(...))`
	// hands the cdylib a value it can pass back through the
	// vtable trampoline (`//export` callbacks reverse the round-
	// trip via `cgo.Handle(uintptr(ptr))`). go vet may flag this
	// shape generally but the runtime keeps the underlying value
	// alive because we retain `cgoHandle` on the Go side until
	// `Free`.
	userCtx := unsafe.Pointer(uintptr(cgoHandle))
	status := C.net_meshos_register_daemon_with_vtable(
		s.ptr, namePtr, nameLen, seedPtr, &vt, userCtx, &raw,
	)
	if err := statusToError(status); err != nil {
		cgoHandle.Delete()
		return nil, err
	}
	h := &MeshOsDaemonHandle{ptr: raw, daemonHandle: cgoHandle}
	runtime.SetFinalizer(h, func(h *MeshOsDaemonHandle) { h.Free() })
	return h, nil
}

// =====================================================================
// //export trampolines — dispatch through cgo.Handle into the
// user's MeshOsDaemon implementation
// =====================================================================

func handleFromCtx(userCtx unsafe.Pointer) MeshOsDaemon {
	h := cgo.Handle(uintptr(userCtx))
	v := h.Value()
	if d, ok := v.(MeshOsDaemon); ok {
		return d
	}
	return nil
}

//export goMeshOsProcessTrampoline
func goMeshOsProcessTrampoline(
	userCtx unsafe.Pointer,
	emitCtx *C.NetMeshOsProcessEmitCtx,
	originHash C.uint64_t,
	sequence C.uint64_t,
	payloadPtr *C.uint8_t,
	payloadLen C.size_t,
) C.int {
	d := handleFromCtx(userCtx)
	if d == nil {
		return C.int(1)
	}
	// `goBytesChecked` rejects an oversized inbound payload length
	// rather than truncating it via the 32-bit C.int cast.
	payload, okLen := goBytesChecked(payloadPtr, payloadLen)
	if !okLen {
		return C.int(1)
	}
	event := MeshOsCausalEvent{
		OriginHash: uint64(originHash),
		Sequence:   uint64(sequence),
		Payload:    payload,
	}
	outputs, err := d.Process(event)
	if err != nil {
		return C.int(1)
	}
	for _, out := range outputs {
		if len(out) == 0 {
			C.net_meshos_process_emit(emitCtx, nil, 0)
		} else {
			C.net_meshos_process_emit(
				emitCtx,
				(*C.uint8_t)(unsafe.Pointer(&out[0])),
				C.size_t(len(out)),
			)
		}
	}
	return C.int(0)
}

//export goMeshOsSnapshotTrampoline
func goMeshOsSnapshotTrampoline(
	userCtx unsafe.Pointer,
	emitCtx *C.NetMeshOsSnapshotEmitCtx,
) {
	d := handleFromCtx(userCtx)
	if d == nil {
		return
	}
	payload, present := d.Snapshot()
	if !present {
		return
	}
	if len(payload) == 0 {
		C.net_meshos_snapshot_emit(emitCtx, nil, 0)
		return
	}
	C.net_meshos_snapshot_emit(
		emitCtx,
		(*C.uint8_t)(unsafe.Pointer(&payload[0])),
		C.size_t(len(payload)),
	)
}

//export goMeshOsRestoreTrampoline
func goMeshOsRestoreTrampoline(
	userCtx unsafe.Pointer,
	payloadPtr *C.uint8_t,
	payloadLen C.size_t,
) C.int {
	d := handleFromCtx(userCtx)
	if d == nil {
		return C.int(1)
	}
	state, okLen := goBytesChecked(payloadPtr, payloadLen)
	if !okLen {
		return C.int(1)
	}
	if err := d.Restore(state); err != nil {
		return C.int(1)
	}
	return C.int(0)
}

//export goMeshOsOnControlTrampoline
func goMeshOsOnControlTrampoline(
	userCtx unsafe.Pointer,
	kind C.int,
	gracePeriodMs C.uint64_t,
	level C.float,
) {
	d := handleFromCtx(userCtx)
	if d == nil {
		return
	}
	d.OnControl(MeshOsDaemonControl{
		Kind:          DaemonControlKind(kind),
		GracePeriodMs: uint64(gracePeriodMs),
		Level:         float32(level),
	})
}

//export goMeshOsHealthTrampoline
func goMeshOsHealthTrampoline(userCtx unsafe.Pointer) C.int {
	d := handleFromCtx(userCtx)
	if d == nil {
		return C.int(C.NET_MESHOS_HEALTH_HEALTHY)
	}
	return C.int(d.Health())
}

//export goMeshOsSaturationTrampoline
func goMeshOsSaturationTrampoline(userCtx unsafe.Pointer) C.float {
	d := handleFromCtx(userCtx)
	if d == nil {
		return C.float(0)
	}
	v := d.Saturation()
	if v < 0 {
		v = 0
	} else if v > 1 {
		v = 1
	}
	return C.float(v)
}

// =====================================================================
// Slice 2 — metadata + capability advertisement
// =====================================================================

// MeshOsMaintenanceStateView mirrors the cdylib's tagged-union
// `MaintenanceStateView` projection. Inspect `Kind` to discriminate;
// variant-specific fields are populated per kind:
//
//   - "active"                — no extra fields.
//   - "entering_maintenance"  — SinceMs + DeadlineRemainingMs (nullable).
//   - "maintenance"           — SinceMs.
//   - "exiting_maintenance"   — SinceMs.
//   - "drain_failed"          — SinceMs + Reason.
//   - "recovery"              — SinceMs.
//   - "unknown"               — forward-compat fallback for substrate
//     variants the binding doesn't yet know.
type MeshOsMaintenanceStateView struct {
	Kind                string  `json:"kind"`
	SinceMs             uint64  `json:"since_ms,omitempty"`
	DeadlineRemainingMs *uint64 `json:"deadline_remaining_ms,omitempty"`
	Reason              string  `json:"reason,omitempty"`
}

// MeshOsPeerSnapshot mirrors the per-peer view inside
// `MetadataView`. Health / maintenance fields are stringified
// substrate enum variants ("Healthy" / "Degraded" / etc.).
type MeshOsPeerSnapshot struct {
	NodeID          uint64   `json:"node_id"`
	RttMs           *uint64  `json:"rtt_ms,omitempty"`
	Health          *string  `json:"health,omitempty"`
	Maintenance     *string  `json:"maintenance,omitempty"`
	CpuLoad1m       *float64 `json:"cpu_load_1m,omitempty"`
	MemUsedBytes    *uint64  `json:"mem_used_bytes,omitempty"`
	MemTotalBytes   *uint64  `json:"mem_total_bytes,omitempty"`
	DiskUsedBytes   *uint64  `json:"disk_used_bytes,omitempty"`
	DiskTotalBytes  *uint64  `json:"disk_total_bytes,omitempty"`
	SaturationTrend *float64 `json:"saturation_trend,omitempty"`
	CapabilitySet   []string `json:"capability_set,omitempty"`
	SoftwareVersion *string  `json:"software_version,omitempty"`
	ForkedFrom      *uint64  `json:"forked_from,omitempty"`
}

// MeshOsMetadataView is the daemon's read-only cluster context.
// Returned by `MeshOsDaemonHandle.Metadata()` /
// `RefreshMetadata()`.
type MeshOsMetadataView struct {
	NodeID           uint64                     `json:"node_id"`
	DaemonID         uint64                     `json:"daemon_id"`
	DaemonName       string                     `json:"daemon_name"`
	MaintenanceState MeshOsMaintenanceStateView `json:"maintenance_state"`
	Peers            []MeshOsPeerSnapshot       `json:"peers"`
}

// Metadata returns a fresh snapshot of the daemon's
// `MetadataView` decoded from the cdylib's JSON envelope.
// Returns `ErrMeshOsAlreadyShutdown` after `GracefulShutdown`.
func (h *MeshOsDaemonHandle) Metadata() (*MeshOsMetadataView, error) {
	if h == nil || h.ptr == nil {
		return nil, ErrMeshOsInvalidArg
	}
	raw := C.net_meshos_metadata(h.ptr)
	if raw == nil {
		return nil, lastMeshOsError()
	}
	defer C.net_meshos_free_string(raw)
	jsonStr := C.GoString(raw)
	var view MeshOsMetadataView
	if err := json.Unmarshal([]byte(jsonStr), &view); err != nil {
		return nil, fmt.Errorf("meshos: decode metadata JSON: %w", err)
	}
	return &view, nil
}

// RefreshMetadata re-pulls from the runtime's latest snapshot
// and returns the freshly-rendered view.
func (h *MeshOsDaemonHandle) RefreshMetadata() (*MeshOsMetadataView, error) {
	if h == nil || h.ptr == nil {
		return nil, ErrMeshOsInvalidArg
	}
	raw := C.net_meshos_refresh_metadata(h.ptr)
	if raw == nil {
		return nil, lastMeshOsError()
	}
	defer C.net_meshos_free_string(raw)
	jsonStr := C.GoString(raw)
	var view MeshOsMetadataView
	if err := json.Unmarshal([]byte(jsonStr), &view); err != nil {
		return nil, fmt.Errorf("meshos: decode metadata JSON: %w", err)
	}
	return &view, nil
}

// PublishCapabilities advertises a fresh capability tag set
// for this daemon. Pass an empty slice to clear the advert.
//
// **Stub today.** The substrate's
// `MeshOsDaemonHandle::publish_capabilities` returns `Ok(())`
// without committing to the capability chain — the call
// succeeds, but the rest of the cluster won't observe the
// update until the substrate's chain-commit path lands. Every
// binding surfaces the same stub semantics so consumers don't
// write code against a contract the substrate doesn't yet
// honor. Cuts over transparently when the substrate ships.
func (h *MeshOsDaemonHandle) PublishCapabilities(tags []string) error {
	if h == nil || h.ptr == nil {
		return ErrMeshOsInvalidArg
	}
	if len(tags) == 0 {
		status := C.net_meshos_publish_capabilities(h.ptr, nil, 0)
		return statusToError(status)
	}
	payload, err := json.Marshal(tags)
	if err != nil {
		return fmt.Errorf("meshos: encode capability tags: %w", err)
	}
	status := C.net_meshos_publish_capabilities(
		h.ptr,
		(*C.char)(unsafe.Pointer(&payload[0])),
		C.size_t(len(payload)),
	)
	return statusToError(status)
}

// =====================================================================
// Slice 3 — ControlEvents channel + context.Context plumbing
// =====================================================================

// ControlEvents returns a buffered channel that delivers
// `MeshOsDaemonControl` events as the supervisor emits them.
// The channel closes when `ctx` is cancelled or the daemon
// handle is shut down. The first call on a handle spawns the
// pumping goroutine; subsequent calls return the same channel.
//
// The goroutine uses a 50ms poll cadence — the cdylib's
// `next_control` blocks with a timeout, so the goroutine
// alternates between polling and checking `ctx.Done()`. For
// hot-path workloads where 50ms is too coarse, fall back to
// `TryNextControl` / `NextControl(timeoutMs)` directly.
func (h *MeshOsDaemonHandle) ControlEvents(ctx context.Context) <-chan MeshOsDaemonControl {
	h.controlOnce.Do(func() {
		h.controlChan = make(chan MeshOsDaemonControl, 16)
		h.pumpStop = make(chan struct{})
		h.pumpDone = make(chan struct{})
		go h.pumpControlEvents(ctx)
	})
	return h.controlChan
}

func (h *MeshOsDaemonHandle) pumpControlEvents(ctx context.Context) {
	defer close(h.pumpDone)
	defer close(h.controlChan)
	for {
		select {
		case <-ctx.Done():
			return
		case <-h.pumpStop:
			// `Free` was called — exit before polling the
			// (about to be freed) FFI handle.
			return
		default:
		}
		// Use a short timeout so we can check ctx between polls.
		ev, err := h.NextControl(50)
		if err != nil {
			// `already_shutdown`, `invalid_argument` (NULL handle
			// after Free), or any other terminal error stops the
			// pump. Treating every error as terminal is correct
			// here — the underlying FFI handle can't transition
			// from a failure state back to producing events.
			return
		}
		if ev.Kind == ControlNone {
			continue
		}
		select {
		case h.controlChan <- ev:
		case <-ctx.Done():
			return
		case <-h.pumpStop:
			return
		}
	}
}

// MetadataContext is the context-aware variant of Metadata.
// Cancellation aborts the call before the JSON-decode step; the
// underlying FFI fetch is fast (microseconds) so cancellation
// inside the FFI is best-effort.
func (h *MeshOsDaemonHandle) MetadataContext(ctx context.Context) (*MeshOsMetadataView, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	return h.Metadata()
}

// GracefulShutdownContext is the context-aware variant of
// `GracefulShutdown`. The context's deadline overrides the
// caller-supplied grace if it expires sooner; cancellation
// before the call propagates as `ctx.Err()`.
func (h *MeshOsDaemonHandle) GracefulShutdownContext(ctx context.Context, grace time.Duration) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	if deadline, ok := ctx.Deadline(); ok {
		until := time.Until(deadline)
		if until < grace {
			grace = until
		}
	}
	if grace < 0 {
		grace = 0
	}
	return h.GracefulShutdown(uint64(grace / time.Millisecond))
}

// lastMeshOsError reads the cdylib's thread-local last-error
// pair and constructs a typed Go error.
func lastMeshOsError() error {
	kindPtr := C.net_meshos_last_error_kind()
	msgPtr := C.net_meshos_last_error_message()
	if kindPtr == nil && msgPtr == nil {
		return ErrMeshOs
	}
	kind := ""
	if kindPtr != nil {
		kind = C.GoString(kindPtr)
	}
	msg := ""
	if msgPtr != nil {
		msg = C.GoString(msgPtr)
	}
	C.net_meshos_clear_last_error()
	if kind == "already_shutdown" {
		return ErrMeshOsAlreadyShutdown
	}
	return fmt.Errorf("meshos: %s: %s", kind, msg)
}
