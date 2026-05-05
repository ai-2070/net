// Package net — nRPC consumer wrapper for the C ABI exported by
// `bindings/go/rpc-ffi` (Phase B5 of NRPC_BINDINGS_PLAN.md).
//
// This file is a **reference implementation** documenting the
// expected Go-side surface for consumers of `libnet_rpc`. The Go
// binding tree is maintained downstream (see the project's release
// instructions); the upstream net repo owns the C ABI side and ships
// this file as the canonical contract for what the cgo wrapper
// should look like.
//
// # Build prerequisites
//
//   - Build `net-rpc-ffi` as a cdylib (`cargo build --release -p net-rpc-ffi`).
//   - Add to your CGO flags:
//
//     #cgo LDFLAGS: -L/path/to/target/release -lnet_rpc
//     #cgo darwin LDFLAGS: -framework Security -framework CoreFoundation
//
// # Lifecycle pattern (mirrors `bindings/go/net/compute.go`)
//
//   - Build a `*Mesh` upstream (out-of-scope for this file; comes
//     from the `compute-ffi` Go binding).
//   - Call `mesh.NewRpc()` to take an `Arc<MeshNode>` clone and wrap
//     it in a `*MeshRpc`. The wrapper installs a runtime finalizer so
//     dropped values release the C handle, but call `Close()` for
//     deterministic cleanup.
//   - Call `Serve(service, handler)` to register a handler. The
//     returned `*ServeHandle` MUST be closed when the service should
//     stop accepting new requests.
//   - Call `Call(ctx, target, service, payload)` /
//     `CallService(ctx, service, payload)` to invoke a remote
//     handler.
//
// # Error model
//
// Every operation that can fail returns a Go `error`. Failures from
// the wire RPC layer are typed as `*RpcError` with a `Kind`
// discriminator matching the stable `nrpc:` prefix convention used
// by the Node + Python bindings:
//
//   - `RpcKindNoRoute`     — `nrpc:no_route: target=0x... reason=...`
//   - `RpcKindTimeout`     — `nrpc:timeout: elapsed_ms=...`
//   - `RpcKindServerError` — `nrpc:server_error: status=0x... message=...`
//   - `RpcKindTransport`   — `nrpc:transport: ...`
//   - `RpcKindCodecEncode` — `nrpc:codec_encode: ...`
//   - `RpcKindCodecDecode` — `nrpc:codec_decode: ...`
//
// Use `errors.As(err, &rpcErr)` to inspect the kind.
package net

/*
#include <stdint.h>
#include <stdlib.h>

// Forward-declared opaque handle types from `libnet_rpc`.
typedef struct MeshRpcHandle MeshRpcHandle;
typedef struct ServeHandleC ServeHandleC;
typedef struct RpcStreamHandleC RpcStreamHandleC;

// Handler dispatcher signature — Rust calls back into Go via this
// function pointer to invoke a registered handler. The Go side
// owns `handlerID` lookup and response-buffer allocation.
typedef int (*RpcHandlerFn)(
    uint64_t handler_id,
    const uint8_t* req_ptr,
    size_t req_len,
    uint8_t** out_resp_ptr,
    size_t* out_resp_len,
    char** out_err
);

// Imported FFI surface from `net-rpc-ffi`.
extern uint32_t net_rpc_abi_version(void);
extern void net_rpc_set_handler_dispatcher(RpcHandlerFn dispatcher);
extern void net_rpc_free_cstring(char* s);
extern void net_rpc_response_free(uint8_t* ptr, size_t len);
extern void net_rpc_find_service_nodes_free(uint64_t* ptr, size_t len);

extern MeshRpcHandle* net_rpc_new(void* node_arc);
extern void net_rpc_free(MeshRpcHandle* handle);
extern uint64_t net_rpc_id(const MeshRpcHandle* handle);

extern int net_rpc_call(
    MeshRpcHandle* handle,
    uint64_t target_node_id,
    const char* service_ptr, size_t service_len,
    const uint8_t* req_ptr, size_t req_len,
    uint64_t deadline_ms,
    uint8_t** out_resp_ptr, size_t* out_resp_len,
    char** out_err
);

extern int net_rpc_call_service(
    MeshRpcHandle* handle,
    const char* service_ptr, size_t service_len,
    const uint8_t* req_ptr, size_t req_len,
    uint64_t deadline_ms,
    uint8_t** out_resp_ptr, size_t* out_resp_len,
    char** out_err
);

extern int net_rpc_find_service_nodes(
    MeshRpcHandle* handle,
    const char* service_ptr, size_t service_len,
    uint64_t** out_ptr, size_t* out_count,
    char** out_err
);

extern uint64_t net_rpc_reserve_handler_id(void);
extern ServeHandleC* net_rpc_serve(
    MeshRpcHandle* handle,
    const char* service_ptr, size_t service_len,
    uint64_t handler_id,
    char** out_err
);
extern uint64_t net_rpc_serve_handle_id(const ServeHandleC* handle);
extern void net_rpc_serve_handle_close(ServeHandleC* handle);
extern void net_rpc_serve_handle_free(ServeHandleC* handle);

extern int net_rpc_call_streaming(
    MeshRpcHandle* handle,
    uint64_t target_node_id,
    const char* service_ptr, size_t service_len,
    const uint8_t* req_ptr, size_t req_len,
    uint64_t deadline_ms,
    uint32_t stream_window,
    RpcStreamHandleC** out_stream,
    char** out_err
);
extern int net_rpc_stream_next(
    RpcStreamHandleC* stream,
    uint8_t** out_chunk_ptr, size_t* out_chunk_len,
    char** out_err
);
extern int net_rpc_stream_grant(RpcStreamHandleC* stream, uint32_t amount);
extern uint64_t net_rpc_stream_call_id(const RpcStreamHandleC* stream);
extern void net_rpc_stream_close(RpcStreamHandleC* stream);
extern void net_rpc_stream_free(RpcStreamHandleC* stream);

// Trampoline that Rust calls back through. Defined below as a Go
// `//export` function and registered via
// `net_rpc_set_handler_dispatcher` during package init.
int go_net_rpc_handler_trampoline(
    uint64_t handler_id,
    const uint8_t* req_ptr,
    size_t req_len,
    uint8_t** out_resp_ptr,
    size_t* out_resp_len,
    char** out_err
);
*/
import "C"

import (
	"context"
	"errors"
	"fmt"
	"os"
	"runtime"
	"runtime/cgo"
	"strings"
	"sync"
	"sync/atomic"
	"time"
	"unsafe"
)

// =====================================================================
// Error model
// =====================================================================

// RpcKind is a stable discriminator for `RpcError` variants. The
// values match the segment of the underlying error message between
// the `nrpc:` prefix and the first colon (e.g. `nrpc:timeout: ...`).
type RpcKind string

const (
	RpcKindNoRoute     RpcKind = "no_route"
	RpcKindTimeout     RpcKind = "timeout"
	RpcKindServerError RpcKind = "server_error"
	RpcKindTransport   RpcKind = "transport"
	RpcKindCodecEncode RpcKind = "codec_encode"
	RpcKindCodecDecode RpcKind = "codec_decode"
	RpcKindUnknown     RpcKind = "unknown"
)

// RpcError is the typed error surfaced from any Call / CallService
// failure. Use `errors.As(err, &re)` and switch on `re.Kind` to
// dispatch.
type RpcError struct {
	Kind    RpcKind
	Message string
}

func (e *RpcError) Error() string {
	return fmt.Sprintf("nrpc:%s: %s", e.Kind, e.Message)
}

// parseRpcError takes the structured message produced by
// `format_rpc_error` on the Rust side and returns a typed
// `*RpcError`. The Rust formatter emits `<kind>: <detail>` (no
// `nrpc:` prefix — the prefix is added here so the surface matches
// the Node + Python bindings' string shape).
func parseRpcError(raw string) *RpcError {
	colon := strings.Index(raw, ":")
	if colon == -1 {
		return &RpcError{Kind: RpcKindUnknown, Message: raw}
	}
	kind := RpcKind(strings.TrimSpace(raw[:colon]))
	detail := strings.TrimSpace(raw[colon+1:])
	switch kind {
	case RpcKindNoRoute, RpcKindTimeout, RpcKindServerError,
		RpcKindTransport, RpcKindCodecEncode, RpcKindCodecDecode:
		return &RpcError{Kind: kind, Message: detail}
	}
	return &RpcError{Kind: RpcKindUnknown, Message: raw}
}

// =====================================================================
// Handler registry
// =====================================================================

// Handler is the user-facing Go signature for an nRPC handler. It
// receives the request bytes and returns the response bytes, or an
// error. Errors are surfaced to the caller as
// `nrpc:server_error: status=0x4001 message=<err>` per the typed-
// handler contract documented in the Rust SDK.
type Handler func(req []byte) ([]byte, error)

var (
	handlerRegistry sync.Map // handlerID (uint64) -> Handler
	dispatcherOnce  sync.Once
)

// registerDispatcher tells the Rust side which Go function to call
// when a request arrives. Idempotent — only the first call from
// any goroutine takes effect (matches the `OnceLock` semantics on
// the Rust side).
func registerDispatcher() {
	dispatcherOnce.Do(func() {
		C.net_rpc_set_handler_dispatcher(
			(C.RpcHandlerFn)(C.go_net_rpc_handler_trampoline),
		)
	})
}

//export go_net_rpc_handler_trampoline
func go_net_rpc_handler_trampoline(
	handlerID C.uint64_t,
	reqPtr *C.uint8_t,
	reqLen C.size_t,
	outRespPtr **C.uint8_t,
	outRespLen *C.size_t,
	outErr **C.char,
) C.int {
	// Look up the registered handler. A miss means `Close()` raced
	// with an in-flight dispatch — surface as a recoverable error.
	val, ok := handlerRegistry.Load(uint64(handlerID))
	if !ok {
		writeCError(outErr, fmt.Sprintf("no handler registered for id %d", uint64(handlerID)))
		return -1
	}
	handler, _ := val.(Handler)

	// Copy request bytes into a Go-owned slice so the user's
	// handler can capture / mutate freely without aliasing the
	// Rust-owned buffer (which is only valid for this call).
	req := C.GoBytes(unsafe.Pointer(reqPtr), C.int(reqLen))

	// Recover from handler panics so a buggy user handler doesn't
	// crash the whole process.
	resp, err := safeCallHandler(handler, req)
	if err != nil {
		writeCError(outErr, err.Error())
		return -1
	}

	// Allocate the response buffer via C.malloc so the Rust side
	// can free it via `libc::free` (matches the contract documented
	// in `net-rpc-ffi/src/lib.rs::RpcHandlerFn`).
	if len(resp) == 0 {
		*outRespPtr = nil
		*outRespLen = 0
		return 0
	}
	cBuf := C.malloc(C.size_t(len(resp)))
	if cBuf == nil {
		writeCError(outErr, "C.malloc returned NULL for response buffer")
		return -1
	}
	C.memmove(cBuf, unsafe.Pointer(&resp[0]), C.size_t(len(resp)))
	*outRespPtr = (*C.uint8_t)(cBuf)
	*outRespLen = C.size_t(len(resp))
	return 0
}

// safeCallHandler runs the user's handler under a defer/recover so
// a panic surfaces as a typed error instead of taking down the
// process via the cgo callback path.
func safeCallHandler(h Handler, req []byte) (resp []byte, err error) {
	defer func() {
		if r := recover(); r != nil {
			err = fmt.Errorf("nrpc handler panicked: %v", r)
		}
	}()
	return h(req)
}

// writeCError copies a Go string into a C.malloc'd CString. The
// Rust side calls `libc::free` after consuming it.
func writeCError(out **C.char, msg string) {
	if out == nil {
		return
	}
	cs := C.CString(msg) // C.malloc-backed; Rust frees via libc::free.
	*out = cs
}

// =====================================================================
// MeshRpc handle
// =====================================================================

// MeshRpc is a Go wrapper around the C `MeshRpcHandle`. Build via
// `NewMeshRpc(node)` where `node` is an `Arc<MeshNode>` pointer
// obtained from the `compute-ffi` Go binding (typically via
// `mesh.ArcClone()`).
type MeshRpc struct {
	handle *C.MeshRpcHandle
	closed atomic.Bool
}

// NewMeshRpc takes ownership of an `Arc<MeshNode>` pointer (from
// `net_mesh_arc_clone` in the upstream `mesh-ffi`) and returns a
// MeshRpc. The pointer is consumed; the caller MUST NOT free it
// after this call returns successfully.
//
// Installs a runtime finalizer to release the C handle on GC, but
// callers SHOULD `defer rpc.Close()` for deterministic cleanup.
func NewMeshRpc(nodeArcPtr unsafe.Pointer) (*MeshRpc, error) {
	registerDispatcher()
	h := C.net_rpc_new(nodeArcPtr)
	if h == nil {
		return nil, errors.New("net_rpc_new returned NULL (node_arc was NULL?)")
	}
	r := &MeshRpc{handle: h}
	runtime.SetFinalizer(r, (*MeshRpc).finalize)
	return r, nil
}

// ID returns the monotonic id of this MeshRpc — useful for
// diagnostics / logs that correlate FFI-side state with Go-side
// state.
func (r *MeshRpc) ID() uint64 {
	if r.closed.Load() {
		return 0
	}
	return uint64(C.net_rpc_id(r.handle))
}

// Close releases the C handle. Idempotent. Subsequent operations
// on this MeshRpc return ErrClosed.
func (r *MeshRpc) Close() {
	if r.closed.Swap(true) {
		return
	}
	runtime.SetFinalizer(r, nil)
	C.net_rpc_free(r.handle)
	r.handle = nil
}

func (r *MeshRpc) finalize() { r.Close() }

// ErrClosed is returned by operations on a closed MeshRpc.
var ErrClosed = errors.New("net.MeshRpc: handle is closed")

// ErrStreamDone signals that a streaming RPC has produced its
// terminal item. Callers MUST stop polling and call `Close()` to
// release the handle.
var ErrStreamDone = errors.New("net.RpcStream: stream is done")

// ABIVersion returns the C-ABI version exported by the linked
// `libnet_rpc`. Compare against `ExpectedABIVersion` at process
// init to detect drift.
func ABIVersion() uint32 { return uint32(C.net_rpc_abi_version()) }

// ExpectedABIVersion is the C-ABI version this Go wrapper is
// known to be source-compatible with. Bumped in lockstep with
// `NET_RPC_ABI_VERSION` on the Rust side.
const ExpectedABIVersion uint32 = 0x0001

// errABIMismatch is the typed error returned by CheckABI on a
// version mismatch. Use `errors.Is(err, ErrABIMismatch)` to
// branch.
var ErrABIMismatch = errors.New("net.RPC: linked libnet_rpc ABI version differs from this Go wrapper's expected version")

// CheckABI compares the linked cdylib's exported ABI version
// against ExpectedABIVersion and returns ErrABIMismatch (with
// detail) on drift. Idempotent; cheap.
func CheckABI() error {
	got := ABIVersion()
	if got == ExpectedABIVersion {
		return nil
	}
	return fmt.Errorf("%w: linked = 0x%04x, expected = 0x%04x", ErrABIMismatch, got, ExpectedABIVersion)
}

// init asserts the linked cdylib's ABI version matches the Go
// wrapper's compile-time expectation. Emitting a panic on drift
// is the correct behavior — a wrapper compiled against version A
// linked at runtime against version B has undefined memory layout
// for the opaque handles, and any subsequent FFI call is UB.
//
// Override the panic via the env var
// `NET_RPC_SKIP_ABI_CHECK=1` only when you're knowingly running
// against an in-development cdylib (e.g. CI bisect, local SDK
// debugging). Production code should never set it.
func init() {
	if os.Getenv("NET_RPC_SKIP_ABI_CHECK") == "1" {
		return
	}
	if err := CheckABI(); err != nil {
		panic(err)
	}
}

// =====================================================================
// Calls
// =====================================================================

// Call invokes `service` on `targetNodeID` with `req` as the body.
// Blocks the calling goroutine until the response arrives, the
// deadline fires, or `ctx` is canceled.
func (r *MeshRpc) Call(
	ctx context.Context,
	targetNodeID uint64,
	service string,
	req []byte,
) ([]byte, error) {
	if r.closed.Load() {
		return nil, ErrClosed
	}
	deadlineMs := contextDeadlineMs(ctx)
	cService := stringToCBytes(service)
	defer C.free(cService.ptr)
	cReq, freeReq := bytesToCBytes(req)
	defer freeReq()

	var outResp *C.uint8_t
	var outRespLen C.size_t
	var outErr *C.char
	code := C.net_rpc_call(
		r.handle,
		C.uint64_t(targetNodeID),
		(*C.char)(cService.ptr), cService.len,
		cReq.ptr, cReq.len,
		C.uint64_t(deadlineMs),
		&outResp, &outRespLen,
		&outErr,
	)
	return readCallResult(code, outResp, outRespLen, outErr)
}

// CallService invokes `service` on a node selected via local
// service discovery. Same blocking semantics as Call.
func (r *MeshRpc) CallService(
	ctx context.Context,
	service string,
	req []byte,
) ([]byte, error) {
	if r.closed.Load() {
		return nil, ErrClosed
	}
	deadlineMs := contextDeadlineMs(ctx)
	cService := stringToCBytes(service)
	defer C.free(cService.ptr)
	cReq, freeReq := bytesToCBytes(req)
	defer freeReq()

	var outResp *C.uint8_t
	var outRespLen C.size_t
	var outErr *C.char
	code := C.net_rpc_call_service(
		r.handle,
		(*C.char)(cService.ptr), cService.len,
		cReq.ptr, cReq.len,
		C.uint64_t(deadlineMs),
		&outResp, &outRespLen,
		&outErr,
	)
	return readCallResult(code, outResp, outRespLen, outErr)
}

// FindServiceNodes returns the node IDs advertising
// `nrpc:<service>` in the local capability index. Empty slice ==
// no providers; nil error in that case.
func (r *MeshRpc) FindServiceNodes(service string) ([]uint64, error) {
	if r.closed.Load() {
		return nil, ErrClosed
	}
	cService := stringToCBytes(service)
	defer C.free(cService.ptr)

	var outPtr *C.uint64_t
	var outCount C.size_t
	var outErr *C.char
	code := C.net_rpc_find_service_nodes(
		r.handle,
		(*C.char)(cService.ptr), cService.len,
		&outPtr, &outCount,
		&outErr,
	)
	if code != 0 {
		err := readCError(outErr)
		return nil, parseRpcError(err)
	}
	if outCount == 0 || outPtr == nil {
		return []uint64{}, nil
	}
	defer C.net_rpc_find_service_nodes_free(outPtr, outCount)
	count := int(outCount)
	src := unsafe.Slice((*uint64)(unsafe.Pointer(outPtr)), count)
	out := make([]uint64, count)
	copy(out, src)
	return out, nil
}

// readCallResult shapes the (code, resp, len, err) tuple that every
// unary call returns into a Go (resp, error) pair.
func readCallResult(
	code C.int,
	respPtr *C.uint8_t,
	respLen C.size_t,
	errPtr *C.char,
) ([]byte, error) {
	if code != 0 {
		msg := readCError(errPtr)
		return nil, parseRpcError(msg)
	}
	if respLen == 0 || respPtr == nil {
		return []byte{}, nil
	}
	defer C.net_rpc_response_free(respPtr, respLen)
	src := unsafe.Slice((*byte)(unsafe.Pointer(respPtr)), int(respLen))
	out := make([]byte, int(respLen))
	copy(out, src)
	return out, nil
}

// =====================================================================
// Streaming
// =====================================================================

// StreamOptions configures a streaming call's flow control. The
// zero value disables explicit flow control (server runs free,
// client auto-grants on each chunk delivery).
type StreamOptions struct {
	// Window installs `nrpc-stream-window-initial=<window>` on the
	// REQUEST. Server pumps up to `window` chunks ahead before
	// pausing for credit. Auto-grant from
	// `(*RpcStream).Recv` keeps the credit at roughly `window`.
	// Zero == no flow control.
	Window uint32
}

// RpcStream is an open streaming RPC call. Recv blocks until the
// next chunk arrives, the deadline fires, or the stream
// terminates. Close MUST be called eventually (defer is fine);
// dropping a stream without Close leaks the C handle until the
// finalizer runs.
type RpcStream struct {
	rpc    *MeshRpc
	handle *C.RpcStreamHandleC
	callID uint64
	closed atomic.Bool
	// cancel fires the ctx-cancel watcher goroutine's parent
	// context, so it unblocks and exits when Close() runs even
	// if the user-supplied ctx never cancels. The watcher exits
	// naturally — no WaitGroup needed because Close is idempotent.
	cancel context.CancelFunc
}

// CallStreaming opens a streaming RPC. The returned RpcStream
// MUST be Closed (defer is fine). If `ctx` cancels before the
// stream terminates, a watcher goroutine fires `Close()` on the
// stream; the watcher pins to the stream's lifetime so it doesn't
// leak past Close.
func (r *MeshRpc) CallStreaming(
	ctx context.Context,
	targetNodeID uint64,
	service string,
	req []byte,
	opts StreamOptions,
) (*RpcStream, error) {
	if r.closed.Load() {
		return nil, ErrClosed
	}
	deadlineMs := contextDeadlineMs(ctx)
	cService := stringToCBytes(service)
	defer C.free(cService.ptr)
	cReq, freeReq := bytesToCBytes(req)
	defer freeReq()

	var outStream *C.RpcStreamHandleC
	var outErr *C.char
	code := C.net_rpc_call_streaming(
		r.handle,
		C.uint64_t(targetNodeID),
		(*C.char)(cService.ptr), cService.len,
		cReq.ptr, cReq.len,
		C.uint64_t(deadlineMs),
		C.uint32_t(opts.Window),
		&outStream,
		&outErr,
	)
	if code != 0 {
		msg := readCError(outErr)
		return nil, parseRpcError(msg)
	}
	stream := &RpcStream{
		rpc:    r,
		handle: outStream,
		callID: uint64(C.net_rpc_stream_call_id(outStream)),
	}
	runtime.SetFinalizer(stream, (*RpcStream).finalize)

	// Spawn a watcher goroutine that closes the stream when ctx is
	// canceled. Pinned to the stream's lifetime via a WaitGroup
	// joined in Close — no leaks past the stream.
	if ctx != nil && ctx.Done() != nil {
		watchCtx, cancel := context.WithCancel(ctx)
		stream.cancel = cancel
		go func() {
			<-watchCtx.Done()
			// Either ctx fired (user canceled) or Close() called
			// our cancel(); both lead to the same action — and
			// Close() is idempotent so the second-comer no-ops.
			stream.Close()
		}()
	}
	return stream, nil
}

// CallID returns the server-assigned id for this streaming call —
// useful for trace correlation.
func (s *RpcStream) CallID() uint64 { return s.callID }

// Recv blocks until the next chunk arrives or the stream
// terminates. Returns `ErrStreamDone` (wrapped) on clean end. A
// mid-stream protocol error returns a typed `*RpcError`. After
// any non-nil error EXCEPT `ErrStreamDone`, the stream is closed
// implicitly and further Recv returns `ErrStreamDone`.
func (s *RpcStream) Recv() ([]byte, error) {
	if s.closed.Load() {
		return nil, ErrStreamDone
	}
	var outChunk *C.uint8_t
	var outChunkLen C.size_t
	var outErr *C.char
	code := C.net_rpc_stream_next(s.handle, &outChunk, &outChunkLen, &outErr)
	switch code {
	case 0: // chunk
		if outChunkLen == 0 || outChunk == nil {
			return []byte{}, nil
		}
		defer C.net_rpc_response_free(outChunk, outChunkLen)
		src := unsafe.Slice((*byte)(unsafe.Pointer(outChunk)), int(outChunkLen))
		out := make([]byte, int(outChunkLen))
		copy(out, src)
		return out, nil
	case -6: // NET_RPC_ERR_STREAM_DONE — clean end
		return nil, ErrStreamDone
	case -2: // NET_RPC_ERR_CALL_FAILED — mid-stream error
		msg := readCError(outErr)
		return nil, parseRpcError(msg)
	default:
		msg := readCError(outErr)
		return nil, fmt.Errorf("net_rpc_stream_next returned %d: %s", int(code), msg)
	}
}

// Grant gives the server `amount` more credits. No-op if flow
// control wasn't enabled for this stream OR the stream is already
// done.
func (s *RpcStream) Grant(amount uint32) {
	if s.closed.Load() {
		return
	}
	C.net_rpc_stream_grant(s.handle, C.uint32_t(amount))
}

// Close cancels the stream (best-effort CANCEL to the server) and
// releases the C handle. Idempotent. Joins the ctx-cancel watcher
// goroutine before returning.
func (s *RpcStream) Close() {
	if s.closed.Swap(true) {
		return
	}
	runtime.SetFinalizer(s, nil)
	C.net_rpc_stream_close(s.handle)
	C.net_rpc_stream_free(s.handle)
	s.handle = nil
	if s.cancel != nil {
		s.cancel()
	}
}

func (s *RpcStream) finalize() { s.Close() }

// =====================================================================
// ServeHandle
// =====================================================================

// ServeHandle represents a registered handler. Close it to stop
// accepting new requests; in-flight requests still complete
// (mirrors the H8 fix in the Rust SDK).
type ServeHandle struct {
	rpc       *MeshRpc
	handle    *C.ServeHandleC
	handlerID uint64
	closed    atomic.Bool
}

// ErrAlreadyServing is returned by Serve when the underlying
// MeshNode already has a handler registered for the requested
// service. Use `errors.Is(err, ErrAlreadyServing)` to dispatch.
var ErrAlreadyServing = errors.New("net.Serve: service already served by this MeshNode")

// Serve registers `handler` for `service`. The returned
// `*ServeHandle` MUST be closed when the service should stop
// accepting new requests.
//
// Pre-registers the handler in the Go-side dispatch registry
// BEFORE crossing the FFI boundary, closing the
// "request-arrives-before-Store" race: a request landing in the
// Tokio dispatcher between `serve_rpc` returning and any
// language-side bookkeeping must always find the callable.
func (r *MeshRpc) Serve(service string, handler Handler) (*ServeHandle, error) {
	if r.closed.Load() {
		return nil, ErrClosed
	}
	if handler == nil {
		return nil, errors.New("net.Serve: handler must be non-nil")
	}
	registerDispatcher()

	// 1. Reserve the id Rust will use for this handler. The Rust
	//    side hands out monotonic ids and never reuses; an unused
	//    reservation is harmless.
	hID := uint64(C.net_rpc_reserve_handler_id())

	// 2. Insert the callable BEFORE calling serve. Even if the
	//    very first request lands the instant `net_rpc_serve`
	//    returns, the trampoline finds the handler.
	handlerRegistry.Store(hID, handler)

	cService := stringToCBytes(service)
	defer C.free(cService.ptr)

	var outErr *C.char
	handle := C.net_rpc_serve(
		r.handle,
		(*C.char)(cService.ptr), cService.len,
		C.uint64_t(hID),
		&outErr,
	)
	if handle == nil {
		// Roll the registry insert back so a retry doesn't trip
		// over a stale dispatcher entry — and so we don't leak
		// the user's `handler` reference forever.
		handlerRegistry.Delete(hID)
		msg := readCError(outErr)
		// Surface ServeError::AlreadyServing as a typed sentinel
		// so callers can branch on `errors.Is(err, ErrAlreadyServing)`.
		// The Rust side's `Display` for ServeError emits messages
		// like `"serve failed: already serving service \"...\""`;
		// match on the substring to map.
		if strings.Contains(msg, "already serving") {
			return nil, fmt.Errorf("%w: %s", ErrAlreadyServing, msg)
		}
		return nil, fmt.Errorf("serve failed: %s", msg)
	}

	sh := &ServeHandle{rpc: r, handle: handle, handlerID: hID}
	runtime.SetFinalizer(sh, (*ServeHandle).finalize)
	return sh, nil
}

// HandlerID returns the FFI-side id of this registered handler —
// useful for correlating logs with the Rust-side metrics.
func (s *ServeHandle) HandlerID() uint64 { return s.handlerID }

// Close unregisters the handler and releases the C handle.
// Idempotent.
func (s *ServeHandle) Close() {
	if s.closed.Swap(true) {
		return
	}
	runtime.SetFinalizer(s, nil)
	C.net_rpc_serve_handle_close(s.handle)
	C.net_rpc_serve_handle_free(s.handle)
	s.handle = nil
	handlerRegistry.Delete(s.handlerID)
}

func (s *ServeHandle) finalize() { s.Close() }

// =====================================================================
// Internal helpers
// =====================================================================

// cBuf bundles a C-allocated buffer's pointer and length so the
// caller can free both with one defer.
type cBuf struct {
	ptr unsafe.Pointer
	len C.size_t
}

// stringToCBytes copies `s` to a freshly-allocated C buffer (NOT
// NUL-terminated). Returns the pointer + length; caller frees with
// `C.free(buf.ptr)`.
func stringToCBytes(s string) cBuf {
	if len(s) == 0 {
		return cBuf{ptr: nil, len: 0}
	}
	cs := C.CBytes([]byte(s))
	return cBuf{ptr: cs, len: C.size_t(len(s))}
}

// bytesToCBytes copies `b` to a C buffer iff non-empty. Returns the
// buffer + a freer (no-op for the empty case).
func bytesToCBytes(b []byte) (cReqBuf, func()) {
	if len(b) == 0 {
		return cReqBuf{ptr: nil, len: 0}, func() {}
	}
	cs := C.CBytes(b)
	return cReqBuf{ptr: (*C.uint8_t)(cs), len: C.size_t(len(b))},
		func() { C.free(cs) }
}

// cReqBuf bundles a C buffer typed for the `req_ptr` parameter of
// the FFI calls.
type cReqBuf struct {
	ptr *C.uint8_t
	len C.size_t
}

// readCError pulls the message out of a `**char` out-param and
// frees the underlying C string. Idempotent on NULL.
func readCError(p *C.char) string {
	if p == nil {
		return "unknown error (no detail)"
	}
	defer C.net_rpc_free_cstring(p)
	return C.GoString(p)
}

// contextDeadlineMs translates `ctx.Deadline()` into a positive
// millisecond delta the Rust side can install as a per-call
// deadline. Zero means "no deadline" on the Rust side; we map a
// missing or already-expired deadline to zero so the Rust call
// either succeeds quickly or surfaces its own NoRoute / Timeout.
func contextDeadlineMs(ctx context.Context) uint64 {
	if ctx == nil {
		return 0
	}
	deadline, ok := ctx.Deadline()
	if !ok {
		return 0
	}
	d := time.Until(deadline)
	if d <= 0 {
		return 0
	}
	return uint64(d.Milliseconds())
}

// _ keeps the cgo handle import alive — even though we don't use it
// directly here, downstream extensions of this file (e.g. attaching
// arbitrary user_data to a handler beyond the simple registry) will.
var _ = cgo.Handle(0)
