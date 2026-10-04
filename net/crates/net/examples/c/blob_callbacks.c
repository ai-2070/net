/*
 * blob_callbacks.c — a blob adapter implemented in C, driven through the
 * process-wide registry (C SDK plan, C4).
 *
 * A net_blob_adapter_vtable_t backed by an in-memory map is registered with
 * net_blob_register_callback_adapter_owned and driven through
 * net_blob_publish / net_blob_resolve. Every buffer it hands out comes from
 * this program's malloc and comes back through its own free_buffer. It
 * checks the owned-registration contract (net.go.h):
 *
 *   - release_fn runs exactly once, after unregister, and never before;
 *   - a refused registration (NULL vtable, a NULL entry, a duplicate id)
 *     never calls release_fn and leaves ctx with the caller;
 *   - every buffer fetch handed out is returned through free_buffer.
 *
 * And it drives a callback's error codes through to the caller. They are
 * not all passed through unchanged (src/ffi/blob.rs code_to_err, then
 * err_to_code); the pairs checked are:
 *
 *   callback returns              caller sees
 *   NET_ERR_BLOB_NOT_FOUND        NET_ERR_BLOB_NOT_FOUND
 *   NET_ERR_BLOB_HASH_MISMATCH    NET_ERR_BLOB_BACKEND
 *   an unlisted code (-999)       NET_ERR_BLOB_BACKEND
 *
 * Built and run by .github/scripts/run-c-consumers.py.
 */

#include <stdlib.h>
#include <string.h>

#include "net.go.h"

#include "consumer_util.h"
#include "loaded_module.h"

#define SLOTS 16

typedef struct {
    cu_mutex* mu;
    char* uri[SLOTS];
    unsigned char* data[SLOTS];
    size_t len[SLOTS];
    int stores, fetches, handed_out, freed, released;
} store_ctx;

static int find(store_ctx* s, const char* uri) {
    int i;
    for (i = 0; i < SLOTS; i++) {
        if (s->uri[i] && strcmp(s->uri[i], uri) == 0) {
            return i;
        }
    }
    return -1;
}

static int cb_store(void* ctx, const char* uri, const uint8_t* hash, uint64_t size, const uint8_t* data,
                    size_t data_len) {
    store_ctx* s = (store_ctx*)ctx;
    int i, slot = -1, rc = 0;
    (void)hash;
    (void)size;
    cu_mutex_lock(s->mu);
    s->stores++;
    for (i = 0; i < SLOTS && slot < 0; i++) {
        if (s->uri[i] == NULL) {
            slot = i;
        }
    }
    if (slot < 0) {
        rc = NET_ERR_BLOB_BACKEND;
    } else {
        s->uri[slot] = (char*)malloc(strlen(uri) + 1);
        s->data[slot] = (unsigned char*)malloc(data_len ? data_len : 1);
        if (!s->uri[slot] || !s->data[slot]) {
            rc = NET_ERR_BLOB_BACKEND;
        } else {
            memcpy(s->uri[slot], uri, strlen(uri) + 1);
            memcpy(s->data[slot], data, data_len);
            s->len[slot] = data_len;
        }
    }
    cu_mutex_unlock(s->mu);
    return rc;
}

/* The URI picks the outcome, so one adapter drives every error pair. */
static int cb_fetch(void* ctx, const char* uri, const uint8_t* hash, uint64_t size, uint8_t** out_data,
                    size_t* out_len) {
    store_ctx* s = (store_ctx*)ctx;
    int i, rc = 0;
    (void)hash;
    (void)size;
    if (strstr(uri, "/missing")) {
        return NET_ERR_BLOB_NOT_FOUND;
    }
    if (strstr(uri, "/corrupt")) {
        return NET_ERR_BLOB_HASH_MISMATCH;
    }
    if (strstr(uri, "/weird")) {
        return -999;
    }
    cu_mutex_lock(s->mu);
    s->fetches++;
    i = find(s, uri);
    if (i < 0) {
        rc = NET_ERR_BLOB_NOT_FOUND;
    } else {
        *out_data = (uint8_t*)malloc(s->len[i] ? s->len[i] : 1);
        if (*out_data == NULL) {
            rc = NET_ERR_BLOB_BACKEND;
        } else {
            memcpy(*out_data, s->data[i], s->len[i]);
            *out_len = s->len[i];
            s->handed_out++;
        }
    }
    cu_mutex_unlock(s->mu);
    return rc;
}

static int cb_fetch_range(void* ctx, const char* uri, const uint8_t* hash, uint64_t size, uint64_t start,
                          uint64_t end, uint8_t** out_data, size_t* out_len) {
    (void)ctx, (void)uri, (void)hash, (void)size, (void)start, (void)end, (void)out_data, (void)out_len;
    return NET_ERR_BLOB_BACKEND;
}

static int cb_exists(void* ctx, const char* uri, const uint8_t* hash, uint64_t size, int* out_exists) {
    store_ctx* s = (store_ctx*)ctx;
    (void)hash;
    (void)size;
    cu_mutex_lock(s->mu);
    *out_exists = find(s, uri) >= 0;
    cu_mutex_unlock(s->mu);
    return 0;
}

static void cb_free_buffer(void* ctx, uint8_t* data, size_t len) {
    store_ctx* s = (store_ctx*)ctx;
    (void)len;
    free(data);
    cu_mutex_lock(s->mu);
    s->freed++;
    cu_mutex_unlock(s->mu);
}

static void cb_release(void* ctx) {
    store_ctx* s = (store_ctx*)ctx;
    cu_mutex_lock(s->mu);
    s->released++;
    cu_mutex_unlock(s->mu);
}

static int read_int(store_ctx* s, const int* field) {
    int v;
    cu_mutex_lock(s->mu);
    v = *field;
    cu_mutex_unlock(s->mu);
    return v;
}

static int resolve_code(const char* uri, uint8_t** ref, size_t* ref_len) {
    uint8_t* out = NULL;
    size_t out_len = 0;
    int rc = net_blob_publish("c4-mem", uri, (const uint8_t*)"x", 1, ref, ref_len);
    if (rc != 0) {
        return 1000 + rc;
    }
    rc = net_blob_resolve("c4-mem", *ref, *ref_len, &out, &out_len);
    net_blob_free_buffer(*ref, *ref_len);
    *ref = NULL;
    if (rc == 0) {
        net_blob_free_buffer(out, out_len);
    }
    return rc;
}

static int run(void) {
    static store_ctx s, other;
    const net_blob_adapter_vtable_t vt = {cb_store, cb_fetch, cb_fetch_range, cb_exists, cb_free_buffer};
    net_blob_adapter_vtable_t holed = vt;
    static const char payload[] = "bytes held by a C adapter";
    uint8_t *ref = NULL, *out = NULL;
    size_t ref_len = 0, out_len = 0;
    int i;

    s.mu = cu_mutex_new();
    other.mu = cu_mutex_new();
    CU_CHECK("mutexes", s.mu != NULL && other.mu != NULL);

    /* ---- refused registrations never take ctx ---- */

    CU_CHECK_RC("register: a NULL vtable is -1",
                net_blob_register_callback_adapter_owned("c4-mem", NULL, &s, cb_release), -1);
    CU_CHECK_RC("register: a NULL release_fn is -1",
                net_blob_register_callback_adapter_owned("c4-mem", &vt, &s, NULL), -1);
    holed.exists = NULL;
    CU_CHECK_RC("register: a NULL vtable entry is NET_ERR_BLOB_BACKEND",
                net_blob_register_callback_adapter_owned("c4-mem", &holed, &s, cb_release),
                NET_ERR_BLOB_BACKEND);
    CU_CHECK_RC("refused registrations: release_fn never ran", read_int(&s, &s.released), 0);
    CU_CHECK_RC("refused registrations: the id is not registered", net_blob_adapter_registered("c4-mem"), 0);

    /* ---- register, publish, resolve ---- */

    CU_CHECK_RC("register", net_blob_register_callback_adapter_owned("c4-mem", &vt, &s, cb_release), 0);
    CU_CHECK_RC("registered", net_blob_adapter_registered("c4-mem"), 1);
    CU_CHECK_RC("register: a duplicate id is NET_ERR_BLOB_DUPLICATE_ID",
                net_blob_register_callback_adapter_owned("c4-mem", &vt, &other, cb_release),
                NET_ERR_BLOB_DUPLICATE_ID);
    CU_CHECK_RC("duplicate: release_fn never ran for its ctx", read_int(&other, &other.released), 0);
    CU_CHECK_RC("duplicate: the first adapter is still released 0 times", read_int(&s, &s.released), 0);

    CU_CHECK_RC("net_blob_publish",
                net_blob_publish("c4-mem", "mesh:c4/payload", (const uint8_t*)payload, sizeof payload, &ref,
                                 &ref_len),
                0);
    CU_CHECK_RC("publish: the C store callback ran once", read_int(&s, &s.stores), 1);
    CU_CHECK_RC("net_blob_resolve", net_blob_resolve("c4-mem", ref, ref_len, &out, &out_len), 0);
    CU_CHECK("resolve: the bytes the C adapter holds",
             out != NULL && out_len == sizeof payload && memcmp(out, payload, out_len) == 0);
    net_blob_free_buffer(out, out_len);
    out = NULL;
    for (i = 0; i < 4; i++) {
        CU_CHECK_RC("resolve again", net_blob_resolve("c4-mem", ref, ref_len, &out, &out_len), 0);
        net_blob_free_buffer(out, out_len);
        out = NULL;
    }
    CU_CHECK_RC("fetch: five buffers handed out", read_int(&s, &s.handed_out), 5);
    CU_CHECK_RC("free_buffer: every one came back", read_int(&s, &s.freed), read_int(&s, &s.handed_out));

    /* ---- a callback's error, as the caller sees it ---- */

    {
        uint8_t* r = NULL;
        size_t rl = 0;
        CU_CHECK_RC("callback NOT_FOUND: the caller sees NET_ERR_BLOB_NOT_FOUND",
                    resolve_code("mesh:c4/missing", &r, &rl), NET_ERR_BLOB_NOT_FOUND);
        CU_CHECK_RC("callback HASH_MISMATCH: the caller sees NET_ERR_BLOB_BACKEND",
                    resolve_code("mesh:c4/corrupt", &r, &rl), NET_ERR_BLOB_BACKEND);
        CU_CHECK_RC("callback -999: the caller sees NET_ERR_BLOB_BACKEND",
                    resolve_code("mesh:c4/weird", &r, &rl), NET_ERR_BLOB_BACKEND);
    }
    CU_CHECK_RC("errors: release_fn still has not run", read_int(&s, &s.released), 0);

    /* ---- unregister: release exactly once ---- */

    CU_CHECK_RC("unregister", net_blob_unregister_adapter("c4-mem"), 1);
    for (i = 0; i < 100 && read_int(&s, &s.released) == 0; i++) {
        cu_sleep_ms(20); /* no call is in flight, so release follows promptly */
    }
    CU_CHECK_RC("unregister: release_fn ran exactly once", read_int(&s, &s.released), 1);
    CU_CHECK_RC("unregister again is 0", net_blob_unregister_adapter("c4-mem"), 0);
    CU_CHECK_RC("not registered any more", net_blob_adapter_registered("c4-mem"), 0);
    cu_sleep_ms(50);
    CU_CHECK_RC("release_fn still ran exactly once", read_int(&s, &s.released), 1);
    CU_CHECK_RC("resolve a real ref after unregister is NET_ERR_BLOB_NOT_REGISTERED",
                net_blob_resolve("c4-mem", ref, ref_len, &out, &out_len), NET_ERR_BLOB_NOT_REGISTERED);
    net_blob_free_buffer(ref, ref_len);

    for (i = 0; i < SLOTS; i++) {
        free(s.uri[i]);
        free(s.data[i]);
    }
    cu_mutex_free(s.mu);
    cu_mutex_free(other.mu);
    return 0;
}

int main(void) {
    static const clm_fn_t used[] = {
        CLM_FN(net_blob_register_callback_adapter_owned),
        CLM_FN(net_blob_adapter_registered),
        CLM_FN(net_blob_unregister_adapter),
        CLM_FN(net_blob_publish),
        CLM_FN(net_blob_resolve),
        CLM_FN(net_blob_free_buffer),
    };
    if (clm_check_loaded_module(used, sizeof used / sizeof used[0]) != 0) {
        return 2;
    }
    if (run() != 0) {
        return 1;
    }
    return cu_finish();
}
