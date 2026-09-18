/*
 * The event log you no longer run (C).
 *
 * One node, one local append-only log. Eight records are appended, replayed
 * from the start, then replayed again from a consumer checkpoint — and the
 * second replay is three records, not eight, because the offset is the
 * consumer's own bookkeeping and the log is not asked to remember it.
 *
 * Kafka needs a cluster for this. `RedexFile` is one file with a monotonic
 * sequence, and the consumer's position lives in the consumer.
 *
 * Mirrors examples/eventlog.rs.
 *
 * Build: gcc eventlog.c -lnet -lpthread -ldl -lm && ./a.out
 *
 * Expected final line: RESULT ok records=8 replayed=8 resumed=3
 */

#include "net.go.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* Records appended to the log. */
#define RECORDS 8
/* Where the consumer left off, in sequence numbers. */
#define CHECKPOINT 5

/*
 * Collect the `"seq":<n>` values of a `read_range` JSON array, in order.
 * Writes up to `cap` of them into `out` (may be NULL / cap 0 to count only)
 * and returns the total number present.
 */
static int seq_values(const char *json, uint64_t *out, int cap) {
    int n = 0;
    const char *needle = "\"seq\":";
    const char *p = json;
    while ((p = strstr(p, needle)) != NULL) {
        p += strlen(needle);
        uint64_t seq = strtoull(p, NULL, 10);
        if (n < cap) out[n] = seq;
        n++;
    }
    return n;
}

int main(void) {
    net_redex_t *redex = net_redex_new(NULL);
    if (!redex) {
        fprintf(stderr, "redex init failed\n");
        return 1;
    }

    net_redex_file_t *log = NULL;
    if (net_redex_open_file(redex, "audit/events", NULL, &log) != 0 || !log) {
        fprintf(stderr, "open_file failed\n");
        return 1;
    }

    for (int index = 0; index < RECORDS; index++) {
        char record[32];
        int n = snprintf(record, sizeof record, "event:%d", index);
        uint64_t seq = 0;
        if (net_redex_file_append(log, (const uint8_t *)record, (size_t)n,
                                  &seq) != 0) {
            fprintf(stderr, "append failed\n");
            return 1;
        }
        printf("appended event %d at seq %llu\n", index,
               (unsigned long long)seq);
    }

    uint64_t total = net_redex_file_len(log);

    /* Full replay. Every record, in append order — the log does not need a
     * broker to hand them back. */
    char *all_json = NULL;
    size_t all_len = 0;
    if (net_redex_file_read_range(log, 0, total, &all_json, &all_len) != 0) {
        fprintf(stderr, "read_range(0, len) failed\n");
        return 1;
    }

    /* The consumer's offset is application state: it stores the sequence it
     * last processed and asks for everything after it. Nothing is committed
     * on the log's side, which is why a second consumer with a different
     * checkpoint costs the log nothing. */
    char *resumed_json = NULL;
    size_t resumed_len = 0;
    if (net_redex_file_read_range(log, CHECKPOINT, total, &resumed_json,
                                  &resumed_len) != 0) {
        fprintf(stderr, "read_range(checkpoint, len) failed\n");
        return 1;
    }

    /* Replay is a read, not a transformation: the same range yields the same
     * sequence, so a fold over it is reproducible. */
    char *again_json = NULL;
    size_t again_len = 0;
    if (net_redex_file_read_range(log, 0, total, &again_json, &again_len) != 0) {
        fprintf(stderr, "read_range(0, len) again failed\n");
        return 1;
    }

    uint64_t all_seq[64];
    uint64_t again_seq[64];
    int all_n = seq_values(all_json, all_seq, 64);
    int again_n = seq_values(again_json, again_seq, 64);
    int resumed_n = seq_values(resumed_json, NULL, 0);

    int stable = (all_n == again_n);
    for (int i = 0; stable && i < all_n; i++) {
        if (all_seq[i] != again_seq[i]) stable = 0;
    }

    printf("records in the log: %llu\n", (unsigned long long)total);
    printf("replayed from 0:    %d\n", all_n);
    printf("resumed from %d:     %d\n", CHECKPOINT, resumed_n);
    printf("replay is stable:   %s\n", stable ? "true" : "false");

    printf("RESULT ok records=%llu replayed=%d resumed=%d\n",
           (unsigned long long)total, all_n, resumed_n);

    net_free_string(all_json);
    net_free_string(resumed_json);
    net_free_string(again_json);
    net_redex_file_close(log);
    net_redex_file_free(log);
    net_redex_free(redex);
    return 0;
}
