package net

// Tests for Go-implemented blob adapters (go/blob_adapter.go).
// Plan: docs/internal/plans/GO_BINDING_CONSOLIDATION_AND_BLOBS_PLAN.md,
// gap G-B (S5b).
//
// The ownership witnesses observe the release trampoline through the
// blobAdapterReleased hook, which fires after the adapter's cgo.Handle is
// deleted. The registry and the hook are process-global, so these tests do
// not run in parallel and each uses its own adapter id.

import (
	"bytes"
	"errors"
	"sync"
	"sync/atomic"
	"testing"
	"time"
	"unsafe"
)

// memAdapter is an in-memory BlobAdapter. A non-nil hold makes every Fetch
// signal entered and then wait for hold to close.
type memAdapter struct {
	mu      sync.Mutex
	blobs   map[[32]byte][]byte
	keys    []BlobKey
	fetches atomic.Int64
	hold    chan struct{}
	entered chan struct{}
	panicOn bool
	failErr error
}

func newMemAdapter() *memAdapter {
	return &memAdapter{blobs: map[[32]byte][]byte{}}
}

// set changes the adapter's behaviour under mu.
func (m *memAdapter) set(f func(m *memAdapter)) {
	m.mu.Lock()
	defer m.mu.Unlock()
	f(m)
}

// holdFetches makes every later Fetch signal entered and wait for release.
func (m *memAdapter) holdFetches() (entered <-chan struct{}, release func()) {
	hold, ent := make(chan struct{}), make(chan struct{}, 1)
	m.set(func(m *memAdapter) { m.hold, m.entered = hold, ent })
	return ent, func() { close(hold) }
}

func (m *memAdapter) Store(key BlobKey, data []byte) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.keys = append(m.keys, key)
	m.blobs[key.Hash] = append([]byte(nil), data...)
	return nil
}

func (m *memAdapter) Fetch(key BlobKey) ([]byte, error) {
	m.fetches.Add(1)
	// The knobs are set by the test goroutine and read on a substrate
	// thread; cgo gives the race detector no edge between the two, so
	// they are read under mu.
	m.mu.Lock()
	panicOn, failErr, hold, entered := m.panicOn, m.failErr, m.hold, m.entered
	m.mu.Unlock()
	if panicOn {
		var nilMap map[string]int
		nilMap["boom"]++
	}
	if failErr != nil {
		return nil, failErr
	}
	if hold != nil {
		entered <- struct{}{}
		<-hold
	}
	m.mu.Lock()
	defer m.mu.Unlock()
	b, ok := m.blobs[key.Hash]
	if !ok {
		return nil, ErrBlobNotFound
	}
	return b, nil
}

func (m *memAdapter) FetchRange(key BlobKey, start, end uint64) ([]byte, error) {
	b, err := m.Fetch(key)
	if err != nil {
		return nil, err
	}
	return b[start:end], nil
}

func (m *memAdapter) storeCount() int {
	m.mu.Lock()
	defer m.mu.Unlock()
	return len(m.keys)
}

func (m *memAdapter) Exists(key BlobKey) (bool, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	_, ok := m.blobs[key.Hash]
	return ok, nil
}

// releaseWatch counts release-trampoline calls per adapter.
type releaseWatch struct {
	mu     sync.Mutex
	counts map[BlobAdapter]int
}

func watchReleases(t *testing.T) *releaseWatch {
	t.Helper()
	w := &releaseWatch{counts: map[BlobAdapter]int{}}
	hook := func(a BlobAdapter) {
		w.mu.Lock()
		w.counts[a]++
		w.mu.Unlock()
	}
	if !blobAdapterReleased.CompareAndSwap(nil, &hook) {
		t.Fatal("another release watch is installed")
	}
	t.Cleanup(func() { blobAdapterReleased.Store(nil) })
	return w
}

func (w *releaseWatch) count(a BlobAdapter) int {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.counts[a]
}

// waitReleased waits for a's first release, then fails if more follow.
func (w *releaseWatch) waitReleased(t *testing.T, a BlobAdapter) {
	t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for w.count(a) == 0 && time.Now().Before(deadline) {
		time.Sleep(5 * time.Millisecond)
	}
	time.Sleep(20 * time.Millisecond)
	if got := w.count(a); got != 1 {
		t.Fatalf("adapter released %d times, want exactly 1", got)
	}
}

func registerGoAdapter(t *testing.T, a BlobAdapter) string {
	t.Helper()
	id := "go-test/" + t.Name()
	if err := RegisterBlobAdapter(id, a); err != nil {
		t.Fatalf("RegisterBlobAdapter(%q): %v", id, err)
	}
	t.Cleanup(func() { UnregisterBlobAdapter(id) })
	return id
}

func TestBlobAdapterGoRoundTrip(t *testing.T) {
	a := newMemAdapter()
	id := registerGoAdapter(t, a)

	data := []byte("stored and served by Go")
	ref, err := BlobPublish(id, "mem://go/round-trip", data)
	if err != nil {
		t.Fatalf("BlobPublish: %v", err)
	}
	hash, err := BlobRefHash(ref)
	if err != nil {
		t.Fatalf("BlobRefHash: %v", err)
	}
	a.mu.Lock()
	keys := append([]BlobKey(nil), a.keys...)
	a.mu.Unlock()
	want := BlobKey{URI: "mem://go/round-trip", Hash: hash, Size: uint64(len(data))}
	if len(keys) != 1 || keys[0] != want {
		t.Fatalf("Store saw %+v, want one call with %+v", keys, want)
	}

	got, err := BlobResolve(id, ref)
	if err != nil {
		t.Fatalf("BlobResolve: %v", err)
	}
	if !bytes.Equal(got, data) {
		t.Fatalf("BlobResolve = %q, want %q", got, data)
	}
	if a.fetches.Load() != 1 {
		t.Fatalf("Fetch called %d times, want 1", a.fetches.Load())
	}
}

func TestBlobAdapterGoEmptyBlob(t *testing.T) {
	id := registerGoAdapter(t, newMemAdapter())
	ref, err := BlobPublish(id, "mem://go/empty", nil)
	if err != nil {
		t.Fatalf("BlobPublish: %v", err)
	}
	got, err := BlobResolve(id, ref)
	if err != nil || len(got) != 0 {
		t.Fatalf("BlobResolve(empty) = %q, %v; want empty, nil", got, err)
	}
}

func TestBlobAdapterGoReleasedOnceAfterUnregister(t *testing.T) {
	w := watchReleases(t)
	a := newMemAdapter()
	id := registerGoAdapter(t, a)
	if _, err := BlobPublish(id, "mem://go/release", []byte("x")); err != nil {
		t.Fatalf("BlobPublish: %v", err)
	}
	if got := w.count(a); got != 0 {
		t.Fatalf("released %d times while registered", got)
	}
	if ok, err := UnregisterBlobAdapter(id); err != nil || !ok {
		t.Fatalf("UnregisterBlobAdapter = %v, %v", ok, err)
	}
	w.waitReleased(t, a)
}

// A refused registration never reaches the release trampoline: the handle
// stays Go's and is deleted by RegisterBlobAdapter itself.
func TestBlobAdapterGoDuplicateIDKeepsHandleInGo(t *testing.T) {
	w := watchReleases(t)
	first, second := newMemAdapter(), newMemAdapter()
	id := registerGoAdapter(t, first)
	if err := RegisterBlobAdapter(id, second); !errors.Is(err, ErrBlobDuplicateID) {
		t.Fatalf("second RegisterBlobAdapter = %v, want ErrBlobDuplicateID", err)
	}
	ref, err := BlobPublish(id, "mem://go/dup", []byte("first owns the id"))
	if err != nil {
		t.Fatalf("BlobPublish: %v", err)
	}
	if _, err := BlobResolve(id, ref); err != nil {
		t.Fatalf("BlobResolve: %v", err)
	}
	if second.fetches.Load() != 0 || second.storeCount() != 0 {
		t.Fatal("the refused adapter was called")
	}
	UnregisterBlobAdapter(id)
	w.waitReleased(t, first)
	if got := w.count(second); got != 0 {
		t.Fatalf("refused adapter released %d times, want 0", got)
	}
}

// Unregister while a Fetch is running inside Go: the call finishes against
// a live adapter, and release waits for it.
func TestBlobAdapterGoUnregisterWaitsForAnInFlightFetch(t *testing.T) {
	w := watchReleases(t)
	a := newMemAdapter()
	id := registerGoAdapter(t, a)
	data := []byte("held inside Fetch")
	ref, err := BlobPublish(id, "mem://go/held", data)
	if err != nil {
		t.Fatalf("BlobPublish: %v", err)
	}

	entered, release := a.holdFetches()
	type result struct {
		b   []byte
		err error
	}
	done := make(chan result, 1)
	go func() {
		b, err := BlobResolve(id, ref)
		done <- result{b, err}
	}()
	select {
	case <-entered:
	case <-time.After(5 * time.Second):
		t.Fatal("Fetch was never entered")
	}

	if ok, err := UnregisterBlobAdapter(id); err != nil || !ok {
		t.Fatalf("UnregisterBlobAdapter = %v, %v", ok, err)
	}
	time.Sleep(20 * time.Millisecond)
	if got := w.count(a); got != 0 {
		t.Fatalf("released %d times while a Fetch still held the handle", got)
	}

	release()
	r := <-done
	if r.err != nil || !bytes.Equal(r.b, data) {
		t.Fatalf("held BlobResolve = %q, %v; want %q", r.b, r.err, data)
	}
	w.waitReleased(t, a)
}

// Re-registering the id while the old adapter still has a call in flight:
// each call reaches the adapter it started on, and only the old one is
// released.
func TestBlobAdapterGoReRegisterWhileOldCallHeld(t *testing.T) {
	w := watchReleases(t)
	old := newMemAdapter()
	id := registerGoAdapter(t, old)
	data := []byte("same bytes, two adapters")
	ref, err := BlobPublish(id, "mem://go/rereg", data)
	if err != nil {
		t.Fatalf("BlobPublish: %v", err)
	}

	entered, release := old.holdFetches()
	done := make(chan error, 1)
	go func() {
		b, err := BlobResolve(id, ref)
		if err == nil && !bytes.Equal(b, data) {
			err = errors.New("old call returned the wrong bytes")
		}
		done <- err
	}()
	<-entered
	UnregisterBlobAdapter(id)

	fresh := newMemAdapter()
	if err := RegisterBlobAdapter(id, fresh); err != nil {
		t.Fatalf("re-register: %v", err)
	}
	if _, err := BlobPublish(id, "mem://go/rereg", data); err != nil {
		t.Fatalf("BlobPublish to the new adapter: %v", err)
	}
	if _, err := BlobResolve(id, ref); err != nil {
		t.Fatalf("BlobResolve via the new adapter: %v", err)
	}
	if fresh.fetches.Load() != 1 || old.fetches.Load() != 1 {
		t.Fatalf("fetches: old %d, new %d; want 1 and 1", old.fetches.Load(), fresh.fetches.Load())
	}

	release()
	if err := <-done; err != nil {
		t.Fatalf("old call: %v", err)
	}
	w.waitReleased(t, old)
	if got := w.count(fresh); got != 0 {
		t.Fatalf("the still-registered adapter was released %d times", got)
	}
}

func TestBlobAdapterGoPanicIsContained(t *testing.T) {
	a := newMemAdapter()
	id := registerGoAdapter(t, a)
	ref, err := BlobPublish(id, "mem://go/panic", []byte("p"))
	if err != nil {
		t.Fatalf("BlobPublish: %v", err)
	}
	a.set(func(m *memAdapter) { m.panicOn = true })
	before := CallbackPanicCount()
	if _, err := BlobResolve(id, ref); !errors.Is(err, ErrBlobBackend) {
		t.Fatalf("BlobResolve through a panicking Fetch = %v, want ErrBlobBackend", err)
	}
	if got := CallbackPanicCount(); got != before+1 {
		t.Fatalf("CallbackPanicCount = %d, want %d", got, before+1)
	}
}

func TestBlobAdapterGoErrorsMap(t *testing.T) {
	a := newMemAdapter()
	id := registerGoAdapter(t, a)
	ref, err := BlobPublish(id, "mem://go/errors", []byte("e"))
	if err != nil {
		t.Fatalf("BlobPublish: %v", err)
	}
	a.set(func(m *memAdapter) { m.failErr = ErrBlobNotFound })
	if _, err := BlobResolve(id, ref); !errors.Is(err, ErrBlobNotFound) {
		t.Fatalf("not-found Fetch = %v, want ErrBlobNotFound", err)
	}
	a.set(func(m *memAdapter) { m.failErr = errors.New("disk on fire") })
	if _, err := BlobResolve(id, ref); !errors.Is(err, ErrBlobBackend) {
		t.Fatalf("failing Fetch = %v, want ErrBlobBackend", err)
	}
}

func TestBlobAdapterGoRefusals(t *testing.T) {
	if err := RegisterBlobAdapter("go-test/nil", nil); !errors.Is(err, ErrBlobInvalidArgument) {
		t.Fatalf("nil adapter = %v, want ErrBlobInvalidArgument", err)
	}
	// A typed nil passes `a == nil`; it must be refused too, not registered
	// to panic on its first callback (cubic review, PR #1165).
	var typedNil *memAdapter
	if err := RegisterBlobAdapter("go-test/typed-nil", typedNil); !errors.Is(err, ErrBlobInvalidArgument) {
		t.Fatalf("typed-nil adapter = %v, want ErrBlobInvalidArgument", err)
	}
	if ok, _ := BlobAdapterRegistered("go-test/typed-nil"); ok {
		t.Fatal("the typed-nil adapter was registered")
	}
	if err := RegisterBlobAdapter("go-test/\x00nul", newMemAdapter()); !errors.Is(err, ErrBlobInvalidArgument) {
		t.Fatalf("NUL id = %v, want ErrBlobInvalidArgument", err)
	}
}

// A failed result allocation is a backend error from the callback, not a
// NULL handed to memcpy (cubic review, PR #1165).
func TestBlobAdapterGoAllocationFailureIsAnError(t *testing.T) {
	id := registerGoAdapter(t, newMemAdapter())
	ref, err := BlobPublish(id, "mem://go/oom", []byte("needs a buffer"))
	if err != nil {
		t.Fatalf("BlobPublish: %v", err)
	}
	failing := func(int) unsafe.Pointer { return nil }
	blobOutAllocHook.Store(&failing)
	defer blobOutAllocHook.Store(nil)
	if _, err := BlobResolve(id, ref); !errors.Is(err, ErrBlobBackend) {
		t.Fatalf("BlobResolve with a failing allocator = %v, want ErrBlobBackend", err)
	}
	blobOutAllocHook.Store(nil)
	if _, err := BlobResolve(id, ref); err != nil {
		t.Fatalf("BlobResolve after the allocator recovers: %v", err)
	}
}
