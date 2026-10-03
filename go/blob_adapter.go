// Package net — Go-implemented blob adapters in the process-wide registry.
//
// RegisterBlobAdapter puts a Go value behind an adapter id, so BlobPublish /
// BlobResolve (and the RedEX overflow path) store and fetch through Go code.
//
// Ownership (plan gap G-B, GO_BINDING_CONSOLIDATION_AND_BLOBS_PLAN.md S5b).
// The adapter is held by a cgo.Handle that the substrate owns once
// registration succeeds: net_blob_register_callback_adapter_owned calls the
// release trampoline exactly once, after the adapter is unregistered AND the
// last in-flight call holding the handle has returned, including the
// free_buffer of a fetch. Only then is the handle deleted. A call already
// running when UnregisterBlobAdapter returns therefore still reaches a live
// adapter, and a refused registration leaves the handle with Go, which
// deletes it at once.
//
// Callbacks run on substrate worker threads and may overlap: an adapter
// must be safe for concurrent use. A panic inside one is contained at the
// boundary (recoverCallback) and surfaces as ErrBlobBackend.

package net

/*
#include "net.h"
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

// Defined in blob_adapter_bridge.c: registers the static Go vtable with the
// handle as ctx. The uintptr_t-to-pointer cast happens in C, so no Go code
// converts an integer to unsafe.Pointer.
extern int netGoRegisterBlobAdapter(const char* adapter_id, uintptr_t handle);

// Also in blob_adapter_bridge.c: plain malloc, which may return NULL. cgo's
// own C.malloc never returns NULL; it aborts the process instead, which a
// fetch callback must not do.
extern void* netGoBlobMalloc(size_t n);
*/
import "C"

import (
	"errors"
	"fmt"
	"reflect"
	"runtime/cgo"
	"sync/atomic"
	"unsafe"
)

// BlobKey names one blob as the substrate addresses it: the URI it was
// published under, its 32-byte BLAKE3 hash, and its size in bytes.
type BlobKey struct {
	URI  string
	Hash [32]byte
	Size uint64
}

// BlobAdapter is a blob store implemented in Go. Methods are called from
// substrate threads, possibly concurrently. Return ErrBlobNotFound (or an
// error wrapping it) for content the adapter does not hold, and
// ErrBlobUnsupportedScheme for a URI it does not accept; any other error is
// reported as ErrBlobBackend.
type BlobAdapter interface {
	// Store keeps data under key. len(data) == key.Size.
	Store(key BlobKey, data []byte) error
	// Fetch returns the whole blob.
	Fetch(key BlobKey) ([]byte, error)
	// FetchRange returns bytes [start, end) of the blob.
	FetchRange(key BlobKey, start, end uint64) ([]byte, error)
	// Exists reports whether the adapter holds the blob.
	Exists(key BlobKey) (bool, error)
}

// blobAdapterReleased is a test hook, called with the adapter after its
// handle is deleted. Nil in production.
var blobAdapterReleased atomic.Pointer[func(BlobAdapter)]

// RegisterBlobAdapter registers a under id. Registering an id that is
// already taken fails with ErrBlobDuplicateID; a is then never called.
func RegisterBlobAdapter(id string, a BlobAdapter) error {
	_, err := registerBlobAdapter(id, a)
	return err
}

// registerBlobAdapter returns the handle the substrate now owns (tests use
// it to target a barrier at this registration).
func registerBlobAdapter(id string, a BlobAdapter) (cgo.Handle, error) {
	if isNilAdapter(a) {
		return 0, fmt.Errorf("%w: nil BlobAdapter", ErrBlobInvalidArgument)
	}
	cID, err := cStringArg("adapter id", id)
	if err != nil {
		return 0, err
	}
	defer C.free(unsafe.Pointer(cID))
	h := cgo.NewHandle(a)
	rc := C.netGoRegisterBlobAdapter(cID, C.uintptr_t(h))
	if err := blobRegistryError("register_callback_adapter", rc); err != nil {
		// Refused: release_fn will never run, so the handle is still ours.
		h.Delete()
		return 0, err
	}
	return h, nil
}

// isNilAdapter reports a nil interface or a typed nil inside one (a nil
// *T), which would register fine and then panic on the first callback.
func isNilAdapter(a BlobAdapter) bool {
	if a == nil {
		return true
	}
	v := reflect.ValueOf(a)
	switch v.Kind() {
	case reflect.Pointer, reflect.Map, reflect.Slice, reflect.Func, reflect.Chan, reflect.Interface:
		return v.IsNil()
	}
	return false
}

// blobCallbackCode maps an adapter error to the code the substrate expects.
func blobCallbackCode(err error) C.int {
	switch {
	case err == nil:
		return 0
	case errors.Is(err, ErrBlobNotFound):
		return -113
	case errors.Is(err, ErrBlobUnsupportedScheme):
		return -116
	default:
		return -115
	}
}

func blobAdapterFor(h C.uintptr_t) (BlobAdapter, bool) {
	a, ok := cgo.Handle(h).Value().(BlobAdapter)
	return a, ok
}

func blobKeyFrom(uri *C.char, hash *C.uint8_t, size C.uint64_t) BlobKey {
	k := BlobKey{URI: C.GoString(uri), Size: uint64(size)}
	copy(k.Hash[:], unsafe.Slice((*byte)(unsafe.Pointer(hash)), 32))
	return k
}

// blobOutAllocHook, when set, replaces the result-buffer allocator, so a
// test can simulate allocation failure. Atomic: callbacks read it on
// substrate threads. Nil in production.
var blobOutAllocHook atomic.Pointer[func(n int) unsafe.Pointer]

// blobOutAlloc allocates a fetch result buffer, nil on failure.
func blobOutAlloc(n int) unsafe.Pointer {
	if hook := blobOutAllocHook.Load(); hook != nil {
		return (*hook)(n)
	}
	return C.netGoBlobMalloc(C.size_t(n))
}

// writeBlobOut copies b into a malloc'd buffer the bridge's free_buffer
// frees. An empty result is (NULL, 0), which the substrate reads as empty.
// It reports false, with the outputs still (NULL, 0), when the allocation
// fails.
func writeBlobOut(b []byte, outData **C.uint8_t, outLen *C.size_t) bool {
	*outData = nil
	*outLen = 0
	if len(b) == 0 {
		return true
	}
	p := blobOutAlloc(len(b))
	if p == nil {
		return false
	}
	C.memcpy(p, unsafe.Pointer(&b[0]), C.size_t(len(b)))
	*outData = (*C.uint8_t)(p)
	*outLen = C.size_t(len(b))
	return true
}

//export goBlobStore
func goBlobStore(h C.uintptr_t, uri *C.char, hash *C.uint8_t, size C.uint64_t,
	data *C.uint8_t, dataLen C.size_t,
) (code C.int) {
	defer func() {
		if recoverCallback("blob.Store", recover()) {
			code = -115
		}
	}()
	a, ok := blobAdapterFor(h)
	if !ok {
		return -115
	}
	body, err := copyCBuf(unsafe.Pointer(data), uint64(dataLen))
	if err != nil {
		return -115
	}
	return blobCallbackCode(a.Store(blobKeyFrom(uri, hash, size), body))
}

//export goBlobFetch
func goBlobFetch(h C.uintptr_t, uri *C.char, hash *C.uint8_t, size C.uint64_t,
	outData **C.uint8_t, outLen *C.size_t,
) (code C.int) {
	defer func() {
		if recoverCallback("blob.Fetch", recover()) {
			code = -115
		}
	}()
	*outData = nil
	*outLen = 0
	a, ok := blobAdapterFor(h)
	if !ok {
		return -115
	}
	b, err := a.Fetch(blobKeyFrom(uri, hash, size))
	if err != nil {
		return blobCallbackCode(err)
	}
	if !writeBlobOut(b, outData, outLen) {
		return -115
	}
	return 0
}

//export goBlobFetchRange
func goBlobFetchRange(h C.uintptr_t, uri *C.char, hash *C.uint8_t, size C.uint64_t,
	start, end C.uint64_t, outData **C.uint8_t, outLen *C.size_t,
) (code C.int) {
	defer func() {
		if recoverCallback("blob.FetchRange", recover()) {
			code = -115
		}
	}()
	*outData = nil
	*outLen = 0
	a, ok := blobAdapterFor(h)
	if !ok {
		return -115
	}
	b, err := a.FetchRange(blobKeyFrom(uri, hash, size), uint64(start), uint64(end))
	if err != nil {
		return blobCallbackCode(err)
	}
	if !writeBlobOut(b, outData, outLen) {
		return -115
	}
	return 0
}

//export goBlobExists
func goBlobExists(h C.uintptr_t, uri *C.char, hash *C.uint8_t, size C.uint64_t,
	outExists *C.int,
) (code C.int) {
	defer func() {
		if recoverCallback("blob.Exists", recover()) {
			code = -115
		}
	}()
	*outExists = 0
	a, ok := blobAdapterFor(h)
	if !ok {
		return -115
	}
	found, err := a.Exists(blobKeyFrom(uri, hash, size))
	if err != nil {
		return blobCallbackCode(err)
	}
	if found {
		*outExists = 1
	}
	return 0
}

//export goBlobRelease
func goBlobRelease(h C.uintptr_t) {
	defer func() { recoverCallback("blob.Release", recover()) }()
	handle := cgo.Handle(h)
	a, _ := handle.Value().(BlobAdapter)
	handle.Delete()
	if hook := blobAdapterReleased.Load(); hook != nil {
		(*hook)(a)
	}
}
