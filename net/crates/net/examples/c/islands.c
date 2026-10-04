/*
 * islands.c — the gang-claim resource-island scheduler, from C (net.go.h,
 * "Gang-claim resource-island scheduler").
 *
 * Node A carries the host tag "gpu:h100" and publishes an island of four
 * units with resident "model:2a". Checked against the header:
 *
 *   - publish forces the island's host to this node and reports the count;
 *   - match_islands finds it by host tags, by resident tags and by
 *     min_units, and finds nothing when a criterion excludes it;
 *   - claim_island reserves the match (out_found = 1, its id); a second
 *     reserve by the holder extends it (won), and release gives it back;
 *   - unparseable criteria or record JSON is NET_ERR_GANG_INVALID;
 *   - a connected peer, B, learns the island from A's broadcast and its
 *     own scheduler matches it.
 */

#include <stdio.h>
#include <string.h>
#include <time.h>

#include "net.go.h"

#include "consumer_util.h"
#include "loaded_module.h"

#define ISLAND 0xD0

static uint64_t now_us(void) {
    return (uint64_t)time(NULL) * 1000000ull;
}

static int match(net_meshnode_t* n, const char* criteria, uint64_t* ids, size_t* count) {
    return net_mesh_match_islands(n, criteria, ids, 8, count);
}

int main(void) {
    static const clm_fn_t used[] = {
        CU_MESH_FNS,
        CLM_FN(net_mesh_start),
        CLM_FN(net_mesh_shutdown),
        CLM_FN(net_mesh_free),
        CLM_FN(net_mesh_announce_capabilities),
        CLM_FN(net_mesh_publish_island_topology),
        CLM_FN(net_mesh_match_islands),
        CLM_FN(net_mesh_claim_island),
        CLM_FN(net_mesh_reserve_island),
        CLM_FN(net_mesh_release_island),
    };
    static const char* record =
        "{\"id\":208,\"units\":[0,1,2,3],\"capabilities\":[\"model:2a\"],\"load\":0.1,\"p50_latency_us\":800}";
    net_meshnode_t *a = NULL, *b = NULL;
    char a_addr[32], b_addr[32];
    uint64_t ids[8], island = 0;
    size_t count = 0, republished = 0;
    int found = -1, outcome = -1, waited;

    if (clm_check_loaded_module(used, sizeof used / sizeof used[0]) != 0) {
        return 2;
    }
    if (cu_net_init() != 0) {
        printf("FAIL setup: cu_net_init\n");
        return 1;
    }
    CU_CHECK_RC("bring-up: A", cu_mesh_build(0xA7, &a, a_addr, sizeof a_addr), 0);
    CU_CHECK_RC("bring-up: B", cu_mesh_build(0xB7, &b, b_addr, sizeof b_addr), 0);
    CU_CHECK_RC("bring-up: handshake", cu_mesh_handshake(b, a, b_addr), 0);
    CU_CHECK_RC("bring-up: start A", net_mesh_start(a), 0);
    CU_CHECK_RC("bring-up: start B", net_mesh_start(b), 0);
    CU_CHECK_RC("A announces its host tag", net_mesh_announce_capabilities(a, "{\"tags\":[\"gpu:h100\"]}"), 0);
    CU_CHECK_RC("B announces (so A's broadcasts reach it)", net_mesh_announce_capabilities(b, "{\"tags\":[]}"), 0);

    CU_CHECK_RC("net_mesh_publish_island_topology", net_mesh_publish_island_topology(a, record, &count), 0);
    CU_CHECK("publish: reports at least the one island", count >= 1);
    CU_CHECK_RC("publish: unparseable record is NET_ERR_GANG_INVALID",
                net_mesh_publish_island_topology(a, "{\"id\":", &count), NET_ERR_GANG_INVALID);

    count = 0;
    for (waited = 0; waited < 5000; waited += 100) {
        if (match(a, "{\"tags_all\":[\"gpu:h100\"]}", ids, &count) == 0 && count >= 1) {
            break;
        }
        cu_sleep_ms(100);
    }
    CU_CHECK("match_islands: by host tag, the island", count == 1 && ids[0] == ISLAND);
    CU_CHECK_RC("match_islands: by resident tag",
                match(a, "{\"tags_all\":[\"gpu:h100\"],\"require_all\":[\"model:2a\"],\"min_units\":4}", ids, &count), 0);
    CU_CHECK("match_islands: resident model:2a with four units", count == 1 && ids[0] == ISLAND);
    CU_CHECK_RC("match_islands: min_units 8", match(a, "{\"tags_all\":[\"gpu:h100\"],\"min_units\":8}", ids, &count), 0);
    CU_CHECK_RC("match_islands: more units than it has excludes it", count, 0);
    CU_CHECK_RC("match_islands: another host tag",
                match(a, "{\"tags_all\":[\"gpu:a100\"]}", ids, &count), 0);
    CU_CHECK_RC("match_islands: excludes it", count, 0);
    CU_CHECK_RC("match_islands: another resident",
                match(a, "{\"tags_all\":[\"gpu:h100\"],\"require_all\":[\"model:9z\"]}", ids, &count), 0);
    CU_CHECK_RC("match_islands: excludes it (resident)", count, 0);
    CU_CHECK_RC("match_islands: unparseable criteria is NET_ERR_GANG_INVALID",
                match(a, "{\"tags_all\":", ids, &count), NET_ERR_GANG_INVALID);

    CU_CHECK_RC("net_mesh_claim_island",
                net_mesh_claim_island(a, "{\"tags_all\":[\"gpu:h100\"],\"selection\":\"least_loaded\"}",
                                      now_us() + 60000000ull, &found, &island),
                0);
    CU_CHECK("claim_island: found, the island", found == 1 && island == ISLAND);
    CU_CHECK_RC("net_mesh_reserve_island: the holder extends", net_mesh_reserve_island(a, ISLAND, now_us() + 90000000ull,
                                                                                       &outcome),
                0);
    CU_CHECK_RC("reserve: won", outcome, 0);
    CU_CHECK_RC("net_mesh_release_island", net_mesh_release_island(a, ISLAND, &outcome), 0);
    CU_CHECK_RC("release: won", outcome, 0);
    CU_CHECK_RC("claim_island: no match", net_mesh_claim_island(a, "{\"tags_all\":[\"gpu:none\"]}",
                                                                now_us() + 60000000ull, &found, &island),
                0);
    CU_CHECK_RC("claim_island: out_found 0 when nothing matches", found, 0);

    count = 0;
    for (waited = 0; waited < 15000; waited += 200) {
        if (match(b, "{\"tags_all\":[\"gpu:h100\"]}", ids, &count) == 0 && count >= 1) {
            break;
        }
        /* Not a named check: how many rounds convergence takes varies. */
        if (net_mesh_publish_island_topology(a, record, &republished) != 0) {
            CU_CHECK("A re-publishes while B converges", 0);
        }
        cu_sleep_ms(200);
    }
    CU_CHECK("B's scheduler matches A's island, learned from A's broadcast", count == 1 && ids[0] == ISLAND);

    CU_CHECK_RC("net_mesh_shutdown: A", net_mesh_shutdown(a), 0);
    CU_CHECK_RC("net_mesh_shutdown: B", net_mesh_shutdown(b), 0);
    net_mesh_free(a);
    net_mesh_free(b);
    return cu_finish();
}
