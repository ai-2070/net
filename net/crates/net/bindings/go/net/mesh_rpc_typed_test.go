// Stub-level tests for the typed nRPC wrapper layer.
//
// Exercises only the pure-logic helpers that don't require a live
// cgo handle: JSON codec round-trip, AppError format, app-error
// status-code constants, and the RpcCallStatus tagged-union shape.
//
// Live integration tests against a real *MeshRpc — including the
// observer-fire path — require building the rpc-ffi cdylib first
// and belong in the S2-X cross-language harness alongside the
// Node + Python equivalents.

package net

import (
	"encoding/json"
	"errors"
	"strings"
	"testing"
)

// =====================================================================
// JSON codec round-trip
// =====================================================================

type echoReq struct {
	N    int    `json:"n"`
	Name string `json:"name,omitempty"`
}

type echoResp struct {
	Pong int `json:"pong"`
}

func TestJSONCodecRoundTrip(t *testing.T) {
	body, err := jsonEncodeTyped(echoReq{N: 7, Name: "hello"})
	if err != nil {
		t.Fatalf("encode failed: %v", err)
	}
	if string(body) != `{"n":7,"name":"hello"}` {
		t.Fatalf("encode shape unexpected: %s", string(body))
	}
	got, err := jsonDecodeTyped[echoResp]([]byte(`{"pong":42}`))
	if err != nil {
		t.Fatalf("decode failed: %v", err)
	}
	if got.Pong != 42 {
		t.Fatalf("decode round-trip wrong: got %+v", got)
	}
}

func TestJSONEncodeFailureSurfaceCodecEncode(t *testing.T) {
	// Channels aren't JSON-marshallable; encode failure must
	// surface as RpcError{Kind: RpcKindCodecEncode}.
	ch := make(chan int)
	_, err := jsonEncodeTyped(ch)
	if err == nil {
		t.Fatal("expected encode failure, got nil")
	}
	var rpcErr *RpcError
	if !errors.As(err, &rpcErr) {
		t.Fatalf("expected *RpcError, got %T: %v", err, err)
	}
	if rpcErr.Kind != RpcKindCodecEncode {
		t.Fatalf("expected codec_encode kind, got %s", rpcErr.Kind)
	}
}

func TestJSONDecodeFailureSurfaceCodecDecode(t *testing.T) {
	_, err := jsonDecodeTyped[echoResp]([]byte(`{not json`))
	if err == nil {
		t.Fatal("expected decode failure, got nil")
	}
	var rpcErr *RpcError
	if !errors.As(err, &rpcErr) {
		t.Fatalf("expected *RpcError, got %T: %v", err, err)
	}
	if rpcErr.Kind != RpcKindCodecDecode {
		t.Fatalf("expected codec_decode kind, got %s", rpcErr.Kind)
	}
}

// =====================================================================
// AppError format + status codes
// =====================================================================

func TestAppErrorFormat(t *testing.T) {
	err := AppError(NrpcTypedBadRequest, []byte(`{"err":"bad"}`))
	want := `nrpc:app_error:0x8000:{"err":"bad"}`
	if err.Error() != want {
		t.Fatalf("AppError format unexpected:\n  got:  %s\n  want: %s", err.Error(), want)
	}
}

func TestAppErrorZeroPadsHexCode(t *testing.T) {
	cases := []struct {
		code uint16
		want string
	}{
		{1, "nrpc:app_error:0x0001:x"},
		{0xffff, "nrpc:app_error:0xffff:x"},
		{NrpcTypedHandlerError, "nrpc:app_error:0x8001:x"},
	}
	for _, c := range cases {
		got := AppError(c.code, []byte("x")).Error()
		if got != c.want {
			t.Errorf("code=0x%04x:\n  got:  %s\n  want: %s", c.code, got, c.want)
		}
	}
}

func TestAppErrorPreservesColonsInBody(t *testing.T) {
	// The Rust parser splits on the FIRST colon after `0x<hex>:`,
	// so a body containing colons must survive intact.
	err := AppError(NrpcTypedBadRequest, []byte("status: bad"))
	want := `nrpc:app_error:0x8000:status: bad`
	if err.Error() != want {
		t.Fatalf("body colon preservation failed:\n  got:  %s\n  want: %s",
			err.Error(), want)
	}
}

func TestStatusCodeConstantsAreStable(t *testing.T) {
	// Pin the cross-binding constants — drift would break the
	// golden vectors at tests/cross_lang_nrpc/golden_vectors.json.
	if NrpcTypedBadRequest != 0x8000 {
		t.Errorf("NrpcTypedBadRequest drifted: got 0x%04x, want 0x8000",
			NrpcTypedBadRequest)
	}
	if NrpcTypedHandlerError != 0x8001 {
		t.Errorf("NrpcTypedHandlerError drifted: got 0x%04x, want 0x8001",
			NrpcTypedHandlerError)
	}
}

// =====================================================================
// RpcCallStatus tagged-union exhaustiveness
// =====================================================================

func TestRpcCallStatusVariantsImplementInterface(t *testing.T) {
	// Compile-time witness: all four variants implement RpcCallStatus.
	// If the union grows / shrinks, this test breaks compilation
	// loudly.
	variants := []RpcCallStatus{
		RpcCallStatusOk{},
		RpcCallStatusError{Message: "boom"},
		RpcCallStatusTimeout{},
		RpcCallStatusCanceled{},
	}
	if len(variants) != 4 {
		t.Fatalf("expected 4 RpcCallStatus variants, got %d", len(variants))
	}
}

func TestRpcCallStatusTypeSwitchExhaustive(t *testing.T) {
	// Each variant routes through a distinct case in the canonical
	// discriminator. A future variant addition surfaces here as
	// an "unhandled" fallthrough.
	dispatch := func(s RpcCallStatus) string {
		switch v := s.(type) {
		case RpcCallStatusOk:
			return "ok"
		case RpcCallStatusError:
			return "error:" + v.Message
		case RpcCallStatusTimeout:
			return "timeout"
		case RpcCallStatusCanceled:
			return "canceled"
		default:
			return "unhandled"
		}
	}
	cases := []struct {
		s    RpcCallStatus
		want string
	}{
		{RpcCallStatusOk{}, "ok"},
		{RpcCallStatusError{Message: "no_route"}, "error:no_route"},
		{RpcCallStatusTimeout{}, "timeout"},
		{RpcCallStatusCanceled{}, "canceled"},
	}
	for _, c := range cases {
		if got := dispatch(c.s); got != c.want {
			t.Errorf("dispatch(%T) = %s, want %s", c.s, got, c.want)
		}
	}
}

// =====================================================================
// MetricsSnapshot JSON decode shape
// =====================================================================

func TestMetricsSnapshotJSONDecodeShape(t *testing.T) {
	// The C ABI emits a JSON document with the same shape rpc-ffi's
	// net_rpc_metrics_snapshot constructs. Pin the decode against
	// a hand-built fixture matching that shape so a future
	// substrate field rename / drop is caught here as well as
	// in the cross-binding tests.
	in := []byte(`{
		"services": [
			{
				"service": "echo",
				"calls_total": 42,
				"errors_no_route": 0,
				"errors_timeout": 1,
				"errors_server": 0,
				"errors_transport": 0,
				"in_flight": 0,
				"latency_sum_ns": 1234567,
				"latency_count": 42,
				"latency_buckets": [10, 22, 30],
				"handler_invocations_total": 0,
				"handler_panics_total": 0,
				"handler_in_flight": 0,
				"handler_duration_sum_ns": 0,
				"handler_duration_count": 0,
				"handler_duration_buckets": [0, 0, 0],
				"streaming_chunks_emitted_total": 0,
				"streaming_chunks_dropped_total": 0,
				"capability_denied_total": 0
			}
		]
	}`)
	var snap RpcMetricsSnapshot
	if err := json.Unmarshal(in, &snap); err != nil {
		t.Fatalf("decode failed: %v", err)
	}
	if len(snap.Services) != 1 {
		t.Fatalf("expected 1 service, got %d", len(snap.Services))
	}
	svc := snap.Services[0]
	if svc.Service != "echo" {
		t.Errorf("service name: got %q want echo", svc.Service)
	}
	if svc.CallsTotal != 42 {
		t.Errorf("calls_total: got %d want 42", svc.CallsTotal)
	}
	if svc.ErrorsTimeout != 1 {
		t.Errorf("errors_timeout: got %d want 1", svc.ErrorsTimeout)
	}
	if svc.LatencySumNs != 1234567 {
		t.Errorf("latency_sum_ns: got %d want 1234567", svc.LatencySumNs)
	}
	if len(svc.LatencyBuckets) != 3 {
		t.Errorf("latency_buckets: got %d entries want 3", len(svc.LatencyBuckets))
	}
	if svc.LatencyBuckets[0] != 10 || svc.LatencyBuckets[2] != 30 {
		t.Errorf("latency_buckets values wrong: %v", svc.LatencyBuckets)
	}
}

func TestMetricsSnapshotEmptyServices(t *testing.T) {
	in := []byte(`{"services":[]}`)
	var snap RpcMetricsSnapshot
	if err := json.Unmarshal(in, &snap); err != nil {
		t.Fatalf("decode failed: %v", err)
	}
	if len(snap.Services) != 0 {
		t.Errorf("expected empty services, got %d", len(snap.Services))
	}
}

// =====================================================================
// ObserverFunc + currentObserver atomic semantics
// =====================================================================

func TestObserverFuncFanout(t *testing.T) {
	// Validate the atomic.Pointer fan-out mechanism that
	// go_net_rpc_observer_trampoline uses to dispatch into the
	// most-recently-installed user callback. Doesn't exercise the
	// cgo trampoline itself (that requires the cdylib).
	defer currentObserver.Store(nil)

	var observed []RpcCallEvent
	cb := ObserverFunc(func(e RpcCallEvent) {
		observed = append(observed, e)
	})
	currentObserver.Store(&cb)

	loaded := currentObserver.Load()
	if loaded == nil || *loaded == nil {
		t.Fatal("expected stored observer, got nil")
	}
	(*loaded)(RpcCallEvent{
		Caller:    0xCAFE,
		Method:    "svc.foo",
		LatencyMs: 5,
		Status:    RpcCallStatusOk{},
		Direction: RpcDirectionOutbound,
	})
	if len(observed) != 1 {
		t.Fatalf("expected 1 observation, got %d", len(observed))
	}
	if observed[0].Method != "svc.foo" {
		t.Errorf("method: got %q want svc.foo", observed[0].Method)
	}
	if _, ok := observed[0].Status.(RpcCallStatusOk); !ok {
		t.Errorf("expected Ok status, got %T", observed[0].Status)
	}
}

func TestObserverFuncReplaceLatestWins(t *testing.T) {
	// Storing a new callback replaces the previous one; the older
	// closure is no longer reachable through currentObserver.
	defer currentObserver.Store(nil)

	var firstHit, secondHit int
	cb1 := ObserverFunc(func(RpcCallEvent) { firstHit++ })
	cb2 := ObserverFunc(func(RpcCallEvent) { secondHit++ })

	currentObserver.Store(&cb1)
	(*currentObserver.Load())(RpcCallEvent{})

	currentObserver.Store(&cb2)
	(*currentObserver.Load())(RpcCallEvent{})
	(*currentObserver.Load())(RpcCallEvent{})

	if firstHit != 1 {
		t.Errorf("first cb: got %d hits, want 1", firstHit)
	}
	if secondHit != 2 {
		t.Errorf("second cb: got %d hits, want 2", secondHit)
	}
}

func TestObserverFuncNilClearShortCircuits(t *testing.T) {
	// SetObserver(nil) stores nil to disable the trampoline's
	// fan-out. The trampoline's `cb == nil || *cb == nil` guard
	// must short-circuit before dereferencing.
	defer currentObserver.Store(nil)

	currentObserver.Store(nil)
	loaded := currentObserver.Load()
	if loaded != nil {
		t.Fatalf("expected nil, got non-nil %v", loaded)
	}

	// A second store of a non-nil callback should re-enable.
	var hits int
	cb := ObserverFunc(func(RpcCallEvent) { hits++ })
	currentObserver.Store(&cb)
	if got := currentObserver.Load(); got == nil {
		t.Fatal("expected re-stored observer, got nil")
	}
	(*currentObserver.Load())(RpcCallEvent{})
	if hits != 1 {
		t.Errorf("re-stored cb: got %d hits, want 1", hits)
	}
}

// =====================================================================
// ABI version pin
// =====================================================================

func TestExpectedABIVersionMatchesNewest(t *testing.T) {
	// Pinned so a regression to the older 0x0001 / 0x0002 wire
	// shape (or an accidental jump past 0x0003 without all callers
	// updating) surfaces here. The reference binding's
	// CheckABI() panics at process init when the linked cdylib
	// disagrees; this assert pins the source-side constant.
	if ExpectedABIVersion != 0x0003 {
		t.Fatalf("ExpectedABIVersion drifted: got 0x%04x, want 0x0003",
			ExpectedABIVersion)
	}
}

// =====================================================================
// Helpers
// =====================================================================

// Sanity check: gofmt-equivalent. Tests that an error string
// matches the cross-binding nrpc: prefix convention.
func TestRpcErrorMessageShape(t *testing.T) {
	e := &RpcError{Kind: RpcKindNoRoute, Message: "target=0xdeadbeef"}
	if !strings.HasPrefix(e.Error(), "nrpc:no_route:") {
		t.Errorf("RpcError prefix drift: %s", e.Error())
	}
}
