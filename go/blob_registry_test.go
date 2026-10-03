package net

// Tests for the process-wide blob adapter registry (go/blob_registry.go).
// Plan: docs/internal/plans/GO_BINDING_CONSOLIDATION_AND_BLOBS_PLAN.md, S5.
//
// The registry is global to the process, so every test registers under its
// own id and unregisters it on cleanup.

import (
	"bytes"
	"encoding/hex"
	"errors"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"testing"
)

// registerFS registers a filesystem adapter under an id unique to the test
// and returns the id and its root.
func registerFS(t *testing.T, suffix string) (string, string) {
	t.Helper()
	id := "go-test/" + t.Name() + "/" + suffix
	root := t.TempDir()
	if err := RegisterFilesystemBlobAdapter(id, root); err != nil {
		t.Fatalf("RegisterFilesystemBlobAdapter(%q): %v", id, err)
	}
	t.Cleanup(func() { UnregisterBlobAdapter(id) })
	return id, root
}

// blobPath is where the filesystem adapter keeps a blob:
// <root>/<hash[0:2]>/<hash>.
func blobPath(t *testing.T, root string, ref []byte) string {
	t.Helper()
	hash, err := BlobRefHash(ref)
	if err != nil {
		t.Fatalf("BlobRefHash: %v", err)
	}
	h := hex.EncodeToString(hash[:])
	return filepath.Join(root, h[:2], h)
}

func TestBlobRegistryFilesystemRoundTrip(t *testing.T) {
	id, root := registerFS(t, "a")
	if ok, err := BlobAdapterRegistered(id); err != nil || !ok {
		t.Fatalf("BlobAdapterRegistered after register = %v, %v; want true", ok, err)
	}

	data := []byte("through the process-wide registry")
	ref, err := BlobPublish(id, "file:///go/registry", data)
	if err != nil {
		t.Fatalf("BlobPublish: %v", err)
	}
	// The bytes are on disk where the adapter says it keeps them.
	onDisk, err := os.ReadFile(blobPath(t, root, ref))
	if err != nil {
		t.Fatalf("published blob is not under the adapter root: %v", err)
	}
	if !bytes.Equal(onDisk, data) {
		t.Fatalf("on-disk blob = %q, want %q", onDisk, data)
	}
	got, err := BlobResolve(id, ref)
	if err != nil {
		t.Fatalf("BlobResolve: %v", err)
	}
	if !bytes.Equal(got, data) {
		t.Fatalf("BlobResolve = %q, want %q", got, data)
	}
}

func TestBlobRegistryDuplicateAndUnregister(t *testing.T) {
	id, _ := registerFS(t, "dup")
	if err := RegisterFilesystemBlobAdapter(id, t.TempDir()); !errors.Is(err, ErrBlobDuplicateID) || !errors.Is(err, ErrBlob) {
		t.Fatalf("duplicate id: want ErrBlobDuplicateID (and ErrBlob), got %v", err)
	}
	ref, err := BlobPublish(id, "file:///go/dup", []byte("x"))
	if err != nil {
		t.Fatalf("BlobPublish: %v", err)
	}

	removed, err := UnregisterBlobAdapter(id)
	if err != nil || !removed {
		t.Fatalf("UnregisterBlobAdapter = %v, %v; want true", removed, err)
	}
	if ok, _ := BlobAdapterRegistered(id); ok {
		t.Fatal("still registered after unregister")
	}
	if _, err := BlobResolve(id, ref); !errors.Is(err, ErrBlobNotRegistered) {
		t.Fatalf("resolve after unregister: want ErrBlobNotRegistered, got %v", err)
	}
	if removed, err := UnregisterBlobAdapter(id); err != nil || removed {
		t.Fatalf("second UnregisterBlobAdapter = %v, %v; want false", removed, err)
	}
	// The id is free again.
	if err := RegisterFilesystemBlobAdapter(id, t.TempDir()); err != nil {
		t.Fatalf("re-register a freed id: %v", err)
	}
}

// The adapter is named explicitly: resolving through a different adapter
// looks only there.
func TestBlobRegistryResolveUsesTheNamedAdapter(t *testing.T) {
	idA, _ := registerFS(t, "a")
	idB, rootB := registerFS(t, "b")
	ref, err := BlobPublish(idA, "file:///go/named", []byte("only in A"))
	if err != nil {
		t.Fatalf("BlobPublish: %v", err)
	}
	if _, err := BlobResolve(idB, ref); !errors.Is(err, ErrBlobNotFound) {
		t.Fatalf("resolve through the other adapter: want ErrBlobNotFound, got %v", err)
	}
	entries, err := os.ReadDir(rootB)
	if err != nil {
		t.Fatal(err)
	}
	if len(entries) != 0 {
		t.Fatalf("a failed resolve wrote into the other adapter's root: %v", entries)
	}
	if _, err := BlobResolve("go-test/never-registered", ref); !errors.Is(err, ErrBlobNotRegistered) {
		t.Fatalf("resolve through an unknown id: want ErrBlobNotRegistered, got %v", err)
	}
}

func TestBlobRegistryRefusals(t *testing.T) {
	id, root := registerFS(t, "refuse")

	if _, err := BlobPublish(id, "mesh://go/wrong-scheme", []byte("x")); !errors.Is(err, ErrBlobUnsupportedScheme) {
		t.Fatalf("mesh: URI on a filesystem adapter: want ErrBlobUnsupportedScheme, got %v", err)
	}
	// Current behavior, pinned: a payload that is not a BlobRef (no
	// BLOB_REF_MAGIC prefix) is inline and resolves to itself.
	inline := []byte("an inline payload, not a ref")
	if got, err := BlobResolve(id, inline); err != nil || !bytes.Equal(got, inline) {
		t.Fatalf("inline payload: got %q, %v; want it back unchanged", got, err)
	}

	ref, err := BlobPublish(id, "file:///go/tamper", []byte("original"))
	if err != nil {
		t.Fatalf("BlobPublish: %v", err)
	}
	// A ref (magic prefix) cut to 7 bytes cannot hold its 40-byte body.
	if _, err := BlobResolve(id, ref[:7]); !errors.Is(err, ErrBlobDecode) {
		t.Fatalf("truncated ref: want ErrBlobDecode, got %v", err)
	}
	// Content that no longer matches its address is refused, not served.
	if err := os.WriteFile(blobPath(t, root, ref), []byte("tampered"), 0o644); err != nil {
		t.Fatal(err)
	}
	if _, err := BlobResolve(id, ref); !errors.Is(err, ErrBlobHashMismatch) {
		t.Fatalf("tampered blob: want ErrBlobHashMismatch, got %v", err)
	}
}

// C.CString stops at a NUL, so "a\x00b" would silently become "a". The
// binding refuses instead of addressing a different adapter.
func TestBlobRegistryRefusesEmbeddedNUL(t *testing.T) {
	prefix := "go-test/" + t.Name()
	if err := RegisterFilesystemBlobAdapter(prefix+"\x00suffix", t.TempDir()); !errors.Is(err, ErrBlobInvalidArgument) {
		t.Fatalf("NUL in id: want ErrBlobInvalidArgument, got %v", err)
	}
	if ok, _ := BlobAdapterRegistered(prefix); ok {
		UnregisterBlobAdapter(prefix)
		t.Fatal("a NUL-bearing id registered its truncated prefix")
	}
	id, _ := registerFS(t, "nul")
	if _, err := BlobPublish(id, "file:///a\x00b", []byte("x")); !errors.Is(err, ErrBlobInvalidArgument) {
		t.Fatalf("NUL in uri: want ErrBlobInvalidArgument, got %v", err)
	}
	if _, err := BlobResolve(id+"\x00", nil); !errors.Is(err, ErrBlobInvalidArgument) {
		t.Fatalf("NUL in resolve id: want ErrBlobInvalidArgument, got %v", err)
	}
}

// The NET_ERR_BLOB_* codes are defined only in Rust (src/ffi/blob.rs), not
// in any header, so the mapping is pinned against that source.
func TestABIStabilityBlobRegistryCodes(t *testing.T) {
	src, err := os.ReadFile("../net/crates/net/src/ffi/blob.rs")
	if err != nil {
		t.Fatalf("read src/ffi/blob.rs: %v", err)
	}
	want := map[string]error{
		"NET_ERR_BLOB_DECODE":                 ErrBlobDecode,
		"NET_ERR_BLOB_DUPLICATE_ID":           ErrBlobDuplicateID,
		"NET_ERR_BLOB_NOT_REGISTERED":         ErrBlobNotRegistered,
		"NET_ERR_BLOB_NOT_FOUND":              ErrBlobNotFound,
		"NET_ERR_BLOB_HASH_MISMATCH":          ErrBlobHashMismatch,
		"NET_ERR_BLOB_BACKEND":                ErrBlobBackend,
		"NET_ERR_BLOB_UNSUPPORTED_SCHEME":     ErrBlobUnsupportedScheme,
		"NET_ERR_BLOB_ADAPTER_NOT_REGISTERED": ErrBlobNotRegistered,
		"NET_ERR_BLOB_UNAUTHORIZED":           ErrBlobUnauthorized,
		"NET_ERR_BLOB_INVALID_ARGUMENT":       ErrBlobInvalidArgument,
		"NET_ERR_BLOB_PANIC":                  ErrBlob, // message only
		"NET_ERR_BLOB_ADAPTER_NOT_CONFIGURED": ErrBlob, // message only
	}
	re := regexp.MustCompile(`(?m)^pub const (NET_ERR_BLOB_\w+): c_int = (-?\d+);`)
	matches := re.FindAllStringSubmatch(string(src), -1)
	if len(matches) == 0 {
		t.Fatal("found no NET_ERR_BLOB_* constants in src/ffi/blob.rs")
	}
	for _, m := range matches {
		sentinel, ok := want[m[1]]
		if !ok {
			t.Errorf("%s (%s) has no pinned Go mapping; add it here and to blobRegistryError", m[1], m[2])
			continue
		}
		code, _ := strconv.Atoi(m[2])
		got := blobRegistryErrorFromInt(code)
		if !errors.Is(got, sentinel) || !errors.Is(got, ErrBlob) {
			t.Errorf("%s (%d) maps to %v, want %v (and ErrBlob)", m[1], code, got, sentinel)
		}
		if sentinel != ErrBlob && strings.Contains(got.Error(), "rc=") {
			t.Errorf("%s (%d) falls through to the generic branch", m[1], code)
		}
	}
	if len(matches) != len(want) {
		t.Errorf("src/ffi/blob.rs has %d NET_ERR_BLOB_* constants, the pin table has %d", len(matches), len(want))
	}
}
