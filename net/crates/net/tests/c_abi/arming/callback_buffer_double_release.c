/*
 * Arming negative for the Linux sanitizer lane (C SDK plan, C5): a buffer a
 * C CALLBACK handed to the library is released by the library through the
 * callback's free_buffer, and then freed again by the program. Not a consumer
 * program; run only by run-c-consumers.py --arming, which requires it to be
 * caught.
 *
 * NET-LANE: sanitize
 * NET-EXPECT: ERROR: AddressSanitizer: attempting double-free
 *
 * The callback adapter's fetch returns a malloc'd buffer and also keeps a
 * pointer to it. net_blob_resolve copies the bytes and hands the buffer back
 * to free_buffer, which frees it (correctly). The program then frees the kept
 * pointer itself: the consumer-owned side of the release contract, broken.
 */

#include <stdlib.h>
#include <string.h>

#include "net.go.h"

#include "consumer_util.h"
#include "loaded_module.h"

static uint8_t* kept;
static uint8_t* held;
static size_t held_len;

static int cb_store(void* c, const char* u, const uint8_t* h, uint64_t s, const uint8_t* d, size_t n) {
    (void)c, (void)u, (void)h, (void)s;
    held = (uint8_t*)malloc(n ? n : 1);
    if (held == NULL) {
        return NET_ERR_BLOB_BACKEND;
    }
    memcpy(held, d, n);
    held_len = n;
    return 0;
}

static int cb_fetch(void* c, const char* u, const uint8_t* h, uint64_t s, uint8_t** out, size_t* out_len) {
    (void)c, (void)u, (void)h, (void)s;
    *out = (uint8_t*)malloc(held_len ? held_len : 1);
    if (*out == NULL) {
        return NET_ERR_BLOB_BACKEND;
    }
    memcpy(*out, held, held_len);
    *out_len = held_len;
    kept = *out; /* kept, to free again below: the defect */
    return 0;
}

static int cb_range(void* c, const char* u, const uint8_t* h, uint64_t s, uint64_t a, uint64_t b, uint8_t** o,
                    size_t* n) {
    (void)c, (void)u, (void)h, (void)s, (void)a, (void)b, (void)o, (void)n;
    return NET_ERR_BLOB_BACKEND;
}

static int cb_exists(void* c, const char* u, const uint8_t* h, uint64_t s, int* e) {
    (void)c, (void)u, (void)h, (void)s;
    *e = held != NULL;
    return 0;
}

static void cb_free(void* c, uint8_t* d, size_t n) {
    (void)c, (void)n;
    free(d);
}

static void cb_release(void* c) {
    (void)c;
}

int main(void) {
    static const clm_fn_t used[] = {CLM_FN(net_blob_register_callback_adapter_owned), CLM_FN(net_blob_publish),
                                    CLM_FN(net_blob_resolve), CLM_FN(net_blob_free_buffer)};
    static const net_blob_adapter_vtable_t vt = {cb_store, cb_fetch, cb_range, cb_exists, cb_free};
    uint8_t *ref = NULL, *out = NULL;
    size_t ref_len = 0, out_len = 0;
    if (clm_check_loaded_module(used, sizeof used / sizeof used[0]) != 0) {
        return 2;
    }
    if (net_blob_register_callback_adapter_owned("arm-cb", &vt, NULL, cb_release) ||
        net_blob_publish("arm-cb", "mesh:arm/cb", (const uint8_t*)"held bytes", 10, &ref, &ref_len) ||
        net_blob_resolve("arm-cb", ref, ref_len, &out, &out_len) || kept == NULL) {
        printf("FAIL setup\n");
        return 1;
    }
    printf("setup ok: resolved %zu bytes; the library already released the callback buffer\n", out_len);
    free(kept); /* the defect: free_buffer already freed it */
    printf("FAIL the second release was not caught\n");
    return 1;
}
