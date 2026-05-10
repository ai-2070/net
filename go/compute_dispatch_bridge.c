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
