/*
 * mesh_opaque.c — the net.go.h side of mesh_opaque.h (see there).
 */

#include "mesh_opaque.h"

#include "consumer_util.h"

int cu_opaque_pair(unsigned char seed_a, unsigned char seed_b, void** out_a, void** out_b) {
    net_meshnode_t *a = NULL, *b = NULL;
    char a_addr[32], b_addr[32];
    if (cu_net_init() != 0 || cu_mesh_build(seed_a, &a, a_addr, sizeof a_addr) != 0 ||
        cu_mesh_build(seed_b, &b, b_addr, sizeof b_addr) != 0 || cu_mesh_handshake(a, b, a_addr) != 0 ||
        net_mesh_start(a) != 0 || net_mesh_start(b) != 0) {
        /* Release whatever was built before the step that failed. */
        if (b != NULL) {
            net_mesh_shutdown(b);
            net_mesh_free(b);
        }
        if (a != NULL) {
            net_mesh_shutdown(a);
            net_mesh_free(a);
        }
        return -1;
    }
    *out_a = a;
    *out_b = b;
    return 0;
}

uint64_t cu_opaque_node_id(void* node) {
    return net_mesh_node_id((net_meshnode_t*)node);
}

int cu_opaque_teardown(void* node) {
    int rc = net_mesh_shutdown((net_meshnode_t*)node);
    net_mesh_free((net_meshnode_t*)node);
    return rc;
}

size_t cu_opaque_fns(clm_fn_t* out, size_t cap) {
    static const clm_fn_t fns[] = {
        CU_MESH_FNS,
        CLM_FN(net_mesh_start),
        CLM_FN(net_mesh_shutdown),
        CLM_FN(net_mesh_free),
    };
    size_t n = sizeof fns / sizeof fns[0];
    size_t i;
    for (i = 0; i < n && i < cap; i++) {
        out[i] = fns[i];
    }
    return n;
}
