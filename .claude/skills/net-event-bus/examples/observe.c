/*
 * Observe a drop (C).
 *
 * Build: gcc observe.c -lnet -lpthread -ldl -lm && ./a.out
 *
 * What it proves: net_stats_ex reports drops, and ingestion returns success
 * while they happen.
 *
 * WHY THIS EXISTS. Backpressure is silent by default — net_ingest_raw returns
 * 0 while events are being dropped. The only evidence is events_dropped.
 *
 * NOTE ON HEADERS. This uses net.h, the event-bus header. It cannot be combined
 * in one translation unit with net.go.h (mesh, capabilities, channels), because
 * both use the NET_SDK_H guard and net.go.h is not a superset. If you need both
 * surfaces, split across two translation units.
 */

#include "net.h"
#include <stdio.h>
#include <string.h>

int main(void) {
    net_handle_t node = net_init("{\"num_shards\": 1}");
    if (!node) {
        fprintf(stderr, "net_init failed\n");
        return 1;
    }

    const char *json = "{\"seq\":1}";
    const size_t len = strlen(json);

    /* Push far more than the buffer holds, with nothing polling. */
    for (int i = 0; i < 1000; i++) {
        /* Returns 0 even while events are being dropped. */
        if (net_ingest_raw(node, json, len) < 0) {
            fprintf(stderr, "ingest failed at %d\n", i);
            net_shutdown(node);
            return 1;
        }
    }

    /* The struct form. net_stats(handle, buf, len) gives the same numbers as
     * JSON if you would rather parse text. */
    net_stats_t stats;
    if (net_stats_ex(node, &stats) < 0) {
        fprintf(stderr, "net_stats_ex failed\n");
        net_shutdown(node);
        return 1;
    }

    printf("ingested=%llu dropped=%llu batches=%llu\n",
           (unsigned long long)stats.events_ingested,
           (unsigned long long)stats.events_dropped,
           (unsigned long long)stats.batches_dispatched);

    if (stats.events_dropped > 0) {
        printf("drops happened and ingest returned success — alert on this counter\n");
    }

    net_shutdown(node);
    return 0;
}
