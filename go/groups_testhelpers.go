// Test-only helper — inject synthetic peers into a mesh's
// capability index so group tests have placement candidates
// without spinning up a real 3-node handshake.
//
// Gated with the `test_helpers` build tag so Go production
// builds (plain `go build` / `go test`) never compile this file
// or reference `net_compute_test_inject_synthetic_peer`; the
// symbol itself is gated at the Rust layer behind the
// `test-helpers` cargo feature on compute-ffi, so a
// `libnet_compute.{dylib,so}` built without that feature does
// not export it either. Running the group tests therefore
// requires:
//
//   1. Build compute-ffi with `--features test-helpers` so the
//      cdylib exports the symbol.
//   2. Run `go test -tags test_helpers` so this file is compiled
//      into the test binary and the extern reference resolves.
//
// A release-mode cdylib paired with a `-tags test_helpers`
// test binary will fail to link, which is the point — the test
// posture is explicit rather than implicit.

//go:build test_helpers

package net

/*
#include "net.h"
#include <stdlib.h>

// The prototypes are deliberately NOT in net.h since that header
// is consumed by production callers. Declared inline here so
// only this test-only TU references the (feature-gated) symbols.
extern void net_compute_test_inject_synthetic_peer(
    net_compute_mesh_arc_t* mesh_arc,
    uint64_t node_id
);
extern void net_compute_test_inject_synthetic_peer_with_tags(
    net_compute_mesh_arc_t* mesh_arc,
    uint64_t node_id,
    const char* tags_json
);
*/
import "C"

import (
	"encoding/json"
	"unsafe"
)

// TestInjectSyntheticPeer is test-only — DO NOT use in
// production. Stages a synthetic peer entry in the given
// mesh's capability index so `place_with_spread` can spread
// group members across multiple "nodes". Mirrors the helpers
// in the NAPI + Python bindings.
func TestInjectSyntheticPeer(mesh *MeshNode, nodeID uint64) {
	if mesh == nil {
		return
	}
	// TOCTOU guard: hold the read lock across the Arc-clone so
	// a concurrent `mesh.Shutdown()` can't free the native
	// handle between the `mesh.handle` load and
	// `net_mesh_arc_clone(h)`. Once the Arc is cloned it keeps
	// the underlying object alive for the duration of this call.
	mesh.mu.RLock()
	h := mesh.handle
	if h == nil {
		mesh.mu.RUnlock()
		return
	}
	arc := C.net_mesh_arc_clone(h)
	mesh.mu.RUnlock()
	if arc == nil {
		return
	}
	defer C.net_mesh_arc_free(arc)
	C.net_compute_test_inject_synthetic_peer(arc, C.uint64_t(nodeID))
}

// TestInjectSyntheticPeerWithTags is the richer variant for the
// Phase 6c aggregation smoke tests — stages a synthetic peer with
// the supplied canonical tag strings (including reserved-prefix
// tags like `scope:region:us-east`). Same lifecycle / lock
// discipline as TestInjectSyntheticPeer.
func TestInjectSyntheticPeerWithTags(mesh *MeshNode, nodeID uint64, tags []string) {
	if mesh == nil {
		return
	}
	mesh.mu.RLock()
	h := mesh.handle
	if h == nil {
		mesh.mu.RUnlock()
		return
	}
	arc := C.net_mesh_arc_clone(h)
	mesh.mu.RUnlock()
	if arc == nil {
		return
	}
	defer C.net_mesh_arc_free(arc)
	if len(tags) == 0 {
		C.net_compute_test_inject_synthetic_peer_with_tags(
			arc, C.uint64_t(nodeID), nil,
		)
		return
	}
	b, err := json.Marshal(tags)
	if err != nil {
		return
	}
	cs := C.CString(string(b))
	defer C.free(unsafe.Pointer(cs))
	C.net_compute_test_inject_synthetic_peer_with_tags(
		arc, C.uint64_t(nodeID), cs,
	)
}
