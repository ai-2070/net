package net

// Panic containment for exported cgo callbacks.
//
// # Why this file exists
//
// A Go panic that reaches an `//export`ed function boundary is not
// recoverable by the caller. Rust's `catch_unwind` cannot translate it:
// the unwind originates in the Go runtime, crosses C, and the process
// dies. So every trampoline Rust calls into has to convert a panic to
// an ABI status *before* returning.
//
// This is not "user code can crash itself". The daemon and observer
// callbacks run user code against peer-shaped input — an event payload
// a remote node chose. An empty payload indexed at [0], a failed type
// assertion, a write to a nil map: ordinary Go bugs, reachable by
// anyone who can send this node an event, and each one took down the
// whole process along with every unrelated daemon, RPC service and
// in-process state it was hosting.
//
// The package already held this line for unary RPC, streaming RPC, Org
// handlers and the compute factory (see `safeCallClientStreamingHandler`
// and friends). The daemon and observer trampolines were the gap.
//
// # The shape
//
// Each trampoline names its return value and installs a deferred
// recover as its *first* statement, so the guard covers the whole body
// — handle lookup and payload conversion included, not just the call
// into user code. Output pointers are initialized before anything that
// can panic, so a recovered trampoline still returns a valid,
// fully-written ABI result rather than leaving Rust reading
// uninitialized memory.

import (
	"fmt"
	"os"
	"runtime/debug"
	"sync/atomic"
)

// callbackPanics counts panics contained at a cgo callback boundary,
// across every trampoline. Exposed through CallbackPanicCount for
// tests and for applications that want to alarm on it.
var callbackPanics atomic.Uint64

// CallbackPanicCount returns the number of panics this process has
// contained at an exported cgo callback boundary.
//
// A nonzero value means user callback code panicked and the failure
// was converted to an error status instead of terminating the process.
// The work that panicked did not happen: a Process output was dropped,
// a Snapshot came back empty, an observation was lost. Treat a rising
// count as a bug in the callback, not as a transient.
func CallbackPanicCount() uint64 {
	return callbackPanics.Load()
}

// recoverCallback is the body of every trampoline's deferred recover.
//
// Reports to stderr rather than through any logging hook: this runs on
// a thread owned by the Rust runtime, mid-unwind, and must not itself
// panic or block. `debug.Stack()` is captured inside the deferred call
// so the trace still points at the panicking frame.
//
// Returns true when a panic was contained, so callers that need to set
// more than one output can branch on it:
//
//	defer func() {
//	    if recoverCallback("meshos.Process", recover()) {
//	        code = 1
//	    }
//	}()
func recoverCallback(callback string, r any) bool {
	if r == nil {
		return false
	}
	callbackPanics.Add(1)
	fmt.Fprintf(
		os.Stderr,
		"net: panic in %s callback contained at the cgo boundary: %v\n"+
			"net: the callback's result was discarded; the process survives, "+
			"but this is a bug in the callback.\n%s\n",
		callback, r, debug.Stack(),
	)
	return true
}
