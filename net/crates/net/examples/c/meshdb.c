/*
 * meshdb.c — the MeshDB query layer, from C (net_meshdb.h).
 *
 * Builds an in-memory reader with two chains, plans queries through the
 * factory functions, executes them on a runner and drains every iterator,
 * checking each row against what was appended. Covers what the header
 * documents:
 *
 *   - the reader: append, and its refusals (NULL reader, NULL payload
 *     with a length);
 *   - the atomic operators At / Between / Latest / LineageEmit, whose rows
 *     carry the raw event bytes (net_meshdb_decode_payload_json is NULL
 *     for them);
 *   - the composite operators Count / Sum / Avg / Min / Max / Percentile /
 *     Window / Join / Filter, whose rows carry a sentinel envelope that
 *     the decoder renders as JSON;
 *   - each factory's documented NULL (start >= end, size 0, an unknown
 *     kind, direction or strategy, p outside [0, 1], unparseable JSON);
 *   - the iterator's END and NET_MESHDB_INVALID_ARG, and the runner's NULL
 *     for NULL arguments;
 *   - the cached runner (two Permanent-policy executions, same rows);
 *   - the reader-free pattern the header calls "snapshot-then-free".
 *
 * Every payload is freed with net_meshdb_payload_free and its length,
 * every decoded string with net_meshdb_free_string.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "net_meshdb.h"

#include "consumer_util.h"
#include "loaded_module.h"

#define CHAIN_A 0xABu
#define CHAIN_B 0xCDu
#define MAX_ROWS 16

typedef struct {
    uint64_t origin, seq;
    char payload[64];
    size_t payload_len;
    char json[512]; /* the decoded sentinel, or "" for a raw row */
} row_t;

/* Drain `it` into rows (freeing every buffer). Returns the row count, or
 * -1 on a non-END failure. Frees the iterator. */
static int drain(MeshDbIter* it, row_t* rows) {
    int n = 0;
    if (!it) {
        return -1;
    }
    for (;;) {
        uint64_t origin = 0, seq = 0;
        uint8_t* payload = NULL;
        size_t len = 0;
        char* json;
        int rc = net_meshdb_iter_next(it, &origin, &seq, &payload, &len);
        if (rc == NET_MESHDB_END) {
            break;
        }
        if (rc != NET_MESHDB_OK || n >= MAX_ROWS) {
            net_meshdb_payload_free(payload, len);
            net_meshdb_iter_free(it);
            return -1;
        }
        rows[n].origin = origin;
        rows[n].seq = seq;
        rows[n].payload_len = len;
        memset(rows[n].payload, 0, sizeof rows[n].payload);
        memcpy(rows[n].payload, payload, len < sizeof rows[n].payload - 1 ? len : sizeof rows[n].payload - 1);
        json = net_meshdb_decode_payload_json(payload, len);
        snprintf(rows[n].json, sizeof rows[n].json, "%s", json ? json : "");
        net_meshdb_free_string(json);
        net_meshdb_payload_free(payload, len);
        n++;
    }
    net_meshdb_iter_free(it);
    return n;
}

static int run(MeshDbRunner* runner, MeshDbQuery* q, row_t* rows) {
    int n = q ? drain(net_meshdb_runner_execute(runner, q), rows) : -1;
    net_meshdb_query_free(q);
    return n;
}

static int append(MeshDbReader* r, uint64_t origin, uint64_t seq, const char* body) {
    return net_meshdb_reader_append(r, origin, seq, (const uint8_t*)body, strlen(body));
}

static int raw_rows(MeshDbRunner* runner) {
    row_t rows[MAX_ROWS];
    int n;
    static const char* entries =
        "[{\"origin\":170,\"depth\":0,\"tip_seq\":3},{\"origin\":187,\"depth\":1,\"tip_seq\":1},"
        "{\"origin\":204,\"depth\":2,\"tip_seq\":null}]";

    n = run(runner, net_meshdb_query_at(CHAIN_A, 2), rows);
    CU_CHECK_RC("At(A, 2): one row", n, 1);
    CU_CHECK("At(A, 2): origin, seq and the appended bytes",
             rows[0].origin == CHAIN_A && rows[0].seq == 2 && strcmp(rows[0].payload, "{\"v\":20}") == 0);
    CU_CHECK("At: an atomic row is not a sentinel (the decoder returns NULL)", rows[0].json[0] == '\0');

    n = run(runner, net_meshdb_query_between(CHAIN_A, 1, 4), rows);
    CU_CHECK_RC("Between(A, 1, 4): three rows", n, 3);
    CU_CHECK("Between: in seq order, with their bytes",
             rows[0].seq == 1 && rows[1].seq == 2 && rows[2].seq == 3 &&
                 strcmp(rows[2].payload, "{\"v\":30}") == 0);
    n = run(runner, net_meshdb_query_between(CHAIN_A, 2, 3), rows);
    CU_CHECK("Between is half-open: [2, 3) is seq 2 alone", n == 1 && rows[0].seq == 2);
    CU_CHECK("Between: start >= end is NULL", net_meshdb_query_between(CHAIN_A, 3, 3) == NULL);

    n = run(runner, net_meshdb_query_latest(CHAIN_A), rows);
    CU_CHECK("Latest(A): the tip, seq 3", n == 1 && rows[0].seq == 3 && strcmp(rows[0].payload, "{\"v\":30}") == 0);
    n = run(runner, net_meshdb_query_latest(0x99), rows);
    CU_CHECK_RC("Latest of an unknown chain: no rows", n, 0);

    n = run(runner, net_meshdb_query_lineage_emit(0xAA, entries, "back"), rows);
    CU_CHECK_RC("LineageEmit: one row per entry", n, 3);
    CU_CHECK("LineageEmit: origin = entry.origin, seq = tip_seq or 0, empty payload",
             rows[0].origin == 170 && rows[0].seq == 3 && rows[1].origin == 187 && rows[1].seq == 1 &&
                 rows[2].origin == 204 && rows[2].seq == 0 && rows[0].payload_len == 0);
    CU_CHECK("LineageEmit: an unknown direction is NULL",
             net_meshdb_query_lineage_emit(0xAA, entries, "sideways") == NULL);
    CU_CHECK("LineageEmit: unparseable entries are NULL",
             net_meshdb_query_lineage_emit(0xAA, "[{", "back") == NULL);
    return 0;
}

/* A one-row aggregate over Between(A, 1, 4), decoded. */
static int aggregate(MeshDbRunner* runner, MeshDbQuery* agg, char* json, size_t json_len) {
    row_t rows[MAX_ROWS];
    int n = run(runner, agg, rows);
    if (n != 1) {
        return -1;
    }
    snprintf(json, json_len, "%s", rows[0].json);
    return 0;
}

static int composites(MeshDbRunner* runner) {
    row_t rows[MAX_ROWS];
    char json[512];
    int n;
    MeshDbQuery* a = net_meshdb_query_between(CHAIN_A, 1, 4);
    MeshDbQuery* b = net_meshdb_query_between(CHAIN_B, 1, 3);
    CU_CHECK("Between(A) and Between(B) for the composites", a != NULL && b != NULL);

    CU_CHECK_RC("Count(Between(A))", aggregate(runner, net_meshdb_query_count(a, NULL), json, sizeof json), 0);
    CU_CHECK("Count: an aggregate sentinel of kind count",
             strstr(json, "\"kind\":\"aggregate\"") && strstr(json, "\"kind\":\"count\""));
    CU_CHECK("Count: 3", strstr(json, "\"value\":3") != NULL);

    CU_CHECK_RC("Sum(seq)", aggregate(runner, net_meshdb_query_numeric_agg(a, "sum", "seq", NULL), json, sizeof json), 0);
    CU_CHECK("Sum(seq): 1 + 2 + 3 = 6", strstr(json, "\"kind\":\"sum\"") && strstr(json, "\"value\":6"));
    CU_CHECK_RC("Min(seq)", aggregate(runner, net_meshdb_query_numeric_agg(a, "min", "seq", NULL), json, sizeof json), 0);
    CU_CHECK("Min(seq): 1", strstr(json, "\"kind\":\"min\"") && strstr(json, "\"value\":1"));
    CU_CHECK_RC("Max(seq)", aggregate(runner, net_meshdb_query_numeric_agg(a, "max", "seq", NULL), json, sizeof json), 0);
    CU_CHECK("Max(seq): 3", strstr(json, "\"kind\":\"max\"") && strstr(json, "\"value\":3"));
    CU_CHECK_RC("Avg(seq)", aggregate(runner, net_meshdb_query_numeric_agg(a, "avg", "seq", NULL), json, sizeof json), 0);
    CU_CHECK("Avg(seq): 2", strstr(json, "\"kind\":\"avg\"") && strstr(json, "\"value\":2"));
    CU_CHECK_RC("Sum(v), a JSON payload path",
                aggregate(runner, net_meshdb_query_numeric_agg(a, "sum", "v", NULL), json, sizeof json), 0);
    CU_CHECK("Sum(v): 10 + 20 + 30 = 60", strstr(json, "\"value\":60") != NULL);
    CU_CHECK("numeric_agg: an unknown kind is NULL", net_meshdb_query_numeric_agg(a, "median", "seq", NULL) == NULL);
    CU_CHECK("count: an unknown group_by is NULL", net_meshdb_query_count(a, "colour") == NULL);

    CU_CHECK_RC("Percentile(seq, 1.0)",
                aggregate(runner, net_meshdb_query_percentile(a, "seq", 1.0, NULL), json, sizeof json), 0);
    CU_CHECK("Percentile(seq, 1.0): the maximum, 3",
             strstr(json, "\"kind\":\"percentile\"") && strstr(json, "\"value\":3"));
    CU_CHECK("Percentile: p > 1 is NULL", net_meshdb_query_percentile(a, "seq", 1.5, NULL) == NULL);
    CU_CHECK("Percentile: p < 0 is NULL", net_meshdb_query_percentile(a, "seq", -0.1, NULL) == NULL);

    /* Seqs 1..3 in tumbling buckets of 2: [0, 2) holds seq 1, [2, 4) holds
     * 2 and 3. The first is the payload that once decoded as an aggregate
     * (the decoder took any type that parsed a prefix). */
    n = run(runner, net_meshdb_query_window(a, 2), rows);
    CU_CHECK_RC("Window(Between(A), 2): two buckets", n, 2);
    CU_CHECK("Window: [0, 2) is a window sentinel",
             strstr(rows[0].json, "{\"kind\":\"window\",\"start\":0,\"end\":2,") == rows[0].json);
    CU_CHECK("Window: [2, 4) is a window sentinel",
             strstr(rows[1].json, "{\"kind\":\"window\",\"start\":2,\"end\":4,") == rows[1].json);
    CU_CHECK("Window: size 0 is NULL", net_meshdb_query_window(a, 0) == NULL);

    n = run(runner, net_meshdb_query_filter_json(a, "{\"kind\":\"numeric_at_least\",\"field\":\"seq\",\"threshold\":2}"),
            rows);
    CU_CHECK("Filter(seq >= 2): seq 2 and 3", n == 2 && rows[0].seq == 2 && rows[1].seq == 3);
    n = run(runner, net_meshdb_query_filter_json(a, "{\"kind\":\"not\",\"child\":{\"kind\":\"exists\",\"field\":\"seq\"}}"),
            rows);
    CU_CHECK_RC("Filter(not exists seq): no rows", n, 0);
    CU_CHECK("Filter: unparseable JSON is NULL", net_meshdb_query_filter_json(a, "{\"kind\":") == NULL);

    n = run(runner, net_meshdb_query_join(a, b, "inner", "seq", NULL, 5.0), rows);
    CU_CHECK_RC("Join(A, B, inner, seq): the two seqs both chains have", n, 2);
    CU_CHECK("Join: each row is a joined sentinel with both sides",
             strstr(rows[0].json, "\"kind\":\"joined\"") && !strstr(rows[0].json, "\"left\":null") &&
                 !strstr(rows[0].json, "\"right\":null"));
    n = run(runner, net_meshdb_query_join(a, b, "left_outer", "seq", "sort_merge", 5.0), rows);
    CU_CHECK_RC("Join(A, B, left_outer, seq, sort_merge): every left row", n, 3);
    CU_CHECK("Join: an unknown kind is NULL", net_meshdb_query_join(a, b, "cross", "seq", NULL, 5.0) == NULL);
    CU_CHECK("Join: an unknown strategy is NULL",
             net_meshdb_query_join(a, b, "inner", "seq", "nested_loop", 5.0) == NULL);

    net_meshdb_query_free(a);
    net_meshdb_query_free(b);
    return 0;
}

static int iterator_and_runner(MeshDbRunner* runner, const MeshDbReader* reader) {
    uint64_t origin = 0, seq = 0;
    uint8_t* payload = NULL;
    size_t len = 0;
    row_t first[MAX_ROWS], second[MAX_ROWS];
    int n1, n2;
    MeshDbQuery* q = net_meshdb_query_latest(CHAIN_A);
    MeshDbIter* it = net_meshdb_runner_execute(runner, q);
    MeshDbRunner* cached;

    CU_CHECK("execute: an iterator", it != NULL);
    CU_CHECK_RC("iter_next: NULL out-pointer is NET_MESHDB_INVALID_ARG",
                net_meshdb_iter_next(it, NULL, &seq, &payload, &len), NET_MESHDB_INVALID_ARG);
    CU_CHECK_RC("iter_next: the row", net_meshdb_iter_next(it, &origin, &seq, &payload, &len), NET_MESHDB_OK);
    net_meshdb_payload_free(payload, len);
    CU_CHECK_RC("iter_next: then END", net_meshdb_iter_next(it, &origin, &seq, &payload, &len), NET_MESHDB_END);
    CU_CHECK_RC("iter_next: END again", net_meshdb_iter_next(it, &origin, &seq, &payload, &len), NET_MESHDB_END);
    net_meshdb_iter_free(it);
    CU_CHECK_RC("iter_next: NULL iterator is NET_MESHDB_INVALID_ARG",
                net_meshdb_iter_next(NULL, &origin, &seq, &payload, &len), NET_MESHDB_INVALID_ARG);

    CU_CHECK("execute: NULL query is NULL", net_meshdb_runner_execute(runner, NULL) == NULL);
    CU_CHECK("execute: NULL runner is NULL", net_meshdb_runner_execute(NULL, q) == NULL);
    CU_CHECK("runner_new: NULL reader is NULL", net_meshdb_runner_new(NULL) == NULL);

    cached = net_meshdb_runner_new_cached(reader);
    CU_CHECK("runner_new_cached", cached != NULL);
    n1 = drain(net_meshdb_runner_execute_with(cached, q, 0, NET_MESHDB_CACHE_PERMANENT, 0.0), first);
    n2 = drain(net_meshdb_runner_execute_with(cached, q, 0, NET_MESHDB_CACHE_PERMANENT, 0.0), second);
    CU_CHECK("execute_with, Permanent, twice: the same row",
             n1 == 1 && n2 == 1 && first[0].seq == second[0].seq && strcmp(first[0].payload, second[0].payload) == 0);
    n1 = drain(net_meshdb_runner_execute_with(cached, q, 1, NET_MESHDB_CACHE_TIME_BOUND, 5.0), first);
    CU_CHECK("execute_with, bypassing the cache: the same row", n1 == 1 && first[0].seq == 3);
    net_meshdb_runner_free(cached);
    net_meshdb_query_free(q);
    return 0;
}

int main(void) {
    static const clm_fn_t used[] = {
        CLM_FN(net_meshdb_reader_new),        CLM_FN(net_meshdb_reader_append),
        CLM_FN(net_meshdb_reader_free),       CLM_FN(net_meshdb_query_at),
        CLM_FN(net_meshdb_query_between),     CLM_FN(net_meshdb_query_latest),
        CLM_FN(net_meshdb_query_lineage_emit), CLM_FN(net_meshdb_query_count),
        CLM_FN(net_meshdb_query_join),        CLM_FN(net_meshdb_runner_new),
        CLM_FN(net_meshdb_runner_execute),    CLM_FN(net_meshdb_iter_next),
        CLM_FN(net_meshdb_iter_free),         CLM_FN(net_meshdb_payload_free),
        CLM_FN(net_meshdb_decode_payload_json), CLM_FN(net_meshdb_free_string),
    };
    MeshDbReader* reader;
    MeshDbRunner* runner;
    row_t rows[MAX_ROWS];
    int n;

    if (clm_check_loaded_module(used, sizeof used / sizeof used[0]) != 0) {
        return 2;
    }
    reader = net_meshdb_reader_new();
    CU_CHECK("reader_new", reader != NULL);
    CU_CHECK_RC("append A/1", append(reader, CHAIN_A, 1, "{\"v\":10}"), NET_MESHDB_OK);
    CU_CHECK_RC("append A/2", append(reader, CHAIN_A, 2, "{\"v\":20}"), NET_MESHDB_OK);
    CU_CHECK_RC("append A/3", append(reader, CHAIN_A, 3, "{\"v\":30}"), NET_MESHDB_OK);
    CU_CHECK_RC("append B/1", append(reader, CHAIN_B, 1, "{\"v\":1}"), NET_MESHDB_OK);
    CU_CHECK_RC("append B/2", append(reader, CHAIN_B, 2, "{\"v\":2}"), NET_MESHDB_OK);
    CU_CHECK_RC("append: an empty payload may be NULL",
                net_meshdb_reader_append(reader, 0x77, 1, NULL, 0), NET_MESHDB_OK);
    CU_CHECK_RC("append: NULL payload with a length is NET_MESHDB_INVALID_ARG",
                net_meshdb_reader_append(reader, 0x77, 2, NULL, 4), NET_MESHDB_INVALID_ARG);
    CU_CHECK_RC("append: NULL reader is NET_MESHDB_INVALID_ARG",
                net_meshdb_reader_append(NULL, 0x77, 2, (const uint8_t*)"x", 1), NET_MESHDB_INVALID_ARG);

    runner = net_meshdb_runner_new(reader);
    CU_CHECK("runner_new", runner != NULL);
    if (raw_rows(runner) || composites(runner) || iterator_and_runner(runner, reader)) {
        return 1;
    }

    /* "snapshot-then-free": the runner holds its own store. */
    net_meshdb_reader_free(reader);
    n = run(runner, net_meshdb_query_latest(CHAIN_B), rows);
    CU_CHECK("after reader_free: the runner still answers", n == 1 && rows[0].seq == 2);
    net_meshdb_runner_free(runner);
    net_meshdb_reader_free(NULL);
    net_meshdb_runner_free(NULL);
    net_meshdb_query_free(NULL);
    net_meshdb_iter_free(NULL);
    net_meshdb_payload_free(NULL, 0);
    net_meshdb_free_string(NULL);
    CU_CHECK("every free accepts NULL", 1);
    return cu_finish();
}
