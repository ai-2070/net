// Dispatcher-trampoline C wrappers for the Go compute surface.
//
// cgo's `//export` directives on the Go side produce C declarations
// with non-const pointer parameters (`GoUint8*`), which don't match
// the `net_compute_process_fn` / `net_compute_restore_fn` typedefs
// in net.h (both take `const uint8_t*`). Declaring the wrappers in
// the Go-file preamble creates duplicate-prototype conflicts with
// cgo's auto-emitted header; declaring them in a separate C file
// sidesteps that while still giving us externally-linkable symbols
// the `init()` function can take the address of.

#include <stdlib.h>

#include "net.h"
#include "_cgo_export.h"

// Note: cgo emits the `goCompute*` prototypes in `_cgo_export.h`
// with its own `GoUint*` type aliases. The casts below are safe
// because those aliases map to the same machine-level types as
// the `uint*_t` C99 integers we declare in net.h — the only
// difference is the `const` qualifier on the pointer parameters,
// which has no ABI impact.

int bridgeProcess(uint64_t daemon_id, uint64_t origin_hash, uint64_t sequence,
                  const uint8_t* payload, size_t payload_len,
                  net_compute_outputs_t* outputs) {
    return goComputeProcess(daemon_id, origin_hash, sequence,
                            (uint8_t*)payload, payload_len, outputs);
}

int bridgeSnapshot(uint64_t daemon_id, uint8_t** out_ptr, size_t* out_len) {
    return goComputeSnapshot(daemon_id, out_ptr, out_len);
}

int bridgeRestore(uint64_t daemon_id, const uint8_t* state, size_t state_len) {
    return goComputeRestore(daemon_id, (uint8_t*)state, state_len);
}

void bridgeFree(uint64_t daemon_id) {
    goComputeFree(daemon_id);
}

int bridgeFactory(uint64_t runtime_id, const char* kind_ptr, size_t kind_len,
                  uint64_t* out_daemon_id) {
    return goComputeFactory(runtime_id, (char*)kind_ptr, kind_len, out_daemon_id);
}

// ---------------------------------------------------------------------------
// Callback-buffer deallocator
// ---------------------------------------------------------------------------
//
// Every Go callback that returns bytes to Rust allocates them with
// `C.malloc`, which resolves to the CRT linked into *this* module —
// the CGO application. Rust then copied the bytes and released them
// with `libc::free` from `net.dll`.
//
// On Linux both resolve to the same glibc heap and the mismatch is
// invisible. On Windows each module carries its own CRT heap, so that
// is a wrong-heap free. Application Verifier caught it directly:
//
//     StopCode 0x6 — Corrupted heap pointer or using wrong heap
//     block 0x1a (26 bytes), the exact size of a Go handler response
//     ucrtbase!free_base <- net!net_org_serve
//
// It is a real heap corruption, and it terminated the process
// deterministically whenever an affected handler returned a non-empty
// response.
//
// This function is the fix: Rust calls back into it instead of
// `libc::free`, so the `free` executes in the same CRT that ran the
// `malloc`. It lives in this translation unit rather than a Go-file
// preamble for the same reason the trampolines above do — the address
// has to be takeable, which needs a real externally-linkable symbol.
//
// NULL is accepted and ignored, matching `free`.
void netGoFreeCallbackBuffer(void* p) {
    free(p);
}
