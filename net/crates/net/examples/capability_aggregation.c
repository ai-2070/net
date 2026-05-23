/*
 * Net C SDK — Capability Aggregation Example
 *
 * Demonstrates the Phase 6c capability-aggregation surface added
 * by `docs/plans/MULTIFOLD_PHASE_6C_CAPACITY_AGGREGATION.md`:
 *
 *   - net_capability_aggregate          — bucket-and-count
 *                                         primitive over the
 *                                         local CapabilityFold
 *   - net_capability_capacity_ranking   — state-broken-down
 *                                         materialized view with
 *                                         optional RTT gate and
 *                                         summed numeric capacity
 *
 * Wire shape: every aggregation type (TagMatcher / GroupBy /
 * Aggregation / CapacityQuery) crosses the C ABI as a
 * serde-JSON-encoded string. Caller marshals JSON, calls into
 * libnet_compute, and unmarshals the JSON-array response.
 *
 * Build (Linux):
 *   cargo build --release --features net
 *   gcc -o capability_aggregation examples/capability_aggregation.c \
 *       -L target/release -lnet -lnet_compute -lpthread -ldl -lm
 *
 * Run:
 *   LD_LIBRARY_PATH=target/release ./capability_aggregation \
 *       <hex_peer_public_key> <peer_addr>
 *
 * A real-world run requires an existing MeshNode with peers that
 * have announced capabilities. This example uses a single local
 * node + its self-index; it announces a small capability set and
 * then queries the fold for it.
 */

#include "../include/net.go.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void print_and_free(const char* label, char* s) {
    if (s == NULL) {
        printf("%s: <null>\n", label);
        return;
    }
    printf("%s: %s\n", label, s);
    net_compute_free_cstring(s);
}

int main(void) {
    /* =====================================================
     * 1. Create a mesh node + announce capabilities.
     *    `net_meshnode_new` and `net_meshnode_announce_capabilities`
     *    are declared in net.go.h's mesh section. We bind to a
     *    loopback addr for the demo. The PSK is a fixed 32-byte
     *    constant — replace with a real shared secret in
     *    production.
     * ===================================================== */
    const uint8_t psk[32] = {0x42};  /* trailing zero-fill */
    net_meshnode_t* node = net_meshnode_new("127.0.0.1:0", psk, NULL);
    if (node == NULL) {
        fprintf(stderr, "net_meshnode_new failed\n");
        return 1;
    }

    /* Announce a capability set with a couple GPU tags. The
     * self-index applies immediately — the fold below sees these
     * tags via the local-publish path. */
    const char* caps =
        "{\"tags\":["
        "\"hardware.gpu\","
        "\"hardware.gpu.h100\","
        "\"hardware.gpu.count=8\","
        "\"software.python=3.11\""
        "],\"metadata\":{}}";
    if (net_meshnode_announce_capabilities(node, caps) != 0) {
        fprintf(stderr, "announce_capabilities failed\n");
        net_meshnode_shutdown(node);
        return 1;
    }

    /* =====================================================
     * 2. Clone an Arc<MeshNode> handle for the
     *    aggregation FFI. The handle is passed as a const
     *    pointer through `net_capability_*`; remember to free
     *    via `net_mesh_arc_free` once we're done.
     * ===================================================== */
    net_compute_mesh_arc_t* arc = net_mesh_arc_clone(node);
    if (arc == NULL) {
        fprintf(stderr, "net_mesh_arc_clone failed\n");
        net_meshnode_shutdown(node);
        return 1;
    }

    /* =====================================================
     * 3. Bucket the fold by GPU stem. JSON shapes match the
     *    Rust core's serde encoding (kind discriminant +
     *    struct fields).
     * ===================================================== */
    {
        const char* matcher = "{\"kind\":\"prefix\",\"value\":\"hardware.gpu\"}";
        const char* group_by = "{\"kind\":\"tag_stem\",\"prefix\":\"hardware.gpu\"}";
        const char* agg = "{\"kind\":\"count\"}";
        char* rows = net_capability_aggregate(arc, matcher, group_by, agg);
        print_and_free("[aggregate] count_by_gpu_stem", rows);
    }

    /* =====================================================
     * 4. Capacity ranking: per-region state breakdown with the
     *    `hardware.gpu.count` numeric tag summed. No RTT map
     *    here — pass NULL to skip the latency gate.
     * ===================================================== */
    {
        const char* query =
            "{"
            "\"matcher\":{\"kind\":\"prefix\",\"value\":\"hardware.gpu\"},"
            "\"group_by\":{\"kind\":\"tag_stem\",\"prefix\":\"hardware.gpu\"},"
            "\"max_rtt_ms\":null,"
            "\"sum_axis_key\":\"hardware.gpu.count\","
            "\"limit\":5"
            "}";
        char* rows = net_capability_capacity_ranking(arc, query, NULL);
        print_and_free("[capacity_ranking] top_5_gpu_stems", rows);
    }

    /* =====================================================
     * 5. Cleanup. The Arc is freed before the node so the
     *    refcount drops cleanly; `net_meshnode_shutdown`
     *    handles the rest.
     * ===================================================== */
    net_mesh_arc_free(arc);
    net_meshnode_shutdown(node);
    return 0;
}
