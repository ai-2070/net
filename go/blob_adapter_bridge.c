// Vtable for Go-implemented blob adapters (blob_adapter.go).
//
// Same split as compute_dispatch_bridge.c: cgo's `//export` prototypes
// take non-const pointers and the Go handle as uintptr_t, while
// net_blob_adapter_vtable_t wants `const` pointers and a `void*` ctx.
// These thunks adapt one to the other. Defining them in a Go preamble
// would collide with cgo's generated `_cgo_export.h`.

#include <stdint.h>
#include <stdlib.h>

#include "net.h"
#include "_cgo_export.h"

static int blobStore(void* ctx, const char* uri, const uint8_t* hash, uint64_t size,
                     const uint8_t* data, size_t data_len) {
    return goBlobStore((uintptr_t)ctx, (char*)uri, (uint8_t*)hash, size,
                       (uint8_t*)data, data_len);
}

static int blobFetch(void* ctx, const char* uri, const uint8_t* hash, uint64_t size,
                     uint8_t** out_data, size_t* out_len) {
    return goBlobFetch((uintptr_t)ctx, (char*)uri, (uint8_t*)hash, size, out_data, out_len);
}

static int blobFetchRange(void* ctx, const char* uri, const uint8_t* hash, uint64_t size,
                          uint64_t range_start, uint64_t range_end,
                          uint8_t** out_data, size_t* out_len) {
    return goBlobFetchRange((uintptr_t)ctx, (char*)uri, (uint8_t*)hash, size,
                            range_start, range_end, out_data, out_len);
}

static int blobExists(void* ctx, const char* uri, const uint8_t* hash, uint64_t size,
                      int* out_exists) {
    return goBlobExists((uintptr_t)ctx, (char*)uri, (uint8_t*)hash, size, out_exists);
}

// Fetch buffers are C.malloc'd by writeBlobOut; no Go call needed to free.
static void blobFreeBuffer(void* ctx, uint8_t* data, size_t len) {
    (void)ctx;
    (void)len;
    free(data);
}

static void blobRelease(void* ctx) {
    goBlobRelease((uintptr_t)ctx);
}

static const net_blob_adapter_vtable_t goBlobVtable = {
    blobStore, blobFetch, blobFetchRange, blobExists, blobFreeBuffer,
};

int netGoRegisterBlobAdapter(const char* adapter_id, uintptr_t handle) {
    return net_blob_register_callback_adapter_owned(adapter_id, &goBlobVtable,
                                                    (void*)handle, blobRelease);
}
