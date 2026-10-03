// Package net — v0.3 blob trees: tree storage with Reed-Solomon erasure
// coding, range reads, repair, the tree-node cache, and BlobRef
// introspection. Parity with the Node and Python bindings.
//
// A tree ref can only be read with FetchRange: core deliberately refuses
// a whole-blob Fetch of a tree (use FetchRange(ref, 0, size)).
//
// Every function here follows the contract in
// docs/internal/plans/GO_BINDING_CONSOLIDATION_AND_BLOBS_PLAN.md, "New C ABI
// contract (S6)". Refusals of a well-formed call are ErrBlobInvalidArgument.

package net

/*
#include "net.h"
#include <stdlib.h>
*/
import "C"

import (
	"encoding/json"
	"fmt"
	"runtime"
	"unsafe"
)

// Capability tags a peer advertises when it supports a v0.3 blob feature.
// They describe a peer's advertisement, not what this build supports.
const (
	DatafortsBlobTreeSupported           = "dataforts:blob-tree-supported"
	DatafortsBlobCDCSupported            = "dataforts:blob-cdc-supported"
	DatafortsBlobErasureSupported        = "dataforts:blob-erasure-supported"
	DatafortsBlobBandwidthClassSupported = "dataforts:blob-bandwidth-class-supported"
)

// BlobEncoding selects how StoreTree protects a tree's chunks.
type BlobEncoding struct {
	// ReedSolomon selects (K, M) erasure coding; false means Replicated.
	ReedSolomon bool
	// K data and M parity chunks per stripe. Both zero with ReedSolomon
	// selects the core defaults; otherwise both must be at least 1 and
	// K+M at most 255. Must be zero for Replicated.
	K, M uint8
}

var (
	// EncodingReplicated stores every chunk replicated (the default).
	EncodingReplicated = BlobEncoding{}
	// EncodingDefaultReedSolomon is Reed-Solomon with the core's default
	// (k, m).
	EncodingDefaultReedSolomon = BlobEncoding{ReedSolomon: true}
)

// EncodingReedSolomon is Reed-Solomon with k data and m parity chunks per
// stripe. A stripe closes only at k full chunks; a short trailing stripe
// is stored Replicated, without parity.
func EncodingReedSolomon(k, m uint8) BlobEncoding {
	return BlobEncoding{ReedSolomon: true, K: k, M: m}
}

// RepairReport is RepairBlob's account of the walk. A nil error from
// RepairBlob does not mean the blob is whole: check StripesUnrecoverable.
type RepairReport struct {
	StripesWalked            uint64 `json:"stripes_walked"`
	StripesAlreadyHealthy    uint64 `json:"stripes_already_healthy"`
	StripesRepaired          uint64 `json:"stripes_repaired"`
	ChunksRestored           uint64 `json:"chunks_restored"`
	StripesUnrecoverable     uint64 `json:"stripes_unrecoverable"`
	ReplicatedStripesSkipped uint64 `json:"replicated_stripes_skipped"`
	ReplicatedLeavesSkipped  uint64 `json:"replicated_leaves_skipped"`
}

// TreeNodeCacheStats are the tree-node cache counters.
type TreeNodeCacheStats struct {
	Hits    uint64 `json:"hits"`
	Misses  uint64 `json:"misses"`
	Bytes   uint64 `json:"bytes"`
	Entries uint64 `json:"entries"`
}

// BlobRefInfo describes an encoded ref. The pointer fields are nil when the
// ref's shape has no such field (never zero-filled).
type BlobRefInfo struct {
	Version   uint8  `json:"version"`
	URI       string `json:"uri"`
	Size      uint64 `json:"size"`
	IsTree    bool   `json:"is_tree"`
	IsChunked bool   `json:"is_chunked"`
	// Hash is the content hash of a small (unchunked) ref, lowercase hex.
	Hash *string `json:"hash"`
	// TreeRootHash and TreeDepth are set for tree refs.
	TreeRootHash *string `json:"tree_root_hash"`
	TreeDepth    *uint8  `json:"tree_depth"`
	// Encoding is set for chunked refs.
	Encoding *BlobEncoding `json:"-"`
}

type blobRefInfoWire struct {
	BlobRefInfo
	Encoding *struct {
		Kind string `json:"kind"`
		K    uint8  `json:"k"`
		M    uint8  `json:"m"`
	} `json:"encoding"`
}

// takeCJSON copies a JSON string the C side allocated, frees it, and
// decodes it into v.
func takeCJSON(op string, s *C.char, v any) error {
	if s == nil {
		return fmt.Errorf("%w: %s returned no JSON", ErrBlob, op)
	}
	defer C.net_free_string(s)
	if err := json.Unmarshal([]byte(C.GoString(s)), v); err != nil {
		return fmt.Errorf("%w: %s: decode JSON: %v", ErrBlob, op, err)
	}
	return nil
}

func refArg(ref []byte) (*C.uint8_t, error) {
	if len(ref) == 0 {
		return nil, fmt.Errorf("%w: blob ref is empty", ErrBlobInvalidArgument)
	}
	return (*C.uint8_t)(unsafe.Pointer(&ref[0])), nil
}

// FetchRange returns bytes [start, end) of the blob ref names, for any ref
// shape. start == end returns an empty slice, even past the end. A reversed
// range, one longer than 1 GiB, or one ending past the blob is
// ErrBlobInvalidArgument. Partial reads are not verified against the
// whole-content hash.
func (a *MeshBlobAdapter) FetchRange(ref []byte, start, end uint64) ([]byte, error) {
	refPtr, err := refArg(ref)
	if err != nil {
		return nil, err
	}
	var out *C.uint8_t
	var outLen C.size_t
	var rc C.int
	if !a.withReadHandle(func(h *C.net_mesh_blob_adapter_t) {
		rc = C.net_mesh_blob_adapter_fetch_range(h, refPtr, C.size_t(len(ref)),
			C.uint64_t(start), C.uint64_t(end), &out, &outLen)
	}) {
		return nil, ErrBlobClosed
	}
	runtime.KeepAlive(ref)
	if err := blobRegistryError("fetch_range", rc); err != nil {
		return nil, err
	}
	defer C.net_blob_free_buffer(out, outLen)
	body, err := copyCBuf(unsafe.Pointer(out), uint64(outLen))
	if err != nil {
		return nil, fmt.Errorf("%w: fetch_range: %w", ErrBlob, err)
	}
	return body, nil
}

// StoreTree stores data as a tree blob with the given encoding and default
// chunking, and returns the encoded tree ref.
func (a *MeshBlobAdapter) StoreTree(data []byte, enc BlobEncoding) ([]byte, error) {
	kind := C.uint8_t(0)
	if enc.ReedSolomon {
		kind = 1
	}
	var dataPtr *C.uint8_t
	if len(data) > 0 {
		dataPtr = (*C.uint8_t)(unsafe.Pointer(&data[0]))
	}
	var out *C.uint8_t
	var outLen C.size_t
	var rc C.int
	if !a.withReadHandle(func(h *C.net_mesh_blob_adapter_t) {
		rc = C.net_mesh_blob_adapter_store_tree(h, dataPtr, C.size_t(len(data)),
			kind, C.uint8_t(enc.K), C.uint8_t(enc.M), &out, &outLen)
	}) {
		return nil, ErrBlobClosed
	}
	runtime.KeepAlive(data)
	if err := blobRegistryError("store_tree", rc); err != nil {
		return nil, err
	}
	defer C.net_blob_free_buffer(out, outLen)
	ref, err := copyCBuf(unsafe.Pointer(out), uint64(outLen))
	if err != nil {
		return nil, fmt.Errorf("%w: store_tree: %w", ErrBlob, err)
	}
	return ref, nil
}

// RepairBlob rebuilds missing data chunks of a Reed-Solomon tree blob from
// parity, in place. Stripes it cannot rebuild are counted in the report,
// not returned as an error.
func (a *MeshBlobAdapter) RepairBlob(ref []byte) (*RepairReport, error) {
	refPtr, err := refArg(ref)
	if err != nil {
		return nil, err
	}
	var out *C.char
	var rc C.int
	if !a.withReadHandle(func(h *C.net_mesh_blob_adapter_t) {
		rc = C.net_mesh_blob_adapter_repair_blob(h, refPtr, C.size_t(len(ref)), &out)
	}) {
		return nil, ErrBlobClosed
	}
	runtime.KeepAlive(ref)
	if err := blobRegistryError("repair_blob", rc); err != nil {
		return nil, err
	}
	var report RepairReport
	if err := takeCJSON("repair_blob", out, &report); err != nil {
		return nil, err
	}
	return &report, nil
}

// TreeNodeCacheStats returns the tree-node cache counters, or nil when the
// adapter was built without a cache (MeshBlobAdapterOpts.TreeNodeCacheBytes
// unset).
func (a *MeshBlobAdapter) TreeNodeCacheStats() (*TreeNodeCacheStats, error) {
	var out *C.char
	var rc C.int
	if !a.withReadHandle(func(h *C.net_mesh_blob_adapter_t) {
		rc = C.net_mesh_blob_adapter_tree_node_cache_stats(h, &out)
	}) {
		return nil, ErrBlobClosed
	}
	if err := blobRegistryError("tree_node_cache_stats", rc); err != nil {
		return nil, err
	}
	var stats *TreeNodeCacheStats
	if err := takeCJSON("tree_node_cache_stats", out, &stats); err != nil {
		return nil, err
	}
	return stats, nil
}

// DescribeBlobRef decodes an encoded ref into its fields.
func DescribeBlobRef(ref []byte) (*BlobRefInfo, error) {
	refPtr, err := refArg(ref)
	if err != nil {
		return nil, err
	}
	var out *C.char
	rc := C.net_blob_ref_describe(refPtr, C.size_t(len(ref)), &out)
	runtime.KeepAlive(ref)
	if err := blobRegistryError("ref_describe", rc); err != nil {
		return nil, err
	}
	var wire blobRefInfoWire
	if err := takeCJSON("ref_describe", out, &wire); err != nil {
		return nil, err
	}
	info := wire.BlobRefInfo
	if e := wire.Encoding; e != nil {
		switch e.Kind {
		case "replicated":
			info.Encoding = &BlobEncoding{}
		case "reed_solomon":
			info.Encoding = &BlobEncoding{ReedSolomon: true, K: e.K, M: e.M}
		default:
			return nil, fmt.Errorf("%w: ref_describe: unknown encoding kind %q", ErrBlob, e.Kind)
		}
	}
	return &info, nil
}

// newMeshBlobAdapterV2 is the constructor path for options the legacy C
// constructor cannot carry (the tree-node cache).
func newMeshBlobAdapterV2(redex *Redex, cID *C.char, persistent C.int, opts *MeshBlobAdapterOpts) (*C.net_mesh_blob_adapter_t, error) {
	body := map[string]any{"tree_node_cache_bytes": *opts.TreeNodeCacheBytes}
	if opts.Overflow != nil {
		body["overflow"] = opts.Overflow
	}
	encoded, err := json.Marshal(body)
	if err != nil {
		return nil, fmt.Errorf("%w: %v", ErrBlobInvalidConfig, err)
	}
	cOpts := C.CString(string(encoded))
	defer C.free(unsafe.Pointer(cOpts))
	redex.mu.RLock()
	defer redex.mu.RUnlock()
	if redex.handle == nil {
		return nil, fmt.Errorf("%w: redex: %w", ErrBlob, ErrShuttingDown)
	}
	var h *C.net_mesh_blob_adapter_t
	rc := C.net_mesh_blob_adapter_new_v2(redex.handle, cID, persistent, cOpts, &h)
	switch {
	case rc == netErrInvalidJSON:
		return nil, fmt.Errorf("%w: new_v2 rc=%d", ErrBlobInvalidConfig, int(rc))
	case rc != 0:
		return nil, blobRegistryError("new_v2", rc)
	case h == nil:
		return nil, fmt.Errorf("%w: new_v2 returned no handle", ErrBlob)
	}
	return h, nil
}
