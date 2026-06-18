package net

import (
	"sync"
	"sync/atomic"
	"testing"
)

// Regression tests for streamHandleGuard — the quiesce primitive that
// lets streaming Close/Finish/Split/ctx-watcher request teardown
// without holding a lock across a blocking cgo call (and without ever
// freeing while an op is in flight). Pure-Go; no cgo.

func TestStreamHandleGuard_FreesOnceWhenIdle(t *testing.T) {
	var freed atomic.Int32
	g := newStreamHandleGuard(func() { freed.Add(1) })

	g.requestFree()
	if got := freed.Load(); got != 1 {
		t.Fatalf("idle requestFree: free ran %d times, want 1", got)
	}
	// Idempotent: a second requestFree (e.g. the ctx watcher after an
	// explicit Close) must not double-free.
	g.requestFree()
	if got := freed.Load(); got != 1 {
		t.Fatalf("double requestFree: free ran %d times, want 1", got)
	}
	// New ops must be refused once teardown has been requested.
	if g.enter() {
		t.Fatal("enter() returned true after teardown was requested")
	}
}

func TestStreamHandleGuard_FreesAfterInflightLeaves(t *testing.T) {
	var freed atomic.Int32
	g := newStreamHandleGuard(func() { freed.Add(1) })

	if !g.enter() {
		t.Fatal("first enter() failed on a fresh guard")
	}
	// Teardown requested while an op is in flight: must NOT free yet.
	g.requestFree()
	if got := freed.Load(); got != 0 {
		t.Fatalf("requestFree with an op in flight freed %d times, want 0", got)
	}
	// And no new op may start.
	if g.enter() {
		t.Fatal("enter() succeeded after teardown was requested")
	}
	// The last op leaving runs the free exactly once.
	g.leave()
	if got := freed.Load(); got != 1 {
		t.Fatalf("after the last op left: free ran %d times, want 1", got)
	}
}

// The core safety property: free runs exactly once, and never while any
// op is still in flight, under heavy concurrency between ops and several
// teardown requesters.
func TestStreamHandleGuard_ConcurrentSingleFreeAfterDrain(t *testing.T) {
	var freed atomic.Int32
	var inUse atomic.Int32
	var freedWhileInUse atomic.Int32
	g := newStreamHandleGuard(func() {
		if inUse.Load() != 0 {
			freedWhileInUse.Add(1)
		}
		freed.Add(1)
	})

	const ops = 256
	var wg sync.WaitGroup
	for i := 0; i < ops; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			if g.enter() {
				inUse.Add(1)
				inUse.Add(-1)
				g.leave()
			}
		}()
	}
	for i := 0; i < 8; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			g.requestFree()
		}()
	}
	wg.Wait()

	// Ensure teardown has been requested at least once and settled.
	g.requestFree()
	if got := freed.Load(); got != 1 {
		t.Fatalf("free ran %d times under concurrency, want exactly 1", got)
	}
	if got := freedWhileInUse.Load(); got != 0 {
		t.Fatalf("free ran while %d ops were in flight, want 0 (use-after-free risk)", got)
	}
}
