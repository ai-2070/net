/*
 * Net C SDK — gang scheduler + task-lifecycle example.
 *
 * Walks the two scheduler surfaces shipped in libnet:
 *
 *   Task lifecycle (net_workflow_*, net_cortex.h):
 *     1. Open an in-memory Redex.
 *     2. Open a WorkflowAdapter against it.
 *     3. Drive a task submit -> start -> complete, then read it back
 *        and confirm it is terminal (`done`).
 *     4. Roll up status counts.
 *
 *   Gang claim (net_mesh_*, net.go.h):
 *     5. Open a mesh node.
 *     6. Reserve an island, then release it (both "won").
 *     7. Release an island this node never held ("lost").
 *
 * (publish_island_topology / match_gpu_islands / claim_gpu_island take a
 *  JSON criteria/record string — see net.go.h; omitted here to keep the
 *  example free of a JSON builder.)
 *
 * Build:
 *   cargo build --release -p net-mesh --features net,netdb,redex-disk
 *   gcc -o scheduler scheduler.c -L ../../target/release -lnet \
 *       -lpthread -ldl -lm
 *
 * Run:
 *   LD_LIBRARY_PATH=../../target/release ./scheduler     (Linux)
 *   DYLD_LIBRARY_PATH=../../target/release ./scheduler   (macOS)
 */

#include "../include/net.go.h"
#include "../include/net_cortex.h"

#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

#define CHECK(expr, what)                                                   \
    do {                                                                    \
        int _rc = (expr);                                                   \
        if (_rc != 0) {                                                     \
            fprintf(stderr, "%s failed: rc=%d\n", (what), _rc);             \
            return 1;                                                       \
        }                                                                   \
    } while (0)

/* Workflow status codes (mirror net_cortex.h). */
#define WF_DONE 4

int main(void) {
    /* ---- Task lifecycle ---- */
    net_redex_t* redex = net_redex_new(NULL); /* in-memory */
    if (!redex) {
        fprintf(stderr, "net_redex_new returned NULL\n");
        return 1;
    }

    net_workflow_adapter_t* wf = NULL;
    CHECK(net_workflow_adapter_open(redex, 0x0F105D01ULL, 0, &wf),
          "net_workflow_adapter_open");

    uint64_t seq = 0;
    CHECK(net_workflow_submit(wf, 1, &seq), "submit");
    CHECK(net_workflow_start(wf, 1, &seq), "start");
    CHECK(net_workflow_advance(wf, 1, &seq), "advance"); /* step 0 -> 1 */
    CHECK(net_workflow_complete(wf, 1, &seq), "complete");
    CHECK(net_workflow_wait_for_seq(wf, seq, 5000), "wait_for_seq");

    int found = 0, status = -1;
    uint32_t step = 0, attempts = 0;
    CHECK(net_workflow_get(wf, 1, &found, &step, &status, &attempts), "get");
    if (!found || status != WF_DONE) {
        fprintf(stderr, "task 1 not done: found=%d status=%d\n", found, status);
        return 1;
    }
    printf("task 1: step=%" PRIu32 " status=done(%d) attempts=%" PRIu32 "\n",
           step, status, attempts);

    net_workflow_status_counts_t counts;
    CHECK(net_workflow_status_counts(wf, &counts), "status_counts");
    printf("status counts: done=%" PRIu64 "\n", counts.done);

    net_workflow_adapter_free(wf);
    net_redex_free(redex);

    /* ---- Gang claim ---- */
    net_meshnode_t* node = NULL;
    const char* cfg =
        "{\"bind_addr\":\"127.0.0.1:0\","
        "\"psk_hex\":\"5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b"
        "5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b\"}";
    CHECK(net_mesh_new(cfg, &node), "net_mesh_new");

    const uint64_t until = 1000000000000000000ULL; /* far-future deadline */
    int outcome = -1;
    CHECK(net_mesh_reserve_island(node, 0xA0, until, &outcome), "reserve_island");
    printf("reserve 0xA0: %s\n", outcome == 0 ? "won" : "lost");

    CHECK(net_mesh_release_island(node, 0xA0, &outcome), "release_island");
    printf("release 0xA0: %s\n", outcome == 0 ? "won" : "lost");

    /* Releasing an island we never held reports "lost", not a false "won". */
    CHECK(net_mesh_release_island(node, 0xBEEF, &outcome), "release unheld");
    printf("release 0xBEEF (unheld): %s\n", outcome == 0 ? "won" : "lost");

    net_mesh_free(node);

    printf("scheduler example OK\n");
    return 0;
}
