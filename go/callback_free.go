package net

/*
#include <stdlib.h>

// Defined in `compute_dispatch_bridge.c`, in this module's own
// translation unit, so the `free` it performs runs in the same CRT
// that ran the `C.malloc`.
extern void netGoFreeCallbackBuffer(void* p);

typedef void (*NetGoCallbackFreeFn)(void*);

extern int net_org_set_callback_free(NetGoCallbackFreeFn f);
extern int net_rpc_set_callback_free(NetGoCallbackFreeFn f);
extern int net_compute_set_callback_free(NetGoCallbackFreeFn f);
*/
import "C"

import (
	"fmt"
	"sync"
)

// Callback-buffer ownership across the cgo boundary.
//
// # The defect this closes
//
// Every Go callback that returns bytes to Rust allocates them with
// `C.malloc` (or `C.CString` for error strings), which resolves to the
// CRT linked into *this* module — the CGO application. Rust copied the
// bytes out and released them with `libc::free`, which resolves to
// `net.dll`'s CRT.
//
// On Linux both are glibc's heap and the mismatch is invisible, which
// is why it went unnoticed. On Windows every module carries its own CRT
// heap, so it is a wrong-heap free. Application Verifier reported it
// directly:
//
//	StopCode 0x6 — Corrupted heap pointer or using wrong heap
//	block 0x1a (26 bytes) — exactly {"n":2,"servedBy":"go-s4"}
//	alloc heap 0x1eb3cb21000, free heap 0x1eb39621000
//	ucrtbase!free_base <- net!net_org_serve
//
// Real heap corruption, and deterministic process termination whenever
// an affected handler returned a non-empty response. It was the root
// cause behind the `STATUS_HEAP_CORRUPTION` (0xC0000374) aborts in the
// subnet, snapshot and migration tests.
//
// # The invariant
//
// **The allocator that creates a callback-owned buffer releases it.**
//
// Rust no longer calls `libc::free` on anything Go allocated. It calls
// `netGoFreeCallbackBuffer`, registered below, which lives in this
// module and therefore frees on the heap that allocated.
//
// # Ordering
//
// The deallocator is registered before any dispatcher. On Windows the
// Rust side enforces that: dispatcher registration fails if no
// deallocator is present, so a Go wrapper built before this existed
// refuses at startup instead of corrupting a heap at the first
// callback. `registerCallbackFree` is therefore called from every
// dispatcher-registration path rather than relying on package `init`
// ordering.

var callbackFreeOnce sync.Once

// registerCallbackFree installs this module's deallocator with all
// three FFI surfaces. Idempotent; safe to call from any goroutine.
//
// Panics if a surface refuses. That is the right response: the
// alternative is running with a dispatcher whose buffers cannot be
// freed correctly, and on Windows the Rust side will refuse the
// dispatcher anyway — failing here names the cause instead of
// surfacing later as a mystery registration error.
func registerCallbackFree() {
	callbackFreeOnce.Do(func() {
		f := C.NetGoCallbackFreeFn(C.netGoFreeCallbackBuffer)
		for _, s := range []struct {
			name string
			code C.int
		}{
			{"org", C.net_org_set_callback_free(f)},
			{"rpc", C.net_rpc_set_callback_free(f)},
			{"compute", C.net_compute_set_callback_free(f)},
		} {
			if s.code != 0 {
				panic(fmt.Sprintf(
					"net: %s FFI refused the callback deallocator (code %d); "+
						"this build of the Go wrapper and libnet disagree about "+
						"callback-buffer ownership",
					s.name, int(s.code)))
			}
		}
	})
}
