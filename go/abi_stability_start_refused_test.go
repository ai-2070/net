package net

// Parity guard for `NET_ERR_MESH_START_REFUSED` (-129).
//
// `net_mesh_start` used to return 0 when the core refused to start
// (an `accept()` was mid-handshake), so `MeshNode.Start` reported
// success for a node that would send but never receive. The C ABI now
// returns -129 for that refusal; this pins the number against the
// header cgo compiles and checks the Go arm maps it to a typed
// sentinel instead of the unknown-error fallback.

import (
	"errors"
	"strings"
	"testing"
)

func TestStartRefusedHeaderParity(t *testing.T) {
	if got := netErrMeshStartRefused; got != -129 {
		t.Fatalf(
			"NET_ERR_MESH_START_REFUSED = %d in go/net.h, want -129 — "+
				"the C ABI, the Rust ffi::mesh constant and every binding "+
				"must agree on this number",
			got,
		)
	}
	err := meshErrorFromCode(netErrMeshStartRefused)
	if !errors.Is(err, ErrStartRefused) {
		t.Fatalf("meshErrorFromCode(%d) = %v, want ErrStartRefused",
			netErrMeshStartRefused, err)
	}
	if strings.Contains(err.Error(), "unknown error") {
		t.Fatalf("code %d falls through to the unknown-error arm: %v",
			netErrMeshStartRefused, err)
	}
}
