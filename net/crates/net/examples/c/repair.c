/*
 * repair.c — Reed-Solomon repair of a tree blob, from C (C SDK plan, C3).
 *
 * NET-BUNDLE: helper
 *
 * Runs against the HELPER bundle only: breaking a shard on purpose needs
 * the test seams net_mesh_blob_adapter_test_drop_data_chunk and
 * _test_chunk_present, which the production library does not export. They
 * are declared in net_test_helpers.h, staged into the helper bundle alone
 * (D3). So everything this program proves is helper-build evidence, and is
 * cited as such; the repair call itself is the same production function.
 *
 * It keeps the Go binding's shape (go/blob_repair_test.go), on the same
 * bytes: one full 16 MiB stripe of RS(4, 2).
 *
 *   1. Store, then read the whole blob back: every chunk is there.
 *   2. Drop data shard (stripe 0, data 1). Prove it gone: _chunk_present
 *      says no. A whole read still succeeds byte for byte: one loss is
 *      within RS(4, 2)'s tolerance, so the read reconstructs from parity.
 *   3. Repair: exactly one chunk restored in one stripe, none unrecoverable.
 *      The shard is present again, and the full range matches byte for
 *      byte. A second repair restores nothing and finds the stripe healthy.
 *   4. Drop three data shards (m = 2): a read that needs them fails, and
 *      the stripe is unrecoverable, which the report counts; it is not an
 *      error.
 *
 * Built and run by .github/scripts/run-c-consumers.py.
 */

#include <stdlib.h>
#include <string.h>

#include "net.go.h"
#include "net_cortex.h"
#include "net_test_helpers.h"

#include "consumer_util.h"
#include "loaded_module.h"

#define MIB ((size_t)1 << 20)
#define SIZE (16 * MIB)

static int run(void) {
    net_redex_t* redex = net_redex_new(NULL);
    net_mesh_blob_adapter_t *a = NULL, *b = NULL;
    unsigned char* data = cu_distinct_chunks(4, 4 * MIB);
    uint8_t *ref = NULL, *ref_b = NULL, *out = NULL;
    size_t ref_len = 0, ref_b_len = 0, out_len = 0;
    uint8_t dropped[32], scratch[32];
    char* json = NULL;
    uint64_t v = 0;
    int rc;
    uint32_t i;

    CU_CHECK("fixture: 16 MiB, one RS(4,2) stripe", redex != NULL && data != NULL);
    CU_CHECK_RC("new_v2", net_mesh_blob_adapter_new_v2(redex, "c3-repair", 0, NULL, &a), 0);
    CU_CHECK_RC("store_tree: RS(4, 2)",
                net_mesh_blob_adapter_store_tree(a, data, SIZE, 1, 4, 2, &ref, &ref_len), 0);
    CU_CHECK_RC("describe", net_blob_ref_describe(ref, ref_len, &json), 0);
    CU_CHECK("describe: reed_solomon k=4 m=2",
             strstr(json, "\"kind\":\"reed_solomon\"") && cu_json_u64(json, "k", &v) == 0 && v == 4 &&
                 cu_json_u64(json, "m", &v) == 0 && v == 2);
    net_free_string(json);
    json = NULL;

    /* 1. Full closure before anything is broken. */
    CU_CHECK_RC("before: fetch_range of the whole blob",
                net_mesh_blob_adapter_fetch_range(a, ref, ref_len, 0, SIZE, &out, &out_len), 0);
    CU_CHECK("before: byte for byte", out_len == SIZE && memcmp(out, data, SIZE) == 0);
    net_blob_free_buffer(out, out_len);
    out = NULL;

    /* 2. Break one data shard and prove it is gone. */
    {
        /* net_test_helpers.h: a non-tree ref is -150. A real small ref, not
         * arbitrary bytes, which would fail earlier as undecodable (-110). */
        uint8_t* small = NULL;
        size_t small_len = 0;
        CU_CHECK_RC("publish a small (non-tree) blob",
                    net_mesh_blob_adapter_publish(a, (const uint8_t*)"mesh:c3/small", 13,
                                                  (const uint8_t*)"small", 5, &small, &small_len),
                    0);
        CU_CHECK_RC("test seam: a non-tree ref is -150",
                    net_mesh_blob_adapter_test_drop_data_chunk(a, small, small_len, 0, 0, scratch),
                    NET_ERR_BLOB_INVALID_ARGUMENT);
        net_blob_free_buffer(small, small_len);
    }
    CU_CHECK_RC("test seam: drop data shard (0, 1)",
                net_mesh_blob_adapter_test_drop_data_chunk(a, ref, ref_len, 0, 1, dropped), 0);
    CU_CHECK_RC("after the drop: the shard is not present",
                net_mesh_blob_adapter_test_chunk_present(a, dropped), 0);
    /* One lost shard is within RS(4, 2)'s tolerance: a read reconstructs it
     * from parity rather than failing. So absence is proved by the seam
     * above, and this proves the degraded read; a read that must fail comes
     * in step 4, past the tolerance. */
    CU_CHECK_RC("degraded read: one shard missing, the whole blob still reads",
                net_mesh_blob_adapter_fetch_range(a, ref, ref_len, 0, SIZE, &out, &out_len), 0);
    CU_CHECK("degraded read: reconstructed byte for byte", out_len == SIZE && memcmp(out, data, SIZE) == 0);
    net_blob_free_buffer(out, out_len);
    out = NULL;

    /* 3. Repair. */
    CU_CHECK_RC("repair_blob", net_mesh_blob_adapter_repair_blob(a, ref, ref_len, &json), 0);
    CU_CHECK("repair: one chunk restored",
             cu_json_u64(json, "chunks_restored", &v) == 0 && v == 1);
    CU_CHECK("repair: in one stripe", cu_json_u64(json, "stripes_repaired", &v) == 0 && v == 1);
    CU_CHECK("repair: none unrecoverable",
             cu_json_u64(json, "stripes_unrecoverable", &v) == 0 && v == 0);
    CU_CHECK("repair: one stripe walked", cu_json_u64(json, "stripes_walked", &v) == 0 && v == 1);
    net_free_string(json);
    json = NULL;
    CU_CHECK_RC("after repair: the shard is present again",
                net_mesh_blob_adapter_test_chunk_present(a, dropped), 1);
    CU_CHECK_RC("after repair: fetch_range of the whole blob",
                net_mesh_blob_adapter_fetch_range(a, ref, ref_len, 0, SIZE, &out, &out_len), 0);
    CU_CHECK("after repair: byte for byte", out_len == SIZE && memcmp(out, data, SIZE) == 0);
    net_blob_free_buffer(out, out_len);
    out = NULL;
    CU_CHECK_RC("repair again", net_mesh_blob_adapter_repair_blob(a, ref, ref_len, &json), 0);
    CU_CHECK("repair again: nothing restored", cu_json_u64(json, "chunks_restored", &v) == 0 && v == 0);
    CU_CHECK("repair again: the stripe is healthy",
             cu_json_u64(json, "stripes_already_healthy", &v) == 0 && v == 1);
    net_free_string(json);
    json = NULL;

    /* 4. More than m shards lost. */
    CU_CHECK_RC("new_v2: a second adapter", net_mesh_blob_adapter_new_v2(redex, "c3-lost", 0, NULL, &b), 0);
    CU_CHECK_RC("store_tree: RS(4, 2) again",
                net_mesh_blob_adapter_store_tree(b, data, SIZE, 1, 4, 2, &ref_b, &ref_b_len), 0);
    for (i = 0; i < 3; i++) {
        CU_CHECK_RC("test seam: drop a data shard (three, m = 2)",
                    net_mesh_blob_adapter_test_drop_data_chunk(b, ref_b, ref_b_len, 0, i, scratch), 0);
    }
    rc = net_mesh_blob_adapter_fetch_range(b, ref_b, ref_b_len, 0, SIZE, &out, &out_len);
    CU_CHECK("past the tolerance: a read that needs the lost shards fails", rc != 0);
    CU_CHECK("past the tolerance: ... with (NULL, 0) outputs", out == NULL && out_len == 0);
    CU_CHECK_RC("repair_blob over the tolerance is not an error",
                net_mesh_blob_adapter_repair_blob(b, ref_b, ref_b_len, &json), 0);
    CU_CHECK("repair: the stripe is counted unrecoverable",
             cu_json_u64(json, "stripes_unrecoverable", &v) == 0 && v == 1);
    CU_CHECK("repair: nothing restored", cu_json_u64(json, "chunks_restored", &v) == 0 && v == 0);
    net_free_string(json);
    json = NULL;

    net_blob_free_buffer(ref, ref_len);
    net_blob_free_buffer(ref_b, ref_b_len);
    free(data);
    net_mesh_blob_adapter_free(a);
    net_mesh_blob_adapter_free(b);
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
        CLM_FN(net_mesh_blob_adapter_repair_blob),
        CLM_FN(net_mesh_blob_adapter_publish),
        CLM_FN(net_mesh_blob_adapter_test_drop_data_chunk),
        CLM_FN(net_mesh_blob_adapter_test_chunk_present),
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
