/*
 * Net C SDK — Deck operator-side example.
 *
 * Walks the canonical operator workflow:
 *
 *   1. Construct a deck client owning a private supervisor runtime
 *      under a deterministic 32-byte operator seed.
 *   2. Read the latest MeshOsSnapshot as a JSON string.
 *   3. Read the rolled-up StatusSummary as a typed struct.
 *   4. Commit an `enter_maintenance` against a target node.
 *   5. Verify the commit's `event_kind` matches.
 *   6. Subscribe to the live snapshot stream + pull one.
 *   7. Tear down.
 *
 * Slice 1 caveat: this example uses the operator-only mode where
 * the cdylib owns its own supervisor runtime. Composing against
 * an externally-managed `NetMeshOsSdk` (from `libnet_meshos`)
 * lands in slice 2.
 *
 * Build:
 *   cargo build --release -p net-deck-ffi
 *   gcc -o deck deck.c -L ../target/release -lnet_deck \
 *       -lpthread -ldl -lm
 *
 * Run:
 *   LD_LIBRARY_PATH=../target/release ./deck     (Linux)
 *   DYLD_LIBRARY_PATH=../target/release ./deck   (macOS)
 */

#include "../include/net_deck.h"
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static const char* event_kind_str(int kind) {
    switch (kind) {
        case NET_DECK_EVENT_KIND_DRAIN:                return "drain";
        case NET_DECK_EVENT_KIND_ENTER_MAINTENANCE:    return "enter_maintenance";
        case NET_DECK_EVENT_KIND_EXIT_MAINTENANCE:     return "exit_maintenance";
        case NET_DECK_EVENT_KIND_CORDON:               return "cordon";
        case NET_DECK_EVENT_KIND_UNCORDON:             return "uncordon";
        case NET_DECK_EVENT_KIND_DROP_REPLICAS:        return "drop_replicas";
        case NET_DECK_EVENT_KIND_INVALIDATE_PLACEMENT: return "invalidate_placement";
        case NET_DECK_EVENT_KIND_RESTART_ALL_DAEMONS:  return "restart_all_daemons";
        case NET_DECK_EVENT_KIND_CLEAR_AVOID_LIST:     return "clear_avoid_list";
        default:                                       return "unknown";
    }
}

static void print_last_error(const char* context) {
    const char* kind = net_deck_last_error_kind();
    const char* msg = net_deck_last_error_message();
    fprintf(stderr, "[%s] kind=%s message=%s\n",
        context,
        kind ? kind : "(none)",
        msg ? msg : "(none)");
    net_deck_clear_last_error();
}

int main(void) {
    /* 1. Construct a deck client under a deterministic seed. */
    uint8_t seed[32];
    memset(seed, 0x42, sizeof(seed));
    NetDeckClient* client = NULL;
    int rc = net_deck_client_new(
        /* this_node                */ 0,
        /* tick_interval_ms         */ 0,
        /* event_queue_capacity     */ 0,
        /* action_queue_capacity    */ 0,
        /* snapshot_poll_interval_ms*/ 0,
        /* ice_signature_threshold  */ 0,
        seed,
        &client
    );
    if (rc != NET_DECK_OK || client == NULL) {
        print_last_error("client_new");
        return 1;
    }
    uint64_t op_id = net_deck_client_operator_id(client);
    printf("constructed deck client operator_id=%#" PRIx64 "\n", op_id);

    /* 2. Read the latest MeshOsSnapshot as JSON. */
    char* snap_json = net_deck_status(client);
    if (snap_json == NULL) {
        print_last_error("status");
    } else {
        size_t prefix = strlen(snap_json);
        if (prefix > 64) prefix = 64;
        printf("snapshot (first %zu chars): %.*s\n", prefix, (int)prefix, snap_json);
        net_deck_free_string(snap_json);
    }

    /* 3. Read the rolled-up StatusSummary. */
    NetDeckStatusSummary summary;
    memset(&summary, 0, sizeof(summary));
    rc = net_deck_status_summary(client, &summary);
    if (rc != NET_DECK_OK) {
        print_last_error("status_summary");
    } else {
        printf("status_summary: peers.healthy=%u daemons.running=%u replica_chains=%u local_maintenance=%d\n",
            summary.peers.healthy,
            summary.daemons.running,
            summary.replica_chains,
            summary.local_maintenance_active);
    }

    /* 4. Commit an enter_maintenance against a target node. */
    NetDeckChainCommit commit;
    memset(&commit, 0, sizeof(commit));
    rc = net_deck_admin_enter_maintenance(
        client,
        /* node          */ 0xABCD,
        /* drain_for_ms  */ 600000,
        /* has_drain_for */ 1,
        &commit
    );
    if (rc != NET_DECK_OK) {
        print_last_error("admin_enter_maintenance");
    } else {
        printf("enter_maintenance commit_id=%#" PRIx64 " operator_id=%#" PRIx64 " event_kind=%s committed_at_ms=%" PRIu64 "\n",
            commit.commit_id,
            commit.operator_id,
            event_kind_str(commit.event_kind),
            commit.committed_at_ms);
    }

    /* 5. Subscribe to the snapshot stream + pull one. */
    NetDeckSnapshotStream* stream = NULL;
    rc = net_deck_subscribe_snapshots(client, &stream);
    if (rc != NET_DECK_OK || stream == NULL) {
        print_last_error("subscribe_snapshots");
    } else {
        char* live_snap = NULL;
        rc = net_deck_snapshot_stream_next(stream, 500, &live_snap);
        if (rc != NET_DECK_OK) {
            print_last_error("snapshot_stream_next");
        } else if (live_snap == NULL) {
            printf("snapshot_stream_next: timeout (no snapshot in 500ms)\n");
        } else {
            printf("got live snapshot (%zu bytes)\n", strlen(live_snap));
            net_deck_free_string(live_snap);
        }
        net_deck_snapshot_stream_free(stream);
    }

    /* 6. Tear down. */
    net_deck_client_free(client);
    printf("deck client freed\n");

    return 0;
}
