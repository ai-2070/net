/*
 * Net C SDK — Basic Example
 *
 * Build:
 *   cargo build --release --features ffi,net
 *   gcc -o basic basic.c -L ../target/release -lnet -lpthread -ldl -lm
 *
 * Run:
 *   LD_LIBRARY_PATH=../target/release ./basic    (Linux)
 *   DYLD_LIBRARY_PATH=../target/release ./basic   (macOS)
 *   PATH=../target/release;%PATH% basic.exe       (Windows — see below)
 *
 * What this proves, and what it does not: `net_init` with no adapter
 * configured selects the default memory adapter, which COUNTS events and
 * DISCARDS them. Every call below succeeds and the poll returns zero events
 * — by design, not by failure. The success condition this example can
 * actually assert is producer-side acceptance, via net_stats_ex. Configure a
 * real adapter before treating a non-empty poll as the thing that worked.
 */

#include "../include/net.h"
#include <stdio.h>
#include <string.h>

int main(void) {
    printf("Net %s\n\n", net_version());

    /* Create a node with 4 shards */
    net_handle_t node = net_init("{\"num_shards\": 4}");
    if (!node) {
        fprintf(stderr, "Failed to initialize\n");
        return 1;
    }
    printf("Node created with %d shards\n", net_num_shards(node));

    /* Ingest a single event with receipt */
    const char* event = "{\"token\": \"hello\", \"index\": 0}";
    net_receipt_t receipt;
    int rc = net_ingest_raw_ex(node, event, strlen(event), &receipt);
    if (rc == NET_SUCCESS) {
        printf("Ingested to shard %d at ts %llu\n",
            receipt.shard_id, (unsigned long long)receipt.timestamp);
    }

    /* Batch ingest */
    const char* events[] = {
        "{\"token\": \"world\", \"index\": 1}",
        "{\"token\": \"foo\",   \"index\": 2}",
        "{\"token\": \"bar\",   \"index\": 3}",
    };
    size_t lens[] = {
        strlen(events[0]),
        strlen(events[1]),
        strlen(events[2]),
    };
    int count = net_ingest_raw_batch(node, events, lens, 3);
    if (count < 0) {
        fprintf(stderr, "Batch ingest failed: %d\n", count);
    } else {
        printf("Batch ingested %d events\n", count);
    }

    /* Flush to ensure events are available for polling */
    rc = net_flush(node);
    if (rc != NET_SUCCESS) {
        fprintf(stderr, "Flush failed: %d\n", rc);
    }

    /* Poll with structured API (no JSON overhead).
     *
     * Expect ZERO. The default memory adapter counted the events above and
     * threw them away, so there is nothing to read back. Polling therefore
     * cannot be this example's success condition — see the stats check
     * below, which is the claim this program can actually make. */
    net_poll_result_t result;
    rc = net_poll_ex(node, 100, NULL, &result);
    if (rc == NET_SUCCESS) {
        printf("\nPolled %zu events (has_more=%d):\n", result.count, result.has_more);
        if (result.count == 0) {
            printf("  (none — the default memory adapter discards events;\n"
                   "   configure an adapter to read them back)\n");
        }
        for (size_t i = 0; i < result.count; i++) {
            printf("  [shard %d] %.*s\n",
                result.events[i].shard_id,
                (int)result.events[i].raw_len,
                result.events[i].raw);
        }
        net_free_poll_result(&result);
    }

    /* Stats (structured). This is the assertion: the producer boundary
     * accepted all 4 events. Fail loudly if it did not, rather than printing
     * a number nobody checks. */
    net_stats_t stats;
    net_stats_ex(node, &stats);
    printf("\nStats: ingested=%llu dropped=%llu batches=%llu\n",
        (unsigned long long)stats.events_ingested,
        (unsigned long long)stats.events_dropped,
        (unsigned long long)stats.batches_dispatched);
    if (stats.events_ingested != 4) {
        fprintf(stderr, "the bus did not accept every event: ingested=%llu, want 4\n",
            (unsigned long long)stats.events_ingested);
        net_shutdown(node);
        return 1;
    }

    /* Shutdown */
    net_shutdown(node);
    printf("Node shut down. %llu events accepted at the producer boundary;\n"
           "nothing consumed them — that needs an adapter.\n",
        (unsigned long long)stats.events_ingested);

    return 0;
}
