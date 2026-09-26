// cgo helper exposed solely for the ABI-stability test in
// `abi_stability_cgo_test.go` (TEST_COVERAGE_PLAN §P3-15). Go
// disallows cgo directives inside `_test.go` files, so the
// helper lives in a regular `.go` file tag-gated on
// `abi_stability` — a distinct tag from `test_helpers` so it
// doesn't pull in the groups test-helper FFI symbols — and
// the shipped Go SDK (built without this tag) doesn't carry
// it.
//
// The helper's job is to give the test a known-typed cgo
// round-trip entry point: it receives and returns `uint64_t`,
// so a test that sends 0 / `math.MaxUint64` in and gets the
// same value out proves cgo's Go↔C type marshaling preserves
// 64 bits in both directions. A regression that narrows
// `C.uint64_t` to a signed 32-bit type would either fail to
// compile or truncate at the boundary.

//go:build abi_stability

package net

/*
#include <stdint.h>
#include "net.h"

static uint64_t abi_stability_u64_roundtrip(uint64_t v) { return v; }
*/
import "C"

// abiStabilityU64Roundtrip passes `v` through the cgo boundary
// (Go uint64 → C.uint64_t → C uint64_t → Go uint64) and returns
// the result. Called only from the ABI-stability test.
func abiStabilityU64Roundtrip(v uint64) uint64 {
	return uint64(C.abi_stability_u64_roundtrip(C.uint64_t(v)))
}

// abiStabilityStreamOccupiedCode is NET_ERR_MESH_STREAM_OCCUPIED as the C
// header declares it, so a test can pin the header's value and the Go
// mapping against each other.
func abiStabilityStreamOccupiedCode() int {
	return int(C.NET_ERR_MESH_STREAM_OCCUPIED)
}

// abiStabilityMeshError runs a raw C return code through the mesh error
// mapping the binding uses.
func abiStabilityMeshError(code int) error {
	return meshErrorFromCode(C.int(code))
}
