// Typed nRPC wrappers for the Go binding.
//
// Sits on top of the raw *MeshRpc / *RpcStream / *ClientStreamCall /
// *DuplexCall surface defined in mesh_rpc.go: translates typed Go
// values to/from JSON on each side of the wire, and presents the
// same shape the Node TS and Python typed wrappers expose. Mirrors
// the Rust SDK's `call_typed` / `serve_typed` ergonomics.
//
// # Why free functions, not methods
//
// Per the cross-binding design decision (NRPC_STREAMING_PARITY_AND_GO_BINDING.md,
// locked decision #3), typed surfaces ship as free functions with
// type parameters — not methods on *TypedMeshRpc — because Go forbids
// type parameters on methods. The free-function shape gives compile-
// time type safety:
//
//	resp, err := TypedCall[EchoReq, EchoResp](ctx, t, target, "echo", req)
//
// Streams + calls remain type-parameterized structs (e.g.
// *TypedClientStreamCall[Req, Resp]) so their methods can use the
// struct-level type params without violating the no-method-generics
// rule.
//
// # Cancellation contract (locked decision #2)
//
// v1 supports `.Close()`-only cancellation for streaming surfaces.
// `context.Context` cancellation IS wired through to the raw layer
// for unary calls (Call / CallService) — the raw *MeshRpc already
// honors `ctx`. For streaming entry points, context cancellation is
// not propagated; invoke the typed call's `Close()` method to abort.
// Unifying signal/token/context propagation across streaming shapes
// is a deliberate post-v1 follow-up.
//
// # Observer + metrics contract (locked decision #1)
//
// Observer callbacks fire synchronously from the substrate dispatch
// path via the C ABI dispatcher. Callbacks must be cheap — push into
// a channel for slow consumers; do not do work inline. See SetObserver
// for the exact contract.

package net

/*
#include <stdint.h>
#include <stdlib.h>

// Forward-declared opaque MeshRpcHandle (defined in mesh_rpc.go).
typedef struct MeshRpcHandle MeshRpcHandle;

// ABI 0x0003: observer + metrics-snapshot FFI surface added by
// rpc-ffi/src/lib.rs (S1-A1 of NRPC_STREAMING_PARITY_AND_GO_BINDING).

// Discriminants for RpcCallEventC.status_kind.
enum {
    NET_RPC_STATUS_OK_C       = 0,
    NET_RPC_STATUS_ERROR_C    = 1,
    NET_RPC_STATUS_TIMEOUT_C  = 2,
    NET_RPC_STATUS_CANCELED_C = 3,
};

// Discriminants for RpcCallEventC.direction.
enum {
    NET_RPC_DIRECTION_OUTBOUND_C = 0,
    NET_RPC_DIRECTION_INBOUND_C  = 1,
};

// POD mirroring `RpcCallEventC` in rpc-ffi/src/lib.rs. All pointer
// fields are borrowed for the duration of the dispatcher call only —
// the Go side MUST copy out anything it wants to keep.
typedef struct RpcCallEventC {
    uint64_t caller;
    uint64_t callee;
    const uint8_t* method_ptr;
    size_t method_len;
    uint32_t latency_ms;
    uint8_t status_kind;
    const uint8_t* status_message_ptr;
    size_t status_message_len;
    uint32_t request_bytes;
    uint32_t response_bytes;
    uint8_t direction;
    uint64_t ts_unix_ms;
} RpcCallEventC;

typedef void (*RpcObserverFn)(const RpcCallEventC* evt);

extern void net_rpc_set_observer_dispatcher(RpcObserverFn observer);
extern int net_rpc_observer_install(const MeshRpcHandle* handle, int enabled);
extern int net_rpc_metrics_snapshot(
    const MeshRpcHandle* handle,
    uint8_t** out_json_ptr, size_t* out_json_len,
    char** out_err
);

// Go-side trampoline that bridges from C → Go. Declared here so the
// cgo prelude can take its address when registering the dispatcher;
// the definition is the //export below.
extern void go_net_rpc_observer_trampoline(const RpcCallEventC* evt);
*/
import "C"

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"sync"
	"sync/atomic"
	"unsafe"
)

// =====================================================================
// TypedMeshRpc — public typed wrapper.
// =====================================================================

// TypedMeshRpc is the user-facing typed envelope around a raw
// *MeshRpc. It owns the raw handle's lifetime: Close on the
// underlying *MeshRpc is exposed via Raw().Close().
//
// Construct with NewTypedMeshRpc(raw). All typed surfaces are free
// functions taking *TypedMeshRpc as the first argument — see the
// package-level doc for the rationale.
type TypedMeshRpc struct {
	raw *MeshRpc
}

// NewTypedMeshRpc builds a TypedMeshRpc against an existing *MeshRpc.
// Cheap — the wrapper holds a pointer to the raw handle without
// taking any additional resources.
func NewTypedMeshRpc(raw *MeshRpc) *TypedMeshRpc {
	return &TypedMeshRpc{raw: raw}
}

// Raw exposes the underlying *MeshRpc for users who need the
// bytes-level surface (cross-codec interop, raw streams).
func (t *TypedMeshRpc) Raw() *MeshRpc { return t.raw }

// =====================================================================
// JSON codec helpers (S1-D1).
// =====================================================================

// jsonEncodeTyped encodes `value` as JSON; encode failure surfaces
// as RpcError{Kind: RpcKindCodecEncode} BEFORE the call hits the
// wire so the diagnostic points at the user's call site.
func jsonEncodeTyped[T any](value T) ([]byte, error) {
	body, err := json.Marshal(value)
	if err != nil {
		return nil, &RpcError{Kind: RpcKindCodecEncode, Message: err.Error()}
	}
	return body, nil
}

// jsonDecodeTyped decodes a response Buffer into the target Resp type.
// Decode failure surfaces as RpcError{Kind: RpcKindCodecDecode}.
func jsonDecodeTyped[T any](buf []byte) (T, error) {
	var zero T
	if err := json.Unmarshal(buf, &zero); err != nil {
		return zero, &RpcError{Kind: RpcKindCodecDecode, Message: err.Error()}
	}
	return zero, nil
}

// =====================================================================
// App-error helper + status code constants.
// =====================================================================

// Application-status code constants — parallel to the Rust SDK's
// NRPC_TYPED_BAD_REQUEST / NRPC_TYPED_HANDLER_ERROR and the Node
// + Python bindings' exports. Pin matches the cross-binding
// golden vectors at tests/cross_lang_nrpc/golden_vectors.json.
const (
	// NrpcTypedBadRequest signals "typed handler couldn't decode
	// the request body." The shim returns this when the inbound
	// JSON fails to unmarshal into the typed Req parameter.
	NrpcTypedBadRequest uint16 = 0x8000

	// NrpcTypedHandlerError signals "typed handler returned an
	// error." Generic handler-error wrapper for cases where the
	// user handler returns a non-app-error error and the wrapper
	// wants to surface it as Application(0x8001) instead of the
	// default Internal mapping.
	NrpcTypedHandlerError uint16 = 0x8001
)

// AppError builds an error a typed serve handler can return to
// surface a specific application status code to the caller. The
// rpc-ffi Go-handler bridge parses messages of the form
// `nrpc:app_error:0x<code>:<body>` and maps them to
// `RpcStatus::Application(code)` — without this prefix the
// returned error becomes a generic `RpcStatus::Internal`. Mirrors
// the Node binding's `appError(code, body)` and the Python
// binding's `RpcAppError(code, body)`.
//
// Use cases: typed handlers that want to return 4xx-style
// application errors (NrpcTypedBadRequest, NrpcTypedHandlerError,
// custom app codes >= 0x8000).
func AppError(code uint16, body []byte) error {
	return fmt.Errorf("nrpc:app_error:0x%04x:%s", code, string(body))
}

// =====================================================================
// Unary: TypedCall / TypedCallService / TypedServe (S1-D1).
// =====================================================================

// TypedCall is a direct-addressed typed unary call. Encodes `req`
// as JSON, calls, decodes the response.
//
// `ctx` is honored: cancellation aborts the in-flight call via the
// raw *MeshRpc's cancel-token wiring. Deadlines from `ctx` flow
// through unchanged.
//
// Returns an *RpcError subclass on wire failure (decode kind on
// reply parse failure; encode kind if `req` doesn't round-trip
// through encoding/json).
func TypedCall[Req, Resp any](
	ctx context.Context,
	t *TypedMeshRpc,
	targetNodeID uint64,
	service string,
	req Req,
) (Resp, error) {
	var zero Resp
	body, err := jsonEncodeTyped(req)
	if err != nil {
		return zero, err
	}
	respBody, err := t.raw.Call(ctx, targetNodeID, service, body)
	if err != nil {
		return zero, err
	}
	return jsonDecodeTyped[Resp](respBody)
}

// TypedCallService is a service-discovery typed unary call.
// Resolves `service` against the local capability index, applies
// the routing policy, calls. Same ctx + error semantics as
// TypedCall.
func TypedCallService[Req, Resp any](
	ctx context.Context,
	t *TypedMeshRpc,
	service string,
	req Req,
) (Resp, error) {
	var zero Resp
	body, err := jsonEncodeTyped(req)
	if err != nil {
		return zero, err
	}
	respBody, err := t.raw.CallService(ctx, service, body)
	if err != nil {
		return zero, err
	}
	return jsonDecodeTyped[Resp](respBody)
}

// TypedHandler is the user-facing typed handler signature for
// TypedServe. The handler receives a decoded `Req` and returns a
// `Resp` (or an error).
//
// Return AppError(code, body) to surface a typed
// RpcStatus::Application(code) to the caller; any other error
// surfaces as RpcStatus::Internal per the historical handler
// contract.
type TypedHandler[Req, Resp any] func(req Req) (Resp, error)

// TypedServe registers a typed handler on `service`. JSON decode
// /encode happens at the binding boundary; decode failure
// auto-surfaces as Application(NrpcTypedBadRequest) per the cross-
// binding contract (matches the Rust integration test
// `cross_lang_error_cases_surface_typed_bad_request`).
//
// Returns a *ServeHandle whose Close() unregisters the service.
// In-flight handlers continue to completion after close.
func TypedServe[Req, Resp any](
	t *TypedMeshRpc,
	service string,
	handler TypedHandler[Req, Resp],
) (*ServeHandle, error) {
	shim := func(reqBytes []byte) ([]byte, error) {
		var req Req
		if err := json.Unmarshal(reqBytes, &req); err != nil {
			body := mustMarshal(struct {
				Err    string `json:"error"`
				Detail string `json:"detail"`
			}{Err: "invalid_request", Detail: err.Error()})
			return nil, AppError(NrpcTypedBadRequest, body)
		}
		resp, err := handler(req)
		if err != nil {
			return nil, err
		}
		return jsonEncodeTyped(resp)
	}
	return t.raw.Serve(service, shim)
}

// mustMarshal is a small helper for shim bodies — the JSON
// canonicalization for the bad-request body never fails for the
// static {"error":"invalid_request","detail":"..."} shape we
// emit, so panicking on Marshal failure is a programmer error.
func mustMarshal(v interface{}) []byte {
	b, err := json.Marshal(v)
	if err != nil {
		panic(fmt.Sprintf("nrpc: app-error body marshal failed (shouldn't happen): %v", err))
	}
	return b
}

// =====================================================================
// Streaming-response: TypedCallStreaming + TypedRpcStream (S1-D2).
// =====================================================================

// TypedRpcStream is a typed iterator over a streaming RPC response.
// Each call to Recv yields a decoded `Resp` value. Decode failure
// closes the underlying *RpcStream and re-throws RpcKindCodecDecode.
//
// Mirrors the Node TS `TypedRpcStream<Resp>` + Python
// `TypedRpcStream` shape. Use Close() to drop the call and emit
// CANCEL to the server (locked decision #2 — `.Close()`-only
// cancellation for streaming).
type TypedRpcStream[Resp any] struct {
	raw  *RpcStream
	done bool
}

// Recv pulls the next decoded response. Returns (zero, false, nil)
// on clean EOF — translates the raw *RpcStream.Recv's
// `ErrStreamDone` sentinel into the idiomatic
// `for { v, ok, err := stream.Recv(); if !ok { break } }` Go
// drain pattern.
//
// Decode failure on a chunk closes the underlying stream and
// returns RpcError{Kind: RpcKindCodecDecode}.
func (s *TypedRpcStream[Resp]) Recv() (Resp, bool, error) {
	var zero Resp
	if s.done {
		return zero, false, nil
	}
	buf, err := s.raw.Recv()
	if err != nil {
		s.done = true
		if errors.Is(err, ErrStreamDone) {
			return zero, false, nil
		}
		return zero, false, err
	}
	if buf == nil {
		s.done = true
		return zero, false, nil
	}
	resp, err := jsonDecodeTyped[Resp](buf)
	if err != nil {
		s.done = true
		s.raw.Close()
		return zero, false, err
	}
	return resp, true, nil
}

// Grant relays a flow-control credit grant to the underlying
// streaming pump. Only meaningful when the call was opened with
// non-zero `StreamWindow` in `StreamOptions`.
func (s *TypedRpcStream[Resp]) Grant(amount uint32) {
	s.raw.Grant(amount)
}

// Close ends the stream early. Emits CANCEL to the server (best-
// effort). Idempotent; safe to call from a defer.
func (s *TypedRpcStream[Resp]) Close() {
	s.done = true
	s.raw.Close()
}

// CallID surfaces the server-assigned `call_id` for diagnostics.
func (s *TypedRpcStream[Resp]) CallID() uint64 { return s.raw.CallID() }

// Raw exposes the underlying *RpcStream for users who need
// bytes-level access.
func (s *TypedRpcStream[Resp]) Raw() *RpcStream { return s.raw }

// TypedCallStreaming opens a typed streaming-response call.
// Returns a *TypedRpcStream[Resp] — drain via Recv() until it
// returns ok=false (clean EOF).
//
// Cancellation: `ctx` is honored end-to-end. When `ctx` fires
// mid-stream the raw layer's ctx-watcher closes the stream
// handle, dropping the SDK future and emitting CANCEL on the
// wire. `stream.Close()` is the explicit-drop surface and is
// idempotent with the ctx-watcher. `ctx.Deadline()` still seeds
// the wire deadline at construction.
func TypedCallStreaming[Req, Resp any](
	ctx context.Context,
	t *TypedMeshRpc,
	targetNodeID uint64,
	service string,
	req Req,
	opts StreamOptions,
) (*TypedRpcStream[Resp], error) {
	body, err := jsonEncodeTyped(req)
	if err != nil {
		return nil, err
	}
	raw, err := t.raw.CallStreaming(ctx, targetNodeID, service, body, opts)
	if err != nil {
		return nil, err
	}
	return &TypedRpcStream[Resp]{raw: raw}, nil
}

// TypedCallServiceStreaming opens a capability-routed typed
// streaming call. Mirrors TypedCallService for target resolution
// + cap-auth gate; mirrors TypedCallStreaming for the
// chunk-iterator return shape.
//
// Used by net.CallToolStreaming for streaming tool invocations.
// Same ctx + Close() semantics as TypedCallStreaming.
func TypedCallServiceStreaming[Req, Resp any](
	ctx context.Context,
	t *TypedMeshRpc,
	service string,
	req Req,
	opts StreamOptions,
) (*TypedRpcStream[Resp], error) {
	body, err := jsonEncodeTyped(req)
	if err != nil {
		return nil, err
	}
	raw, err := t.raw.CallServiceStreaming(ctx, service, body, opts)
	if err != nil {
		return nil, err
	}
	return &TypedRpcStream[Resp]{raw: raw}, nil
}

// =====================================================================
// Client-streaming: TypedCallClientStream + TypedServeClientStream
// + TypedClientStreamCall + TypedRequestStream (S1-D3).
// =====================================================================

// TypedClientStreamCall is the caller-side handle for a typed
// client-streaming call. Push typed requests via Send, then Finish
// to await the terminal response.
//
// Cancellation contract (locked decision #2): `ctx` is NOT wired
// into the raw layer for streaming. Invoke Close() to abort an
// in-flight call.
type TypedClientStreamCall[Req, Resp any] struct {
	raw *ClientStreamCall
}

// Send encodes `value` as JSON and pushes it as one request chunk.
// Encode failure surfaces as RpcError{Kind: RpcKindCodecEncode}
// and the chunk is NOT sent.
func (c *TypedClientStreamCall[Req, Resp]) Send(value Req) error {
	body, err := jsonEncodeTyped(value)
	if err != nil {
		return err
	}
	return c.raw.Send(body)
}

// Finish closes the upload direction and awaits the terminal
// response. Consumes the call — subsequent Send / Finish return
// an `nrpc:stream_closed` error.
func (c *TypedClientStreamCall[Req, Resp]) Finish() (Resp, error) {
	respBody, err := c.raw.Finish()
	if err != nil {
		var zero Resp
		return zero, err
	}
	return jsonDecodeTyped[Resp](respBody)
}

// CallID surfaces the server-assigned `call_id`.
func (c *TypedClientStreamCall[Req, Resp]) CallID() uint64 { return c.raw.CallID() }

// Close fires CANCEL via the SDK's Drop if the initial REQUEST
// has already flown. Idempotent; safe to call from a defer.
func (c *TypedClientStreamCall[Req, Resp]) Close() { c.raw.Close() }

// Raw exposes the underlying *ClientStreamCall.
func (c *TypedClientStreamCall[Req, Resp]) Raw() *ClientStreamCall { return c.raw }

// TypedRequestStream is the server-side typed inbound request
// stream surfaced to client-streaming + duplex handlers. Each call
// to Recv yields a decoded `Req`. Decode failure on a chunk closes
// the stream and returns RpcKindCodecDecode.
type TypedRequestStream[Req any] struct {
	raw  *RequestStreamRecv
	done bool
}

// Recv pulls the next decoded request. Returns ok=false on clean
// EOF; ErrStreamDone from the raw layer translates to (zero, false, nil)
// so the typed drain loop is the same idiom as TypedRpcStream.Recv.
//
// Decode failure on a chunk marks the stream done and returns
// RpcError{Kind: RpcKindCodecDecode}.
func (s *TypedRequestStream[Req]) Recv() (Req, bool, error) {
	var zero Req
	if s.done {
		return zero, false, nil
	}
	buf, err := s.raw.Recv()
	if err != nil {
		s.done = true
		if errors.Is(err, ErrStreamDone) {
			return zero, false, nil
		}
		return zero, false, err
	}
	if buf == nil {
		s.done = true
		return zero, false, nil
	}
	req, err := jsonDecodeTyped[Req](buf)
	if err != nil {
		s.done = true
		return zero, false, err
	}
	return req, true, nil
}

// Raw exposes the underlying *RequestStreamRecv.
//
// Diagnostic getters (caller_origin, call_id, deadline_ns,
// headers) are NOT yet surfaced by the reference Go binding's
// *RequestStreamRecv. Once the raw layer exposes them, the typed
// wrapper will add matching getters; until then, downstream
// consumers can either ignore them or add the raw-side getters
// in their own fork.
func (s *TypedRequestStream[Req]) Raw() *RequestStreamRecv { return s.raw }

// TypedClientStreamHandler is the user-facing typed handler
// signature for TypedServeClientStream.
type TypedClientStreamHandler[Req, Resp any] func(stream *TypedRequestStream[Req]) (Resp, error)

// TypedCallClientStream opens a typed client-streaming call.
// `opts` configures the upload-side flow-control window.
//
// Cancellation: `ctx` is honored end-to-end via the raw layer's
// ctx-watcher (closes the call handle on ctx.Done, dropping the
// SDK future and emitting CANCEL on the wire). `.Close()`
// remains the explicit-drop surface and is idempotent with the
// ctx-watcher. `ctx.Deadline()` still seeds the wire deadline.
func TypedCallClientStream[Req, Resp any](
	ctx context.Context,
	t *TypedMeshRpc,
	targetNodeID uint64,
	service string,
	opts ClientStreamOptions,
) (*TypedClientStreamCall[Req, Resp], error) {
	raw, err := t.raw.CallClientStream(ctx, targetNodeID, service, opts)
	if err != nil {
		return nil, err
	}
	return &TypedClientStreamCall[Req, Resp]{raw: raw}, nil
}

// TypedServeClientStream registers a typed client-streaming
// handler. The handler receives a *TypedRequestStream[Req] (auto-
// decodes each chunk) and returns a `Resp` (auto-encoded). Return
// AppError(code, body) to surface a typed Application status.
func TypedServeClientStream[Req, Resp any](
	t *TypedMeshRpc,
	service string,
	handler TypedClientStreamHandler[Req, Resp],
) (*ServeHandle, error) {
	shim := func(raw *RequestStreamRecv) ([]byte, error) {
		typed := &TypedRequestStream[Req]{raw: raw}
		resp, err := handler(typed)
		if err != nil {
			return nil, err
		}
		return jsonEncodeTyped(resp)
	}
	return t.raw.ServeClientStream(service, shim)
}

// =====================================================================
// Duplex: TypedCallDuplex + TypedServeDuplex + TypedDuplexCall +
// TypedDuplexSink + TypedDuplexStream + TypedResponseSink (S1-D4).
// =====================================================================

// TypedDuplexCall is the caller-side handle for a typed duplex
// call. Push typed requests via Send, pull typed responses via
// Recv. IntoSplit yields independent sink + stream halves.
type TypedDuplexCall[Req, Resp any] struct {
	raw  *DuplexCall
	done bool
}

// Send encodes + pushes one request chunk.
func (c *TypedDuplexCall[Req, Resp]) Send(value Req) error {
	body, err := jsonEncodeTyped(value)
	if err != nil {
		return err
	}
	return c.raw.Send(body)
}

// FinishSending closes the upload direction (emit REQUEST_END).
// Does NOT close the response stream — drain it via Recv until ok=false.
func (c *TypedDuplexCall[Req, Resp]) FinishSending() error {
	return c.raw.FinishSending()
}

// Recv pulls the next decoded response. Returns ok=false on clean
// EOF (translates the raw layer's ErrStreamDone to (zero, false, nil)).
// Decode failure closes the underlying duplex call.
func (c *TypedDuplexCall[Req, Resp]) Recv() (Resp, bool, error) {
	var zero Resp
	if c.done {
		return zero, false, nil
	}
	buf, err := c.raw.Recv()
	if err != nil {
		c.done = true
		if errors.Is(err, ErrStreamDone) {
			return zero, false, nil
		}
		return zero, false, err
	}
	if buf == nil {
		c.done = true
		return zero, false, nil
	}
	resp, err := jsonDecodeTyped[Resp](buf)
	if err != nil {
		c.done = true
		c.raw.Close()
		return zero, false, err
	}
	return resp, true, nil
}

// Split yields independent typed sink + stream halves. After
// return, this *TypedDuplexCall is consumed — subsequent Send /
// FinishSending / Recv return `stream_closed`. CANCEL fires only
// when BOTH split halves drop without observing the response
// stream's terminal frame.
//
// (Named Split to match the raw *DuplexCall.Split; the Node /
// Python wrappers' `intoSplit` / `into_split` is the cross-binding
// alias.)
func (c *TypedDuplexCall[Req, Resp]) Split() (
	*TypedDuplexSink[Req],
	*TypedDuplexStream[Resp],
	error,
) {
	rawSink, rawStream, err := c.raw.Split()
	if err != nil {
		return nil, nil, err
	}
	c.done = true
	return &TypedDuplexSink[Req]{raw: rawSink}, &TypedDuplexStream[Resp]{raw: rawStream}, nil
}

// CallID / Close / Raw — pass-throughs.
func (c *TypedDuplexCall[Req, Resp]) CallID() uint64   { return c.raw.CallID() }
func (c *TypedDuplexCall[Req, Resp]) Close()           { c.done = true; c.raw.Close() }
func (c *TypedDuplexCall[Req, Resp]) Raw() *DuplexCall { return c.raw }

// TypedDuplexSink is the send-half of a typed duplex call after
// Split.
type TypedDuplexSink[Req any] struct {
	raw *DuplexSink
}

func (s *TypedDuplexSink[Req]) Send(value Req) error {
	body, err := jsonEncodeTyped(value)
	if err != nil {
		return err
	}
	return s.raw.Send(body)
}
func (s *TypedDuplexSink[Req]) Finish() error    { return s.raw.Finish() }
func (s *TypedDuplexSink[Req]) CallID() uint64   { return s.raw.CallID() }
func (s *TypedDuplexSink[Req]) Close()           { s.raw.Close() }
func (s *TypedDuplexSink[Req]) Raw() *DuplexSink { return s.raw }

// TypedDuplexStream is the receive-half of a typed duplex call.
type TypedDuplexStream[Resp any] struct {
	raw  *DuplexStream
	done bool
}

func (s *TypedDuplexStream[Resp]) Recv() (Resp, bool, error) {
	var zero Resp
	if s.done {
		return zero, false, nil
	}
	buf, err := s.raw.Recv()
	if err != nil {
		s.done = true
		if errors.Is(err, ErrStreamDone) {
			return zero, false, nil
		}
		return zero, false, err
	}
	if buf == nil {
		s.done = true
		return zero, false, nil
	}
	resp, err := jsonDecodeTyped[Resp](buf)
	if err != nil {
		s.done = true
		s.raw.Close()
		return zero, false, err
	}
	return resp, true, nil
}
func (s *TypedDuplexStream[Resp]) CallID() uint64     { return s.raw.CallID() }
func (s *TypedDuplexStream[Resp]) Close()             { s.done = true; s.raw.Close() }
func (s *TypedDuplexStream[Resp]) Raw() *DuplexStream { return s.raw }

// TypedResponseSink is the typed outbound sink surfaced to duplex
// server handlers. Encodes each Send + delegates to the raw
// *ResponseSinkSend.
//
// Flow control: the raw sink try_sends into a bounded 1024-chunk
// mpsc; bursts past the credit window are dropped (counted by
// `streaming_chunks_dropped_total`). Pace Send calls to the
// REQUEST_GRANT cadence for lossless flow control.
type TypedResponseSink[Resp any] struct {
	raw *ResponseSinkSend
}

// Send encodes + emits one response chunk. Returns nil on
// successful enqueue, ErrStreamDone if the sink has been torn
// down, or RpcError{Kind: RpcKindCodecEncode} on encode failure
// (the chunk is NOT sent in that case).
func (s *TypedResponseSink[Resp]) Send(value Resp) error {
	body, err := jsonEncodeTyped(value)
	if err != nil {
		return err
	}
	return s.raw.Send(body)
}
func (s *TypedResponseSink[Resp]) Raw() *ResponseSinkSend { return s.raw }

// TypedDuplexHandler is the user-facing typed handler signature.
type TypedDuplexHandler[Req, Resp any] func(
	stream *TypedRequestStream[Req],
	sink *TypedResponseSink[Resp],
) error

// TypedCallDuplex opens a typed duplex call.
//
// Cancellation: same end-to-end semantics as
// TypedCallClientStream — `ctx` fires substrate-level cancel and
// `.Close()` is the explicit-drop surface.
func TypedCallDuplex[Req, Resp any](
	ctx context.Context,
	t *TypedMeshRpc,
	targetNodeID uint64,
	service string,
	opts DuplexOptions,
) (*TypedDuplexCall[Req, Resp], error) {
	raw, err := t.raw.CallDuplex(ctx, targetNodeID, service, opts)
	if err != nil {
		return nil, err
	}
	return &TypedDuplexCall[Req, Resp]{raw: raw}, nil
}

// TypedServeDuplex registers a typed duplex handler. User signature
// is `(stream, sink) → error`; the raw side's
// `DuplexHandler(stream, sink) → error` is wrapped to provide the
// typed views.
func TypedServeDuplex[Req, Resp any](
	t *TypedMeshRpc,
	service string,
	handler TypedDuplexHandler[Req, Resp],
) (*ServeHandle, error) {
	shim := func(rawStream *RequestStreamRecv, rawSink *ResponseSinkSend) error {
		typedStream := &TypedRequestStream[Req]{raw: rawStream}
		typedSink := &TypedResponseSink[Resp]{raw: rawSink}
		return handler(typedStream, typedSink)
	}
	return t.raw.ServeDuplex(service, shim)
}

// TypedStreamingHandler is the user-facing typed handler signature
// for TypedServeStreaming. Receives the JSON-decoded request and a
// typed response sink for emitting chunks.
type TypedStreamingHandler[Req, Resp any] func(
	req Req,
	sink *TypedResponseSink[Resp],
) error

// TypedServeStreaming registers a typed server-streaming handler.
// User signature is `(req, sink) → error`. The raw side's
// `StreamingHandler(rawReq []byte, sink *ResponseSinkSend) → error`
// is wrapped to JSON-decode the request + provide a typed sink.
//
// JSON decode failures on the request surface as the
// canonical-bad-request error so the caller sees a typed mapping
// rather than an opaque internal.
func TypedServeStreaming[Req, Resp any](
	t *TypedMeshRpc,
	service string,
	handler TypedStreamingHandler[Req, Resp],
) (*ServeHandle, error) {
	shim := func(rawReq []byte, rawSink *ResponseSinkSend) error {
		req, err := jsonDecodeTyped[Req](rawReq)
		if err != nil {
			return fmt.Errorf("nrpc:bad_request: decode failed: %w", err)
		}
		typedSink := &TypedResponseSink[Resp]{raw: rawSink}
		return handler(req, typedSink)
	}
	return t.raw.ServeStreaming(service, shim)
}

// =====================================================================
// Observer + metrics (S1-D5).
//
// SetObserver installs a process-global C-ABI dispatcher (Go's
// `RpcObserverFn` shim) on first use, then enables the observer on
// the given *TypedMeshRpc. Pass nil to clear.
//
// Per locked decision #1, the observer fires synchronously from the
// substrate dispatch path. Callbacks must be cheap — push into a
// channel; do not block on I/O, locks, or syscalls.
// =====================================================================

// RpcCallStatus is a sealed-interface tagged union for an observed
// RPC call's outcome. Match with a type switch:
//
//	switch s := evt.Status.(type) {
//	case RpcCallStatusOk:            ...
//	case RpcCallStatusError:         ... // s.Message available
//	case RpcCallStatusTimeout:       ...
//	case RpcCallStatusCanceled:      ...
//	}
type RpcCallStatus interface {
	// isRpcCallStatus is an unexported method that pins the
	// sealed-union shape: only types in this package can implement
	// RpcCallStatus, so a future variant additions surfaces as a
	// build-time type-switch exhaustiveness check at user call
	// sites.
	isRpcCallStatus()
}

// RpcCallStatusOk indicates the callee returned a successful
// response.
type RpcCallStatusOk struct{}

// RpcCallStatusError indicates the server returned a typed error
// or a transport-level failure surfaced before the response could
// be parsed.
type RpcCallStatusError struct{ Message string }

// RpcCallStatusTimeout indicates the caller's deadline elapsed
// before the response arrived.
type RpcCallStatusTimeout struct{}

// RpcCallStatusCanceled indicates the call was dropped before
// completion (future drop / cancel-token trip). Reserved — not
// yet emitted by v1.
type RpcCallStatusCanceled struct{}

func (RpcCallStatusOk) isRpcCallStatus()       {}
func (RpcCallStatusError) isRpcCallStatus()    {}
func (RpcCallStatusTimeout) isRpcCallStatus()  {}
func (RpcCallStatusCanceled) isRpcCallStatus() {}

// RpcDirection is the boundary direction relative to the local
// node. v1 only emits Outbound.
type RpcDirection string

const (
	RpcDirectionOutbound RpcDirection = "outbound"
	RpcDirectionInbound  RpcDirection = "inbound"
)

// RpcCallEvent is the typed observer payload surfaced to handlers
// installed via SetObserver. Mirrors the substrate's RpcCallEvent
// field-by-field — the tagged-union Status discriminator is
// reconstructed from the C ABI's flat status_kind + status_message
// pair.
type RpcCallEvent struct {
	Caller        uint64
	Callee        uint64
	Method        string
	LatencyMs     uint32
	Status        RpcCallStatus
	RequestBytes  uint32
	ResponseBytes uint32
	Direction     RpcDirection
	TsUnixMs      uint64
}

// ObserverFunc is the user-facing observer signature. The handler
// fires once per completed outbound RPC.
//
// Per locked decision #1, the handler runs synchronously on the
// substrate dispatch thread. Implementations MUST be cheap — push
// into a channel, do not block on I/O.
type ObserverFunc func(evt RpcCallEvent)

// Global observer state. The C ABI dispatcher is registered once
// (sync.Once); per-*TypedMeshRpc enabling toggles the substrate-
// side observer slot.
var (
	observerDispatcherOnce sync.Once
	// Atomic-ptr to the current observer. Multiple meshes share the
	// same dispatcher; the Go side fans out by invoking the
	// currently-installed func. A nil load means "no observer
	// installed" — the dispatcher short-circuits.
	currentObserver atomic.Pointer[ObserverFunc]
)

//export go_net_rpc_observer_trampoline
func go_net_rpc_observer_trampoline(evt *C.RpcCallEventC) {
	cb := currentObserver.Load()
	if cb == nil || *cb == nil {
		return
	}
	(*cb)(rpcCallEventFromC(evt))
}

func rpcCallEventFromC(evt *C.RpcCallEventC) RpcCallEvent {
	var status RpcCallStatus
	switch evt.status_kind {
	case C.uint8_t(C.NET_RPC_STATUS_OK_C):
		status = RpcCallStatusOk{}
	case C.uint8_t(C.NET_RPC_STATUS_ERROR_C):
		msg := ""
		if evt.status_message_ptr != nil && evt.status_message_len > 0 {
			msg = string(C.GoBytes(
				unsafe.Pointer(evt.status_message_ptr),
				C.int(evt.status_message_len),
			))
		}
		status = RpcCallStatusError{Message: msg}
	case C.uint8_t(C.NET_RPC_STATUS_TIMEOUT_C):
		status = RpcCallStatusTimeout{}
	case C.uint8_t(C.NET_RPC_STATUS_CANCELED_C):
		status = RpcCallStatusCanceled{}
	default:
		status = RpcCallStatusError{Message: fmt.Sprintf("unknown:%d", evt.status_kind)}
	}
	method := ""
	if evt.method_ptr != nil && evt.method_len > 0 {
		method = string(C.GoBytes(
			unsafe.Pointer(evt.method_ptr),
			C.int(evt.method_len),
		))
	}
	direction := RpcDirectionOutbound
	if evt.direction == C.uint8_t(C.NET_RPC_DIRECTION_INBOUND_C) {
		direction = RpcDirectionInbound
	}
	return RpcCallEvent{
		Caller:        uint64(evt.caller),
		Callee:        uint64(evt.callee),
		Method:        method,
		LatencyMs:     uint32(evt.latency_ms),
		Status:        status,
		RequestBytes:  uint32(evt.request_bytes),
		ResponseBytes: uint32(evt.response_bytes),
		Direction:     direction,
		TsUnixMs:      uint64(evt.ts_unix_ms),
	}
}

// SetObserver installs (pass a non-nil callback) or clears (pass
// nil) the caller-side nRPC observer on the given *TypedMeshRpc.
// The handler fires once per completed outbound RPC.
//
// **Callback contract (locked decision #1).** The handler runs
// synchronously on the substrate dispatch thread. It MUST be
// cheap — push into a channel; do not block on I/O, locks, or
// syscalls. A slow handler stalls the substrate's hot path.
//
// **Process-global dispatcher.** The C ABI side has a single
// dispatcher per process. The Go binding's first call to
// SetObserver registers a trampoline that fans out to the most-
// recently-installed callback; subsequent SetObserver calls on
// any *TypedMeshRpc replace the active callback. Multiple meshes
// installing different observers in the same process is
// supported, but only the most recent observer fires.
//
// v1 emits only `Direction == RpcDirectionOutbound` events; the
// server-side hook is a planned follow-up.
func SetObserver(t *TypedMeshRpc, observer ObserverFunc) error {
	if observer == nil {
		// Clear: load nil to short-circuit the trampoline, then
		// disable on the substrate side.
		currentObserver.Store(nil)
		return setObserverOnMesh(t, false)
	}
	// Register the dispatcher exactly once per process.
	observerDispatcherOnce.Do(func() {
		C.net_rpc_set_observer_dispatcher(
			C.RpcObserverFn(C.go_net_rpc_observer_trampoline),
		)
	})
	// Store the callback (heap-stable so we can atomic.Pointer it).
	cb := observer
	currentObserver.Store(&cb)
	return setObserverOnMesh(t, true)
}

func setObserverOnMesh(t *TypedMeshRpc, enabled bool) error {
	flag := C.int(0)
	if enabled {
		flag = C.int(1)
	}
	var code C.int
	if err := t.raw.withHandle(func(h *C.MeshRpcHandle) {
		code = C.net_rpc_observer_install(h, flag)
	}); err != nil {
		return err
	}
	if code != 0 {
		return fmt.Errorf("nrpc: observer install failed (code=%d)", int(code))
	}
	return nil
}

// =====================================================================
// Metrics snapshot.
// =====================================================================

// ServiceMetrics holds the per-service caller + server-side
// counters surfaced by MetricsSnapshot. Decoded from the JSON
// payload returned by `net_rpc_metrics_snapshot`.
type ServiceMetrics struct {
	Service string `json:"service"`
	// Caller-side
	CallsTotal      uint64 `json:"calls_total"`
	ErrorsNoRoute   uint64 `json:"errors_no_route"`
	ErrorsTimeout   uint64 `json:"errors_timeout"`
	ErrorsServer    uint64 `json:"errors_server"`
	ErrorsTransport uint64 `json:"errors_transport"`
	InFlight        int64  `json:"in_flight"`
	LatencySumNs    uint64 `json:"latency_sum_ns"`
	LatencyCount    uint64 `json:"latency_count"`
	// Cumulative bucket counts. Index `i` corresponds to the
	// substrate's `DEFAULT_LATENCY_BUCKETS_SECS[i]`; last entry
	// is the `+Inf` bucket.
	LatencyBuckets []uint64 `json:"latency_buckets"`
	// Server-side
	HandlerInvocationsTotal     uint64   `json:"handler_invocations_total"`
	HandlerPanicsTotal          uint64   `json:"handler_panics_total"`
	HandlerInFlight             int64    `json:"handler_in_flight"`
	HandlerDurationSumNs        uint64   `json:"handler_duration_sum_ns"`
	HandlerDurationCount        uint64   `json:"handler_duration_count"`
	HandlerDurationBuckets      []uint64 `json:"handler_duration_buckets"`
	StreamingChunksEmittedTotal uint64   `json:"streaming_chunks_emitted_total"`
	StreamingChunksDroppedTotal uint64   `json:"streaming_chunks_dropped_total"`
	CapabilityDeniedTotal       uint64   `json:"capability_denied_total"`
}

// RpcMetricsSnapshot is the snapshot of the per-service nRPC
// metrics registry. Returned by MetricsSnapshot.
type RpcMetricsSnapshot struct {
	// One entry per service that has been called at least once
	// since the mesh was created. Sorted by service name.
	Services []ServiceMetrics `json:"services"`
}

// MetricsSnapshot returns the per-service nRPC counters as of the
// time of the call. Cheap on the substrate side (one DashMap
// iteration); safe to call on every Prometheus scrape.
//
// The C ABI returns the snapshot as a JSON document; we decode
// into the typed RpcMetricsSnapshot for the user.
func MetricsSnapshot(t *TypedMeshRpc) (*RpcMetricsSnapshot, error) {
	var (
		outPtr *C.uint8_t
		outLen C.size_t
		outErr *C.char
		code   C.int
	)
	if err := t.raw.withHandle(func(h *C.MeshRpcHandle) {
		code = C.net_rpc_metrics_snapshot(h, &outPtr, &outLen, &outErr)
	}); err != nil {
		return nil, err
	}
	if code != 0 {
		msg := "metrics snapshot failed"
		if outErr != nil {
			msg = C.GoString(outErr)
			C.net_rpc_free_cstring(outErr)
		}
		return nil, errors.New(msg)
	}
	if outPtr == nil || outLen == 0 {
		return &RpcMetricsSnapshot{Services: nil}, nil
	}
	bytes := C.GoBytes(unsafe.Pointer(outPtr), C.int(outLen))
	C.net_rpc_response_free(outPtr, outLen)
	var snap RpcMetricsSnapshot
	if err := json.Unmarshal(bytes, &snap); err != nil {
		return nil, &RpcError{Kind: RpcKindCodecDecode, Message: err.Error()}
	}
	return &snap, nil
}
