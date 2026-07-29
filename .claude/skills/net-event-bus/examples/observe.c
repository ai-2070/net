/*
 * What the drop counters actually tell you (C).
 *
 * Build: gcc observe.c -lnet -lpthread -ldl -lm && ./a.out
 *
 * THE POINT OF THIS FILE, AND IT IS NOT WHAT YOU EXPECT.
 *
 * The usual advice — "backpressure is silent, so watch events_dropped" — is
 * wrong for the one mode where it matters most, and that mode is the default.
 *
 * Both counters sit at the producer boundary: they record what the bus accepted
 * or refused from you, not what survived to an adapter.
 *
 *   - DropOldest (the default) evicts to make room, so net_ingest_raw always
 *     succeeds and events_dropped never moves. Events are lost and nothing
 *     in-process says so.
 *   - DropNewest and FailProducer refuse the producer instead, so the ingest
 *     call and events_dropped both see it.
 *
 * The exact counts vary between runs and between bindings — the drain worker
 * races the producer. The shape is what matters, not the numbers.
 *
 * NOTE ON HEADERS: this uses net.h, the event-bus header. It cannot be combined
 * in one translation unit with net.go.h (mesh, capabilities, channels) — both
 * use the NET_SDK_H guard and net.go.h is not a superset.
 */

#include "net.h"
#include <stdio.h>
#include <string.h>

static int measure(const char *mode, const char *config) {
    net_handle_t node = net_init(config);
    if (!node) {
        fprintf(stderr, "net_init failed for %s\n", mode);
        return 1;
    }

    const char *json = "{\"seq\":1}";
    const size_t len = strlen(json);
    long refused = 0;
    for (int i = 0; i < 20000; i++) {
        if (net_ingest_raw(node, json, len) < 0) refused++;
    }

    net_stats_t stats;
    if (net_stats_ex(node, &stats) < 0) {
        fprintf(stderr, "net_stats_ex failed\n");
        net_shutdown(node);
        return 1;
    }

    printf("%-14s ingested=%-6llu dropped=%-6llu batches=%-6llu ingest_errors=%ld\n",
           mode,
           (unsigned long long)stats.events_ingested,
           (unsigned long long)stats.events_dropped,
           (unsigned long long)stats.batches_dispatched,
           refused);

    net_shutdown(node);
    return 0;
}

int main(void) {
    printf("20,000 events into a 1024-slot buffer, nothing consuming:\n\n");

    /* ring_buffer_capacity must be a power of two and >= 1024. Either casing
     * of backpressure_mode parses; an unrecognised value is rejected. */
    if (measure("DropOldest",
                "{\"num_shards\":1,\"ring_buffer_capacity\":1024,"
                "\"backpressure_mode\":\"DropOldest\"}")) return 1;
    if (measure("DropNewest",
                "{\"num_shards\":1,\"ring_buffer_capacity\":1024,"
                "\"backpressure_mode\":\"DropNewest\"}")) return 1;
    if (measure("FailProducer",
                "{\"num_shards\":1,\"ring_buffer_capacity\":1024,"
                "\"backpressure_mode\":\"FailProducer\"}")) return 1;

    printf("\nDropOldest loses events silently — and it is the default.\n");
    printf("The other two refuse the producer, so ingest and the counter agree.\n");
    return 0;
}
