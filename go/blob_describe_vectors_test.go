package net

// Cross-language BlobRef describe fixture (plan S6, "cross-language
// fixture"). Go produces the refs; this test and the Python suite
// (bindings/python/tests/test_cross_lang_blob_describe.py) both decode them
// and must agree with the frozen, normalized description.
//
// Normalized form: version, uri, size, is_tree, is_chunked always; hash
// (lowercase hex) only for small refs; tree_root_hash (hex) and tree_depth
// only for tree refs; encoding only for chunked refs. Python exposes no
// encoding getter and zero-fills shape-specific getters, so it compares
// every field but encoding, by shape.
//
// Regenerate (only when the ref format changes, deliberately):
//
//	NET_REGEN_BLOB_DESCRIBE_VECTORS=1 go test -run TestCrossLangBlobDescribeVectors .

import (
	"encoding/hex"
	"encoding/json"
	"os"
	"reflect"
	"testing"
)

const blobDescribeVectorsPath = "../net/crates/net/tests/cross_lang_blob/describe_vectors.json"

type blobDescribeVector struct {
	Name       string         `json:"name"`
	EncodedHex string         `json:"encoded_hex"`
	Expected   map[string]any `json:"expected"`
}

type blobDescribeFixture struct {
	Description string               `json:"description"`
	Vectors     []blobDescribeVector `json:"vectors"`
}

// normalizeBlobRefInfo renders a description in the fixture's normalized
// form, round-tripped through JSON so numbers compare as the fixture's do.
func normalizeBlobRefInfo(t *testing.T, info *BlobRefInfo) map[string]any {
	t.Helper()
	m := map[string]any{
		"version":    info.Version,
		"uri":        info.URI,
		"size":       info.Size,
		"is_tree":    info.IsTree,
		"is_chunked": info.IsChunked,
	}
	if info.Hash != nil {
		m["hash"] = *info.Hash
	}
	if info.TreeRootHash != nil {
		m["tree_root_hash"] = *info.TreeRootHash
	}
	if info.TreeDepth != nil {
		m["tree_depth"] = *info.TreeDepth
	}
	if e := info.Encoding; e != nil {
		if e.ReedSolomon {
			m["encoding"] = map[string]any{"kind": "reed_solomon", "k": e.K, "m": e.M}
		} else {
			m["encoding"] = map[string]any{"kind": "replicated"}
		}
	}
	raw, err := json.Marshal(m)
	if err != nil {
		t.Fatal(err)
	}
	var out map[string]any
	if err := json.Unmarshal(raw, &out); err != nil {
		t.Fatal(err)
	}
	return out
}

func generateBlobDescribeVectors(t *testing.T) blobDescribeFixture {
	t.Helper()
	r, a := newBlobAdapter(t, "", "go-describe-vectors", nil)
	defer r.Free()
	defer a.Close()

	type src struct {
		name string
		make func() ([]byte, error)
	}
	srcs := []src{
		{"small", func() ([]byte, error) { return a.Publish("mesh://fixtures/small", []byte("abc")) }},
		{"small_unicode_uri", func() ([]byte, error) {
			return a.Publish("mesh://fixtures/ünïcödé/κλειδί", []byte("unicode uri"))
		}},
		{"tree_replicated", func() ([]byte, error) { return a.StoreTree(distinctChunks(9, mib), EncodingReplicated) }},
		{"tree_reed_solomon_4_2", func() ([]byte, error) {
			return a.StoreTree(distinctChunks(4, 4*mib), EncodingReedSolomon(4, 2))
		}},
	}
	fx := blobDescribeFixture{
		Description: "Cross-language BlobRef describe fixture. Refs are produced by the Go " +
			"binding (go/blob_describe_vectors_test.go). Go (DescribeBlobRef) and Python " +
			"(BlobRef.from_encoded getters) must decode each to `expected`, and Python must " +
			"re-encode it to the same bytes. Normalized: hash only for small refs, tree_root_hash " +
			"and tree_depth only for tree refs, encoding only for chunked refs (Go checks it; " +
			"Python has no encoding getter). Hashes are lowercase hex.",
	}
	for _, s := range srcs {
		ref, err := s.make()
		if err != nil {
			t.Fatalf("%s: %v", s.name, err)
		}
		info, err := DescribeBlobRef(ref)
		if err != nil {
			t.Fatalf("%s: describe: %v", s.name, err)
		}
		fx.Vectors = append(fx.Vectors, blobDescribeVector{
			Name:       s.name,
			EncodedHex: hex.EncodeToString(ref),
			Expected:   normalizeBlobRefInfo(t, info),
		})
	}
	return fx
}

func TestCrossLangBlobDescribeVectors(t *testing.T) {
	if os.Getenv("NET_REGEN_BLOB_DESCRIBE_VECTORS") == "1" {
		fx := generateBlobDescribeVectors(t)
		out, err := json.MarshalIndent(fx, "", "  ")
		if err != nil {
			t.Fatal(err)
		}
		if err := os.MkdirAll("../net/crates/net/tests/cross_lang_blob", 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(blobDescribeVectorsPath, append(out, '\n'), 0o644); err != nil {
			t.Fatal(err)
		}
		t.Logf("regenerated %s (%d vectors)", blobDescribeVectorsPath, len(fx.Vectors))
	}

	raw, err := os.ReadFile(blobDescribeVectorsPath)
	if err != nil {
		t.Fatalf("read fixture: %v", err)
	}
	var fx blobDescribeFixture
	if err := json.Unmarshal(raw, &fx); err != nil {
		t.Fatalf("decode fixture: %v", err)
	}
	if len(fx.Vectors) < 4 {
		t.Fatalf("fixture has %d vectors, want at least 4", len(fx.Vectors))
	}
	shapes := map[string]bool{}
	for _, v := range fx.Vectors {
		ref, err := hex.DecodeString(v.EncodedHex)
		if err != nil {
			t.Fatalf("%s: bad hex: %v", v.Name, err)
		}
		info, err := DescribeBlobRef(ref)
		if err != nil {
			t.Fatalf("%s: DescribeBlobRef: %v", v.Name, err)
		}
		got := normalizeBlobRefInfo(t, info)
		if !reflect.DeepEqual(got, v.Expected) {
			t.Fatalf("%s: describe = %v\nwant %v", v.Name, got, v.Expected)
		}
		shapes[map[bool]string{true: "tree", false: "small"}[info.IsTree]] = true
	}
	if !shapes["tree"] || !shapes["small"] {
		t.Fatalf("fixture must cover small and tree refs, covers %v", shapes)
	}
}
