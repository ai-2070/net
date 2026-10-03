package net

// Tests for the MeshBlobAdapter binding (go/blob.go) and the checked
// C-buffer copy it uses (go/cbuf.go). Plan:
// docs/internal/plans/GO_BINDING_CONSOLIDATION_AND_BLOBS_PLAN.md, slice S1.
//
// These pin the binding's CURRENT behavior. Where that behavior is
// surprising (an out-of-range overflow ratio is accepted; explicit zeros
// come back as defaults), the test says so rather than asserting a policy
// the native side does not implement.

import (
	"bytes"
	"encoding/hex"
	"errors"
	"math"
	"os"
	"strconv"
	"strings"
	"sync"
	"testing"
	"unsafe"
)

// BLAKE3("abc"), the official test vector. A small blob's content address
// is the BLAKE3 of its bytes, so Publish("abc") must name this hash.
const blake3ABC = "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"

// newBlobAdapter opens an adapter on its own Redex. persistentDir "" is
// heap-only.
func newBlobAdapter(t *testing.T, persistentDir, id string, opts *MeshBlobAdapterOpts) (*Redex, *MeshBlobAdapter) {
	t.Helper()
	r := NewRedex(persistentDir)
	a, err := NewMeshBlobAdapter(r, id, opts)
	if err != nil {
		r.Free()
		t.Fatalf("NewMeshBlobAdapter: %v", err)
	}
	return r, a
}

// ---------------------------------------------------------------------------
// Checked C-buffer copies
// ---------------------------------------------------------------------------

// The boundary the helper exists for: lengths that a `C.int(n)` conversion
// would wrap are carried through unchanged. Nothing here allocates.
func TestBlobCheckedLenBoundaries(t *testing.T) {
	if strconv.IntSize != 64 {
		t.Skip("boundary values above 2^31 need a 64-bit int")
	}
	for _, n := range []uint64{0, 1, math.MaxInt32, 1 << 31, 1<<32 + 1, math.MaxInt} {
		got, err := checkedLen(n)
		if err != nil {
			t.Fatalf("checkedLen(%d): unexpected error %v", n, err)
		}
		if uint64(got) != n {
			t.Fatalf("checkedLen(%d) = %d, lost bits", n, got)
		}
	}
	// Witness for why: the old conversion turns 2^31 negative. (A
	// variable, so the conversion happens at run time as C.int(n) does.)
	twoTo31 := uint32(1 << 31)
	if wrapped := int32(twoTo31); wrapped >= 0 {
		t.Fatalf("expected a 32-bit C int to wrap 2^31 negative, got %d", wrapped)
	}
	for _, n := range []uint64{uint64(math.MaxInt) + 1, math.MaxUint64} {
		if _, err := checkedLen(n); !errors.Is(err, errCBuf) {
			t.Fatalf("checkedLen(%d): want errCBuf, got %v", n, err)
		}
	}
}

func TestBlobCopyCBuf(t *testing.T) {
	empty, err := copyCBuf(nil, 0)
	if err != nil || empty == nil || len(empty) != 0 {
		t.Fatalf("copyCBuf(nil, 0) = %v, %v; want empty non-nil slice", empty, err)
	}
	if _, err := copyCBuf(nil, 1); !errors.Is(err, errCBuf) {
		t.Fatalf("copyCBuf(nil, 1): want errCBuf, got %v", err)
	}
	if _, err := copyCBuf(nil, math.MaxUint64); !errors.Is(err, errCBuf) {
		t.Fatalf("copyCBuf(nil, MaxUint64): want errCBuf, got %v", err)
	}

	src := []byte("checked copy")
	got, err := copyCBuf(unsafe.Pointer(&src[0]), uint64(len(src)))
	if err != nil {
		t.Fatalf("copyCBuf: %v", err)
	}
	if !bytes.Equal(got, src) {
		t.Fatalf("copyCBuf = %q, want %q", got, src)
	}
	// A copy, not an alias: the caller frees the C buffer right after.
	src[0] = 'X'
	if got[0] == 'X' {
		t.Fatal("copyCBuf aliased the source buffer")
	}
	zero, err := copyCBuf(unsafe.Pointer(&src[0]), 0)
	if err != nil || zero == nil || len(zero) != 0 {
		t.Fatalf("copyCBuf(p, 0) = %v, %v; want empty non-nil slice", zero, err)
	}
}

// Every blob buffer copy goes through copyCBuf. A reintroduced
// `C.GoBytes(…, C.int(n))` would compile and pass every functional test
// below with small payloads, so it is pinned at the source.
func TestBlobSourceHasNoGoBytes(t *testing.T) {
	src, err := os.ReadFile("blob.go")
	if err != nil {
		t.Fatalf("read blob.go: %v", err)
	}
	if strings.Contains(string(src), "C.GoBytes(") {
		t.Fatal("blob.go calls C.GoBytes; use copyCBuf (C.GoBytes truncates lengths through C.int)")
	}
}

// ---------------------------------------------------------------------------
// MeshBlobAdapter
// ---------------------------------------------------------------------------

func TestBlobPublishFetchRoundTrip(t *testing.T) {
	r, a := newBlobAdapter(t, "", "go-blob-roundtrip", nil)
	defer r.Free()
	defer a.Close()

	data := []byte("abc")
	ref, err := a.Publish("mesh://go/roundtrip", data)
	if err != nil {
		t.Fatalf("Publish: %v", err)
	}
	if len(ref) == 0 {
		t.Fatal("Publish returned an empty ref")
	}
	hash, err := BlobRefHash(ref)
	if err != nil {
		t.Fatalf("BlobRefHash: %v", err)
	}
	if got := hex.EncodeToString(hash[:]); got != blake3ABC {
		t.Fatalf("BlobRefHash = %s, want BLAKE3(\"abc\") = %s", got, blake3ABC)
	}
	got, err := a.Fetch(ref)
	if err != nil {
		t.Fatalf("Fetch: %v", err)
	}
	if !bytes.Equal(got, data) {
		t.Fatalf("Fetch = %q, want %q", got, data)
	}
}

// Publish also stores, so the "absent" half of Exists needs an adapter that
// never saw the object: a second, empty adapter on its own Redex.
func TestBlobExistsAndStoreOnIndependentAdapter(t *testing.T) {
	rA, a := newBlobAdapter(t, "", "go-blob-producer", nil)
	defer rA.Free()
	defer a.Close()
	rB, b := newBlobAdapter(t, "", "go-blob-consumer", nil)
	defer rB.Free()
	defer b.Close()

	data := []byte("stored by ref on a second adapter")
	ref, err := a.Publish("mesh://go/exists", data)
	if err != nil {
		t.Fatalf("Publish: %v", err)
	}

	present, err := b.Exists(ref)
	if err != nil {
		t.Fatalf("Exists before Store: %v", err)
	}
	if present {
		t.Fatal("Exists = true on an adapter that never stored the blob")
	}
	if _, err := b.Fetch(ref); err == nil {
		t.Fatal("Fetch succeeded on an adapter that never stored the blob")
	}

	if err := b.Store(ref, data); err != nil {
		t.Fatalf("Store: %v", err)
	}
	present, err = b.Exists(ref)
	if err != nil {
		t.Fatalf("Exists after Store: %v", err)
	}
	if !present {
		t.Fatal("Exists = false after Store")
	}
	got, err := b.Fetch(ref)
	if err != nil {
		t.Fatalf("Fetch after Store: %v", err)
	}
	if !bytes.Equal(got, data) {
		t.Fatalf("Fetch = %q, want %q", got, data)
	}
}

// Store verifies the bytes against the ref's content address. This is also
// what catches Store's two byte-slice arguments being swapped.
func TestBlobStoreRefusesMismatchedBytes(t *testing.T) {
	rA, a := newBlobAdapter(t, "", "go-blob-mismatch-src", nil)
	defer rA.Free()
	defer a.Close()
	rB, b := newBlobAdapter(t, "", "go-blob-mismatch-dst", nil)
	defer rB.Free()
	defer b.Close()

	data := []byte("the real bytes")
	ref, err := a.Publish("mesh://go/mismatch", data)
	if err != nil {
		t.Fatalf("Publish: %v", err)
	}
	if err := b.Store(ref, []byte("other bytes")); !errors.Is(err, ErrBlob) {
		t.Fatalf("Store with mismatched bytes: want ErrBlob, got %v", err)
	}
	if err := b.Store(data, ref); !errors.Is(err, ErrBlob) {
		t.Fatalf("Store with swapped arguments: want ErrBlob, got %v", err)
	}
	if present, _ := b.Exists(ref); present {
		t.Fatal("a refused Store left the blob present")
	}
}

// Persistence across a restart: close the ADAPTER and free the Redex (the
// adapter holds its own Arc<Redex>, so freeing only the Redex is not a
// restart), reopen both on the same directory, and fetch. The in-memory
// control proves the test can tell the difference.
func TestBlobPersistenceAcrossReopen(t *testing.T) {
	data := []byte("survives a restart")

	run := func(t *testing.T, persistent bool) ([]byte, error) {
		dir := t.TempDir()
		opts := &MeshBlobAdapterOpts{Persistent: persistent}

		r1, a1 := newBlobAdapter(t, dir, "go-blob-persist", opts)
		ref, err := a1.Publish("mesh://go/persist", data)
		if err != nil {
			a1.Close()
			r1.Free()
			t.Fatalf("Publish: %v", err)
		}
		if err := a1.Close(); err != nil {
			t.Fatalf("Close: %v", err)
		}
		r1.Free()

		r2, a2 := newBlobAdapter(t, dir, "go-blob-persist", opts)
		defer r2.Free()
		defer a2.Close()
		return a2.Fetch(ref)
	}

	t.Run("persistent", func(t *testing.T) {
		got, err := run(t, true)
		if err != nil {
			t.Fatalf("Fetch after reopen: %v", err)
		}
		if !bytes.Equal(got, data) {
			t.Fatalf("Fetch after reopen = %q, want %q", got, data)
		}
	})
	t.Run("in-memory control", func(t *testing.T) {
		if _, err := run(t, false); err == nil {
			t.Fatal("an in-memory adapter's blob survived a restart; the persistent case proves nothing")
		}
	})
}

// ---------------------------------------------------------------------------
// Overflow configuration
// ---------------------------------------------------------------------------

func TestBlobOverflowConfigRoundTrip(t *testing.T) {
	// Non-zero everywhere: the JSON tags are omitempty, so a zero would be
	// sent as "absent" (see TestBlobOverflowConfigZeroMeansDefault).
	want := OverflowConfig{
		Enabled:          true,
		HighWaterRatio:   0.9,
		LowWaterRatio:    0.6,
		MaxPushesPerTick: 7,
		Scope:            "zone",
		TickIntervalMs:   1234,
	}

	t.Run("at construction", func(t *testing.T) {
		cfg := want
		r, a := newBlobAdapter(t, "", "go-blob-overflow-ctor", &MeshBlobAdapterOpts{Overflow: &cfg})
		defer r.Free()
		defer a.Close()
		assertOverflowConfig(t, a, want)
	})
	t.Run("at runtime", func(t *testing.T) {
		r, a := newBlobAdapter(t, "", "go-blob-overflow-set", nil)
		defer r.Free()
		defer a.Close()
		if on, err := a.OverflowEnabled(); err != nil || on {
			t.Fatalf("OverflowEnabled on a default adapter = %v, %v; want false", on, err)
		}
		cfg := want
		if err := a.SetOverflowConfig(&cfg); err != nil {
			t.Fatalf("SetOverflowConfig: %v", err)
		}
		assertOverflowConfig(t, a, want)
		if err := a.SetOverflowEnabled(false); err != nil {
			t.Fatalf("SetOverflowEnabled(false): %v", err)
		}
		if on, err := a.OverflowEnabled(); err != nil || on {
			t.Fatalf("OverflowEnabled after disabling = %v, %v; want false", on, err)
		}
		if active, err := a.OverflowActive(); err != nil || active {
			t.Fatalf("OverflowActive with no tick run = %v, %v; want false", active, err)
		}
	})
}

func assertOverflowConfig(t *testing.T, a *MeshBlobAdapter, want OverflowConfig) {
	t.Helper()
	got, err := a.OverflowConfig()
	if err != nil {
		t.Fatalf("OverflowConfig: %v", err)
	}
	if *got != want {
		t.Fatalf("OverflowConfig = %+v, want %+v", *got, want)
	}
	on, err := a.OverflowEnabled()
	if err != nil {
		t.Fatalf("OverflowEnabled: %v", err)
	}
	if on != want.Enabled {
		t.Fatalf("OverflowEnabled = %v, want %v", on, want.Enabled)
	}
}

// Current behavior, pinned: an explicit zero is indistinguishable from
// "absent" on the wire (omitempty), so it comes back as the native default
// (DEFAULT_OVERFLOW_* in dataforts/blob/mesh.rs).
func TestBlobOverflowConfigZeroMeansDefault(t *testing.T) {
	r, a := newBlobAdapter(t, "", "go-blob-overflow-zero", nil)
	defer r.Free()
	defer a.Close()
	if err := a.SetOverflowConfig(&OverflowConfig{Enabled: true}); err != nil {
		t.Fatalf("SetOverflowConfig: %v", err)
	}
	assertOverflowConfig(t, a, OverflowConfig{
		Enabled:          true,
		HighWaterRatio:   0.85,
		LowWaterRatio:    0.70,
		MaxPushesPerTick: 16,
		Scope:            "mesh",
		TickIntervalMs:   30_000,
	})
}

// Current behavior, pinned: the native parser assigns any finite ratio as
// given and the core setter does not range-check it. Ratio validation would
// be a behavior change, out of this plan's scope.
func TestBlobOverflowOutOfRangeRatioIsAccepted(t *testing.T) {
	r, a := newBlobAdapter(t, "", "go-blob-overflow-range", nil)
	defer r.Free()
	defer a.Close()
	cfg := OverflowConfig{Enabled: true, HighWaterRatio: 1.5, LowWaterRatio: -0.25}
	if err := a.SetOverflowConfig(&cfg); err != nil {
		t.Fatalf("SetOverflowConfig with out-of-range ratios: %v (the native side accepts these today)", err)
	}
	got, err := a.OverflowConfig()
	if err != nil {
		t.Fatalf("OverflowConfig: %v", err)
	}
	if got.HighWaterRatio != 1.5 || got.LowWaterRatio != -0.25 {
		t.Fatalf("ratios = %v / %v, want 1.5 / -0.25", got.HighWaterRatio, got.LowWaterRatio)
	}
}

// Inputs that really are refused, and where each refusal happens.
func TestBlobOverflowConfigRefusals(t *testing.T) {
	r, a := newBlobAdapter(t, "", "go-blob-overflow-refuse", nil)
	defer r.Free()
	defer a.Close()
	before, err := a.OverflowConfig()
	if err != nil {
		t.Fatalf("OverflowConfig: %v", err)
	}

	// Native refusal: the parser rejects an unknown scope (InvalidJson).
	err = a.SetOverflowConfig(&OverflowConfig{Enabled: true, Scope: "galaxy"})
	if !errors.Is(err, ErrBlobInvalidConfig) || !errors.Is(err, ErrBlob) {
		t.Fatalf("unknown scope: want ErrBlobInvalidConfig (and ErrBlob), got %v", err)
	}
	// Go-side refusal: a NaN cannot be marshalled, so the body never
	// reaches the parser. Malformed JSON is otherwise unreachable from this
	// API — the body is always marshalled from the struct.
	err = a.SetOverflowConfig(&OverflowConfig{Enabled: true, HighWaterRatio: math.NaN()})
	if !errors.Is(err, ErrBlobInvalidConfig) {
		t.Fatalf("NaN ratio: want ErrBlobInvalidConfig, got %v", err)
	}
	if err := a.SetOverflowConfig(nil); !errors.Is(err, ErrBlobInvalidConfig) {
		t.Fatalf("nil config: want ErrBlobInvalidConfig, got %v", err)
	}
	after, err := a.OverflowConfig()
	if err != nil {
		t.Fatalf("OverflowConfig: %v", err)
	}
	if *after != *before {
		t.Fatalf("a refused config changed state: %+v -> %+v", *before, *after)
	}

	// At construction the native side returns only a null handle, so the
	// refusal is the generic ErrBlob.
	rc := NewRedex("")
	defer rc.Free()
	bad := &MeshBlobAdapterOpts{Overflow: &OverflowConfig{Enabled: true, Scope: "galaxy"}}
	if _, err := NewMeshBlobAdapter(rc, "go-blob-overflow-bad-ctor", bad); !errors.Is(err, ErrBlob) {
		t.Fatalf("unknown scope at construction: want ErrBlob, got %v", err)
	}
}

func TestBlobPrometheusText(t *testing.T) {
	r, a := newBlobAdapter(t, "", "go-blob-metrics", nil)
	defer r.Free()
	defer a.Close()
	if _, err := a.Publish("mesh://go/metrics", []byte("count me")); err != nil {
		t.Fatalf("Publish: %v", err)
	}
	body, err := a.PrometheusText()
	if err != nil {
		t.Fatalf("PrometheusText: %v", err)
	}
	// Same family prefix as tests/net_blob_cli.rs
	// (metrics_emits_prometheus_text_with_dataforts_blob_prefix).
	if !strings.Contains(body, "dataforts_blob_") {
		t.Fatalf("PrometheusText has no dataforts_blob_ family:\n%s", body)
	}
}

// ---------------------------------------------------------------------------
// Handle lifetime
// ---------------------------------------------------------------------------

// Close racing in-flight calls: no crash, and every call that starts after
// Close sees ErrBlobClosed. Meaningful under -race (CI runs this file with
// it), but the post-Close assertions hold without.
func TestBlobCloseRacesInFlightFetches(t *testing.T) {
	r, a := newBlobAdapter(t, "", "go-blob-close-race", nil)
	defer r.Free()
	data := []byte("raced")
	ref, err := a.Publish("mesh://go/race", data)
	if err != nil {
		t.Fatalf("Publish: %v", err)
	}

	const workers = 32
	start := make(chan struct{})
	var wg sync.WaitGroup
	errs := make(chan error, workers)
	for i := 0; i < workers; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			<-start
			for j := 0; j < 50; j++ {
				got, err := a.Fetch(ref)
				if errors.Is(err, ErrBlobClosed) {
					return
				}
				if err != nil {
					errs <- err
					return
				}
				if !bytes.Equal(got, data) {
					errs <- errors.New("fetch returned the wrong bytes during the race")
					return
				}
			}
		}()
	}
	close(start)
	if err := a.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	wg.Wait()
	close(errs)
	for err := range errs {
		t.Fatalf("in-flight fetch: %v", err)
	}

	if err := a.Close(); err != nil {
		t.Fatalf("second Close (idempotent): %v", err)
	}
	if _, err := a.Fetch(ref); !errors.Is(err, ErrBlobClosed) {
		t.Fatalf("Fetch after Close: want ErrBlobClosed, got %v", err)
	}
	if _, err := a.Publish("mesh://go/race", data); !errors.Is(err, ErrBlobClosed) {
		t.Fatalf("Publish after Close: want ErrBlobClosed, got %v", err)
	}
	if err := a.Store(ref, data); !errors.Is(err, ErrBlobClosed) {
		t.Fatalf("Store after Close: want ErrBlobClosed, got %v", err)
	}
	if _, err := a.Exists(ref); !errors.Is(err, ErrBlobClosed) {
		t.Fatalf("Exists after Close: want ErrBlobClosed, got %v", err)
	}
	if _, err := a.PrometheusText(); !errors.Is(err, ErrBlobClosed) {
		t.Fatalf("PrometheusText after Close: want ErrBlobClosed, got %v", err)
	}
	if _, err := a.OverflowConfig(); !errors.Is(err, ErrBlobClosed) {
		t.Fatalf("OverflowConfig after Close: want ErrBlobClosed, got %v", err)
	}
}

// ---------------------------------------------------------------------------
// Cross-node transfer
// ---------------------------------------------------------------------------

func TestBlobTwoNodeFetch(t *testing.T) {
	a, b, cleanup := meshHandshakePair(t)
	defer cleanup()

	rA, adA := newBlobAdapter(t, "", "go-blob-holder", nil)
	defer rA.Free()
	defer adA.Close()
	rB, adB := newBlobAdapter(t, "", "go-blob-fetcher", nil)
	defer rB.Free()
	defer adB.Close()

	data := bytes.Repeat([]byte("cross-node "), 512)
	ref, err := adA.Publish("mesh://go/two-node", data)
	if err != nil {
		t.Fatalf("Publish: %v", err)
	}
	hash, err := BlobRefHash(ref)
	if err != nil {
		t.Fatalf("BlobRefHash: %v", err)
	}

	// A fetch needs the engine on the fetching node too.
	if _, err := b.FetchBlob(a.NodeID(), hash[:]); !errors.Is(err, ErrTransferEngineNotInstalled) {
		t.Fatalf("FetchBlob before ServeBlobTransfer: want ErrTransferEngineNotInstalled, got %v", err)
	}

	if err := a.ServeBlobTransfer(adA); err != nil {
		t.Fatalf("ServeBlobTransfer(a): %v", err)
	}
	if err := b.ServeBlobTransfer(adB); err != nil {
		t.Fatalf("ServeBlobTransfer(b): %v", err)
	}

	got, err := b.FetchBlob(a.NodeID(), hash[:])
	if err != nil {
		t.Fatalf("FetchBlob: %v", err)
	}
	if !bytes.Equal(got, data) {
		t.Fatalf("FetchBlob returned %d bytes, want the %d published", len(got), len(data))
	}

	// The hash length is exact, checked before the cgo call.
	for _, n := range []int{31, 33} {
		if _, err := b.FetchBlob(a.NodeID(), make([]byte, n)); !errors.Is(err, ErrTransferInvalidArgument) {
			t.Fatalf("FetchBlob with a %d-byte hash: want ErrTransferInvalidArgument, got %v", n, err)
		}
	}
}
