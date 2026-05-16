// Package net — MeshOS daemon-author SDK consumer wrapper for the
// C ABI exported by `net::ffi::meshos` (compiled as
// `libnet_meshos`).
//
// # Scope (slice 1a)
//
// Lifecycle, control receive, log emission, graceful shutdown.
// User-supplied daemon callbacks (`process` / `snapshot` /
// `restore` / `onControl`) are NOT plumbed yet — the registered
// daemon is an internal no-op on the Rust side. Slice 1b adds the
// cgo `//export` callback bridge.
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
*/
import "C"

import (
	"errors"
	"fmt"
	"runtime"
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

// Free releases the handle. Idempotent on nil receivers.
func (h *MeshOsDaemonHandle) Free() {
	if h == nil || h.ptr == nil {
		return
	}
	C.net_meshos_handle_free(h.ptr)
	h.ptr = nil
	runtime.SetFinalizer(h, nil)
}
