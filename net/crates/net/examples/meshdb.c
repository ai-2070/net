/*
 * Net C SDK — MeshDB Example
 *
 * Walks the canonical query lifecycle: build an in-memory
 * reader, seed it with a chain of events, plan a query through
 * the factory AST, execute it on a runner, and drain the
 * resulting iterator. Exercises three operator families:
 *
 *   - Latest: atomic operator returning a single row whose
 *     payload is the raw event body.
 *   - Between + Count: composite pipeline emitting a postcard-
 *     encoded aggregate sentinel; decoded to JSON at the FFI
 *     boundary via `net_meshdb_decode_payload_json`.
 *   - LineageEmit: pre-walked entries form (the SDK does not
 *     walk the fork-of: graph itself).
 *
 * Build:
 *   cargo build --release -p net-meshdb-ffi
 *   gcc -o meshdb meshdb.c -L ../target/release -lnet_meshdb \
 *       -lpthread -ldl -lm
 *
 * Run:
 *   LD_LIBRARY_PATH=../target/release ./meshdb     (Linux)
 *   DYLD_LIBRARY_PATH=../target/release ./meshdb   (macOS)
 */

#include "../include/net_meshdb.h"
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int seed_chain(MeshDbReader* reader, uint64_t origin) {
    /* Three events on `origin`, seq 1..3. */
    const char* payloads[] = {"v1", "v2", "v3"};
    for (uint64_t s = 1; s <= 3; s++) {
        const char* p = payloads[s - 1];
        int rc = net_meshdb_reader_append(
            reader, origin, s, (const uint8_t*)p, strlen(p));
        if (rc != NET_MESHDB_OK) {
            fprintf(stderr, "reader_append failed: rc=%d\n", rc);
            return rc;
        }
    }
    return NET_MESHDB_OK;
}

static void drain_iter(MeshDbIter* iter, const char* label) {
    printf("[%s]\n", label);
    for (;;) {
        uint64_t origin = 0;
        uint64_t seq = 0;
        uint8_t* payload = NULL;
        size_t payload_len = 0;
        int rc = net_meshdb_iter_next(
            iter, &origin, &seq, &payload, &payload_len);
        if (rc == NET_MESHDB_END) {
            break;
        }
        if (rc != NET_MESHDB_OK) {
            fprintf(stderr, "  iter_next failed: rc=%d\n", rc);
            break;
        }
        /* Try the sentinel-envelope decoder first; on NULL the
         * payload is a plain event body (atomic operator). */
        char* json = net_meshdb_decode_payload_json(payload, payload_len);
        if (json) {
            printf("  origin=0x%" PRIx64 " seq=%" PRIu64 " sentinel=%s\n",
                   origin, seq, json);
            net_meshdb_free_string(json);
        } else {
            printf("  origin=0x%" PRIx64 " seq=%" PRIu64 " payload=\"%.*s\"\n",
                   origin, seq, (int)payload_len, (const char*)payload);
        }
        net_meshdb_payload_free(payload, payload_len);
    }
}

int main(void) {
    MeshDbReader* reader = net_meshdb_reader_new();
    if (!reader) {
        fprintf(stderr, "reader_new returned NULL\n");
        return 1;
    }

    if (seed_chain(reader, 0xAB) != NET_MESHDB_OK) {
        net_meshdb_reader_free(reader);
        return 1;
    }

    MeshDbRunner* runner = net_meshdb_runner_new(reader);
    if (!runner) {
        fprintf(stderr, "runner_new returned NULL\n");
        net_meshdb_reader_free(reader);
        return 1;
    }

    /* 1. Latest — emits the tip row. */
    {
        MeshDbQuery* q = net_meshdb_query_latest(0xAB);
        MeshDbIter* it = net_meshdb_runner_execute(runner, q);
        drain_iter(it, "latest(0xAB)");
        net_meshdb_iter_free(it);
        net_meshdb_query_free(q);
    }

    /* 2. Between + Count — composite pipeline. */
    {
        MeshDbQuery* between = net_meshdb_query_between(0xAB, 1, 4);
        MeshDbQuery* count = net_meshdb_query_count(between, NULL);
        MeshDbIter* it = net_meshdb_runner_execute(runner, count);
        drain_iter(it, "count(between(0xAB, 1, 4))");
        net_meshdb_iter_free(it);
        net_meshdb_query_free(count);
        net_meshdb_query_free(between);
    }

    /* 3. LineageEmit — pre-walked entries form. */
    {
        const char* entries =
            "[{\"origin\":170,\"depth\":0,\"tip_seq\":3},"
            " {\"origin\":187,\"depth\":1,\"tip_seq\":1},"
            " {\"origin\":204,\"depth\":2,\"tip_seq\":null}]";
        MeshDbQuery* q = net_meshdb_query_lineage_emit(0xAA, entries, "back");
        MeshDbIter* it = net_meshdb_runner_execute(runner, q);
        drain_iter(it, "lineage_emit(0xAA, 3 entries, back)");
        net_meshdb_iter_free(it);
        net_meshdb_query_free(q);
    }

    /* 4. Cached runner via execute_with — opt-in Phase F cache
     * with Permanent policy. The second call should be served
     * from the cache; the result is observably identical. */
    {
        MeshDbRunner* cached = net_meshdb_runner_new_cached(reader);
        if (!cached) {
            fprintf(stderr, "runner_new_cached returned NULL\n");
            net_meshdb_runner_free(runner);
            net_meshdb_reader_free(reader);
            return 1;
        }
        MeshDbQuery* q = net_meshdb_query_latest(0xAB);
        MeshDbIter* first = net_meshdb_runner_execute_with(
            cached, q, /*bypass_cache=*/0,
            NET_MESHDB_CACHE_PERMANENT, /*ttl=*/0.0);
        drain_iter(first, "cached latest(0xAB) — first call (miss)");
        net_meshdb_iter_free(first);
        MeshDbIter* second = net_meshdb_runner_execute_with(
            cached, q, /*bypass_cache=*/0,
            NET_MESHDB_CACHE_PERMANENT, /*ttl=*/0.0);
        drain_iter(second, "cached latest(0xAB) — second call (hit)");
        net_meshdb_iter_free(second);
        net_meshdb_query_free(q);
        net_meshdb_runner_free(cached);
    }

    net_meshdb_runner_free(runner);
    net_meshdb_reader_free(reader);
    return 0;
}
