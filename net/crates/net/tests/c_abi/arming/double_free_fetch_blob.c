/*
 * Arming negative for the Linux sanitizer lane (C SDK plan, C5): a buffer
 * libnet returned is released twice through its own free function. Not a
 * consumer program; run only by run-c-consumers.py --arming, which requires
 * it to be caught.
 *
 * NET-LANE: sanitize
 * NET-EXPECT: ERROR: AddressSanitizer: attempting double-free
 *
 * A real net_fetch_blob output is passed to net_transport_free_buffer twice
 * (net_transport.h: "Do not call twice on the same pointer"). The second
 * release reaches the system allocator inside the uninstrumented library,
 * which ASan intercepts.
 */

#include <string.h>

#include "net.go.h"
#include "net_cortex.h"
#include "net_transport.h"

#include "consumer_util.h"
#include "loaded_module.h"

int main(void) {
    static const clm_fn_t used[] = {CU_MESH_FNS, CLM_FN(net_fetch_blob), CLM_FN(net_transport_free_buffer)};
    net_meshnode_t *holder = NULL, *reader = NULL;
    char ha[32], ra[32];
    static unsigned char payload[2048];
    uint8_t *ref = NULL, *out = NULL, hash[32];
    size_t ref_len = 0, out_len = 0;
    net_redex_t *rh, *rr;
    net_mesh_blob_adapter_t *sh, *sr;
    if (clm_check_loaded_module(used, sizeof used / sizeof used[0]) != 0) {
        return 2;
    }
    memset(payload, 0x6D, sizeof payload);
    if (cu_mesh_build(0xE3, &holder, ha, sizeof ha) || cu_mesh_build(0xE4, &reader, ra, sizeof ra) ||
        cu_mesh_handshake(holder, reader, ha) || net_mesh_start(holder) || net_mesh_start(reader)) {
        printf("FAIL setup: nodes\n");
        return 1;
    }
    rh = net_redex_new(NULL);
    rr = net_redex_new(NULL);
    sh = net_mesh_blob_adapter_new(rh, "objects", 0, NULL);
    sr = net_mesh_blob_adapter_new(rr, "objects", 0, NULL);
    if (!sh || !sr || net_serve_blob_transfer(holder, sh) || net_serve_blob_transfer(reader, sr) ||
        net_mesh_blob_adapter_publish(sh, (const uint8_t*)"mesh:arm/df", 11, payload, sizeof payload, &ref,
                                      &ref_len) ||
        net_blob_ref_hash(ref, ref_len, hash) ||
        net_fetch_blob(reader, net_mesh_node_id(holder), hash, &out, &out_len)) {
        printf("FAIL setup: fetch\n");
        return 1;
    }
    printf("setup ok: fetched %zu bytes; releasing them twice on purpose\n", out_len);
    net_transport_free_buffer(out, out_len);
    net_transport_free_buffer(out, out_len); /* the defect */
    printf("FAIL the second release was not caught\n");
    return 1;
}
