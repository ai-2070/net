/*
 * Arming negative for the Linux sanitizer lane (C SDK plan, C5): a buffer
 * libnet RETURNED is leaked. Not a consumer program; run only by
 * run-c-consumers.py --arming, which requires it to be caught.
 *
 * NET-EXPECT: ERROR: LeakSanitizer: detected memory leaks
 * NET-EXPECT: Direct leak of 4093 byte\(s\) in 1 object\(s\)
 *
 * A real, non-empty net_fetch_blob output, fetched across two nodes, is never
 * passed to net_transport_free_buffer; everything else is released. LSan sees
 * it because it intercepts the system allocator the Rust library uses. The
 * size is deliberately odd, so the report is matched to this one buffer, not
 * to any leak: a leak of the program's own malloc would not prove the lane
 * sees library-returned memory.
 */

#include <string.h>

#include "net.go.h"
#include "net_cortex.h"
#include "net_transport.h"

#include "consumer_util.h"
#include "loaded_module.h"

#define LEAK_LEN 4093

int main(void) {
    static const clm_fn_t used[] = {CU_MESH_FNS, CLM_FN(net_fetch_blob)};
    net_meshnode_t *holder = NULL, *reader = NULL;
    char ha[32], ra[32];
    static unsigned char payload[LEAK_LEN];
    uint8_t *ref = NULL, *out = NULL, hash[32];
    size_t ref_len = 0, out_len = 0;
    net_redex_t *rh, *rr;
    net_mesh_blob_adapter_t *sh, *sr;
    if (clm_check_loaded_module(used, sizeof used / sizeof used[0]) != 0) {
        return 2;
    }
    memset(payload, 0x5C, sizeof payload);
    if (cu_mesh_build(0xE1, &holder, ha, sizeof ha) || cu_mesh_build(0xE2, &reader, ra, sizeof ra) ||
        cu_mesh_handshake(holder, reader, ha) || net_mesh_start(holder) || net_mesh_start(reader)) {
        printf("FAIL setup: nodes\n");
        return 1;
    }
    rh = net_redex_new(NULL);
    rr = net_redex_new(NULL);
    sh = net_mesh_blob_adapter_new(rh, "objects", 0, NULL);
    sr = net_mesh_blob_adapter_new(rr, "objects", 0, NULL);
    if (!sh || !sr || net_serve_blob_transfer(holder, sh) || net_serve_blob_transfer(reader, sr) ||
        net_mesh_blob_adapter_publish(sh, (const uint8_t*)"mesh:arm/leak", 13, payload, sizeof payload, &ref,
                                      &ref_len) ||
        net_blob_ref_hash(ref, ref_len, hash) ||
        net_fetch_blob(reader, net_mesh_node_id(holder), hash, &out, &out_len) || out_len != LEAK_LEN) {
        printf("FAIL setup: fetch\n");
        return 1;
    }
    printf("setup ok: fetched %zu bytes; leaking them on purpose\n", out_len);
    /* The defect: no net_transport_free_buffer(out, out_len). */
    net_blob_free_buffer(ref, ref_len);
    net_mesh_blob_adapter_free(sh);
    net_mesh_blob_adapter_free(sr);
    net_redex_free(rh);
    net_redex_free(rr);
    net_mesh_shutdown(holder);
    net_mesh_shutdown(reader);
    net_mesh_free(holder);
    net_mesh_free(reader);
    out = NULL; /* drop the last reference, so LSan reports it as a direct leak */
    return 0;
}
