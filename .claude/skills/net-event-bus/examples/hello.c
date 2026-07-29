/*
 * Minimal sanity check for the C ABI.
 *
 * Build: gcc hello.c -lnet -lpthread -ldl -lm && ./a.out
 *
 * What it proves: the header and library link, a node starts, a raw JSON event
 * is accepted, and shutdown is clean.
 *
 * WHAT IT DELIBERATELY DOES NOT DO: read the event back.
 *
 * The default transport is memory, which selects the Noop adapter. Events flow
 * producer -> ring buffer -> drain worker -> adapter, and the Noop adapter
 * counts batches and discards them (adapter/noop.rs: "Just count, don't store";
 * its poll_shard returns an empty result). So on memory transport net_poll_ex
 * always returns zero events — a round-trip example here would spin forever.
 *
 * To actually receive events you need an adapter that retains them: Redis or
 * JetStream, or the mesh transport between two nodes. See mesh.md.
 *
 * NOTE ON HEADERS: this uses net.h, the event-bus header. It cannot be combined
 * in one translation unit with net.go.h (mesh, capabilities, channels) — both
 * use the NET_SDK_H guard and net.go.h is not a superset.
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

    const char *json = "{\"msg\":\"hello, mesh\"}";
    if (net_ingest_raw(node, json, strlen(json)) < 0) {
        fprintf(stderr, "ingest failed\n");
        net_shutdown(node);
        return 1;
    }

    /* events_ingested counts at the *producer* boundary — it says the bus
     * accepted the event, not that anything received or stored it. */
    net_stats_t stats;
    if (net_stats_ex(node, &stats) < 0) {
        fprintf(stderr, "net_stats_ex failed\n");
        net_shutdown(node);
        return 1;
    }
    if (stats.events_ingested != 1) {
        fprintf(stderr, "the bus did not accept the event\n");
        net_shutdown(node);
        return 1;
    }

    printf("accepted: ingested=%llu dropped=%llu\n",
           (unsigned long long)stats.events_ingested,
           (unsigned long long)stats.events_dropped);

    net_shutdown(node);
    return 0;
}
