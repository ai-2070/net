// ABI-stability cgo round-trip test. Paired with
// `abi_stability_cgo.go` — same `abi_stability` build tag — so
// the cgo helper function and the test that drives it compile
// together only when invoked via `go test -tags abi_stability`.
//
// Why the tag: cgo directives are disallowed inside `_test.go`
// files, so the helper must live in a regular `.go` file. We
// don't want the helper shipped in production binaries of the
// Go SDK, so it's tag-gated. We use a distinct `abi_stability`
// tag rather than reusing `test_helpers` — the latter pulls
// in groups-test FFI symbols that require a feature-enabled
// Rust cdylib, which would needlessly entangle the ABI test
// with an unrelated build matrix.
//
// Corresponds to TEST_COVERAGE_PLAN §P3-15.

//go:build abi_stability

package net

import (
	"errors"
	"math"
	"testing"
	"unsafe"
)

// TestABIStabilityU64FFIRoundTrip exercises the actual cgo
// boundary: we pass Go `uint64` values through the
// `abi_stability_u64_roundtrip` cgo helper declared with
// `uint64_t` on both sides, and assert the value comes back
// bit-identical. Catches regressions that a pure-Go constant
// comparison cannot — e.g. a refactor that accidentally
// narrows the Go-side type, or a toolchain regression that
// picks a 32-bit `C.uint64_t` on a niche target. 0 and
// math.MaxUint64 are the boundary values SDK callers
// actually hit (node IDs, origins, timestamps).
func TestABIStabilityU64FFIRoundTrip(t *testing.T) {
	// Size pin: a `uint64` must be 8 bytes on the Go side. A
	// toolchain narrowing would fail this immediately — and
	// then the round-trip assertions below would corroborate
	// by truncating bits on transit.
	if got := unsafe.Sizeof(uint64(0)); got != 8 {
		t.Fatalf("Go uint64 size = %d, want 8", got)
	}

	cases := []struct {
		name string
		in   uint64
	}{
		{"zero", 0},
		{"one", 1},
		{"u32_max_plus_one", uint64(math.MaxUint32) + 1},
		{"mid_range_pattern", 0xDEAD_BEEF_CAFE_F00D},
		{"u64_max", math.MaxUint64},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			out := abiStabilityU64Roundtrip(tc.in)
			if out != tc.in {
				t.Fatalf(
					"FFI round-trip drifted bits: in=%#x out=%#x — "+
						"cgo uint64_t marshaling lost precision "+
						"(likely narrowed to signed int64)",
					tc.in, out,
				)
			}
		})
	}
}

// TestABIStabilityStreamOccupied pins the stream-inbox refusal end to end:
// the header's value, its mapping to the Go sentinel, and the sentinel's
// text. A renumbered constant or a dropped `case` would otherwise surface
// as "mesh unknown error (code -109)" at runtime.
func TestABIStabilityStreamOccupied(t *testing.T) {
	if got := abiStabilityStreamOccupiedCode(); got != -109 {
		t.Fatalf("NET_ERR_MESH_STREAM_OCCUPIED = %d, want -109", got)
	}
	if err := abiStabilityMeshError(-109); !errors.Is(err, ErrStreamOccupied) {
		t.Fatalf("code -109 maps to %v, want ErrStreamOccupied", err)
	}
	const want = "stream already has a receiver"
	if got := ErrStreamOccupied.Error(); got != want {
		t.Fatalf("ErrStreamOccupied = %q, want %q", got, want)
	}
}
