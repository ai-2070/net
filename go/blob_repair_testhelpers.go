// Test-only bridge to the two fixtures-gated blob seams the Reed-Solomon
// repair witness needs (blob_repair_test.go).
//
// Same posture as event_too_large_attribution_testhelpers.go: the symbols
// exist only in a libnet built with the core crate's `fixtures` feature,
// which `cargo build --release -p net-ffi --features net-ffi/test-helpers`
// turns on, and this file compiles only under `go test -tags test_helpers`.
// The prototypes are declared here, not in net.h, because production
// headers must not name symbols a production build does not export.
//
// WHY A SEAM. The witness must make a real data shard unavailable and prove
// it gone before repair. Deleting a file from under a live adapter proves
// nothing while its cached state can still serve the chunk, and no
// production C function deletes a chunk. The seam goes through the
// adapter's own deletion path (which also drops its cache entries) and
// checks the chunk no longer fetches before returning.

//go:build test_helpers

package net

/*
#include "net.h"

extern int net_mesh_blob_adapter_test_drop_data_chunk(
    const net_mesh_blob_adapter_t* handle,
    const uint8_t* blob_ref_bytes,
    size_t blob_ref_len,
    uint32_t stripe_index,
    uint32_t data_index,
    uint8_t* out_hash);

extern int net_mesh_blob_adapter_test_chunk_present(
    const net_mesh_blob_adapter_t* handle,
    const uint8_t* hash);
*/
import "C"

import (
	"runtime"
	"unsafe"
)

// testDropDataChunk deletes data chunk dataIndex of stripe stripeIndex of a
// Reed-Solomon tree blob and returns its hash.
func testDropDataChunk(a *MeshBlobAdapter, ref []byte, stripeIndex, dataIndex uint32) ([32]byte, error) {
	var hash [32]byte
	var rc C.int
	if !a.withReadHandle(func(h *C.net_mesh_blob_adapter_t) {
		rc = C.net_mesh_blob_adapter_test_drop_data_chunk(h,
			(*C.uint8_t)(unsafe.Pointer(&ref[0])), C.size_t(len(ref)),
			C.uint32_t(stripeIndex), C.uint32_t(dataIndex),
			(*C.uint8_t)(unsafe.Pointer(&hash[0])))
	}) {
		return hash, ErrBlobClosed
	}
	runtime.KeepAlive(ref)
	return hash, blobRegistryError("test_drop_data_chunk", rc)
}

// testChunkPresent reports whether the chunk with this hash fetches.
func testChunkPresent(a *MeshBlobAdapter, hash [32]byte) (bool, error) {
	var rc C.int
	if !a.withReadHandle(func(h *C.net_mesh_blob_adapter_t) {
		rc = C.net_mesh_blob_adapter_test_chunk_present(h, (*C.uint8_t)(unsafe.Pointer(&hash[0])))
	}) {
		return false, ErrBlobClosed
	}
	if rc < 0 {
		return false, blobRegistryError("test_chunk_present", rc)
	}
	return rc == 1, nil
}
