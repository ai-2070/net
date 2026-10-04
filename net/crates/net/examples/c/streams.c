/*
 * streams.c — per-peer mesh streams and the stream inbox, from C
 * (net.go.h, "Per-peer streams" and "Stream inbox").
 *
 * Two in-process nodes. B opens an inbox on a stream id derived from a
 * label; A opens a reliable stream to B on that id and sends. Checked
 * against the header:
 *
 *   - net_stream_id_from_label is deterministic, and labels differ;
 *   - one inbox per stream id per node (NET_ERR_MESH_STREAM_OCCUPIED);
 *   - a batch sent by A arrives at B in order, each event with A as its
 *     authenticated sender, each buffer released with net_free_bytes;
 *   - an event larger than this peer can carry is refused with
 *     NET_ERR_MESH_EVENT_TOO_LARGE, and only then are out_size / out_limit
 *     written (net_mesh_max_event_size is the one-packet bound);
 *   - net_mesh_stream_stats is JSON for an open stream and `null` for one
 *     that is not;
 *   - recv times out with 0, and a closed inbox returns 0;
 *   - net_mesh_close_stream releases the stream, and the same id reopens.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "net.go.h"

#include "consumer_util.h"
#include "loaded_module.h"

int main(void) {
    static const clm_fn_t used[] = {
        CU_MESH_FNS,
        CLM_FN(net_mesh_start),
        CLM_FN(net_mesh_shutdown),
        CLM_FN(net_mesh_free),
        CLM_FN(net_stream_id_from_label),
        CLM_FN(net_mesh_open_stream_inbox),
        CLM_FN(net_mesh_stream_inbox_recv),
        CLM_FN(net_mesh_open_stream),
        CLM_FN(net_mesh_send),
        CLM_FN(net_mesh_close_stream),
        CLM_FN(net_mesh_stream_stats),
        CLM_FN(net_mesh_max_event_size),
        CLM_FN(net_free_bytes),
    };
    static const char* bodies[3] = {"first", "second", "third"};
    net_meshnode_t *a = NULL, *b = NULL;
    net_mesh_stream_inbox_t *inbox = NULL, *second = NULL;
    net_mesh_stream_t* stream = NULL;
    char a_addr[32], b_addr[32];
    char* stats = NULL;
    size_t stats_len = 0, size = 7777, limit = 7777, max_event;
    uint64_t sid = 0, sid_again = 0, other = 0, a_id, b_id;
    const uint8_t* ptrs[3];
    size_t lens[3];
    uint8_t* big;
    int i, rc;

    if (clm_check_loaded_module(used, sizeof used / sizeof used[0]) != 0) {
        return 2;
    }
    if (cu_net_init() != 0) {
        printf("FAIL setup: cu_net_init\n");
        return 1;
    }
    CU_CHECK_RC("bring-up: A", cu_mesh_build(0xF1, &a, a_addr, sizeof a_addr), 0);
    CU_CHECK_RC("bring-up: B", cu_mesh_build(0xF2, &b, b_addr, sizeof b_addr), 0);
    CU_CHECK_RC("bring-up: handshake", cu_mesh_handshake(b, a, b_addr), 0);
    CU_CHECK_RC("bring-up: start A", net_mesh_start(a), 0);
    CU_CHECK_RC("bring-up: start B", net_mesh_start(b), 0);
    a_id = net_mesh_node_id(a);
    b_id = net_mesh_node_id(b);

    CU_CHECK_RC("net_stream_id_from_label", net_stream_id_from_label("store/c-streams", &sid), 0);
    CU_CHECK_RC("net_stream_id_from_label: again", net_stream_id_from_label("store/c-streams", &sid_again), 0);
    CU_CHECK_RC("net_stream_id_from_label: another label", net_stream_id_from_label("store/other", &other), 0);
    CU_CHECK("stream ids: deterministic, and distinct per label", sid == sid_again && sid != other);

    CU_CHECK_RC("net_mesh_open_stream_inbox: B", net_mesh_open_stream_inbox(b, sid, 8, &inbox), 0);
    CU_CHECK_RC("net_mesh_open_stream_inbox: a second receiver is NET_ERR_MESH_STREAM_OCCUPIED",
                net_mesh_open_stream_inbox(b, sid, 8, &second), NET_ERR_MESH_STREAM_OCCUPIED);

    stats = NULL;
    CU_CHECK_RC("net_mesh_stream_stats: before the stream opens",
                net_mesh_stream_stats(a, b_id, sid, &stats, &stats_len), 0);
    CU_CHECK("net_mesh_stream_stats: null for a stream that is not open", stats != NULL && strcmp(stats, "null") == 0);
    net_free_string(stats);

    CU_CHECK_RC("net_mesh_open_stream: A to B, reliable",
                net_mesh_open_stream(a, b_id, sid, "{\"reliability\":\"reliable\"}", &stream), 0);
    for (i = 0; i < 3; i++) {
        ptrs[i] = (const uint8_t*)bodies[i];
        lens[i] = strlen(bodies[i]);
    }
    rc = net_mesh_send(stream, ptrs, lens, 3, a, &size, &limit);
    CU_CHECK_RC("net_mesh_send: a batch of three", rc, 0);
    CU_CHECK("net_mesh_send: out_size / out_limit untouched on success", size == 7777 && limit == 7777);
    for (i = 0; i < 3; i++) {
        uint64_t from = 0;
        uint8_t* buf = NULL;
        size_t len = 0;
        char label[64];
        snprintf(label, sizeof label, "inbox recv %d: an event", i + 1);
        CU_CHECK_RC(label, net_mesh_stream_inbox_recv(inbox, 5000, &from, &buf, &len), 1);
        snprintf(label, sizeof label, "inbox recv %d: from A, in order", i + 1);
        CU_CHECK(label, from == a_id && len == strlen(bodies[i]) && memcmp(buf, bodies[i], len) == 0);
        net_free_bytes(buf, len);
    }

    stats = NULL;
    CU_CHECK_RC("net_mesh_stream_stats: an open stream", net_mesh_stream_stats(a, b_id, sid, &stats, &stats_len), 0);
    CU_CHECK("net_mesh_stream_stats: a JSON object", stats != NULL && stats[0] == '{' && stats_len == strlen(stats));
    net_free_string(stats);

    max_event = net_mesh_max_event_size();
    CU_CHECK("net_mesh_max_event_size: never 0", max_event > 0);
    big = (uint8_t*)calloc(max_event * 16, 1);
    CU_CHECK("a payload far over the limit", big != NULL);
    ptrs[0] = big;
    lens[0] = max_event * 16;
    rc = net_mesh_send(stream, ptrs, lens, 1, a, &size, &limit);
    CU_CHECK_RC("net_mesh_send: too large is NET_ERR_MESH_EVENT_TOO_LARGE", rc, NET_ERR_MESH_EVENT_TOO_LARGE);
    CU_CHECK("net_mesh_send: the refused size and the limit that applied",
             size == max_event * 16 && limit >= max_event && limit < size);
    free(big);

    {
        uint64_t from = 0;
        uint8_t* buf = NULL;
        size_t len = 0;
        CU_CHECK_RC("inbox recv: nothing more is a timeout (0)",
                    net_mesh_stream_inbox_recv(inbox, 200, &from, &buf, &len), 0);
        CU_CHECK("inbox: nothing dropped", net_mesh_stream_inbox_dropped(inbox) == 0);
        CU_CHECK_RC("net_mesh_stream_inbox_close", net_mesh_stream_inbox_close(inbox), 0);
        CU_CHECK_RC("inbox recv after close: 0", net_mesh_stream_inbox_recv(inbox, 200, &from, &buf, &len), 0);
    }
    net_mesh_stream_inbox_free(inbox);

    CU_CHECK_RC("net_mesh_close_stream", net_mesh_close_stream(stream), 0);
    stream = NULL;
    CU_CHECK_RC("net_mesh_open_stream: the same id reopens after close",
                net_mesh_open_stream(a, b_id, sid, NULL, &stream), 0);
    net_mesh_stream_free(stream);

    CU_CHECK_RC("net_mesh_shutdown: A", net_mesh_shutdown(a), 0);
    CU_CHECK_RC("net_mesh_shutdown: B", net_mesh_shutdown(b), 0);
    net_mesh_free(a);
    net_mesh_free(b);
    return cu_finish();
}
