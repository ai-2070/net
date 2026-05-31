// Package net — transport (blob + directory transfer) consumer wrapper
// for the C ABI exported by the `net::ffi::transport` module in the main
// `net` crate (declared in `include/net_transport.h`).
//
// This file is a **reference implementation** documenting the expected
// Go-side surface for consumers of `libnet`; the upstream `net` repo
// owns the C ABI and ships this file as the canonical contract for the
// cgo wrapper.
//
// # Build prerequisites
//
//   - Build the main `net` crate as a cdylib with the transport feature
//     set (the blob-adapter handle needs netdb + redex-disk):
//
//     cd net/crates/net
//     cargo build --release --features "net dataforts netdb redex-disk"
//
//   - Add to your CGO flags:
//
//     #cgo LDFLAGS: -L/path/to/target/release -lnet
//     #cgo darwin LDFLAGS: -framework Security -framework CoreFoundation
//
// # Handle model
//
// Transfer is node-driven, so the fetch/dir calls take the mesh-node
// handle (`net_meshnode_t*`, from the mesh wrapper) and the store/serve
// calls take the blob-adapter handle (`net_mesh_blob_adapter_t*`, from
// the blob-adapter wrapper). Both are passed as `unsafe.Pointer` — the
// caller obtained them from the respective Go wrappers and retains
// ownership; this surface only borrows them. A node MUST call
// ServeBlobTransfer before it can serve chunks OR fetch.
//
// # Memory
//
// Byte buffers returned by the C layer are copied into Go slices and the
// C buffer is freed immediately (via net_transport_free_buffer /
// net_free_string), so the returned values are owned by the Go GC — no
// caller-side free, no SetFinalizer needed for results.
package net

/*
#include <stdint.h>
#include <stdlib.h>

// Forward-declared opaque handle types from `libnet` (identical to the
// typedefs in net_transport.h / net.go.h).
typedef struct net_meshnode_s          net_meshnode_t;
typedef struct net_mesh_blob_adapter_s net_mesh_blob_adapter_t;

// Imported FFI surface from `net::ffi::transport`.
extern int net_serve_blob_transfer(const net_meshnode_t* node,
                                   const net_mesh_blob_adapter_t* adapter);
extern int net_fetch_blob(const net_meshnode_t* node, uint64_t holder_id,
                          const uint8_t* hash, uint8_t** out_bytes, size_t* out_len);
extern int net_fetch_blob_discovered(const net_meshnode_t* node, const uint8_t* hash,
                                     uint8_t** out_bytes, size_t* out_len);
extern int net_store_dir(const net_mesh_blob_adapter_t* adapter, const char* root_path,
                         uint8_t** out_manifest_ref, size_t* out_len);
extern int net_fetch_dir(const net_meshnode_t* node, uint64_t source_id,
                         const uint8_t* manifest_ref, size_t manifest_ref_len,
                         const char* dest_path, uint64_t* out_files, uint64_t* out_bytes);
extern int net_dir_manifest_read(const net_meshnode_t* node, uint64_t source_id,
                                 const uint8_t* manifest_ref, size_t manifest_ref_len,
                                 char** out_json, size_t* out_len);
extern void net_transport_free_buffer(uint8_t* ptr, size_t len);
extern void net_free_string(char* s);
*/
import "C"

import (
	"fmt"
	"unsafe"
)

// Transport error codes — mirror include/net_transport.h (kept in sync
// by tests/transport_error_codes.rs on the Rust side).
const (
	transportOK                   = 0
	errTransferNotFound           = -200
	errTransferHashMismatch       = -201
	errTransferAllPeersFailed     = -202
	errTransferCancelled          = -203
	errTransferNullPointer        = -204
	errTransferShuttingDown       = -205
	errTransferEngineNotInstalled = -206
	errTransferBackend            = -207
	errTransferPanic              = -208
	errTransferInvalidArgument    = -209
	errDirInvalidManifest         = -210
	errDirPathInvalid             = -211
	errDirIO                      = -213
	// errFeatureNotBuilt mirrors the cortex `NET_ERR_FEATURE_NOT_BUILT`
	// (-107). Not a transport-band code: it's what the feature-off
	// stubs (`ffi::transport_stubs`) return when libnet was built
	// without the `net + dataforts + netdb + redex-disk` quad, so the
	// transport symbols resolve but every call routes here instead of
	// crashing at program load.
	errFeatureNotBuilt = -107
)

// TransferError is the typed error returned by the transport wrappers.
// Code is the raw negative C status; Kind is a stable string discriminant
// matching the cross-language error pinning.
type TransferError struct {
	Code int
	Kind string
}

func (e *TransferError) Error() string {
	return fmt.Sprintf("transfer: %s (code %d)", e.Kind, e.Code)
}

func errFromCode(code int) error {
	if code == transportOK {
		return nil
	}
	kind := func() string {
		switch code {
		case errTransferNotFound:
			return "not-found"
		case errTransferHashMismatch:
			return "hash-mismatch"
		case errTransferAllPeersFailed:
			return "all-peers-failed"
		case errTransferCancelled:
			return "cancelled"
		case errTransferNullPointer:
			return "null-pointer"
		case errTransferShuttingDown:
			return "shutting-down"
		case errTransferEngineNotInstalled:
			return "engine-not-installed"
		case errTransferBackend:
			return "backend"
		case errTransferPanic:
			return "panic"
		case errTransferInvalidArgument:
			return "invalid-argument"
		case errDirInvalidManifest:
			return "dir-invalid-manifest"
		case errDirPathInvalid:
			return "dir-path-invalid"
		case errDirIO:
			return "dir-io"
		case errFeatureNotBuilt:
			return "feature-not-built"
		default:
			return "unknown"
		}
	}()
	return &TransferError{Code: code, Kind: kind}
}

// goBytesAndFree copies a C buffer into a Go slice and frees the C
// allocation. A (nil, 0) buffer yields a nil slice.
func goBytesAndFree(ptr *C.uint8_t, n C.size_t) []byte {
	if ptr == nil || n == 0 {
		return nil
	}
	out := C.GoBytes(unsafe.Pointer(ptr), C.int(n))
	C.net_transport_free_buffer(ptr, n)
	return out
}

// DirStats reports what a FetchDir reconstructed.
type DirStats struct {
	Files uint64
	Bytes uint64
}

// ServeBlobTransfer installs the blob-transfer engine on the node over
// the adapter. Required before the node can serve chunks OR fetch.
// Idempotent. `meshNode` / `adapter` are handles from the mesh / blob
// wrappers.
func ServeBlobTransfer(meshNode, adapter unsafe.Pointer) error {
	rc := C.net_serve_blob_transfer(
		(*C.net_meshnode_t)(meshNode),
		(*C.net_mesh_blob_adapter_t)(adapter),
	)
	return errFromCode(int(rc))
}

// FetchBlob fetches the blob addressed by the 32-byte hash from the known
// holder, returning the reassembled, BLAKE3-verified bytes.
func FetchBlob(meshNode unsafe.Pointer, holderID uint64, hash []byte) ([]byte, error) {
	if len(hash) != 32 {
		return nil, &TransferError{Code: errTransferInvalidArgument, Kind: "invalid-argument"}
	}
	var out *C.uint8_t
	var outLen C.size_t
	rc := C.net_fetch_blob(
		(*C.net_meshnode_t)(meshNode),
		C.uint64_t(holderID),
		(*C.uint8_t)(unsafe.Pointer(&hash[0])),
		&out,
		&outLen,
	)
	if err := errFromCode(int(rc)); err != nil {
		return nil, err
	}
	return goBytesAndFree(out, outLen), nil
}

// FetchBlobDiscovered is like FetchBlob but discovers the holder among
// connected peers. Returns an "all-peers-failed" TransferError if no peer
// has the content.
func FetchBlobDiscovered(meshNode unsafe.Pointer, hash []byte) ([]byte, error) {
	if len(hash) != 32 {
		return nil, &TransferError{Code: errTransferInvalidArgument, Kind: "invalid-argument"}
	}
	var out *C.uint8_t
	var outLen C.size_t
	rc := C.net_fetch_blob_discovered(
		(*C.net_meshnode_t)(meshNode),
		(*C.uint8_t)(unsafe.Pointer(&hash[0])),
		&out,
		&outLen,
	)
	if err := errFromCode(int(rc)); err != nil {
		return nil, err
	}
	return goBytesAndFree(out, outLen), nil
}

// StoreDir stores the local directory at root as content-addressed blobs
// in the adapter, returning the encoded directory-manifest BlobRef (the
// token a receiver passes to FetchDir / DirManifestRead).
func StoreDir(adapter unsafe.Pointer, root string) ([]byte, error) {
	cRoot := C.CString(root)
	defer C.free(unsafe.Pointer(cRoot))
	var out *C.uint8_t
	var outLen C.size_t
	rc := C.net_store_dir(
		(*C.net_mesh_blob_adapter_t)(adapter),
		cRoot,
		&out,
		&outLen,
	)
	if err := errFromCode(int(rc)); err != nil {
		return nil, err
	}
	return goBytesAndFree(out, outLen), nil
}

// FetchDir fetches the directory whose encoded manifest BlobRef is
// manifestRef from sourceID and reconstructs it under dest.
func FetchDir(meshNode unsafe.Pointer, sourceID uint64, manifestRef []byte, dest string) (DirStats, error) {
	if len(manifestRef) == 0 {
		return DirStats{}, &TransferError{Code: errTransferInvalidArgument, Kind: "invalid-argument"}
	}
	cDest := C.CString(dest)
	defer C.free(unsafe.Pointer(cDest))
	var files, bytes C.uint64_t
	rc := C.net_fetch_dir(
		(*C.net_meshnode_t)(meshNode),
		C.uint64_t(sourceID),
		(*C.uint8_t)(unsafe.Pointer(&manifestRef[0])),
		C.size_t(len(manifestRef)),
		cDest,
		&files,
		&bytes,
	)
	if err := errFromCode(int(rc)); err != nil {
		return DirStats{}, err
	}
	return DirStats{Files: uint64(files), Bytes: uint64(bytes)}, nil
}

// DirManifestRead fetches + decodes the directory manifest at manifestRef
// from sourceID WITHOUT reconstructing the tree, returning it as a JSON
// string for introspection.
func DirManifestRead(meshNode unsafe.Pointer, sourceID uint64, manifestRef []byte) (string, error) {
	if len(manifestRef) == 0 {
		return "", &TransferError{Code: errTransferInvalidArgument, Kind: "invalid-argument"}
	}
	var out *C.char
	var outLen C.size_t
	rc := C.net_dir_manifest_read(
		(*C.net_meshnode_t)(meshNode),
		C.uint64_t(sourceID),
		(*C.uint8_t)(unsafe.Pointer(&manifestRef[0])),
		C.size_t(len(manifestRef)),
		&out,
		&outLen,
	)
	if err := errFromCode(int(rc)); err != nil {
		return "", err
	}
	if out == nil {
		return "", nil
	}
	s := C.GoStringN(out, C.int(outLen))
	C.net_free_string(out)
	return s, nil
}
