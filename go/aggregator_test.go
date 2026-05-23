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
