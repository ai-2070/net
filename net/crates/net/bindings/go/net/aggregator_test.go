// Aggregator-registry + fold-query binding tests — exercise the
// surface declared in aggregator.go.
//
// # Run prerequisites
//
// This binding tier (`bindings/go/net/`) ships without a `go.mod`
// — it's the reference implementation for downstream Go consumers.
// To run these tests, wire the directory into your own go.mod
// (see `redex.go` for the cdylib build prerequisite + CGO_LDFLAGS
// notes). The cdylib must be built from the main `net-mesh` crate
// (the aggregator symbols live in `src/ffi/aggregator.rs`):
//
//	cargo build --release --features net
//	export CGO_LDFLAGS="-L$(pwd)/target/release -lnet"
//
// # Coverage summary
//
//   - TestRegistryClientErrorRendering — Error() format + kind.
//   - TestFoldQueryClientErrorRendering — same for fold-query.
//   - TestRegistryErrFromKindMapping — every C kind discriminant
//     maps to the documented kebab-case string.
//   - TestFoldQueryErrFromKindMapping — same for fold-query kinds.
//   - TestNilMeshHandleRejected — defensive constructor guard.
//
// Round-trip tests against `net-aggregator-daemon` live downstream
// in the consumer go.mod (boot the binary, capture pubkey, invoke
// against a real RegistryClient). Those rely on the daemon binary
// being on PATH, which the upstream `net` repo's CI doesn't
// provide.

package net

// cgo directives are disallowed inside `_test.go` files, so this
// test exercises `registryErrFromKind` / `foldQueryErrFromKind`
// through their Go-typed `int32` signatures + the `netRegistry*`
// constants declared in `aggregator.go`. The constants mirror the
// C `#define`s value-for-value and are themselves part of the
// public ABI contract — any drift fails both sides in lockstep.

import (
	"errors"
	"strings"
	"testing"
	"unsafe"
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
	// Go binding should surface it as `unknown-N` rather than
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
	// Smoke test: a manually-fabricated zero-value client (handle
	// == nil) can be closed repeatedly without panicking. The
	// real lifecycle still goes through NewRegistryClient + Close
	// — this just guards against a regression where Close()
	// crashes on the nil sentinel state Set by a previous Close.
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
	rc := &RegistryClient{}
	if _, err := rc.List(nil, 0); !errors.Is(err, ErrAggregatorHandleClosed) {
		t.Errorf("List on nil-handle = %v, want ErrAggregatorHandleClosed", err)
	}
	if _, err := rc.Spawn(nil, 0, "tpl", "grp", 1); !errors.Is(err, ErrAggregatorHandleClosed) {
		t.Errorf("Spawn on nil-handle = %v, want ErrAggregatorHandleClosed", err)
	}
	if _, err := rc.Unregister(nil, 0, "grp"); !errors.Is(err, ErrAggregatorHandleClosed) {
		t.Errorf("Unregister on nil-handle = %v, want ErrAggregatorHandleClosed", err)
	}

	fq := &FoldQueryClient{}
	if _, err := fq.QueryLatest(nil, 0, 0); !errors.Is(err, ErrAggregatorHandleClosed) {
		t.Errorf("QueryLatest on nil-handle = %v, want ErrAggregatorHandleClosed", err)
	}
	if _, err := fq.QuerySummarizeNow(nil, 0, 0); !errors.Is(err, ErrAggregatorHandleClosed) {
		t.Errorf("QuerySummarizeNow on nil-handle = %v, want ErrAggregatorHandleClosed", err)
	}
}

// _ keeps unsafe.Pointer in scope as a documented dependency of
// the public API — go vet otherwise flags the import as unused
// when only the constructor signatures reference it.
var _ unsafe.Pointer
