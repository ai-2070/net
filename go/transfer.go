// Package net — directory transfer and discovered blob fetch.
//
// Wraps the directory half of `net::ffi::transport` (declared in
// include/net_transport.h, mirrored into net.go.h / go/net.h). The blob half
// (ServeBlobTransfer, FetchBlob, BlobRefHash) lives in blob.go, and errors
// share its transferErrorFromCode mapping, so every failure here matches
// ErrTransfer.
//
// A node must call ServeBlobTransfer before it can serve chunks to peers or
// fetch them itself; both peers of a transfer need the engine.
//
// # Memory
//
// Byte outputs are copied into Go memory and freed with
// net_transport_free_buffer; the manifest JSON is copied and freed with
// net_free_string. Nothing returned here needs a caller-side free.

package net

/*
#include "net.h"
#include <stdint.h>
#include <stdlib.h>
*/
import "C"

import (
	"encoding/json"
	"fmt"
	"runtime"
	"strings"
	"unsafe"
)

// DirStats reports what a FetchDir reconstructed.
type DirStats struct {
	Files uint64
	Bytes uint64
}

// DirManifest is a stored directory tree's manifest, as DirManifestRead
// decodes it. Entries are sorted by path.
type DirManifest struct {
	Version uint8      `json:"version"`
	Entries []DirEntry `json:"entries"`
}

// DirEntry is one node of the tree. Path is relative to the tree root and
// always `/`-separated. Exactly one of File, Dir and Symlink is set.
type DirEntry struct {
	Path    string
	File    *DirFileEntry
	Dir     *DirDirEntry
	Symlink *DirSymlinkEntry
}

// DirFileEntry is a regular file. Mode is the Unix permission bits (0 when
// the tree was stored on a non-Unix host); Blob is the encoded BlobRef of
// the content, usable with BlobRefHash and MeshBlobAdapter.Fetch.
type DirFileEntry struct {
	Mode uint32
	Blob []byte
}

// DirDirEntry is a directory, recorded so empty ones survive the trip.
type DirDirEntry struct {
	Mode uint32
}

// DirSymlinkEntry is a symbolic link, stored verbatim.
type DirSymlinkEntry struct {
	Target string
}

// The wire shape: serde's externally tagged enum, with the blob as a JSON
// array of numbers (serde_json's Vec<u8>), not Go's base64.
type dirEntryJSON struct {
	Path string `json:"path"`
	Kind map[string]struct {
		Mode   *uint32  `json:"mode"`
		Blob   []uint16 `json:"blob"`
		Target *string  `json:"target"`
	} `json:"kind"`
}

func (e *DirEntry) UnmarshalJSON(data []byte) error {
	var raw dirEntryJSON
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	if len(raw.Kind) != 1 {
		return fmt.Errorf("entry %q: want exactly one kind, got %d", raw.Path, len(raw.Kind))
	}
	*e = DirEntry{Path: raw.Path}
	for tag, body := range raw.Kind {
		switch tag {
		case "File":
			if body.Mode == nil || body.Blob == nil {
				return fmt.Errorf("entry %q: File needs mode and blob", raw.Path)
			}
			blob := make([]byte, len(body.Blob))
			for i, v := range body.Blob {
				if v > 0xFF {
					return fmt.Errorf("entry %q: blob byte %d out of range", raw.Path, v)
				}
				blob[i] = byte(v)
			}
			e.File = &DirFileEntry{Mode: *body.Mode, Blob: blob}
		case "Dir":
			if body.Mode == nil {
				return fmt.Errorf("entry %q: Dir needs mode", raw.Path)
			}
			e.Dir = &DirDirEntry{Mode: *body.Mode}
		case "Symlink":
			if body.Target == nil {
				return fmt.Errorf("entry %q: Symlink needs target", raw.Path)
			}
			e.Symlink = &DirSymlinkEntry{Target: *body.Target}
		default:
			return fmt.Errorf("entry %q: unknown kind %q", raw.Path, tag)
		}
	}
	return nil
}

// refuseNULPath rejects a path with an embedded NUL. C.CString would cut it
// at the NUL, and the native call would act on that prefix instead: for
// FetchDir, an existing directory the caller never named.
func refuseNULPath(op, name, path string) error {
	if strings.IndexByte(path, 0) >= 0 {
		return fmt.Errorf("%w: %s: %s contains a NUL byte", ErrTransferInvalidArgument, op, name)
	}
	return nil
}

// StoreDir stores the local directory tree at root as content-addressed
// blobs in this adapter and returns the encoded manifest BlobRef: the token
// a receiver passes to FetchDir or DirManifestRead.
func (a *MeshBlobAdapter) StoreDir(root string) ([]byte, error) {
	if err := refuseNULPath("store_dir", "root", root); err != nil {
		return nil, err
	}
	cRoot := C.CString(root)
	defer C.free(unsafe.Pointer(cRoot))
	var out *C.uint8_t
	var outLen C.size_t
	var rc C.int
	if !a.withReadHandle(func(handle *C.net_mesh_blob_adapter_t) {
		rc = C.net_store_dir(handle, cRoot, &out, &outLen)
	}) {
		return nil, ErrBlobClosed
	}
	if err := transferErrorFromCode(rc); err != nil {
		return nil, err
	}
	defer C.net_transport_free_buffer(out, outLen)
	ref, err := copyCBuf(unsafe.Pointer(out), uint64(outLen))
	if err != nil {
		return nil, fmt.Errorf("%w: store_dir: %w", ErrTransfer, err)
	}
	return ref, nil
}

// FetchDir fetches the tree named by manifestRef from sourceID and
// reconstructs it under dest (created if absent). Manifest paths are
// checked to stay inside dest.
func (m *MeshNode) FetchDir(sourceID uint64, manifestRef []byte, dest string) (DirStats, error) {
	if len(manifestRef) == 0 {
		return DirStats{}, fmt.Errorf("%w: manifest ref is empty", ErrTransferInvalidArgument)
	}
	if err := refuseNULPath("fetch_dir", "dest", dest); err != nil {
		return DirStats{}, err
	}
	cDest := C.CString(dest)
	defer C.free(unsafe.Pointer(cDest))
	var files, total C.uint64_t
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return DirStats{}, ErrShuttingDown
	}
	rc := C.net_fetch_dir(
		m.handle,
		C.uint64_t(sourceID),
		(*C.uint8_t)(unsafe.Pointer(&manifestRef[0])), C.size_t(len(manifestRef)),
		cDest,
		&files, &total,
	)
	runtime.KeepAlive(manifestRef)
	if err := transferErrorFromCode(rc); err != nil {
		return DirStats{}, err
	}
	return DirStats{Files: uint64(files), Bytes: uint64(total)}, nil
}

// DirManifestRead fetches and decodes the manifest named by manifestRef
// from sourceID without reconstructing the tree.
//
// A manifest the holder does not have fails with ErrTransferNotFound; bytes
// that were fetched but are not a manifest fail with ErrDirInvalidManifest.
func (m *MeshNode) DirManifestRead(sourceID uint64, manifestRef []byte) (*DirManifest, error) {
	if len(manifestRef) == 0 {
		return nil, fmt.Errorf("%w: manifest ref is empty", ErrTransferInvalidArgument)
	}
	var out *C.char
	var outLen C.size_t
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return nil, ErrShuttingDown
	}
	rc := C.net_dir_manifest_read(
		m.handle,
		C.uint64_t(sourceID),
		(*C.uint8_t)(unsafe.Pointer(&manifestRef[0])), C.size_t(len(manifestRef)),
		&out, &outLen,
	)
	runtime.KeepAlive(manifestRef)
	if err := transferErrorFromCode(rc); err != nil {
		return nil, err
	}
	defer C.net_free_string(out)
	body, err := copyCBuf(unsafe.Pointer(out), uint64(outLen))
	if err != nil {
		return nil, fmt.Errorf("%w: dir_manifest_read: %w", ErrTransfer, err)
	}
	var manifest DirManifest
	if err := json.Unmarshal(body, &manifest); err != nil {
		return nil, fmt.Errorf("%w: decode manifest JSON: %v", ErrDirInvalidManifest, err)
	}
	return &manifest, nil
}

// FetchBlobDiscovered is FetchBlob without a known holder: it discovers one
// among connected peers, and fails with ErrTransferAllPeersFailed if none
// serves the content.
func (m *MeshNode) FetchBlobDiscovered(hash []byte) ([]byte, error) {
	// Exact, as in FetchBlob: the C side reads 32 bytes from the pointer.
	if len(hash) != 32 {
		return nil, fmt.Errorf(
			"%w: hash must be 32 bytes, got %d", ErrTransferInvalidArgument, len(hash))
	}
	var out *C.uint8_t
	var outLen C.size_t
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return nil, ErrShuttingDown
	}
	rc := C.net_fetch_blob_discovered(
		m.handle,
		(*C.uint8_t)(unsafe.Pointer(&hash[0])),
		&out, &outLen,
	)
	runtime.KeepAlive(hash)
	if err := transferErrorFromCode(rc); err != nil {
		return nil, err
	}
	defer C.net_transport_free_buffer(out, outLen)
	body, err := copyCBuf(unsafe.Pointer(out), uint64(outLen))
	if err != nil {
		return nil, fmt.Errorf("%w: fetch: %w", ErrTransfer, err)
	}
	return body, nil
}
