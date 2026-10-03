package net

// Tests for the v0.3 blob surface (go/blob_tree.go). Plan:
// docs/internal/plans/GO_BINDING_CONSOLIDATION_AND_BLOBS_PLAN.md, S6. The
// Reed-Solomon repair witness needs a fixtures-only seam and lives in
// blob_repair_test.go (build tag test_helpers).

import (
	"bytes"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"os"
	"strings"
	"testing"
)

const mib = 1 << 20

// distinctChunks returns n chunks of size bytes, each with its own content,
// so content addressing cannot collapse two of them into one.
func distinctChunks(n, size int) []byte {
	out := make([]byte, 0, n*size)
	for i := 0; i < n; i++ {
		chunk := bytes.Repeat([]byte{byte(0x10 + i), byte(i * 7), 0xA5, byte(255 - i)}, size/4)
		out = append(out, chunk...)
	}
	return out
}

func TestBlobTreeStoreAndFetchRange(t *testing.T) {
	r, a := newBlobAdapter(t, "", "go-tree-replicated", nil)
	defer r.Free()
	defer a.Close()

	// 2.25 chunks at the default 4 MiB chunk size: several leaves.
	data := distinctChunks(9, mib)
	ref, err := a.StoreTree(data, EncodingReplicated)
	if err != nil {
		t.Fatalf("StoreTree: %v", err)
	}
	info, err := DescribeBlobRef(ref)
	if err != nil {
		t.Fatalf("DescribeBlobRef: %v", err)
	}
	if !info.IsTree || !info.IsChunked || info.Size != uint64(len(data)) {
		t.Fatalf("tree ref described as %+v", info)
	}
	if info.TreeRootHash == nil || len(*info.TreeRootHash) != 64 || info.TreeDepth == nil || *info.TreeDepth == 0 {
		t.Fatalf("tree fields missing: root=%v depth=%v", info.TreeRootHash, info.TreeDepth)
	}
	if info.Hash != nil {
		t.Fatalf("a tree ref has no single content hash, got %q", *info.Hash)
	}
	if info.Encoding == nil || info.Encoding.ReedSolomon {
		t.Fatalf("encoding = %+v, want Replicated", info.Encoding)
	}

	whole, err := a.FetchRange(ref, 0, info.Size)
	if err != nil {
		t.Fatalf("FetchRange whole: %v", err)
	}
	if !bytes.Equal(whole, data) {
		t.Fatal("FetchRange(0, size) differs from the stored bytes")
	}
	// A range that crosses a chunk boundary.
	start, end := uint64(4*mib-100), uint64(4*mib+4096)
	part, err := a.FetchRange(ref, start, end)
	if err != nil {
		t.Fatalf("FetchRange [%d,%d): %v", start, end, err)
	}
	if !bytes.Equal(part, data[start:end]) {
		t.Fatal("FetchRange across a chunk boundary returned the wrong bytes")
	}
	// Core refuses a whole-blob Fetch of a tree; FetchRange is the way.
	if _, err := a.Fetch(ref); err == nil {
		t.Fatal("Fetch of a tree ref succeeded; core is expected to refuse it")
	}
}

// The range checks run in core's order: reversed first, then empty (which
// succeeds anywhere), then cap and extent.
func TestBlobTreeRangeContract(t *testing.T) {
	r, a := newBlobAdapter(t, "", "go-tree-range", nil)
	defer r.Free()
	defer a.Close()
	data := []byte("a small tree")
	ref, err := a.StoreTree(data, EncodingReplicated)
	if err != nil {
		t.Fatalf("StoreTree: %v", err)
	}
	size := uint64(len(data))

	if _, err := a.FetchRange(ref, 5, 4); !errors.Is(err, ErrBlobInvalidArgument) {
		t.Fatalf("reversed range: want ErrBlobInvalidArgument, got %v", err)
	}
	for _, at := range []uint64{0, 3, size, size + 1000} {
		got, err := a.FetchRange(ref, at, at)
		if err != nil || got == nil || len(got) != 0 {
			t.Fatalf("empty range at %d = %v, %v; want empty, nil", at, got, err)
		}
	}
	if _, err := a.FetchRange(ref, 0, size+1); !errors.Is(err, ErrBlobInvalidArgument) {
		t.Fatalf("range past the end: want ErrBlobInvalidArgument, got %v", err)
	}
	// The cap needs a ref whose extent covers the range, or the
	// past-the-end check refuses it first and the cap goes untested. A small
	// ref (the cross-lang `small` vector) re-encoded to claim 2 GiB: the cap
	// refuses 1 GiB + 1, and exactly 1 GiB passes the checks and fails only
	// as not found, since the adapter holds no such content.
	big := bigSmallRef(t, 2<<30)
	if _, err := a.FetchRange(big, 0, 1<<30+1); !errors.Is(err, ErrBlobInvalidArgument) {
		t.Fatalf("range over the 1 GiB cap: want ErrBlobInvalidArgument, got %v", err)
	}
	if _, err := a.FetchRange(big, 0, 1<<30); !errors.Is(err, ErrBlobNotFound) {
		t.Fatalf("range of exactly 1 GiB: want ErrBlobNotFound past the checks, got %v", err)
	}
	if _, err := a.FetchRange(nil, 0, 1); !errors.Is(err, ErrBlobInvalidArgument) {
		t.Fatalf("empty ref: want ErrBlobInvalidArgument, got %v", err)
	}
	if got, err := a.FetchRange(ref, 2, 7); err != nil || string(got) != string(data[2:7]) {
		t.Fatalf("FetchRange [2,7) = %q, %v; want %q", got, err, data[2:7])
	}
}

func TestBlobTreeEncodingRefusals(t *testing.T) {
	r, a := newBlobAdapter(t, "", "go-tree-encoding", nil)
	defer r.Free()
	defer a.Close()
	data := []byte("x")
	for name, enc := range map[string]BlobEncoding{
		"replicated with k": {K: 1},
		"replicated with m": {M: 1},
		"rs with only m":    EncodingReedSolomon(0, 2),
		"rs with only k":    EncodingReedSolomon(4, 0),
		"rs with k+m = 256": EncodingReedSolomon(200, 56),
	} {
		if _, err := a.StoreTree(data, enc); !errors.Is(err, ErrBlobInvalidArgument) {
			t.Fatalf("%s: want ErrBlobInvalidArgument, got %v", name, err)
		}
	}
	// Both zero selects the core defaults.
	ref, err := a.StoreTree(data, EncodingDefaultReedSolomon)
	if err != nil {
		t.Fatalf("StoreTree default RS: %v", err)
	}
	info, err := DescribeBlobRef(ref)
	if err != nil {
		t.Fatalf("DescribeBlobRef: %v", err)
	}
	if info.Encoding == nil || !info.Encoding.ReedSolomon || info.Encoding.K == 0 || info.Encoding.M == 0 {
		t.Fatalf("default RS described as %+v, want non-zero (k, m)", info.Encoding)
	}
	if got, err := a.FetchRange(ref, 0, info.Size); err != nil || !bytes.Equal(got, data) {
		t.Fatalf("FetchRange of an RS tree = %q, %v", got, err)
	}
}

func TestBlobRefDescribeSmall(t *testing.T) {
	r, a := newBlobAdapter(t, "", "go-describe-small", nil)
	defer r.Free()
	defer a.Close()
	data := []byte("abc")
	ref, err := a.Publish("mesh://go/describe", data)
	if err != nil {
		t.Fatalf("Publish: %v", err)
	}
	info, err := DescribeBlobRef(ref)
	if err != nil {
		t.Fatalf("DescribeBlobRef: %v", err)
	}
	hash, _ := BlobRefHash(ref)
	if info.Hash == nil || *info.Hash != hex.EncodeToString(hash[:]) {
		t.Fatalf("small ref hash = %v, want %x", info.Hash, hash)
	}
	if info.URI != "mesh://go/describe" || info.Size != 3 || info.IsTree || info.IsChunked {
		t.Fatalf("small ref described as %+v", info)
	}
	if info.TreeRootHash != nil || info.TreeDepth != nil || info.Encoding != nil {
		t.Fatalf("small ref carries tree fields: root=%v depth=%v enc=%v", info.TreeRootHash, info.TreeDepth, info.Encoding)
	}
	if _, err := DescribeBlobRef([]byte{0xB0, 0xB1, 0xB2, 0xB3, 1, 2}); !errors.Is(err, ErrBlobDecode) {
		t.Fatalf("truncated ref: want ErrBlobDecode, got %v", err)
	}
}

// Absent cache, a real cache, and a zero-capacity cache are three states;
// the legacy overflow-only constructor is unchanged.
func TestBlobTreeNodeCacheStates(t *testing.T) {
	data := distinctChunks(9, mib)

	t.Run("absent", func(t *testing.T) {
		r, a := newBlobAdapter(t, "", "go-cache-none", nil)
		defer r.Free()
		defer a.Close()
		if stats, err := a.TreeNodeCacheStats(); err != nil || stats != nil {
			t.Fatalf("no cache: stats = %+v, %v; want nil, nil", stats, err)
		}
	})
	t.Run("enabled without overflow", func(t *testing.T) {
		capBytes := uint64(4 * mib)
		r, a := newBlobAdapter(t, "", "go-cache-on", &MeshBlobAdapterOpts{TreeNodeCacheBytes: &capBytes})
		defer r.Free()
		defer a.Close()
		ref, err := a.StoreTree(data, EncodingReplicated)
		if err != nil {
			t.Fatalf("StoreTree: %v", err)
		}
		for i := 0; i < 2; i++ {
			if _, err := a.FetchRange(ref, 0, uint64(len(data))); err != nil {
				t.Fatalf("FetchRange: %v", err)
			}
		}
		stats, err := a.TreeNodeCacheStats()
		if err != nil || stats == nil {
			t.Fatalf("cache on: stats = %v, %v", stats, err)
		}
		if stats.Hits+stats.Misses == 0 {
			t.Fatalf("two tree walks recorded no cache lookups: %+v", stats)
		}
		if on, _ := a.OverflowEnabled(); on {
			t.Fatal("a cache-only config turned overflow on")
		}
	})
	t.Run("zero capacity", func(t *testing.T) {
		zero := uint64(0)
		r, a := newBlobAdapter(t, "", "go-cache-zero", &MeshBlobAdapterOpts{TreeNodeCacheBytes: &zero})
		defer r.Free()
		defer a.Close()
		stats, err := a.TreeNodeCacheStats()
		if err != nil || stats == nil {
			t.Fatalf("zero cache: stats = %v, %v; want present", stats, err)
		}
		if stats.Bytes != 0 || stats.Entries != 0 {
			t.Fatalf("zero-capacity cache holds %+v", stats)
		}
	})
	t.Run("cache with overflow", func(t *testing.T) {
		capBytes := uint64(mib)
		ov := OverflowConfig{Enabled: true, HighWaterRatio: 0.9, LowWaterRatio: 0.6, MaxPushesPerTick: 7, Scope: "zone", TickIntervalMs: 1234}
		r, a := newBlobAdapter(t, "", "go-cache-overflow", &MeshBlobAdapterOpts{TreeNodeCacheBytes: &capBytes, Overflow: &ov})
		defer r.Free()
		defer a.Close()
		assertOverflowConfig(t, a, ov)
		if stats, err := a.TreeNodeCacheStats(); err != nil || stats == nil {
			t.Fatalf("cache + overflow: stats = %v, %v", stats, err)
		}
		bad := uint64(mib)
		badOv := OverflowConfig{Enabled: true, Scope: "galaxy"}
		rb := NewRedex("")
		defer rb.Free()
		if _, err := NewMeshBlobAdapter(rb, "go-cache-bad", &MeshBlobAdapterOpts{TreeNodeCacheBytes: &bad, Overflow: &badOv}); !errors.Is(err, ErrBlobInvalidConfig) {
			t.Fatalf("bad overflow through new_v2: want ErrBlobInvalidConfig, got %v", err)
		}
	})
	t.Run("legacy overflow-only", func(t *testing.T) {
		ov := OverflowConfig{Enabled: true, HighWaterRatio: 0.9, LowWaterRatio: 0.6, MaxPushesPerTick: 7, Scope: "zone", TickIntervalMs: 1234}
		r, a := newBlobAdapter(t, "", "go-cache-legacy", &MeshBlobAdapterOpts{Overflow: &ov})
		defer r.Free()
		defer a.Close()
		assertOverflowConfig(t, a, ov)
		if stats, err := a.TreeNodeCacheStats(); err != nil || stats != nil {
			t.Fatalf("legacy constructor grew a cache: %+v, %v", stats, err)
		}
	})
}

// The tag strings are the core's, verbatim.
func TestBlobFeatureTagsMatchCore(t *testing.T) {
	for file, tag := range map[string]string{
		"blob_tree.rs": DatafortsBlobTreeSupported,
		"cdc.rs":       DatafortsBlobCDCSupported,
		"erasure.rs":   DatafortsBlobErasureSupported,
		"bandwidth.rs": DatafortsBlobBandwidthClassSupported,
	} {
		src, err := os.ReadFile("../net/crates/net/src/adapter/net/dataforts/blob/" + file)
		if err != nil {
			t.Fatalf("read %s: %v", file, err)
		}
		if !strings.Contains(string(src), `"`+tag+`"`) {
			t.Fatalf("%s does not define the tag %q", file, tag)
		}
	}
}

// bigSmallRef is the cross-lang `small` describe vector (magic, tag 0x01,
// 32-byte hash, little-endian u64 size, empty URI) with its size replaced.
func bigSmallRef(t *testing.T, size uint64) []byte {
	t.Helper()
	ref, err := hex.DecodeString("b0b1b2b3016437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d850300000000000000")
	if err != nil {
		t.Fatal(err)
	}
	binary.LittleEndian.PutUint64(ref[5+32:5+32+8], size)
	info, err := DescribeBlobRef(ref)
	if err != nil || info.Size != size {
		t.Fatalf("re-encoded ref describes as %+v, %v; want size %d", info, err, size)
	}
	return ref
}
