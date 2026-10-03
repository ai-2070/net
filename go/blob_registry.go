// Package net — the process-wide blob adapter registry.
//
// A blob adapter registered here is addressed by its id from anywhere in
// the process: BlobPublish stores bytes through it and returns the encoded
// BlobRef, and BlobResolve turns that ref back into the bytes. This is the
// registry the RedEX blob-overflow path consults; it is separate from a
// MeshBlobAdapter value, which you hold directly.
//
// The filesystem adapter is built in; a Go-implemented adapter registers
// through RegisterBlobAdapter (blob_adapter.go).

package net

/*
#include "net.h"
#include <stdlib.h>
*/
import "C"

import (
	"fmt"
	"runtime"
	"strings"
	"unsafe"
)

// Registry failures. All match ErrBlob.
var (
	// ErrBlobDecode - the payload is not a decodable BlobRef.
	ErrBlobDecode = fmt.Errorf("%w: not a decodable blob ref", ErrBlob)
	// ErrBlobDuplicateID - an adapter is already registered under the id.
	ErrBlobDuplicateID = fmt.Errorf("%w: adapter id already registered", ErrBlob)
	// ErrBlobNotRegistered - no adapter is registered under the id.
	ErrBlobNotRegistered = fmt.Errorf("%w: adapter id not registered", ErrBlob)
	// ErrBlobNotFound - the adapter does not hold the referenced content.
	ErrBlobNotFound = fmt.Errorf("%w: content not found", ErrBlob)
	// ErrBlobHashMismatch - the content does not hash to the reference.
	ErrBlobHashMismatch = fmt.Errorf("%w: content does not match its hash", ErrBlob)
	// ErrBlobBackend - any other adapter failure.
	ErrBlobBackend = fmt.Errorf("%w: adapter backend failure", ErrBlob)
	// ErrBlobUnsupportedScheme - the adapter does not accept the URI's
	// scheme (the filesystem adapter accepts only file:).
	ErrBlobUnsupportedScheme = fmt.Errorf("%w: unsupported URI scheme", ErrBlob)
	// ErrBlobUnauthorized - an auth gate refused the operation.
	ErrBlobUnauthorized = fmt.Errorf("%w: unauthorized", ErrBlob)
	// ErrBlobInvalidArgument - an argument refused before the operation
	// runs: by the binding (an empty ref, or a string with an embedded NUL,
	// which C would silently truncate into a different id), or natively
	// as NET_ERR_BLOB_INVALID_ARGUMENT (a reversed or out-of-extent range,
	// a bad encoding).
	ErrBlobInvalidArgument = fmt.Errorf("%w: invalid argument", ErrBlob)
)

func blobRegistryError(op string, code C.int) error {
	return blobRegistryOpError(op, int(code))
}

// blobRegistryErrorFromInt is the mapping without an operation name, on a
// plain int so the ABI test can drive it (a _test.go file cannot construct
// a C.int).
func blobRegistryErrorFromInt(code int) error {
	return blobRegistryOpError("blob", code)
}

func blobRegistryOpError(op string, code int) error {
	switch code {
	case 0:
		return nil
	case -110:
		return fmt.Errorf("%s: %w", op, ErrBlobDecode)
	case -111:
		return fmt.Errorf("%s: %w", op, ErrBlobDuplicateID)
	case -112, -119:
		return fmt.Errorf("%s: %w", op, ErrBlobNotRegistered)
	case -113:
		return fmt.Errorf("%s: %w", op, ErrBlobNotFound)
	case -114:
		return fmt.Errorf("%s: %w", op, ErrBlobHashMismatch)
	case -115:
		return fmt.Errorf("%s: %w", op, ErrBlobBackend)
	case -116:
		return fmt.Errorf("%s: %w", op, ErrBlobUnsupportedScheme)
	case -120:
		return fmt.Errorf("%s: %w", op, ErrBlobUnauthorized)
	case -150:
		return fmt.Errorf("%s: %w", op, ErrBlobInvalidArgument)
	case -107:
		return fmt.Errorf("%w: %s: %w", ErrBlob, op, ErrFeatureNotBuilt)
	default:
		return fmt.Errorf("%w: %s failed (rc=%d)", ErrBlob, op, code)
	}
}

// cStringArg converts s for a C call, refusing an embedded NUL. The caller
// frees the result.
func cStringArg(name, s string) (*C.char, error) {
	if strings.IndexByte(s, 0) >= 0 {
		return nil, fmt.Errorf("%w: %s contains a NUL byte", ErrBlobInvalidArgument, name)
	}
	return C.CString(s), nil
}

// RegisterFilesystemBlobAdapter registers a filesystem adapter under id,
// storing blobs below root. It accepts file: URIs. Registering an id that
// is already taken fails with ErrBlobDuplicateID.
func RegisterFilesystemBlobAdapter(id, root string) error {
	cID, err := cStringArg("adapter id", id)
	if err != nil {
		return err
	}
	defer C.free(unsafe.Pointer(cID))
	cRoot, err := cStringArg("root", root)
	if err != nil {
		return err
	}
	defer C.free(unsafe.Pointer(cRoot))
	return blobRegistryError("register_fs_adapter", C.net_blob_register_fs_adapter(cID, cRoot))
}

// UnregisterBlobAdapter removes the adapter registered under id. It reports
// whether one was removed.
func UnregisterBlobAdapter(id string) (bool, error) {
	cID, err := cStringArg("adapter id", id)
	if err != nil {
		return false, err
	}
	defer C.free(unsafe.Pointer(cID))
	rc := C.net_blob_unregister_adapter(cID)
	if rc < 0 {
		return false, blobRegistryError("unregister_adapter", rc)
	}
	return rc == 1, nil
}

// BlobAdapterRegistered reports whether an adapter is registered under id.
func BlobAdapterRegistered(id string) (bool, error) {
	cID, err := cStringArg("adapter id", id)
	if err != nil {
		return false, err
	}
	defer C.free(unsafe.Pointer(cID))
	rc := C.net_blob_adapter_registered(cID)
	if rc < 0 {
		return false, blobRegistryError("adapter_registered", rc)
	}
	return rc == 1, nil
}

// BlobPublish stores data through the adapter registered under adapterID,
// under uri, and returns the encoded BlobRef that names it.
func BlobPublish(adapterID, uri string, data []byte) ([]byte, error) {
	cID, err := cStringArg("adapter id", adapterID)
	if err != nil {
		return nil, err
	}
	defer C.free(unsafe.Pointer(cID))
	cURI, err := cStringArg("uri", uri)
	if err != nil {
		return nil, err
	}
	defer C.free(unsafe.Pointer(cURI))
	var dataPtr *C.uint8_t
	if len(data) > 0 {
		dataPtr = (*C.uint8_t)(unsafe.Pointer(&data[0]))
	}
	var out *C.uint8_t
	var outLen C.size_t
	rc := C.net_blob_publish(cID, cURI, dataPtr, C.size_t(len(data)), &out, &outLen)
	runtime.KeepAlive(data)
	if err := blobRegistryError("publish", rc); err != nil {
		return nil, err
	}
	defer C.net_blob_free_buffer(out, outLen)
	ref, err := copyCBuf(unsafe.Pointer(out), uint64(outLen))
	if err != nil {
		return nil, fmt.Errorf("%w: publish: %w", ErrBlob, err)
	}
	return ref, nil
}

// BlobResolve returns the content an encoded BlobRef names, read through the
// adapter registered under adapterID. The adapter is named explicitly; there
// is no lookup by the ref's URI.
func BlobResolve(adapterID string, ref []byte) ([]byte, error) {
	cID, err := cStringArg("adapter id", adapterID)
	if err != nil {
		return nil, err
	}
	defer C.free(unsafe.Pointer(cID))
	var refPtr *C.uint8_t
	if len(ref) > 0 {
		refPtr = (*C.uint8_t)(unsafe.Pointer(&ref[0]))
	}
	var out *C.uint8_t
	var outLen C.size_t
	rc := C.net_blob_resolve(cID, refPtr, C.size_t(len(ref)), &out, &outLen)
	runtime.KeepAlive(ref)
	if err := blobRegistryError("resolve", rc); err != nil {
		return nil, err
	}
	defer C.net_blob_free_buffer(out, outLen)
	body, err := copyCBuf(unsafe.Pointer(out), uint64(outLen))
	if err != nil {
		return nil, fmt.Errorf("%w: resolve: %w", ErrBlob, err)
	}
	return body, nil
}
