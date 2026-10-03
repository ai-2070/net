// Test-only bridge to the fixtures-gated callback barrier in
// src/ffi/blob.rs, for the S5b ownership witnesses
// (blob_adapter_barrier_test.go).
//
// Same posture as blob_repair_testhelpers.go: the symbols exist only in a
// libnet built with `--features net-ffi/test-helpers`, this file compiles
// only under `go test -tags test_helpers`, and the prototypes live here
// rather than in net.h.
//
// WHY A SEAM. Stage 1 (about to enter the callback) can be held from Go by
// blocking inside Fetch, but stage 2 (the callback has returned its buffer,
// free_buffer has not run yet) is entirely inside the substrate. Only a
// native barrier can hold a call there while the test unregisters.

//go:build test_helpers

package net

/*
#include <stdint.h>
#include <stdlib.h>

extern void net_blob_test_barrier_arm(int stage, void* ctx);
extern int net_blob_test_barrier_wait_held(int stage, uint32_t timeout_ms);
extern void net_blob_test_barrier_release(int stage);

// The handle crosses as an integer; the cast to the ctx pointer the
// barrier compares against happens here, in C.
static void netGoBlobBarrierArm(int stage, uintptr_t handle) {
    net_blob_test_barrier_arm(stage, (void*)handle);
}
*/
import "C"

import (
	"runtime/cgo"
	"unsafe"
)

// blobBarrierArm holds the next fetch of the registration owning h at
// stage (1 before the callback, 2 before free_buffer).
func blobBarrierArm(stage int, h cgo.Handle) {
	C.netGoBlobBarrierArm(C.int(stage), C.uintptr_t(h))
}

// blobBarrierWaitHeld reports whether a fetch reached the armed stage
// within timeoutMs.
func blobBarrierWaitHeld(stage int, timeoutMs uint32) bool {
	return C.net_blob_test_barrier_wait_held(C.int(stage), C.uint32_t(timeoutMs)) == 1
}

// blobBarrierRelease lets the call held at stage continue.
func blobBarrierRelease(stage int) {
	C.net_blob_test_barrier_release(C.int(stage))
}

// blobFetchThroughDeletedHandle calls the fetch trampoline with a handle
// that has already been deleted, so the lookup itself panics (cgo.Handle
// panics on an invalid handle). It returns the trampoline's code.
func blobFetchThroughDeletedHandle() int {
	h := cgo.NewHandle(struct{}{})
	h.Delete()
	uri := C.CString("mem://go/deleted")
	defer C.free(unsafe.Pointer(uri))
	hash := (*C.uint8_t)(C.calloc(32, 1))
	defer C.free(unsafe.Pointer(hash))
	var out *C.uint8_t
	var outLen C.size_t
	return int(goBlobFetch(C.uintptr_t(h), uri, hash, 0, &out, &outLen))
}
