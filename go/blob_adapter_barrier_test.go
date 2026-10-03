//go:build test_helpers

package net

// Native-barrier witnesses for Go blob adapter ownership (gap G-B, S5b).
// Needs a libnet built with `--features net-ffi/test-helpers` and
// `go test -tags test_helpers`.

import (
	"bytes"
	"fmt"
	"testing"
	"time"
)

// Unregister while a fetch is held inside the substrate, at both stages:
// before the Go callback runs (1) and after it returned its buffer but
// before free_buffer (2). The release trampoline must not run until the
// held call finishes, and must then run exactly once.
func TestBlobAdapterGoReleaseWaitsForAHeldNativeCall(t *testing.T) {
	for _, stage := range []int{1, 2} {
		t.Run(fmt.Sprintf("stage%d", stage), func(t *testing.T) {
			w := watchReleases(t)
			a := newMemAdapter()
			id := "go-test/" + t.Name()
			h, err := registerBlobAdapter(id, a)
			if err != nil {
				t.Fatalf("registerBlobAdapter: %v", err)
			}
			t.Cleanup(func() { UnregisterBlobAdapter(id) })
			data := []byte(fmt.Sprintf("held at stage %d", stage))
			ref, err := BlobPublish(id, "mem://go/barrier", data)
			if err != nil {
				t.Fatalf("BlobPublish: %v", err)
			}

			blobBarrierArm(stage, h)
			type result struct {
				b   []byte
				err error
			}
			done := make(chan result, 1)
			go func() {
				b, err := BlobResolve(id, ref)
				done <- result{b, err}
			}()
			if !blobBarrierWaitHeld(stage, 5000) {
				blobBarrierRelease(stage)
				t.Fatalf("no fetch reached stage %d", stage)
			}
			wantFetches := int64(stage - 1) // stage 2 is past the Go callback
			if got := a.fetches.Load(); got != wantFetches {
				t.Fatalf("Fetch ran %d times while held at stage %d, want %d", got, stage, wantFetches)
			}

			if ok, err := UnregisterBlobAdapter(id); err != nil || !ok {
				t.Fatalf("UnregisterBlobAdapter = %v, %v", ok, err)
			}
			time.Sleep(20 * time.Millisecond)
			if got := w.count(a); got != 0 {
				t.Fatalf("released %d times while a call was held at stage %d", got, stage)
			}

			blobBarrierRelease(stage)
			r := <-done
			if r.err != nil || !bytes.Equal(r.b, data) {
				t.Fatalf("held BlobResolve = %q, %v; want %q", r.b, r.err, data)
			}
			w.waitReleased(t, a)
		})
	}
}

// A panic in the trampoline's own handle lookup, before any user code, is
// contained like one in the adapter.
func TestBlobAdapterGoLookupPanicIsContained(t *testing.T) {
	before := CallbackPanicCount()
	if code := blobFetchThroughDeletedHandle(); code != -115 {
		t.Fatalf("fetch through a deleted handle = %d, want -115 (backend)", code)
	}
	if got := CallbackPanicCount(); got != before+1 {
		t.Fatalf("CallbackPanicCount = %d, want %d", got, before+1)
	}
}
