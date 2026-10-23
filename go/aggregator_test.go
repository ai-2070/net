// Aggregator-registry + fold-query client tests.
//
// Cgo-light coverage: error rendering, kind discriminant mapping,
// nil-handle defense, idempotent Close. Daemon-driven round-trip
// tests are deferred to the integration suite — they need the
// `net-aggregator-daemon` binary on PATH plus the
// `RUN_INTEGRATION_TESTS=1` env that the integration_test.go
// fixture honors.

package net

// cgo directives are disallowed inside `_test.go` files, so this
// test exercises `registryErrFromKind` / `foldQueryErrFromKind`
// through their Go-typed `int32` signatures + the `netRegistry*`
// constants declared in `aggregator.go`. The constants mirror the
// C `#define`s value-for-value and are themselves part of the
// public ABI contract — any drift fails both sides in lockstep.

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"
)

func TestRegistryClientErrorRendering(t *testing.T) {
	for _, tt := range []struct {
		err  *RegistryClientError
		want string
	}{
		{
			&RegistryClientError{Kind: RegistryErrKindUnknownTemplate, Detail: "reservation-v2"},
			"agg:unknown-template: reservation-v2",
		},
		{
			&RegistryClientError{Kind: RegistryErrKindTransport, Detail: ""},
			"agg:transport",
		},
		{
			&RegistryClientError{Kind: RegistryErrKindCodec, Detail: "bad bytes"},
			"agg:codec: bad bytes",
		},
	} {
		if got := tt.err.Error(); got != tt.want {
			t.Errorf("Error() = %q, want %q", got, tt.want)
		}
	}
}

func TestFoldQueryClientErrorRendering(t *testing.T) {
	for _, tt := range []struct {
		err  *FoldQueryClientError
		want string
	}{
		{
			&FoldQueryClientError{Kind: FoldQueryErrKindUnknownKind, Detail: "0x0042"},
			"agg:unknown-kind: 0x0042",
		},
		{
			&FoldQueryClientError{Kind: FoldQueryErrKindTransport, Detail: ""},
			"agg:transport",
		},
	} {
		if got := tt.err.Error(); got != tt.want {
			t.Errorf("Error() = %q, want %q", got, tt.want)
		}
	}
}

func TestRegistryErrFromKindMapping(t *testing.T) {
	for _, tt := range []struct {
		raw  int32
		want RegistryErrorKind
	}{
		{netRegistryErrTransport, RegistryErrKindTransport},
		{netRegistryErrCodec, RegistryErrKindCodec},
		{netRegistryErrUnknownTemplate, RegistryErrKindUnknownTemplate},
		{netRegistryErrDuplicateGroupName, RegistryErrKindDuplicateGroupName},
		{netRegistryErrSpawnRejected, RegistryErrKindSpawnRejected},
		{netRegistryErrSpawnNotSupported, RegistryErrKindSpawnNotSupported},
		// 8-11 were declared in the C ABI but unmapped, so an operator
		// refusal surfaced as "unknown-11".
		{netRegistryErrUnknownGroup, RegistryErrKindUnknownGroup},
		{netRegistryErrScaleRejected, RegistryErrKindScaleRejected},
		{netRegistryErrScaleNotSupported, RegistryErrKindScaleNotSupported},
		{netRegistryErrUnauthorized, RegistryErrKindUnauthorized},
		{netRegistryErrInvalidArgs, RegistryErrKindInvalidArgs},
	} {
		got := registryErrFromKind(tt.raw, "detail")
		if got.Kind != tt.want {
			t.Errorf("raw=%d: kind = %q, want %q", tt.raw, got.Kind, tt.want)
		}
		if got.Detail != "detail" {
			t.Errorf("raw=%d: detail = %q, want %q", tt.raw, got.Detail, "detail")
		}
	}
}

func TestFoldQueryErrFromKindMapping(t *testing.T) {
	for _, tt := range []struct {
		raw  int32
		want FoldQueryErrorKind
	}{
		{netRegistryErrTransport, FoldQueryErrKindTransport},
		{netRegistryErrCodec, FoldQueryErrKindCodec},
		{netRegistryErrUnknownKind, FoldQueryErrKindUnknownKind},
		{netRegistryErrInvalidArgs, FoldQueryErrKindInvalidArgs},
		// fold.query.v2 (AGGREGATOR_MULTI_HOST_PLAN.md Q4).
		{netRegistryErrUnknownGroup, FoldQueryErrKindUnknownGroup},
		{netRegistryErrUnauthorized, FoldQueryErrKindUnauthorized},
		{netRegistryErrServerTooOld, FoldQueryErrKindServerTooOld},
	} {
		got := foldQueryErrFromKind(tt.raw, "detail")
		if got.Kind != tt.want {
			t.Errorf("raw=%d: kind = %q, want %q", tt.raw, got.Kind, tt.want)
		}
	}
}

func TestUnknownErrKindFallsThrough(t *testing.T) {
	// A future C ABI release might add a new discriminant; the
	// Go binding must surface it as `unknown-N` rather than
	// silently mapping it to an existing variant.
	got := registryErrFromKind(int32(123), "future")
	if !strings.HasPrefix(string(got.Kind), "unknown-") {
		t.Errorf("expected unknown-N fallback, got %q", got.Kind)
	}

	gotFold := foldQueryErrFromKind(int32(123), "future")
	if !strings.HasPrefix(string(gotFold.Kind), "unknown-") {
		t.Errorf("expected unknown-N fallback, got %q", gotFold.Kind)
	}
}

func TestNilMeshHandleRejected(t *testing.T) {
	_, err := NewRegistryClient(nil)
	if err == nil {
		t.Fatal("expected error for nil mesh handle, got nil")
	}
	var regErr *RegistryClientError
	if !errors.As(err, &regErr) {
		t.Fatalf("expected *RegistryClientError, got %T", err)
	}
	if regErr.Kind != RegistryErrKindInvalidArgs {
		t.Errorf("kind = %q, want %q", regErr.Kind, RegistryErrKindInvalidArgs)
	}

	_, err = NewFoldQueryClient(nil)
	if err == nil {
		t.Fatal("expected error for nil mesh handle, got nil")
	}
	var fqErr *FoldQueryClientError
	if !errors.As(err, &fqErr) {
		t.Fatalf("expected *FoldQueryClientError, got %T", err)
	}
	if fqErr.Kind != FoldQueryErrKindInvalidArgs {
		t.Errorf("kind = %q, want %q", fqErr.Kind, FoldQueryErrKindInvalidArgs)
	}
}

func TestCloseIsIdempotent(t *testing.T) {
	rc := &RegistryClient{}
	if err := rc.Close(); err != nil {
		t.Errorf("first Close on nil-handle = %v, want nil", err)
	}
	if err := rc.Close(); err != nil {
		t.Errorf("second Close on nil-handle = %v, want nil", err)
	}

	fq := &FoldQueryClient{}
	if err := fq.Close(); err != nil {
		t.Errorf("first Close on nil-handle = %v, want nil", err)
	}
	if err := fq.Close(); err != nil {
		t.Errorf("second Close on nil-handle = %v, want nil", err)
	}
}

func TestClosedClientReturnsHandleClosedError(t *testing.T) {
	ctx := context.Background()
	rc := &RegistryClient{}
	if _, err := rc.List(ctx, 0); !errors.Is(err, ErrAggregatorHandleClosed) {
		t.Errorf("List on nil-handle = %v, want ErrAggregatorHandleClosed", err)
	}
	if _, err := rc.Spawn(ctx, 0, "tpl", "grp", 1); !errors.Is(err, ErrAggregatorHandleClosed) {
		t.Errorf("Spawn on nil-handle = %v, want ErrAggregatorHandleClosed", err)
	}
	if _, err := rc.Unregister(ctx, 0, "grp"); !errors.Is(err, ErrAggregatorHandleClosed) {
		t.Errorf("Unregister on nil-handle = %v, want ErrAggregatorHandleClosed", err)
	}

	fq := &FoldQueryClient{}
	if _, err := fq.QueryLatest(ctx, 0, 0); !errors.Is(err, ErrAggregatorHandleClosed) {
		t.Errorf("QueryLatest on nil-handle = %v, want ErrAggregatorHandleClosed", err)
	}
	if _, err := fq.QuerySummarizeNow(ctx, 0, 0); !errors.Is(err, ErrAggregatorHandleClosed) {
		t.Errorf("QuerySummarizeNow on nil-handle = %v, want ErrAggregatorHandleClosed", err)
	}
	if _, err := fq.QueryGroupLatest(ctx, 0, 0, "g"); !errors.Is(err, ErrAggregatorHandleClosed) {
		t.Errorf("QueryGroupLatest on nil-handle = %v, want ErrAggregatorHandleClosed", err)
	}
	if _, err := fq.QueryGroupSummarizeNow(ctx, 0, 0, ""); !errors.Is(err, ErrAggregatorHandleClosed) {
		t.Errorf("QueryGroupSummarizeNow on nil-handle = %v, want ErrAggregatorHandleClosed", err)
	}

	// `InvalidateCache` / `InvalidateTarget` are no-ops on a
	// nil-handle client (the C call is guarded). Just confirm
	// they don't panic.
	fq.InvalidateCache()
	fq.InvalidateTarget(0)
}

// _ keeps `time.Duration` referenced even when the deadline
// path is opaque to the test reader — go vet would otherwise
// flag this import as unused under refactors that move
// `WithDeadline` somewhere else.
var _ = time.Second

// SDK audit 2026-10-11: the constructors took an unsafe.Pointer, and the
// only one a caller outside this package could supply was
// unsafe.Pointer(node) — a Go struct Rust then read as the native mesh
// handle. They now take the *MeshNode and pass its native handle.
func TestAggregatorClientsAreBuiltFromAMeshNode(t *testing.T) {
	node := newMeshForCaps(t)
	reg, err := NewRegistryClient(node)
	if err != nil {
		t.Fatalf("NewRegistryClient(live node): %v", err)
	}
	_ = reg.Close()
	fq, err := NewFoldQueryClient(node)
	if err != nil {
		t.Fatalf("NewFoldQueryClient(live node): %v", err)
	}
	_ = fq.Close()

	node.Shutdown()
	if _, err := NewRegistryClient(node); err == nil {
		t.Fatal("a shut-down node must be refused")
	}
	if _, err := NewFoldQueryClient(node); err == nil {
		t.Fatal("a shut-down node must be refused")
	}
}

// fold.query.v2 from Go (AGGREGATOR_MULTI_HOST_PLAN.md Q4), live. Serving
// it is the aggregator daemon's job, so the call goes to a connected peer
// that serves no fold query.
//
// Every Go-built node installs a channel registry, and RPC reply channels
// are per service (`<service>.replies.<origin>`). So the caller's
// reply-channel subscribe for the unserved service is refused before the
// request is answered. The call fails as a typed `transport` error that
// says so, not as `server-too-old`; that kind needs a registry-less peer
// (every pre-Q1 daemon), which Go cannot build. The Node and Python
// bindings witness it against one.
func TestQueryGroupAgainstAPeerWithoutV2IsATypedTransportError(t *testing.T) {
	a, b, cleanup := meshHandshakePair(t)
	defer cleanup()
	fq, err := NewFoldQueryClient(a)
	if err != nil {
		t.Fatalf("NewFoldQueryClient: %v", err)
	}
	defer fq.Close()
	fq.WithDeadline(3 * time.Second).WithTTL(0)

	ctx := context.Background()
	for name, call := range map[string]func() ([]SummaryRecord, error){
		"QueryGroupLatest(named)": func() ([]SummaryRecord, error) {
			return fq.QueryGroupLatest(ctx, b.NodeID(), 1, "some-group")
		},
		"QueryGroupSummarizeNow(all)": func() ([]SummaryRecord, error) {
			return fq.QueryGroupSummarizeNow(ctx, b.NodeID(), 1, "")
		},
	} {
		records, err := call()
		if err == nil {
			t.Fatalf("%s: want an error from a peer without fold.query.v2, got %v", name, records)
		}
		var fqErr *FoldQueryClientError
		if !errors.As(err, &fqErr) {
			t.Fatalf("%s: want *FoldQueryClientError, got %T (%v)", name, err, err)
		}
		if fqErr.Kind != FoldQueryErrKindTransport {
			t.Errorf("%s: kind = %q, want %q", name, fqErr.Kind, FoldQueryErrKindTransport)
		}
		if !strings.Contains(fqErr.Detail, "reply-channel subscribe rejected") {
			t.Errorf("%s: detail should say the reply-channel subscribe was refused: %q", name, fqErr.Detail)
		}
	}
}
