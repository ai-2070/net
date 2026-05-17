// Package net — MeshOS daemon-author SDK.
//
// Wraps the C ABI exported by `libnet_meshos` (the cdylib shipped with
// the `meshos` Cargo feature). Provides:
//
//   - StartMeshOsDaemonSdk → MeshOsDaemonSdk (lifecycle + tear-down).
//   - MeshOsDaemonSdk.RegisterDaemon(name, seed)  — lifecycle-only;
//     substrate runs an internal no-op daemon.
//   - MeshOsDaemonSdk.RegisterDaemonWithCallbacks(daemon, seed) — full
//     user-driven dispatch via the cgo //export trampoline bridge.
//   - MeshOsDaemonHandle methods: TryNextControl / NextControl /
//     PublishLog / PublishCapabilities / Metadata / RefreshMetadata /
//     GracefulShutdown / ControlEvents (channel + context.Context).
//
// Port of the reference impl at net/crates/net/bindings/go/net/meshos.go.
// Same C ABI, same callback model. Build prerequisite: `cargo build
// --release -p net-meshos-ffi`.

package net

/*
#cgo LDFLAGS: -L${SRCDIR}/../net/crates/net/target/release -lnet_meshos
#include <stdint.h>
#include <stdlib.h>

typedef struct NetMeshOsSdk NetMeshOsSdk;
typedef struct NetMeshOsHandle NetMeshOsHandle;

#define NET_MESHOS_OK 0
#define NET_MESHOS_ERR_NULL -1
#define NET_MESHOS_ERR_CALL_FAILED -2
#define NET_MESHOS_ERR_INVALID_ARG -3
#define NET_MESHOS_ERR_ALREADY_SHUTDOWN -4

#define NET_MESHOS_CONTROL_NONE 0
#define NET_MESHOS_CONTROL_SHUTDOWN 1
#define NET_MESHOS_CONTROL_DRAIN_START 2
#define NET_MESHOS_CONTROL_DRAIN_FINISH 3
#define NET_MESHOS_CONTROL_BACKPRESSURE_ON 4
#define NET_MESHOS_CONTROL_BACKPRESSURE_OFF 5
#define NET_MESHOS_CONTROL_UNKNOWN 99

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

extern int net_meshos_try_next_control(
    NetMeshOsHandle* handle,
    NetMeshOsDaemonControl* out
);
extern int net_meshos_next_control(
    NetMeshOsHandle* handle,
    uint64_t timeout_ms,
    NetMeshOsDaemonControl* out
);

extern int net_meshos_publish_log(
    NetMeshOsHandle* handle,
    int level,
    const char* message_ptr,
    size_t message_len
);

extern int net_meshos_graceful_shutdown(
    NetMeshOsHandle* handle,
    uint64_t grace_ms
);

extern const char* net_meshos_last_error_message(void);
extern const char* net_meshos_last_error_kind(void);
extern void net_meshos_clear_last_error(void);

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

extern int topGoMeshOsProcessTrampoline(
    void* user_ctx,
    NetMeshOsProcessEmitCtx* emit_ctx,
    uint64_t origin_hash,
    uint64_t sequence,
    const uint8_t* payload_ptr,
    size_t payload_len
);
extern void topGoMeshOsSnapshotTrampoline(
    void* user_ctx,
    NetMeshOsSnapshotEmitCtx* emit_ctx
);
extern int topGoMeshOsRestoreTrampoline(
    void* user_ctx,
    const uint8_t* payload_ptr,
    size_t payload_len
);
extern void topGoMeshOsOnControlTrampoline(
    void* user_ctx,
    int kind,
    uint64_t grace_period_ms,
    float level
);
extern int topGoMeshOsHealthTrampoline(void* user_ctx);
extern float topGoMeshOsSaturationTrampoline(void* user_ctx);

static inline NetMeshOsDaemonVtable topGoMeshOsBuildVtable(void) {
    NetMeshOsDaemonVtable vt;
    vt.process    = topGoMeshOsProcessTrampoline;
    vt.snapshot   = topGoMeshOsSnapshotTrampoline;
    vt.restore    = topGoMeshOsRestoreTrampoline;
    vt.on_control = topGoMeshOsOnControlTrampoline;
    vt.health     = topGoMeshOsHealthTrampoline;
    vt.saturation = topGoMeshOsSaturationTrampoline;
    return vt;
}

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

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

var (
	// ErrMeshOs is the root sentinel for any MeshOS-side failure.
	ErrMeshOs = errors.New("meshos")
	// ErrMeshOsInvalidArg covers null-pointer / out-of-range inputs.
	ErrMeshOsInvalidArg = errors.New("meshos: invalid argument")
	// ErrMeshOsAlreadyShutdown — SDK / handle already consumed.
	ErrMeshOsAlreadyShutdown = errors.New("meshos: already shutdown")
	// ErrMeshOsCallFailed — substrate-side failure (register rejected,
	// queue full, runtime shutdown failed, etc.).
	ErrMeshOsCallFailed = errors.New("meshos: call failed")
)

// MeshOsSdkError carries the substrate's structured error envelope.
// `Kind` is the cross-binding discriminator
// (`<<meshos-sdk-kind:KIND>>` — `register_failed`, `queue_full`, etc.).
type MeshOsSdkError struct {
	Sentinel error
	Kind     string
	Message  string
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

func meshosStatusToError(status C.int) error {
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

// ---------------------------------------------------------------------------
// Control events + log levels
// ---------------------------------------------------------------------------

// DaemonControlKind discriminates the variant carried by
// MeshOsDaemonControl. Values match the FFI's NET_MESHOS_CONTROL_*
// constants.
type DaemonControlKind int

const (
	ControlNone            DaemonControlKind = 0
	ControlShutdown        DaemonControlKind = 1
	ControlDrainStart      DaemonControlKind = 2
	ControlDrainFinish     DaemonControlKind = 3
	ControlBackpressureOn  DaemonControlKind = 4
	ControlBackpressureOff DaemonControlKind = 5
	ControlUnknown         DaemonControlKind = 99
)

// MeshOsDaemonControl is the Go-side projection of the substrate's
// DaemonControl enum. GracePeriodMs is valid for Shutdown / DrainStart;
// Level is valid for BackpressureOn.
type MeshOsDaemonControl struct {
	Kind          DaemonControlKind
	GracePeriodMs uint64
	Level         float32
}

func meshosControlFromC(c C.NetMeshOsDaemonControl) MeshOsDaemonControl {
	return MeshOsDaemonControl{
		Kind:          DaemonControlKind(c.kind),
		GracePeriodMs: uint64(c.grace_period_ms),
		Level:         float32(c.level),
	}
}

// MeshOsLogLevel matches the FFI's NET_MESHOS_LOG_* constants.
type MeshOsLogLevel int

const (
	LogTrace MeshOsLogLevel = MeshOsLogLevel(C.NET_MESHOS_LOG_TRACE)
	LogDebug MeshOsLogLevel = MeshOsLogLevel(C.NET_MESHOS_LOG_DEBUG)
	LogInfo  MeshOsLogLevel = MeshOsLogLevel(C.NET_MESHOS_LOG_INFO)
	LogWarn  MeshOsLogLevel = MeshOsLogLevel(C.NET_MESHOS_LOG_WARN)
	LogError MeshOsLogLevel = MeshOsLogLevel(C.NET_MESHOS_LOG_ERROR)
)

// ---------------------------------------------------------------------------
// SDK
// ---------------------------------------------------------------------------

// MeshOsConfig mirrors the substrate's MeshOsConfig knobs. All fields
// are optional — zero values pick the substrate defaults.
type MeshOsConfig struct {
	ThisNode            uint64
	TickIntervalMs      uint64
	EventQueueCapacity  int
	ActionQueueCapacity int
	ControlCapacity     int
}

// MeshOsDaemonSdk is the Go-side handle for the daemon-author SDK.
type MeshOsDaemonSdk struct {
	ptr *C.NetMeshOsSdk
}

// StartMeshOsDaemonSdk starts the MeshOS SDK with the substrate's
// LoggingDispatcher. Returns a handle the caller must Shutdown (or
// rely on the finalizer to free).
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
	if err := meshosStatusToError(status); err != nil {
		return nil, err
	}
	sdk := &MeshOsDaemonSdk{ptr: raw}
	runtime.SetFinalizer(sdk, func(s *MeshOsDaemonSdk) { s.Free() })
	return sdk, nil
}

// DroppedControlEvents — diagnostic counter for control events the
// router dropped across every registered daemon because the daemon's
// channel was full.
func (s *MeshOsDaemonSdk) DroppedControlEvents() uint64 {
	if s == nil || s.ptr == nil {
		return 0
	}
	return uint64(C.net_meshos_sdk_dropped_control_events(s.ptr))
}

// RegisterDaemon registers a daemon under the supplied 32-byte ed25519
// seed. The substrate uses an internal no-op daemon — user-supplied
// dispatch lands via RegisterDaemonWithCallbacks.
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
	if err := meshosStatusToError(status); err != nil {
		return nil, err
	}
	h := &MeshOsDaemonHandle{ptr: raw}
	runtime.SetFinalizer(h, func(h *MeshOsDaemonHandle) { h.Free() })
	return h, nil
}

// Shutdown drives a clean shutdown. Consumes the SDK — subsequent ops
// return ErrMeshOsAlreadyShutdown.
func (s *MeshOsDaemonSdk) Shutdown() error {
	if s == nil || s.ptr == nil {
		return ErrMeshOsInvalidArg
	}
	return meshosStatusToError(C.net_meshos_sdk_shutdown(s.ptr))
}

// Free releases the SDK handle. Idempotent.
func (s *MeshOsDaemonSdk) Free() {
	if s == nil || s.ptr == nil {
		return
	}
	C.net_meshos_sdk_free(s.ptr)
	s.ptr = nil
	runtime.SetFinalizer(s, nil)
}

// ---------------------------------------------------------------------------
// Handle
// ---------------------------------------------------------------------------

// MeshOsDaemonHandle is the Go-side handle for a registered daemon.
type MeshOsDaemonHandle struct {
	ptr          *C.NetMeshOsHandle
	daemonHandle cgo.Handle
	freeOnce     sync.Once
	controlOnce  sync.Once
	controlChan  chan MeshOsDaemonControl
	pumpStop     chan struct{}
	pumpDone     chan struct{}
}

// DaemonID returns the substrate identifier (origin hash). Stable
// across the handle's lifetime.
func (h *MeshOsDaemonHandle) DaemonID() uint64 {
	if h == nil || h.ptr == nil {
		return 0
	}
	return uint64(C.net_meshos_handle_daemon_id(h.ptr))
}

// DaemonName returns the daemon's registered name.
func (h *MeshOsDaemonHandle) DaemonName() string {
	if h == nil || h.ptr == nil {
		return ""
	}
	if cstr := C.net_meshos_handle_daemon_name(h.ptr); cstr != nil {
		return C.GoString(cstr)
	}
	return ""
}

// TryNextControl returns the next control event without blocking, or
// ControlNone if empty.
func (h *MeshOsDaemonHandle) TryNextControl() (MeshOsDaemonControl, error) {
	if h == nil || h.ptr == nil {
		return MeshOsDaemonControl{}, ErrMeshOsInvalidArg
	}
	var out C.NetMeshOsDaemonControl
	if err := meshosStatusToError(C.net_meshos_try_next_control(h.ptr, &out)); err != nil {
		return MeshOsDaemonControl{}, err
	}
	return meshosControlFromC(out), nil
}

// NextControl blocks until the next control event, the runtime shuts
// down, or timeoutMs elapses. Pass 0 for unbounded wait.
func (h *MeshOsDaemonHandle) NextControl(timeoutMs uint64) (MeshOsDaemonControl, error) {
	if h == nil || h.ptr == nil {
		return MeshOsDaemonControl{}, ErrMeshOsInvalidArg
	}
	var out C.NetMeshOsDaemonControl
	if err := meshosStatusToError(C.net_meshos_next_control(h.ptr, C.uint64_t(timeoutMs), &out)); err != nil {
		return MeshOsDaemonControl{}, err
	}
	return meshosControlFromC(out), nil
}

// PublishLog publishes a log line tagged with this daemon's id.
// Non-blocking — surfaces MeshOsSdkError(kind: "queue_full" |
// "loop_closed") when the substrate's log ring is saturated.
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
	return meshosStatusToError(C.net_meshos_publish_log(h.ptr, C.int(level), msgPtr, msgLen))
}

// GracefulShutdown sends Shutdown { gracePeriodMs }, parks for graceMs,
// then unregisters. Pass 0 for the substrate default (5 seconds).
// Consumes the inner handle.
func (h *MeshOsDaemonHandle) GracefulShutdown(graceMs uint64) error {
	if h == nil || h.ptr == nil {
		return ErrMeshOsInvalidArg
	}
	return meshosStatusToError(C.net_meshos_graceful_shutdown(h.ptr, C.uint64_t(graceMs)))
}

// Free releases the handle. Safe to call concurrently and repeatedly.
// Stops the ControlEvents pump goroutine before freeing.
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

// ---------------------------------------------------------------------------
// MeshOsDaemon interface + DefaultDaemon
// ---------------------------------------------------------------------------

// MeshOsCausalEvent is the value handed to a daemon's Process callback.
type MeshOsCausalEvent struct {
	OriginHash uint64
	Sequence   uint64
	Payload    []byte
}

// MeshOsDaemonHealthKind discriminates a daemon's reported health.
type MeshOsDaemonHealthKind int

const (
	HealthHealthy   MeshOsDaemonHealthKind = MeshOsDaemonHealthKind(C.NET_MESHOS_HEALTH_HEALTHY)
	HealthDegraded  MeshOsDaemonHealthKind = MeshOsDaemonHealthKind(C.NET_MESHOS_HEALTH_DEGRADED)
	HealthUnhealthy MeshOsDaemonHealthKind = MeshOsDaemonHealthKind(C.NET_MESHOS_HEALTH_UNHEALTHY)
)

// MeshOsDaemon is the Go-side daemon contract. Embed MeshOsDefaultDaemon
// to inherit no-op defaults for everything except Name + Process.
//
// Callbacks fire from tokio worker threads — protect shared state with
// sync.Mutex / atomics as needed.
type MeshOsDaemon interface {
	Name() string
	Process(event MeshOsCausalEvent) ([][]byte, error)
	Snapshot() ([]byte, bool)
	Restore(state []byte) error
	OnControl(event MeshOsDaemonControl)
	Health() MeshOsDaemonHealthKind
	Saturation() float32
}

// MeshOsDefaultDaemon — zero-method base supplying no-op defaults for
// every method except Name and Process.
type MeshOsDefaultDaemon struct{}

func (MeshOsDefaultDaemon) Snapshot() ([]byte, bool)        { return nil, false }
func (MeshOsDefaultDaemon) Restore(_ []byte) error          { return nil }
func (MeshOsDefaultDaemon) OnControl(_ MeshOsDaemonControl) {}
func (MeshOsDefaultDaemon) Health() MeshOsDaemonHealthKind  { return HealthHealthy }
func (MeshOsDefaultDaemon) Saturation() float32             { return 0 }

// RegisterDaemonWithCallbacks registers a user-supplied MeshOsDaemon
// under the given 32-byte ed25519 seed. The wrapper allocates a
// cgo.Handle for the daemon, builds the vtable with pointers to
// //export'd trampolines, and hands ownership of the cgo.Handle to
// the returned MeshOsDaemonHandle.
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
	vt := C.topGoMeshOsBuildVtable()

	var raw *C.NetMeshOsHandle
	var namePtr *C.char
	var nameLen C.size_t
	if len(name) > 0 {
		nameBytes := []byte(name)
		namePtr = (*C.char)(unsafe.Pointer(&nameBytes[0]))
		nameLen = C.size_t(len(nameBytes))
	}
	seedPtr := (*C.uint8_t)(unsafe.Pointer(&seed[0]))
	userCtx := unsafe.Pointer(uintptr(cgoHandle))
	status := C.net_meshos_register_daemon_with_vtable(
		s.ptr, namePtr, nameLen, seedPtr, &vt, userCtx, &raw,
	)
	if err := meshosStatusToError(status); err != nil {
		cgoHandle.Delete()
		return nil, err
	}
	h := &MeshOsDaemonHandle{ptr: raw, daemonHandle: cgoHandle}
	runtime.SetFinalizer(h, func(h *MeshOsDaemonHandle) { h.Free() })
	return h, nil
}

// ---------------------------------------------------------------------------
// //export trampolines — dispatch through cgo.Handle into the user's
// MeshOsDaemon implementation.
//
// Symbol names are prefixed `topGoMeshOs*` so they don't collide with
// the reference impl's `goMeshOs*` exports if both packages are linked
// into the same process.
// ---------------------------------------------------------------------------

func meshosHandleFromCtx(userCtx unsafe.Pointer) MeshOsDaemon {
	h := cgo.Handle(uintptr(userCtx))
	v := h.Value()
	if d, ok := v.(MeshOsDaemon); ok {
		return d
	}
	return nil
}

//export topGoMeshOsProcessTrampoline
func topGoMeshOsProcessTrampoline(
	userCtx unsafe.Pointer,
	emitCtx *C.NetMeshOsProcessEmitCtx,
	originHash C.uint64_t,
	sequence C.uint64_t,
	payloadPtr *C.uint8_t,
	payloadLen C.size_t,
) C.int {
	d := meshosHandleFromCtx(userCtx)
	if d == nil {
		return C.int(1)
	}
	var payload []byte
	if payloadLen > 0 && payloadPtr != nil {
		payload = C.GoBytes(unsafe.Pointer(payloadPtr), C.int(payloadLen))
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

//export topGoMeshOsSnapshotTrampoline
func topGoMeshOsSnapshotTrampoline(
	userCtx unsafe.Pointer,
	emitCtx *C.NetMeshOsSnapshotEmitCtx,
) {
	d := meshosHandleFromCtx(userCtx)
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

//export topGoMeshOsRestoreTrampoline
func topGoMeshOsRestoreTrampoline(
	userCtx unsafe.Pointer,
	payloadPtr *C.uint8_t,
	payloadLen C.size_t,
) C.int {
	d := meshosHandleFromCtx(userCtx)
	if d == nil {
		return C.int(1)
	}
	var state []byte
	if payloadLen > 0 && payloadPtr != nil {
		state = C.GoBytes(unsafe.Pointer(payloadPtr), C.int(payloadLen))
	}
	if err := d.Restore(state); err != nil {
		return C.int(1)
	}
	return C.int(0)
}

//export topGoMeshOsOnControlTrampoline
func topGoMeshOsOnControlTrampoline(
	userCtx unsafe.Pointer,
	kind C.int,
	gracePeriodMs C.uint64_t,
	level C.float,
) {
	d := meshosHandleFromCtx(userCtx)
	if d == nil {
		return
	}
	d.OnControl(MeshOsDaemonControl{
		Kind:          DaemonControlKind(kind),
		GracePeriodMs: uint64(gracePeriodMs),
		Level:         float32(level),
	})
}

//export topGoMeshOsHealthTrampoline
func topGoMeshOsHealthTrampoline(userCtx unsafe.Pointer) C.int {
	d := meshosHandleFromCtx(userCtx)
	if d == nil {
		return C.int(C.NET_MESHOS_HEALTH_HEALTHY)
	}
	return C.int(d.Health())
}

//export topGoMeshOsSaturationTrampoline
func topGoMeshOsSaturationTrampoline(userCtx unsafe.Pointer) C.float {
	d := meshosHandleFromCtx(userCtx)
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

// ---------------------------------------------------------------------------
// Metadata + capability advertisement
// ---------------------------------------------------------------------------

type MeshOsMaintenanceStateView struct {
	Kind                string  `json:"kind"`
	SinceMs             uint64  `json:"since_ms,omitempty"`
	DeadlineRemainingMs *uint64 `json:"deadline_remaining_ms,omitempty"`
	Reason              string  `json:"reason,omitempty"`
}

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

type MeshOsMetadataView struct {
	NodeID           uint64                     `json:"node_id"`
	DaemonID         uint64                     `json:"daemon_id"`
	DaemonName       string                     `json:"daemon_name"`
	MaintenanceState MeshOsMaintenanceStateView `json:"maintenance_state"`
	Peers            []MeshOsPeerSnapshot       `json:"peers"`
}

// Metadata returns a fresh snapshot of the daemon's MetadataView.
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

// RefreshMetadata re-pulls from the runtime's latest snapshot.
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

// PublishCapabilities advertises a fresh capability tag set. Pass an
// empty slice to clear the advert. Note: the substrate currently
// returns Ok(()) without committing to the capability chain — every
// binding ships the same stub semantics until the chain-commit path
// lands.
func (h *MeshOsDaemonHandle) PublishCapabilities(tags []string) error {
	if h == nil || h.ptr == nil {
		return ErrMeshOsInvalidArg
	}
	if len(tags) == 0 {
		status := C.net_meshos_publish_capabilities(h.ptr, nil, 0)
		return meshosStatusToError(status)
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
	return meshosStatusToError(status)
}

// ---------------------------------------------------------------------------
// ControlEvents channel + context.Context plumbing
// ---------------------------------------------------------------------------

// ControlEvents returns a buffered channel that delivers control
// events as the supervisor emits them. The channel closes when ctx is
// cancelled or the handle is freed. First call spawns the pump
// goroutine; subsequent calls return the same channel.
//
// Internal cadence: 50ms poll between ctx.Done() checks. For tighter
// latency requirements, use TryNextControl / NextControl directly.
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
			return
		default:
		}
		ev, err := h.NextControl(50)
		if err != nil {
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

// MetadataContext is the context-aware variant of Metadata. Cancellation
// aborts before the JSON decode; the FFI fetch itself is fast.
func (h *MeshOsDaemonHandle) MetadataContext(ctx context.Context) (*MeshOsMetadataView, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	return h.Metadata()
}

// GracefulShutdownContext is the context-aware variant of
// GracefulShutdown. The context's deadline overrides the caller-supplied
// grace if it expires sooner.
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
