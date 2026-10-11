package net

import (
	"sync/atomic"
	"time"
)

// serverHandleGate guards a per-call server-side handle (request stream or
// response sink) that Rust frees the moment the Go dispatcher returns.
//
// A handler may hand the stream or sink to a goroutine that outlives the
// handler body. The old guard was a plain "invalidated" flag checked before
// each cgo call: an op that had already passed the check — or was parked
// inside native `next` — kept a raw pointer to a handle Rust freed as soon as
// the trampoline returned, and wrote through it on the next chunk, END or
// CANCEL (SDK audit 2026-10-11).
//
// Now every op brackets its cgo call with enter/leave, and the trampoline
// calls retire before returning to Rust: it refuses new ops, wakes a parked
// one (via the handle's native retire), and waits until in-flight ops have
// left. Only then does Rust free the handle.
type serverHandleGate struct {
	// bit 0 = retired; bits >= 1 = in-flight op count.
	state atomic.Int64
}

// enter registers an in-flight op; false once retired (the op must not touch
// the handle). On true the caller must pair it with exactly one leave.
func (g *serverHandleGate) enter() bool {
	for {
		s := g.state.Load()
		if s&1 != 0 {
			return false
		}
		if g.state.CompareAndSwap(s, s+2) {
			return true
		}
	}
}

func (g *serverHandleGate) leave() { g.state.Add(-2) }

// retire refuses further ops, calls wake (if any op is in flight) so a
// parked native call returns, and waits for in-flight ops to leave.
func (g *serverHandleGate) retire(wake func()) {
	var s int64
	for {
		s = g.state.Load()
		if g.state.CompareAndSwap(s, s|1) {
			break
		}
	}
	if s>>1 == 0 {
		return
	}
	if wake != nil {
		wake()
	}
	backoff := 10 * time.Microsecond
	for g.state.Load()>>1 != 0 {
		time.Sleep(backoff)
		if backoff < time.Millisecond {
			backoff *= 2
		}
	}
}
