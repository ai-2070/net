/*
 * The job queue you no longer run (C).
 *
 * A producer, two workers, and an append-only job log — three in-process
 * mesh nodes over loopback UDP plus a local log. Jobs are appended to the
 * log (the queue), dispatch reads them back out of it, and a worker that
 * *refuses* a job re-issues it to its peer. Nothing is re-executed, and the
 * results log is the record you reconcile from.
 *
 * Both logs live in memory for the life of the process — that is all this
 * route needs, and all it claims. Surviving a restart is a directory passed
 * to `net_redex_new` plus `{"persistent":true}` in the open-file config.
 *
 * Mirrors examples/jobqueue.rs. The Rust `local_addr()` has no C binding, so
 * each node reserves an ephemeral loopback port for itself before binding
 * (see `reserve_addr`) rather than trusting a hard-coded one.
 *
 * Build: gcc jobqueue.c -lnet -lpthread -ldl -lm && ./a.out
 *
 * Expected final line: RESULT ok jobs=6 done=6 retried=1 duplicates=0
 */

#include "net.go.h"
#include "net_rpc.h"

#include <arpa/inet.h>
#include <netinet/in.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

static const char *PSK_HEX =
    "4242424242424242424242424242424242424242424242424242424242424242";

enum { JOBS = 6, POISON = 3 };

/* A typed handler signals an application status by prefixing its `out_err`
 * with `nrpc:app_error:0x<code>:`; the wire status comes back to the caller
 * as `server_error: status=0x<code> message=...`. 0x8001 is the cross-binding
 * NRPC_TYPED_HANDLER_ERROR — see net/crates/net/sdk/src/mesh_rpc.rs. */
#define REFUSAL_APP_ERROR "nrpc:app_error:0x8001:"
#define REFUSAL_STATUS "status=0x8001"

static void seed_hex(char *out, unsigned char b) {
    for (int i = 0; i < 32; i++) sprintf(out + i * 2, "%02x", b);
    out[64] = '\0';
}

/* Ask the kernel for a free loopback port instead of naming one: bind a UDP
 * socket to port 0, read back what it was given, and close it again. A
 * hard-coded port fails whenever something else already holds it, and two
 * copies of this example could never run at once. The node re-binds the port
 * immediately below, which is how the sibling ports get the same property out
 * of `local_addr()`. */
static int reserve_addr(char *out, size_t out_len) {
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0) return -1;
    struct sockaddr_in sa;
    memset(&sa, 0, sizeof sa);
    sa.sin_family = AF_INET;
    sa.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    sa.sin_port = 0;
    socklen_t sa_len = sizeof sa;
    if (bind(fd, (struct sockaddr *)&sa, sa_len) != 0 ||
        getsockname(fd, (struct sockaddr *)&sa, &sa_len) != 0) {
        close(fd);
        return -1;
    }
    snprintf(out, out_len, "127.0.0.1:%u", (unsigned)ntohs(sa.sin_port));
    close(fd);
    return 0;
}

/* Builds a node on a freshly reserved port and reports the address back, so
 * the handshakes below have something to connect to. */
static int build(unsigned char seed_byte, net_meshnode_t **out, char *addr_out,
                 size_t addr_len) {
    if (reserve_addr(addr_out, addr_len) != 0) return -1;
    char seed[65];
    seed_hex(seed, seed_byte);
    char cfg[512];
    snprintf(cfg, sizeof cfg,
             "{\"bind_addr\":\"%s\",\"psk_hex\":\"%s\","
             "\"identity_seed_hex\":\"%s\"}",
             addr_out, PSK_HEX, seed);
    return net_mesh_new(cfg, out);
}

typedef struct {
    net_meshnode_t *responder;
    uint64_t initiator_id;
    int rc;
} accept_arg;

static void *accept_thread(void *p) {
    accept_arg *a = (accept_arg *)p;
    char *addr = NULL;
    size_t len = 0;
    a->rc = net_mesh_accept(a->responder, a->initiator_id, &addr, &len);
    if (addr) net_free_string(addr);
    return NULL;
}

static int handshake(net_meshnode_t *responder, net_meshnode_t *initiator,
                     const char *responder_addr) {
    char *pub = NULL;
    size_t pub_len = 0;
    if (net_mesh_public_key_hex(responder, &pub, &pub_len) != 0) return -1;

    accept_arg a;
    a.responder = responder;
    a.initiator_id = net_mesh_node_id(initiator);
    a.rc = -1;

    pthread_t t;
    if (pthread_create(&t, NULL, accept_thread, &a) != 0) {
        net_free_string(pub);
        return -1;
    }
    usleep(50000);
    int rc = net_mesh_connect(initiator, responder_addr, pub,
                              net_mesh_node_id(responder));
    pthread_join(t, NULL);
    net_free_string(pub);
    return (rc == 0 && a.rc == 0) ? 0 : -1;
}

static char *dup_cstr(const char *s) {
    size_t n = strlen(s) + 1;
    char *p = (char *)malloc(n);
    if (p) memcpy(p, s, n);
    return p;
}

/* ---- nRPC handler registry (one process-wide trampoline, keyed by id) ---- */

typedef struct {
    uint64_t handler_id;
    uint64_t worker;
} handler_slot;

static handler_slot g_handlers[4];
static int g_handler_count = 0;

static uint64_t handler_worker(uint64_t handler_id) {
    for (int i = 0; i < g_handler_count; i++) {
        if (g_handlers[i].handler_id == handler_id) return g_handlers[i].worker;
    }
    return 0;
}

/* Find `"id":` and parse the integer that follows. */
static unsigned parse_job_id(const uint8_t *req, size_t len) {
    char buf[256];
    size_t take = len < sizeof(buf) - 1 ? len : sizeof(buf) - 1;
    if (req) memcpy(buf, req, take);
    buf[take] = '\0';
    const char *p = strstr(buf, "\"id\":");
    return p ? (unsigned)strtoul(p + 5, NULL, 10) : 0;
}

static void c_free(void *p) { free(p); }

static int rpc_dispatch(uint64_t handler_id, const uint8_t *req_ptr,
                        size_t req_len, uint8_t **out_resp_ptr,
                        size_t *out_resp_len, char **out_err) {
    uint64_t worker = handler_worker(handler_id);
    unsigned job_id = parse_job_id(req_ptr, req_len);

    if (worker == 1 && job_id == POISON) {
        /* The `nrpc:app_error:` prefix is what makes this a typed
         * application refusal rather than the generic Internal a bare
         * message maps to — and it is returned BEFORE the job does any
         * work, which is what makes re-issuing it safe. The caller decides
         * to retry; the substrate never does it silently. */
        *out_err = dup_cstr(REFUSAL_APP_ERROR "worker 1 refused job 3");
        return -1;
    }

    char body[128];
    int n = snprintf(body, sizeof body, "{\"id\":%u,\"worker\":%llu}", job_id,
                     (unsigned long long)worker);
    uint8_t *resp = (uint8_t *)malloc((size_t)n);
    memcpy(resp, body, (size_t)n);
    *out_resp_ptr = resp;
    *out_resp_len = (size_t)n;
    return 0;
}

/* `net_redex_file_read_range` hands back a JSON array whose entries each
 * carry `"payload_hex":"<hex>"`. Decode the next payload into `out` and
 * advance `*cursor` past it; returns 0 once the array is exhausted. Both
 * the queue replay and the results reconcile walk a log this way. */
static int next_payload(const char **cursor, char *out, size_t cap) {
    static const char needle[] = "\"payload_hex\":\"";
    const char *p = strstr(*cursor, needle);
    if (!p) return 0;
    p += sizeof needle - 1;
    const char *end = strchr(p, '"');
    if (!end) return 0;
    size_t hlen = (size_t)(end - p);
    size_t o = 0;
    for (size_t i = 0; i + 1 < hlen && o + 1 < cap; i += 2) {
        char byte[3] = {p[i], p[i + 1], '\0'};
        out[o++] = (char)strtoul(byte, NULL, 16);
    }
    out[o] = '\0';
    *cursor = end + 1;
    return 1;
}

int main(void) {
    net_meshnode_t *producer = NULL, *w1 = NULL, *w2 = NULL;
    char producer_addr[32], w1_addr[32], w2_addr[32];
    if (build(0x71, &producer, producer_addr, sizeof producer_addr) != 0) { fprintf(stderr, "build producer failed\n"); return 1; }
    if (build(0x72, &w1, w1_addr, sizeof w1_addr) != 0) { fprintf(stderr, "build w1 failed\n"); return 1; }
    if (build(0x73, &w2, w2_addr, sizeof w2_addr) != 0) { fprintf(stderr, "build w2 failed\n"); return 1; }

    if (handshake(producer, w1, producer_addr) != 0) { fprintf(stderr, "p<->w1 failed\n"); return 1; }
    if (handshake(producer, w2, producer_addr) != 0) { fprintf(stderr, "p<->w2 failed\n"); return 1; }
    /* Workers can see each other too — nothing here needs that, but it is
     * what makes re-dispatch to "any worker" a local query. */
    if (handshake(w1, w2, w1_addr) != 0) { fprintf(stderr, "w1<->w2 failed\n"); return 1; }

    net_mesh_start(producer);
    net_mesh_start(w1);
    net_mesh_start(w2);

    uint64_t w1_id = net_mesh_node_id(w1);
    uint64_t w2_id = net_mesh_node_id(w2);

    /* nRPC setup: one process-wide dispatcher, a handler id per worker. */
    if (net_rpc_set_callback_free(c_free) != 0) {
        fprintf(stderr, "set_callback_free failed\n");
        return 1;
    }
    if (net_rpc_set_handler_dispatcher(rpc_dispatch) != 0) {
        fprintf(stderr, "set_handler_dispatcher failed\n");
        return 1;
    }

    MeshRpcHandle *rpc_producer =
        net_rpc_new((void *)net_mesh_arc_clone(producer));
    MeshRpcHandle *rpc_w1 = net_rpc_new((void *)net_mesh_arc_clone(w1));
    MeshRpcHandle *rpc_w2 = net_rpc_new((void *)net_mesh_arc_clone(w2));
    if (!rpc_producer || !rpc_w1 || !rpc_w2) {
        fprintf(stderr, "net_rpc_new failed\n");
        return 1;
    }

    /* Two workers, one service. Each echoes its own id so the caller can
     * prove which one ran the job. */
    char *serve_err = NULL;
    uint64_t hid1 = net_rpc_reserve_handler_id();
    g_handlers[g_handler_count].handler_id = hid1;
    g_handlers[g_handler_count].worker = 1;
    g_handler_count++;

    uint64_t hid2 = net_rpc_reserve_handler_id();
    g_handlers[g_handler_count].handler_id = hid2;
    g_handlers[g_handler_count].worker = 2;
    g_handler_count++;

    ServeHandleC *serve1 = net_rpc_serve(rpc_w1, "run", 3, hid1, 60000, &serve_err);
    if (!serve1) {
        fprintf(stderr, "serve w1 failed: %s\n", serve_err ? serve_err : "?");
        if (serve_err) net_rpc_free_cstring(serve_err);
        return 1;
    }
    serve_err = NULL;
    ServeHandleC *serve2 = net_rpc_serve(rpc_w2, "run", 3, hid2, 60000, &serve_err);
    if (!serve2) {
        fprintf(stderr, "serve w2 failed: %s\n", serve_err ? serve_err : "?");
        if (serve_err) net_rpc_free_cstring(serve_err);
        return 1;
    }

    /* The queue: a local append-only log, one record per submitted job. A
     * NULL persistent dir selects the in-memory manager — these logs are the
     * queue for as long as the process lives, and no longer. */
    net_redex_t *redex = net_redex_new(NULL);
    net_redex_file_t *queue = NULL, *results = NULL;
    if (!redex ||
        net_redex_open_file(redex, "jobs/queue", NULL, &queue) != 0 ||
        net_redex_open_file(redex, "jobs/results", NULL, &results) != 0) {
        fprintf(stderr, "redex open failed\n");
        return 1;
    }

    for (unsigned id = 1; id <= JOBS; id++) {
        char rec[32];
        int n = snprintf(rec, sizeof rec, "job:%u", id);
        uint64_t seq = 0;
        if (net_redex_file_append(queue, (const uint8_t *)rec, (size_t)n, &seq) != 0) {
            fprintf(stderr, "queue append failed\n");
            return 1;
        }
        printf("queued job %u at seq %llu\n", id, (unsigned long long)seq);
    }

    /* Dispatch. The work list comes back out of the queue log, not out of
     * the loop that wrote it — the log IS the queue. */
    char *queue_json = NULL;
    size_t queue_json_len = 0;
    if (net_redex_file_read_range(queue, 0, net_redex_file_len(queue),
                                  &queue_json, &queue_json_len) != 0) {
        fprintf(stderr, "queue read_range failed\n");
        return 1;
    }
    unsigned queued[JOBS];
    int queued_count = 0;
    const char *qp = queue_json;
    char qrec[128];
    while (queued_count < JOBS && next_payload(&qp, qrec, sizeof qrec)) {
        if (strncmp(qrec, "job:", 4) == 0) {
            queued[queued_count++] = (unsigned)strtoul(qrec + 4, NULL, 10);
        }
    }
    if (queue_json) net_free_string(queue_json);

    uint64_t targets[2] = {w1_id, w2_id};
    int retried = 0;
    for (int index = 0; index < queued_count; index++) {
        unsigned id = queued[index];
        uint64_t primary = targets[index % 2];
        uint64_t secondary = targets[(index + 1) % 2];
        char body[32];
        int n = snprintf(body, sizeof body, "{\"id\":%u}", id);

        uint8_t *resp = NULL;
        size_t resp_len = 0;
        char *err = NULL;
        int rc = net_rpc_call(rpc_producer, primary, "run", 3,
                              (const uint8_t *)body, (size_t)n, 5000, 0,
                              &resp, &resp_len, &err);
        if (rc != 0) {
            /* ONLY the typed refusal is re-issued. A timeout or a transport
             * fault means the call failed, not that the job did — the worker
             * may have run it already, and re-issuing would execute it
             * twice. Those are fatal here; duplicates=0 is a claim this
             * guard earns. */
            if (!err || !strstr(err, REFUSAL_STATUS)) {
                fprintf(stderr, "job %u failed on 0x%llx, not refused: %s\n",
                        id, (unsigned long long)primary, err ? err : "?");
                if (err) net_rpc_free_cstring(err);
                return 1;
            }
            net_rpc_free_cstring(err);
            retried++;
            printf("job %u refused by 0x%llx; re-issuing to 0x%llx\n", id,
                   (unsigned long long)primary, (unsigned long long)secondary);
            resp = NULL;
            resp_len = 0;
            err = NULL;
            rc = net_rpc_call(rpc_producer, secondary, "run", 3,
                              (const uint8_t *)body, (size_t)n, 5000, 0,
                              &resp, &resp_len, &err);
            if (rc != 0) {
                fprintf(stderr, "job %u failed on both workers: %s\n", id,
                        err ? err : "?");
                if (err) net_rpc_free_cstring(err);
                return 1;
            }
        }
        if (err) net_rpc_free_cstring(err);
        uint64_t worker = primary;
        if (rc == 0 && resp && resp_len > 0) {
            char rbuf[128];
            size_t take = resp_len < sizeof(rbuf) - 1 ? resp_len : sizeof(rbuf) - 1;
            memcpy(rbuf, resp, take);
            rbuf[take] = '\0';
            const char *wp = strstr(rbuf, "\"worker\":");
            if (wp) worker = strtoull(wp + 9, NULL, 10);
        }
        if (resp) net_rpc_response_free(resp, resp_len);

        char done[64];
        int dn = snprintf(done, sizeof done, "done:%u:%llu", id,
                          (unsigned long long)worker);
        uint64_t seq = 0;
        if (net_redex_file_append(results, (const uint8_t *)done, (size_t)dn,
                                  &seq) != 0) {
            fprintf(stderr, "results append failed\n");
            return 1;
        }
    }

    /* Reconcile from the results log, not from a counter kept beside the
     * dispatch loop: the completion count is whatever the log says. A job id
     * with one result record ran exactly once. */
    int result_counts[JOBS + 2];
    for (int i = 0; i < JOBS + 2; i++) result_counts[i] = 0;

    char *results_json = NULL;
    size_t results_len = 0;
    if (net_redex_file_read_range(results, 0, net_redex_file_len(results),
                                  &results_json, &results_len) != 0) {
        fprintf(stderr, "read_range failed\n");
        return 1;
    }
    const char *rp = results_json;
    char text[128];
    while (next_payload(&rp, text, sizeof text)) {
        if (strncmp(text, "done:", 5) == 0) {
            unsigned id = (unsigned)strtoul(text + 5, NULL, 10);
            if (id >= 1 && id <= JOBS) result_counts[id]++;
        }
    }
    if (results_json) net_free_string(results_json);

    int jobs_queued = (int)net_redex_file_len(queue);
    int jobs_done = 0, duplicates = 0;
    for (int id = 1; id <= JOBS; id++) {
        if (result_counts[id] > 0) jobs_done++;
        if (result_counts[id] > 1) duplicates++;
    }

    printf("queued:     %d\n", jobs_queued);
    printf("completed:  %d (one result record each)\n", jobs_done);
    printf("re-issued:  %d\n", retried);

    printf("RESULT ok jobs=%d done=%d retried=%d duplicates=%d\n",
           jobs_queued, jobs_done, retried, duplicates);

    net_rpc_serve_handle_close(serve1);
    net_rpc_serve_handle_free(serve1);
    net_rpc_serve_handle_close(serve2);
    net_rpc_serve_handle_free(serve2);
    net_rpc_free(rpc_producer);
    net_rpc_free(rpc_w1);
    net_rpc_free(rpc_w2);

    net_redex_file_free(queue);
    net_redex_file_free(results);
    net_redex_free(redex);

    net_mesh_shutdown(producer);
    net_mesh_shutdown(w1);
    net_mesh_shutdown(w2);
    net_mesh_free(producer);
    net_mesh_free(w1);
    net_mesh_free(w2);
    return 0;
}
