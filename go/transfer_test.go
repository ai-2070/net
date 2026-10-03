package net

// Tests for directory transfer and discovered fetch (go/transfer.go). Plan:
// docs/internal/plans/GO_BINDING_CONSOLIDATION_AND_BLOBS_PLAN.md, slice S2.

import (
	"bytes"
	"errors"
	"os"
	"path/filepath"
	"runtime"
	"sort"
	"testing"
)

// transferPair is two handshaken nodes, each with a served adapter. Both
// need the engine: a fetch needs it as much as a serve does.
type transferPair struct {
	a, b     *MeshNode
	adA, adB *MeshBlobAdapter
}

func newTransferPair(t *testing.T) *transferPair {
	t.Helper()
	a, b, cleanup := meshHandshakePair(t)
	rA, adA := newBlobAdapter(t, "", "go-transfer-a", nil)
	rB, adB := newBlobAdapter(t, "", "go-transfer-b", nil)
	t.Cleanup(func() {
		adA.Close()
		adB.Close()
		rA.Free()
		rB.Free()
		cleanup()
	})
	if err := a.ServeBlobTransfer(adA); err != nil {
		t.Fatalf("ServeBlobTransfer(a): %v", err)
	}
	if err := b.ServeBlobTransfer(adB); err != nil {
		t.Fatalf("ServeBlobTransfer(b): %v", err)
	}
	return &transferPair{a: a, b: b, adA: adA, adB: adB}
}

// The fixture tree: three files (one binary, two levels deep) and an
// empty directory, which only survives if Dir entries are carried.
var transferTreeFiles = map[string][]byte{
	"a.txt":            []byte("top-level file\n"),
	"sub/b.bin":        {0x00, 0xFF, 0x10, 0x80, 0x7F, 0x00},
	"sub/deeper/c.txt": bytes.Repeat([]byte("deep "), 300),
}

func writeTransferTree(t *testing.T) string {
	t.Helper()
	root := t.TempDir()
	for rel, body := range transferTreeFiles {
		p := filepath.Join(root, filepath.FromSlash(rel))
		if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(p, body, 0o644); err != nil {
			t.Fatal(err)
		}
		// Pin the bits regardless of umask, for the manifest mode check.
		if err := os.Chmod(p, 0o644); err != nil {
			t.Fatal(err)
		}
	}
	if err := os.MkdirAll(filepath.Join(root, "empty"), 0o755); err != nil {
		t.Fatal(err)
	}
	return root
}

func TestTransferDirRoundTrip(t *testing.T) {
	p := newTransferPair(t)
	manifestRef, err := p.adA.StoreDir(writeTransferTree(t))
	if err != nil {
		t.Fatalf("StoreDir: %v", err)
	}
	if len(manifestRef) == 0 {
		t.Fatal("StoreDir returned an empty manifest ref")
	}

	dest := filepath.Join(t.TempDir(), "out")
	stats, err := p.b.FetchDir(p.a.NodeID(), manifestRef, dest)
	if err != nil {
		t.Fatalf("FetchDir: %v", err)
	}

	var wantBytes uint64
	for rel, want := range transferTreeFiles {
		wantBytes += uint64(len(want))
		got, err := os.ReadFile(filepath.Join(dest, filepath.FromSlash(rel)))
		if err != nil {
			t.Fatalf("read fetched %s: %v", rel, err)
		}
		if !bytes.Equal(got, want) {
			t.Fatalf("fetched %s differs: %d bytes, want %d", rel, len(got), len(want))
		}
	}
	if fi, err := os.Stat(filepath.Join(dest, "empty")); err != nil || !fi.IsDir() {
		t.Fatalf("empty directory did not survive the transfer: %v", err)
	}
	if stats.Files != uint64(len(transferTreeFiles)) || stats.Bytes != wantBytes {
		t.Fatalf("DirStats = %+v, want {Files:%d Bytes:%d}", stats, len(transferTreeFiles), wantBytes)
	}
}

// The JSON boundary itself: a FetchDir byte-compare never touches it.
func TestTransferDirManifestRead(t *testing.T) {
	p := newTransferPair(t)
	manifestRef, err := p.adA.StoreDir(writeTransferTree(t))
	if err != nil {
		t.Fatalf("StoreDir: %v", err)
	}
	m, err := p.b.DirManifestRead(p.a.NodeID(), manifestRef)
	if err != nil {
		t.Fatalf("DirManifestRead: %v", err)
	}
	if m.Version == 0 {
		t.Fatal("manifest version is 0")
	}

	paths := make([]string, len(m.Entries))
	files := map[string]*DirFileEntry{}
	dirs := map[string]bool{}
	for i, e := range m.Entries {
		paths[i] = e.Path
		kinds := 0
		if e.File != nil {
			kinds++
			files[e.Path] = e.File
		}
		if e.Dir != nil {
			kinds++
			dirs[e.Path] = true
		}
		if e.Symlink != nil {
			kinds++
		}
		if kinds != 1 {
			t.Fatalf("entry %q decoded with %d kinds, want exactly 1", e.Path, kinds)
		}
	}
	if !sort.StringsAreSorted(paths) {
		t.Fatalf("entries not sorted by path: %v", paths)
	}
	if len(files) != len(transferTreeFiles) {
		t.Fatalf("manifest has %d file entries, want %d: %v", len(files), len(transferTreeFiles), paths)
	}
	if !dirs["empty"] {
		t.Fatalf("manifest has no Dir entry for the empty directory: %v", paths)
	}

	wantMode := uint32(0o644)
	if runtime.GOOS == "windows" {
		wantMode = 0 // non-Unix stores record no permission bits
	}
	for rel, body := range transferTreeFiles {
		f, ok := files[rel]
		if !ok {
			t.Fatalf("manifest is missing file %q: %v", rel, paths)
		}
		if f.Mode&0o777 != wantMode {
			t.Fatalf("%s mode = %o, want %o", rel, f.Mode&0o777, wantMode)
		}
		// The embedded ref must address this file's bytes. Content
		// addressing makes a fresh publish of the same bytes the oracle.
		got, err := BlobRefHash(f.Blob)
		if err != nil {
			t.Fatalf("%s: embedded ref does not decode: %v", rel, err)
		}
		probe, err := p.adB.Publish("mesh://go/probe", body)
		if err != nil {
			t.Fatalf("Publish probe: %v", err)
		}
		want, _ := BlobRefHash(probe)
		if got != want {
			t.Fatalf("%s: embedded ref hash %x, want %x", rel, got, want)
		}
	}
}

// A well-formed ref to content the reachable holder does not have is a
// fetch failure (NET_ERR_TRANSFER_NOT_FOUND), not a manifest error.
func TestTransferDirManifestMissingContent(t *testing.T) {
	p := newTransferPair(t)
	rC, adC := newBlobAdapter(t, "", "go-transfer-elsewhere", nil)
	defer rC.Free()
	defer adC.Close()
	ref, err := adC.Publish("mesh://go/elsewhere", []byte("only on an unserved adapter"))
	if err != nil {
		t.Fatalf("Publish: %v", err)
	}
	_, err = p.b.DirManifestRead(p.a.NodeID(), ref)
	if !errors.Is(err, ErrTransferNotFound) || !errors.Is(err, ErrTransfer) {
		t.Fatalf("missing content: want ErrTransferNotFound (and ErrTransfer), got %v", err)
	}
	if errors.Is(err, ErrDirInvalidManifest) {
		t.Fatalf("missing content was classified as a malformed manifest: %v", err)
	}
}

// Bytes that fetch fine but are not a manifest: NET_ERR_DIR_INVALID_MANIFEST.
func TestTransferDirManifestNotAManifest(t *testing.T) {
	p := newTransferPair(t)
	ref, err := p.adA.Publish("mesh://go/not-a-manifest", []byte("an ordinary file, not a manifest"))
	if err != nil {
		t.Fatalf("Publish: %v", err)
	}
	_, err = p.b.DirManifestRead(p.a.NodeID(), ref)
	if !errors.Is(err, ErrDirInvalidManifest) || !errors.Is(err, ErrTransfer) {
		t.Fatalf("non-manifest bytes: want ErrDirInvalidManifest (and ErrTransfer), got %v", err)
	}
	if errors.Is(err, ErrTransferNotFound) {
		t.Fatalf("fetched bytes were classified as missing: %v", err)
	}
	// FetchDir reads the same manifest first, so it refuses the same way.
	_, err = p.b.FetchDir(p.a.NodeID(), ref, filepath.Join(t.TempDir(), "out"))
	if !errors.Is(err, ErrDirInvalidManifest) {
		t.Fatalf("FetchDir of a non-manifest: want ErrDirInvalidManifest, got %v", err)
	}
}

// A destination with no final name component (a filesystem root) cannot
// host the sibling temp dir the atomic install needs: DirError::UnsafePath.
func TestTransferFetchDirRefusesRootDestination(t *testing.T) {
	p := newTransferPair(t)
	manifestRef, err := p.adA.StoreDir(writeTransferTree(t))
	if err != nil {
		t.Fatalf("StoreDir: %v", err)
	}
	root := string(filepath.Separator)
	if runtime.GOOS == "windows" {
		root = filepath.VolumeName(t.TempDir()) + `\`
	}
	_, err = p.b.FetchDir(p.a.NodeID(), manifestRef, root)
	if !errors.Is(err, ErrDirPathInvalid) || !errors.Is(err, ErrTransfer) {
		t.Fatalf("FetchDir into %q: want ErrDirPathInvalid (and ErrTransfer), got %v", root, err)
	}
}

func TestTransferFetchBlobDiscovered(t *testing.T) {
	p := newTransferPair(t)
	data := bytes.Repeat([]byte("discovered "), 200)
	ref, err := p.adA.Publish("mesh://go/discovered", data)
	if err != nil {
		t.Fatalf("Publish: %v", err)
	}
	hash, err := BlobRefHash(ref)
	if err != nil {
		t.Fatalf("BlobRefHash: %v", err)
	}
	got, err := p.b.FetchBlobDiscovered(hash[:])
	if err != nil {
		t.Fatalf("FetchBlobDiscovered: %v", err)
	}
	if !bytes.Equal(got, data) {
		t.Fatalf("FetchBlobDiscovered returned %d bytes, want %d", len(got), len(data))
	}

	unknown := bytes.Repeat([]byte{0x42}, 32)
	if _, err := p.b.FetchBlobDiscovered(unknown); !errors.Is(err, ErrTransferAllPeersFailed) || !errors.Is(err, ErrTransfer) {
		t.Fatalf("hash nobody holds: want ErrTransferAllPeersFailed (and ErrTransfer), got %v", err)
	}
	for _, n := range []int{0, 31, 33} {
		if _, err := p.b.FetchBlobDiscovered(make([]byte, n)); !errors.Is(err, ErrTransferInvalidArgument) {
			t.Fatalf("%d-byte hash: want ErrTransferInvalidArgument, got %v", n, err)
		}
	}
}

func TestTransferEmptyManifestRefIsRefusedBeforeCgo(t *testing.T) {
	p := newTransferPair(t)
	if _, err := p.b.FetchDir(p.a.NodeID(), nil, t.TempDir()); !errors.Is(err, ErrTransferInvalidArgument) {
		t.Fatalf("FetchDir(nil ref): want ErrTransferInvalidArgument, got %v", err)
	}
	if _, err := p.b.DirManifestRead(p.a.NodeID(), nil); !errors.Is(err, ErrTransferInvalidArgument) {
		t.Fatalf("DirManifestRead(nil ref): want ErrTransferInvalidArgument, got %v", err)
	}
}

// A path with an embedded NUL is refused before cgo, which would otherwise
// truncate it and act on the prefix (cubic review, PR #1165).
func TestTransferRefusesNULPaths(t *testing.T) {
	p := newTransferPair(t)
	prefix := t.TempDir()
	if _, err := p.b.FetchDir(p.a.NodeID(), []byte{1}, prefix+"\x00/elsewhere"); !errors.Is(err, ErrTransferInvalidArgument) {
		t.Fatalf("FetchDir(dest with NUL): want ErrTransferInvalidArgument, got %v", err)
	}
	r, a := newBlobAdapter(t, "", "go-transfer-nul-root", nil)
	defer r.Free()
	defer a.Close()
	if _, err := a.StoreDir(prefix + "\x00/elsewhere"); !errors.Is(err, ErrTransferInvalidArgument) {
		t.Fatalf("StoreDir(root with NUL): want ErrTransferInvalidArgument, got %v", err)
	}
}

// StoreDir runs on the adapter handle, so it obeys Close like the rest.
func TestTransferStoreDirAfterClose(t *testing.T) {
	r, a := newBlobAdapter(t, "", "go-transfer-closed", nil)
	defer r.Free()
	a.Close()
	if _, err := a.StoreDir(t.TempDir()); !errors.Is(err, ErrBlobClosed) {
		t.Fatalf("StoreDir after Close: want ErrBlobClosed, got %v", err)
	}
}

// A missing source directory is a store-time failure under ErrTransfer.
func TestTransferStoreDirMissingRoot(t *testing.T) {
	r, a := newBlobAdapter(t, "", "go-transfer-missing-root", nil)
	defer r.Free()
	defer a.Close()
	_, err := a.StoreDir(filepath.Join(t.TempDir(), "does-not-exist"))
	if !errors.Is(err, ErrTransfer) {
		t.Fatalf("StoreDir of a missing root: want ErrTransfer, got %v", err)
	}
}
