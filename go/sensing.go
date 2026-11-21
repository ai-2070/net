// Capability sensing over the C ABI (SENSING_OLB_COMPLETION_PLAN B2).
//
// Every behaviour is the Rust SDK's, forwarded through `libnet`; this file is
// marshaling, exactly like org.go:
//
//   - Provider: ProvideReadiness serves readiness as a stored VALUE. Rust signs
//     the current value on every attestation beat, so no Go callback runs per
//     beat.
//   - Consumer: WatchSensing observes this node's own organization's
//     authorized providers of one capability. Changed(ctx) blocks until
//     something that can change the answer moved. Cancelling ctx ends the
//     wait without consuming an edge and WITHOUT closing the watch. Close from
//     another goroutine ends a pending Changed with a SensingError of kind
//     "watch_closed".
//   - Errors: a refusal is a *SensingError carrying Rust's own kind and
//     terminal flag, read from the wire string through the Rust classifier,
//     never from a Go table.
//
// Enabling sensing is construction state:
//
//	node, err := net.NewMeshNode(net.MeshConfig{
//		BindAddr: addr, PskHex: psk, IdentitySeedHex: seed,
//		EnableSensing:          true,
//		SensingIncarnationPath: "/var/lib/app/sensing.incarnation",
//	})
package net

/*
#cgo LDFLAGS: -L${SRCDIR}/../net/crates/net/target/release -lnet
#include <stdint.h>
#include <stdlib.h>

// Mirror include/net_sensing.h. The Rust org-ffi crate is the source of
// truth (its header mirror test), and sensing_test.go pins this preamble
// against the header.
#define NET_SENSING_OK                 0
#define NET_SENSING_ERR_NULL          -1
#define NET_SENSING_ERR_INVALID_UTF8  -2
#define NET_SENSING_ERR_SENSING       -3
#define NET_SENSING_TIMEOUT           -4
#define NET_SENSING_ERR_RPC           -5
#define NET_SENSING_ERR_INCARNATION   -6
#define NET_SENSING_ERR_INVALID_ARG   -7
#define NET_SENSING_STATE_READY        0
#define NET_SENSING_STATE_NOT_READY    1
#define NET_SENSING_STATE_UNEVALUABLE  2

typedef struct NetSensingProvider NetSensingProvider;
typedef struct NetSensingWatch    NetSensingWatch;
typedef struct NetSensingSnapshot NetSensingSnapshot;

extern uint32_t net_sensing_abi_version(void);
extern int net_sensing_next_incarnation(const char* path_ptr, size_t path_len,
                                        uint64_t* out_incarnation, char** out_err);
extern int net_sensing_error_class(const char* wire_ptr, size_t wire_len,
                                   char** out_kind, int* out_terminal);
extern int  net_sensing_provide(void* mesh_arc, const char* capability_ptr, size_t capability_len,
                                int state, int64_t estimated_start_ms, uint32_t reason,
                                NetSensingProvider** out_provider, char** out_err);
extern int  net_sensing_provider_set(const NetSensingProvider* provider, int state,
                                     int64_t estimated_start_ms, uint32_t reason, int* out_moved);
extern int  net_sensing_provider_close(const NetSensingProvider* provider);
extern void net_sensing_provider_free(NetSensingProvider** provider);
extern int  net_sensing_watch(void* mesh_arc, const char* capability_ptr, size_t capability_len,
                              int64_t within_ms, int64_t start_within_ms,
                              NetSensingWatch** out_watch, char** out_err);
extern int  net_sensing_watch_snapshot(const NetSensingWatch* watch,
                                       NetSensingSnapshot** out_snapshot, char** out_err);
extern int  net_sensing_watch_changed(const NetSensingWatch* watch, int64_t timeout_ms, char** out_err);
extern int  net_sensing_watch_close(const NetSensingWatch* watch);
extern void net_sensing_watch_free(NetSensingWatch** watch);
extern int  net_sensing_snapshot_json(const NetSensingSnapshot* snapshot, char** out_json);
extern void net_sensing_snapshot_free(NetSensingSnapshot** snapshot);
extern int  net_sensing_call_service(void* mesh_arc, const char* service_ptr, size_t service_len,
                                     const uint8_t* req_ptr, size_t req_len,
                                     const NetSensingSnapshot* snapshot, uint64_t timeout_ms,
                                     uint8_t** out_resp, size_t* out_len, char** out_err);
extern void net_org_free_cstring(char* s);
extern void net_org_response_free(uint8_t* ptr, size_t len);
*/
import "C"

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"runtime"
	"sync"
	"time"
	"unsafe"
)

// sensingCConstants is the cgo preamble's numeric contract, exposed so
// sensing_test.go can pin it against include/net_sensing.h (test files
// cannot use cgo).
var sensingCConstants = map[string]int{
	"NET_SENSING_OK":                int(C.NET_SENSING_OK),
	"NET_SENSING_ERR_NULL":          int(C.NET_SENSING_ERR_NULL),
	"NET_SENSING_ERR_INVALID_UTF8":  int(C.NET_SENSING_ERR_INVALID_UTF8),
	"NET_SENSING_ERR_SENSING":       int(C.NET_SENSING_ERR_SENSING),
	"NET_SENSING_TIMEOUT":           int(C.NET_SENSING_TIMEOUT),
	"NET_SENSING_ERR_RPC":           int(C.NET_SENSING_ERR_RPC),
	"NET_SENSING_ERR_INCARNATION":   int(C.NET_SENSING_ERR_INCARNATION),
	"NET_SENSING_ERR_INVALID_ARG":   int(C.NET_SENSING_ERR_INVALID_ARG),
	"NET_SENSING_STATE_READY":       int(C.NET_SENSING_STATE_READY),
	"NET_SENSING_STATE_NOT_READY":   int(C.NET_SENSING_STATE_NOT_READY),
	"NET_SENSING_STATE_UNEVALUABLE": int(C.NET_SENSING_STATE_UNEVALUABLE),
}

// SensingError is a capability-sensing refusal, classified by Rust.
type SensingError struct {
	// Kind is the stable classification token, e.g. "watch_closed".
	Kind string
	// Terminal reports whether the refusal is permanent for the node or
	// handle that refused it.
	Terminal bool
	// Wire is the Rust SDK's `sensing:<kind>: <message>` string.
	Wire string
}

func (e *SensingError) Error() string { return e.Wire }

// ClassifySensingError returns Rust's classification of a sensing wire
// string, or nil when it is not one this build knows.
func ClassifySensingError(wire string) *SensingError {
	cWire := C.CString(wire)
	defer C.free(unsafe.Pointer(cWire))
	var kind *C.char
	var terminal C.int
	if C.net_sensing_error_class(cWire, C.size_t(len(wire)), &kind, &terminal) != C.NET_SENSING_OK {
		return nil
	}
	defer C.net_org_free_cstring(kind)
	return &SensingError{Kind: C.GoString(kind), Terminal: terminal != 0, Wire: wire}
}

// sensingErrorFromCall turns a (code, out_err) pair into a Go error.
func sensingErrorFromCall(code C.int, errPtr *C.char) error {
	if code == C.NET_SENSING_OK {
		return nil
	}
	message := ""
	if errPtr != nil {
		message = C.GoString(errPtr)
		C.net_org_free_cstring(errPtr)
	}
	switch code {
	case C.NET_SENSING_ERR_SENSING:
		if classified := ClassifySensingError(message); classified != nil {
			return classified
		}
		return fmt.Errorf("net: unclassified sensing refusal: %s", message)
	case C.NET_SENSING_ERR_NULL:
		return errors.New("net: sensing: invalid handle")
	case C.NET_SENSING_ERR_INVALID_UTF8:
		return errors.New("net: sensing: argument is not UTF-8")
	case C.NET_SENSING_ERR_INVALID_ARG:
		return errors.New("net: sensing: argument out of range")
	case C.NET_SENSING_ERR_INCARNATION:
		return fmt.Errorf("net: %s", message)
	case C.NET_SENSING_ERR_RPC:
		return fmt.Errorf("net: sensed call failed: %s", message)
	default:
		return fmt.Errorf("net: sensing: unexpected code %d %s", int(code), message)
	}
}

// SensingNextIncarnation reads, increments and durably persists the sensing
// boot counter at path, returning the new incarnation. NewMeshNode calls it
// for you when MeshConfig.SensingIncarnationPath is set.
func SensingNextIncarnation(path string) (uint64, error) {
	cPath := C.CString(path)
	defer C.free(unsafe.Pointer(cPath))
	var out C.uint64_t
	var errPtr *C.char
	code := C.net_sensing_next_incarnation(cPath, C.size_t(len(path)), &out, &errPtr)
	if err := sensingErrorFromCall(code, errPtr); err != nil {
		return 0, err
	}
	return uint64(out), nil
}

// ReadinessState is a provider's readiness state.
type ReadinessState int

const (
	// ReadinessReady: the provider can take work.
	ReadinessReady ReadinessState = ReadinessState(C.NET_SENSING_STATE_READY)
	// ReadinessNotReady: the provider cannot, with a diagnostic Reason.
	ReadinessNotReady ReadinessState = ReadinessState(C.NET_SENSING_STATE_NOT_READY)
	// ReadinessUnevaluable: the provider cannot currently say (read as
	// Unknown, which never prunes).
	ReadinessUnevaluable ReadinessState = ReadinessState(C.NET_SENSING_STATE_UNEVALUABLE)
)

// ReadinessValue is a provider's readiness as a plain value.
type ReadinessValue struct {
	State ReadinessState
	// EstimatedStart is the provider's estimate of time-to-start (Ready
	// only). Nil means no estimate.
	EstimatedStart *time.Duration
	// Reason is a provider-defined diagnostic code (NotReady only).
	Reason uint16
}

func (v ReadinessValue) cArgs() (C.int, C.int64_t, C.uint32_t) {
	estimated := C.int64_t(-1)
	if v.EstimatedStart != nil {
		estimated = C.int64_t(v.EstimatedStart.Milliseconds())
	}
	return C.int(v.State), estimated, C.uint32_t(v.Reason)
}

// ReadinessProvider owns one value-backed readiness registration.
type ReadinessProvider struct {
	mu sync.Mutex
	h  *C.NetSensingProvider
}

// ProvideReadiness serves readiness for capability from a stored value. The
// node needs EnableSensing, a seeded identity, and a sensing incarnation.
func ProvideReadiness(node *MeshNode, capability string, value ReadinessValue) (*ReadinessProvider, error) {
	if node == nil {
		return nil, errors.New("net.ProvideReadiness: node must be non-nil")
	}
	arc := node.arcClonePtr()
	if arc == nil {
		return nil, ErrShuttingDown
	}
	cCap := C.CString(capability)
	defer C.free(unsafe.Pointer(cCap))
	state, estimated, reason := value.cArgs()
	var out *C.NetSensingProvider
	var errPtr *C.char
	code := C.net_sensing_provide(arc, cCap, C.size_t(len(capability)), state, estimated, reason, &out, &errPtr)
	if err := sensingErrorFromCall(code, errPtr); err != nil {
		return nil, err
	}
	p := &ReadinessProvider{h: out}
	runtime.SetFinalizer(p, (*ReadinessProvider).free)
	return p, nil
}

// Set publishes a new value, then wakes every live observation path. It
// reports whether an observation moved. A Set after Close does nothing.
func (p *ReadinessProvider) Set(value ReadinessValue) (bool, error) {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.h == nil {
		return false, nil
	}
	state, estimated, reason := value.cArgs()
	var moved C.int
	code := C.net_sensing_provider_set(p.h, state, estimated, reason, &moved)
	if err := sensingErrorFromCall(code, nil); err != nil {
		return false, err
	}
	return moved != 0, nil
}

// Close stops serving readiness. It reports whether this call removed the
// registration. The native handle is freed by the finalizer.
func (p *ReadinessProvider) Close() bool {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.h == nil {
		return false
	}
	return C.net_sensing_provider_close(p.h) != 0
}

func (p *ReadinessProvider) free() {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.h != nil {
		C.net_sensing_provider_free(&p.h)
	}
}

// SensedProvider is one authorized provider's row in a snapshot.
type SensedProvider struct {
	NodeID uint64 `json:"node_id"`
	// Readiness is "ready", "not_ready" or "unknown".
	Readiness string `json:"readiness"`
	// Viability is "viable", "potential" or "not_viable".
	Viability        string   `json:"viability"`
	EstimatedStartMs *float64 `json:"estimated_start_ms"`
	RouteEstimateMs  *float64 `json:"route_estimate_ms"`
}

// SensingSnapshot is one immutable readiness capture. Release it when done
// (a finalizer backstops a forgotten Release).
type SensingSnapshot struct {
	Capability string           `json:"capability"`
	Providers  []SensedProvider `json:"providers"`
	// Ranked lists the viable providers in sensed rank order.
	Ranked []uint64 `json:"ranked"`
	// Preferred is the provider to try first, or nil when nothing is viable.
	Preferred *uint64 `json:"preferred"`

	mu sync.Mutex
	h  *C.NetSensingSnapshot
}

// Release frees the native snapshot.
func (s *SensingSnapshot) Release() {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.h != nil {
		C.net_sensing_snapshot_free(&s.h)
	}
}

// SensingQueryOptions are a watch's two optional bounds.
type SensingQueryOptions struct {
	// Within is the consumer-local end-to-end budget.
	Within *time.Duration
	// StartWithin is the provider-start predicate that rides the signed
	// interest.
	StartWithin *time.Duration
}

func optionalMs(d *time.Duration) C.int64_t {
	if d == nil {
		return -1
	}
	return C.int64_t(d.Milliseconds())
}

// SensingWatch owns one observation of a capability.
type SensingWatch struct {
	// mu: read-held across every cgo call on h, write-held to free it. Close
	// signals with only the read lock, so it can end a Changed blocked under
	// another read lock.
	mu sync.RWMutex
	h  *C.NetSensingWatch
}

// WatchSensing observes this node's own organization's authorized providers
// of capability. The node needs EnableSensing and an installed organization
// authority.
func WatchSensing(node *MeshNode, capability string, opts SensingQueryOptions) (*SensingWatch, error) {
	if node == nil {
		return nil, errors.New("net.WatchSensing: node must be non-nil")
	}
	arc := node.arcClonePtr()
	if arc == nil {
		return nil, ErrShuttingDown
	}
	cCap := C.CString(capability)
	defer C.free(unsafe.Pointer(cCap))
	var out *C.NetSensingWatch
	var errPtr *C.char
	code := C.net_sensing_watch(arc, cCap, C.size_t(len(capability)),
		optionalMs(opts.Within), optionalMs(opts.StartWithin), &out, &errPtr)
	if err := sensingErrorFromCall(code, errPtr); err != nil {
		return nil, err
	}
	w := &SensingWatch{h: out}
	runtime.SetFinalizer(w, (*SensingWatch).free)
	return w, nil
}

// errSensingHandleFreed is reachable only after the finalizer ran, i.e. never
// through a live reference: Close keeps the handle so that every later call
// reaches Rust and gets Rust's own `watch_closed` refusal.
var errSensingHandleFreed = errors.New("net: sensing: handle freed")

// Snapshot takes a readiness capture.
func (w *SensingWatch) Snapshot() (*SensingSnapshot, error) {
	w.mu.RLock()
	defer w.mu.RUnlock()
	if w.h == nil {
		return nil, errSensingHandleFreed
	}
	var handle *C.NetSensingSnapshot
	var errPtr *C.char
	code := C.net_sensing_watch_snapshot(w.h, &handle, &errPtr)
	if err := sensingErrorFromCall(code, errPtr); err != nil {
		return nil, err
	}
	var js *C.char
	if C.net_sensing_snapshot_json(handle, &js) != C.NET_SENSING_OK {
		C.net_sensing_snapshot_free(&handle)
		return nil, errors.New("net: sensing: snapshot could not be rendered")
	}
	text := C.GoString(js)
	C.net_org_free_cstring(js)
	snap := &SensingSnapshot{}
	if err := json.Unmarshal([]byte(text), snap); err != nil {
		C.net_sensing_snapshot_free(&handle)
		return nil, fmt.Errorf("net: sensing: decode snapshot: %w", err)
	}
	snap.h = handle
	runtime.SetFinalizer(snap, (*SensingSnapshot).Release)
	return snap, nil
}

// changedSlice bounds each native wait so ctx cancellation is observed. A
// timed-out slice consumes no edge (the Rust wait is cancel-safe).
const changedSlice = 250 * time.Millisecond

// Changed blocks until something that can change the answer moved. It
// returns ctx.Err() if ctx ends first, without consuming an edge and without
// closing the watch. It returns a *SensingError of kind "watch_closed" if the
// watch is closed, including by Close from another goroutine.
func (w *SensingWatch) Changed(ctx context.Context) error {
	for {
		if err := ctx.Err(); err != nil {
			return err
		}
		slice := changedSlice
		if deadline, ok := ctx.Deadline(); ok {
			if remaining := time.Until(deadline); remaining < slice {
				slice = remaining
			}
		}
		if slice < 0 {
			slice = 0
		}
		code, err := w.changedOnce(slice)
		if code == C.NET_SENSING_TIMEOUT {
			continue
		}
		return err
	}
}

func (w *SensingWatch) changedOnce(timeout time.Duration) (C.int, error) {
	w.mu.RLock()
	defer w.mu.RUnlock()
	if w.h == nil {
		return C.NET_SENSING_ERR_SENSING, errSensingHandleFreed
	}
	var errPtr *C.char
	code := C.net_sensing_watch_changed(w.h, C.int64_t(timeout.Milliseconds()), &errPtr)
	if code == C.NET_SENSING_TIMEOUT {
		return code, nil
	}
	return code, sensingErrorFromCall(code, errPtr)
}

// Close stops observing. A Changed pending on another goroutine returns a
// "watch_closed" SensingError, and every later call returns Rust's own
// refusal. It reports whether this call closed the watch. The native handle
// is freed by the finalizer once the watch is unreachable.
func (w *SensingWatch) Close() bool {
	w.mu.RLock()
	defer w.mu.RUnlock()
	if w.h == nil {
		return false
	}
	return C.net_sensing_watch_close(w.h) != 0
}

func (w *SensingWatch) free() {
	w.mu.Lock()
	defer w.mu.Unlock()
	if w.h != nil {
		C.net_sensing_watch_free(&w.h)
	}
}

// CallServiceSensed is the sensed service call: the candidates are
// authorized exactly as an unsensed call authorizes them, the target is chosen
// from snapshot (a capture of "nrpc:<service>"; any other capability is no
// evidence), and the target is invoked once and never retried. timeout == 0
// uses the default call deadline.
func CallServiceSensed(node *MeshNode, service string, request []byte, snapshot *SensingSnapshot, timeout time.Duration) ([]byte, error) {
	if node == nil || snapshot == nil {
		return nil, errors.New("net.CallServiceSensed: node and snapshot must be non-nil")
	}
	snapshot.mu.Lock()
	defer snapshot.mu.Unlock()
	if snapshot.h == nil {
		return nil, errors.New("net.CallServiceSensed: snapshot was released")
	}
	arc := node.arcClonePtr()
	if arc == nil {
		return nil, ErrShuttingDown
	}
	cSvc := C.CString(service)
	defer C.free(unsafe.Pointer(cSvc))
	var reqPtr *C.uint8_t
	if len(request) > 0 {
		reqPtr = (*C.uint8_t)(C.CBytes(request))
		defer C.free(unsafe.Pointer(reqPtr))
	}
	var respPtr *C.uint8_t
	var respLen C.size_t
	var errPtr *C.char
	code := C.net_sensing_call_service(arc, cSvc, C.size_t(len(service)), reqPtr, C.size_t(len(request)),
		snapshot.h, C.uint64_t(timeout.Milliseconds()), &respPtr, &respLen, &errPtr)
	if err := sensingErrorFromCall(code, errPtr); err != nil {
		return nil, err
	}
	defer C.net_org_response_free(respPtr, respLen)
	return copyCBuf(unsafe.Pointer(respPtr), uint64(respLen))
}
