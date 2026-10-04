/*
 * tree_range.c — tree blobs, Reed-Solomon encoding and range reads, from C
 * (C SDK plan, C3). Production bundle.
 *
 * Mirrors the Go binding's tests of the same surface (go/blob_tree_test.go)
 * on the same bytes (cu_distinct_chunks is Go's distinctChunks), so both
 * bindings check one contract. That contract is net.go.h's "v0.3 tree /
 * erasure / range / repair" block:
 *
 *   - required out-pointers are checked first (NULL -> -1, nothing written);
 *   - after that, every out slot is (NULL, 0) before any later failure;
 *   - a well-formed call refused on its arguments is -150
 *     (NET_ERR_BLOB_INVALID_ARGUMENT);
 *   - fetch_range: start > end, a non-empty range over 1 GiB, or end past
 *     the size is -150; start == end is 0 with (NULL, 0), even past the size.
 *
 * The 1 GiB cap is checked against the same re-encoded ref Go's bigSmallRef
 * builds (a small ref claiming 2 GiB), so both bindings test one boundary.
 *
 * Built and run by .github/scripts/run-c-consumers.py.
 */

#include <stdlib.h>
#include <string.h>

#include "net.go.h"
#include "net_cortex.h"

#include "consumer_util.h"
#include "loaded_module.h"

#define MIB ((size_t)1 << 20)
#define GIB ((uint64_t)1 << 30)

/* Go's bigSmallRef: the cross-language `small` vector, size field rewritten. */
static const char* SMALL_REF_HEX =
    "b0b1b2b3016437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d850300000000000000";

static int hex_nibble(char c) {
    if (c >= '0' && c <= '9') {
        return c - '0';
    }
    if (c >= 'a' && c <= 'f') {
        return c - 'a' + 10;
    }
    return -1;
}

static size_t hex_decode(const char* hex, uint8_t* out, size_t cap) {
    size_t n = strlen(hex) / 2, i;
    if (n > cap) {
        return 0;
    }
    for (i = 0; i < n; i++) {
        int hi = hex_nibble(hex[2 * i]), lo = hex_nibble(hex[2 * i + 1]);
        if (hi < 0 || lo < 0) {
            return 0;
        }
        out[i] = (uint8_t)(hi * 16 + lo);
    }
    return n;
}

static int run(void) {
    net_redex_t* redex = net_redex_new(NULL);
    net_mesh_blob_adapter_t *a = NULL, *cached = NULL, *refused = (net_mesh_blob_adapter_t*)1;
    unsigned char *nine = NULL, *sixteen = NULL;
    uint8_t *ref = NULL, *rs_ref = NULL, *small_ref = NULL, *out = NULL;
    size_t ref_len = 0, rs_ref_len = 0, small_ref_len = 0, out_len = 0;
    char* json = NULL;
    uint64_t v = 0;
    static const char small[] = "a small tree";
    const uint64_t small_size = sizeof small - 1;
    uint8_t big[64];
    size_t big_len;
    int i;

    /* ---- adapters (net_mesh_blob_adapter_new_v2) ---- */

    CU_CHECK("net_redex_new", redex != NULL);
    CU_CHECK_RC("new_v2: NULL out-pointer is -1",
                net_mesh_blob_adapter_new_v2(redex, "c3-tree", 0, NULL, NULL), -1);
    CU_CHECK_RC("new_v2: an unknown option key is NET_ERR_INVALID_JSON",
                net_mesh_blob_adapter_new_v2(redex, "c3-bad", 0, "{\"no_such_key\":1}", &refused),
                NET_ERR_INVALID_JSON);
    CU_CHECK("new_v2: a refused call leaves its handle slot NULL", refused == NULL);
    CU_CHECK_RC("new_v2: no options", net_mesh_blob_adapter_new_v2(redex, "c3-tree", 0, NULL, &a), 0);
    CU_CHECK_RC("new_v2: a tree-node cache",
                net_mesh_blob_adapter_new_v2(redex, "c3-cached", 0, "{\"tree_node_cache_bytes\":4194304}",
                                             &cached),
                0);
    CU_CHECK_RC("tree_node_cache_stats: no cache", net_mesh_blob_adapter_tree_node_cache_stats(a, &json), 0);
    CU_CHECK("tree_node_cache_stats: no cache is JSON null", json != NULL && strcmp(json, "null") == 0);
    net_free_string(json);
    json = NULL;

    /* ---- a replicated tree over several leaves ---- */

    nine = cu_distinct_chunks(9, MIB);
    CU_CHECK("fixture: 9 MiB of distinct chunks", nine != NULL);
    CU_CHECK_RC("store_tree: Replicated",
                net_mesh_blob_adapter_store_tree(cached, nine, 9 * MIB, 0, 0, 0, &ref, &ref_len), 0);
    CU_CHECK_RC("net_blob_ref_describe: the tree ref", net_blob_ref_describe(ref, ref_len, &json), 0);
    CU_CHECK("describe: is_tree and is_chunked", cu_json_bool(json, "is_tree") == 1 &&
                                                     cu_json_bool(json, "is_chunked") == 1);
    CU_CHECK("describe: size is 9 MiB", cu_json_u64(json, "size", &v) == 0 && v == 9 * MIB);
    CU_CHECK("describe: a tree root and depth, no single hash",
             cu_json_has(json, "tree_root_hash") && cu_json_u64(json, "tree_depth", &v) == 0 && v > 0 &&
                 !cu_json_has(json, "hash"));
    CU_CHECK("describe: replicated encoding", strstr(json, "\"kind\":\"replicated\"") != NULL);
    net_free_string(json);
    json = NULL;

    CU_CHECK_RC("fetch_range: the whole tree",
                net_mesh_blob_adapter_fetch_range(cached, ref, ref_len, 0, 9 * MIB, &out, &out_len), 0);
    CU_CHECK("fetch_range: the whole tree, byte for byte",
             out != NULL && out_len == 9 * MIB && memcmp(out, nine, out_len) == 0);
    net_blob_free_buffer(out, out_len);
    out = NULL;
    CU_CHECK_RC("fetch_range: across a chunk boundary",
                net_mesh_blob_adapter_fetch_range(cached, ref, ref_len, 4 * MIB - 100, 4 * MIB + 4096, &out,
                                                  &out_len),
                0);
    CU_CHECK("fetch_range: across a chunk boundary, byte for byte",
             out_len == 4196 && memcmp(out, nine + 4 * MIB - 100, out_len) == 0);
    net_blob_free_buffer(out, out_len);
    out = NULL;
    CU_CHECK("net_mesh_blob_adapter_fetch: a whole tree is refused (fetch_range is the way)",
             net_mesh_blob_adapter_fetch(cached, ref, ref_len, &out, &out_len) != 0);
    CU_CHECK_RC("tree_node_cache_stats: with a cache",
                net_mesh_blob_adapter_tree_node_cache_stats(cached, &json), 0);
    {
        uint64_t hits = 0, misses = 0;
        CU_CHECK("tree_node_cache_stats: the reads went through it (lookups counted)",
                 cu_json_u64(json, "hits", &hits) == 0 && cu_json_u64(json, "misses", &misses) == 0 &&
                     hits + misses > 0 && cu_json_has(json, "bytes") && cu_json_has(json, "entries"));
    }
    net_free_string(json);
    json = NULL;

    /* ---- Reed-Solomon ---- */

    sixteen = cu_distinct_chunks(4, 4 * MIB);
    CU_CHECK("fixture: 16 MiB, one RS(4,2) stripe", sixteen != NULL);
    CU_CHECK_RC("store_tree: Reed-Solomon(4, 2)",
                net_mesh_blob_adapter_store_tree(a, sixteen, 16 * MIB, 1, 4, 2, &rs_ref, &rs_ref_len), 0);
    CU_CHECK_RC("describe: the RS ref", net_blob_ref_describe(rs_ref, rs_ref_len, &json), 0);
    CU_CHECK("describe: reed_solomon k=4 m=2",
             strstr(json, "\"kind\":\"reed_solomon\"") && cu_json_u64(json, "k", &v) == 0 && v == 4 &&
                 cu_json_u64(json, "m", &v) == 0 && v == 2);
    net_free_string(json);
    json = NULL;
    CU_CHECK_RC("fetch_range: the whole RS tree",
                net_mesh_blob_adapter_fetch_range(a, rs_ref, rs_ref_len, 0, 16 * MIB, &out, &out_len), 0);
    CU_CHECK("fetch_range: the whole RS tree, byte for byte",
             out_len == 16 * MIB && memcmp(out, sixteen, out_len) == 0);
    net_blob_free_buffer(out, out_len);
    out = NULL;

    /* ---- encoding refusals (-150) ---- */
    {
        static const struct {
            const char* name;
            uint8_t kind, k, m;
        } bad[] = {
            {"store_tree: Replicated with k is -150", 0, 1, 0},
            {"store_tree: Replicated with m is -150", 0, 0, 1},
            {"store_tree: RS with only m is -150", 1, 0, 2},
            {"store_tree: RS with only k is -150", 1, 4, 0},
            {"store_tree: RS with k + m = 256 is -150", 1, 200, 56},
            {"store_tree: an unknown encoding kind is -150", 2, 0, 0},
        };
        uint8_t* r = NULL;
        size_t rl = 0;
        for (i = 0; i < (int)(sizeof bad / sizeof bad[0]); i++) {
            CU_CHECK_RC(bad[i].name,
                        net_mesh_blob_adapter_store_tree(a, (const uint8_t*)"x", 1, bad[i].kind, bad[i].k,
                                                         bad[i].m, &r, &rl),
                        NET_ERR_BLOB_INVALID_ARGUMENT);
        }
        CU_CHECK_RC("store_tree: RS with both zero selects the defaults",
                    net_mesh_blob_adapter_store_tree(a, (const uint8_t*)"x", 1, 1, 0, 0, &r, &rl), 0);
        CU_CHECK_RC("describe: the default RS ref", net_blob_ref_describe(r, rl, &json), 0);
        CU_CHECK("describe: default RS has non-zero k and m",
                 cu_json_u64(json, "k", &v) == 0 && v > 0 && cu_json_u64(json, "m", &v) == 0 && v > 0);
        net_free_string(json);
        json = NULL;
        net_blob_free_buffer(r, rl);
    }

    /* ---- the range contract, in core's order ---- */

    CU_CHECK_RC("store_tree: a small tree",
                net_mesh_blob_adapter_store_tree(a, (const uint8_t*)small, (size_t)small_size, 0, 0, 0,
                                                 &small_ref, &small_ref_len),
                0);
    CU_CHECK_RC("fetch_range: reversed is -150",
                net_mesh_blob_adapter_fetch_range(a, small_ref, small_ref_len, 5, 4, &out, &out_len),
                NET_ERR_BLOB_INVALID_ARGUMENT);
    {
        const uint64_t at[] = {0, 3, small_size, small_size + 1000};
        for (i = 0; i < 4; i++) {
            out = (uint8_t*)1;
            out_len = 9;
            CU_CHECK_RC("fetch_range: an empty range succeeds anywhere",
                        net_mesh_blob_adapter_fetch_range(a, small_ref, small_ref_len, at[i], at[i], &out,
                                                          &out_len),
                        0);
            CU_CHECK("fetch_range: ... with (NULL, 0)", out == NULL && out_len == 0);
        }
    }
    CU_CHECK_RC("fetch_range: past the end is -150",
                net_mesh_blob_adapter_fetch_range(a, small_ref, small_ref_len, 0, small_size + 1, &out,
                                                  &out_len),
                NET_ERR_BLOB_INVALID_ARGUMENT);
    big_len = hex_decode(SMALL_REF_HEX, big, sizeof big);
    CU_CHECK("the 2 GiB ref: decoded", big_len == 45);
    for (i = 0; i < 8; i++) {
        big[5 + 32 + i] = (uint8_t)((2 * GIB) >> (8 * i));
    }
    CU_CHECK_RC("the 2 GiB ref: describes", net_blob_ref_describe(big, big_len, &json), 0);
    CU_CHECK("the 2 GiB ref: claims 2 GiB", cu_json_u64(json, "size", &v) == 0 && v == 2 * GIB);
    net_free_string(json);
    json = NULL;
    CU_CHECK_RC("fetch_range: over the 1 GiB cap is -150",
                net_mesh_blob_adapter_fetch_range(a, big, big_len, 0, GIB + 1, &out, &out_len),
                NET_ERR_BLOB_INVALID_ARGUMENT);
    CU_CHECK_RC("fetch_range: exactly 1 GiB passes the checks and is NOT_FOUND",
                net_mesh_blob_adapter_fetch_range(a, big, big_len, 0, GIB, &out, &out_len),
                NET_ERR_BLOB_NOT_FOUND);
    /* Two "no ref" shapes, neither of them -150 from C. A NULL ref pointer is
     * a NULL argument (src/ffi/blob.rs checks it with the handle); a real but
     * empty buffer reaches core, which cannot decode it. Go's
     * FetchRange(nil, ...) is -150 only because its wrapper (refArg in
     * go/blob_tree.go) refuses an empty ref before calling in. */
    CU_CHECK_RC("fetch_range: a NULL ref is -1",
                net_mesh_blob_adapter_fetch_range(a, NULL, 0, 0, 1, &out, &out_len), -1);
    CU_CHECK_RC("fetch_range: an empty ref buffer is NET_ERR_BLOB_DECODE",
                net_mesh_blob_adapter_fetch_range(a, small_ref, 0, 0, 1, &out, &out_len),
                NET_ERR_BLOB_DECODE);
    CU_CHECK_RC("fetch_range: an exact sub-range",
                net_mesh_blob_adapter_fetch_range(a, small_ref, small_ref_len, 2, 7, &out, &out_len), 0);
    CU_CHECK("fetch_range: [2, 7) byte for byte", out_len == 5 && memcmp(out, small + 2, 5) == 0);
    net_blob_free_buffer(out, out_len);
    out = NULL;

    /* ---- output storage ---- */

    out = (uint8_t*)1;
    out_len = 77;
    CU_CHECK_RC("fetch_range: a later failure ...",
                net_mesh_blob_adapter_fetch_range(a, small_ref, small_ref_len, 5, 4, &out, &out_len),
                NET_ERR_BLOB_INVALID_ARGUMENT);
    CU_CHECK("... leaves valid outputs at (NULL, 0)", out == NULL && out_len == 0);
    out = (uint8_t*)1;
    CU_CHECK_RC("fetch_range: a mixed-null output pair is -1",
                net_mesh_blob_adapter_fetch_range(a, small_ref, small_ref_len, 0, 1, &out, NULL), -1);
    CU_CHECK("... and the non-NULL slot is untouched", out == (uint8_t*)1);
    out = NULL;

    /* ---- teardown ---- */

    net_blob_free_buffer(ref, ref_len);
    net_blob_free_buffer(rs_ref, rs_ref_len);
    net_blob_free_buffer(small_ref, small_ref_len);
    free(nine);
    free(sixteen);
    net_mesh_blob_adapter_free(a);
    net_mesh_blob_adapter_free(cached);
    net_redex_free(redex);
    return 0;
}

int main(void) {
    static const clm_fn_t used[] = {
        CLM_FN(net_redex_new),
        CLM_FN(net_redex_free),
        CLM_FN(net_mesh_blob_adapter_new_v2),
        CLM_FN(net_mesh_blob_adapter_free),
        CLM_FN(net_mesh_blob_adapter_store_tree),
        CLM_FN(net_mesh_blob_adapter_fetch_range),
        CLM_FN(net_mesh_blob_adapter_fetch),
        CLM_FN(net_mesh_blob_adapter_tree_node_cache_stats),
        CLM_FN(net_blob_ref_describe),
        CLM_FN(net_blob_free_buffer),
        CLM_FN(net_free_string),
    };
    if (clm_check_loaded_module(used, sizeof used / sizeof used[0]) != 0) {
        return 2;
    }
    if (run() != 0) {
        return 1;
    }
    return cu_finish();
}
