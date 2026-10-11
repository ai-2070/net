package net

import (
	"sync"
	"unsafe"
)

// inflightCancel delivers a ctx cancellation to ONE blocking native call
// without touching the owning client's RWMutex.
//
// The unary call paths (MeshRpc.Call / CallService, OrgClient.CallBytes /
// CallExportedBytes) hold the client's read lock across the blocking cgo
// call so Close cannot free the handle mid-flight. The ctx watcher used to
// re-take that same read lock to fire the cancel — but once Close is queued
// for the write lock, Go's RWMutex refuses new readers, so the usual
// `cancel(); client.Close()` shutdown deadlocked: the call waited for the
// cancel, the watcher waited for Close, Close waited for the call (SDK
// audit 2026-10-11).
//
// Instead the call publishes its live handle here for exactly the span it
// is inside native code (enter/leave, both under the client's read lock, so
// the handle cannot be freed while it is published), and the watcher fires
// against that. A cancel that lands before enter is remembered and fired on
// enter; the substrate's CancelRegistry accepts a cancel before the call
// registers its token (CR-13), so the call still returns promptly.
type inflightCancel struct {
	mu     sync.Mutex
	handle unsafe.Pointer
	fired  bool
	token  uint64
	fire   func(handle unsafe.Pointer, token uint64)
}

func newInflightCancel(token uint64, fire func(unsafe.Pointer, uint64)) *inflightCancel {
	return &inflightCancel{token: token, fire: fire}
}

// enter publishes the live handle. Call it inside the client's withHandle
// closure, immediately before the blocking native call. Nil-safe.
func (c *inflightCancel) enter(handle unsafe.Pointer) {
	if c == nil {
		return
	}
	c.mu.Lock()
	c.handle = handle
	if c.fired {
		c.fire(handle, c.token)
	}
	c.mu.Unlock()
}

// leave withdraws the handle. Call it inside the same closure, immediately
// after the native call returns, so the watcher never sees a handle that
// Close may free. Nil-safe.
func (c *inflightCancel) leave() {
	if c == nil {
		return
	}
	c.mu.Lock()
	c.handle = nil
	c.mu.Unlock()
}

// cancel records the cancellation and, if the call is in flight, delivers it.
func (c *inflightCancel) cancel() {
	c.mu.Lock()
	c.fired = true
	if c.handle != nil {
		c.fire(c.handle, c.token)
	}
	c.mu.Unlock()
}

// watchCtx spawns the ctx watcher for `c` and returns its idempotent stop,
// which the caller must defer so the goroutine exits on the success path.
func (c *inflightCancel) watchCtx(done <-chan struct{}) func() {
	stop := make(chan struct{})
	exited := make(chan struct{})
	go func() {
		defer close(exited)
		select {
		case <-done:
			c.cancel()
		case <-stop:
		}
	}()
	var once sync.Once
	return func() {
		once.Do(func() { close(stop) })
		<-exited
	}
}
