/*
 * The job queue you no longer run (C).
 *
 * A producer, two workers, and a durable job log — three in-process mesh
 * nodes over loopback UDP plus a local append-only log. Jobs are appended to
 * the log (the queue), dispatched to a worker over nRPC, and a worker that
 * fails a job is retried on its peer. Nothing is re-executed, and the log is
 * the record you reconcile from.
 *
 * Mirrors examples/jobqueue.rs. The Rust `local_addr()` has no C binding, so
 * each node binds a chosen free loopback port instead of ":0".
 *
 * Build: gcc jobqueue.c -lnet -lpthread -ldl -lm && ./a.out
 *
 * Expected final line: RESULT ok jobs=6 done=6 retried=1 duplicates=0
 */

#include "net.go.h"
#include "net_rpc.h"

#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static const char *PSK_HEX =
    "4242424242424242424242424242424242424242424242424242424242424242";

enum { JOBS = 6, POISON = 3 };

static void seed_hex(char *out, unsigned char b) {
    for (int i = 0; i < 32; i++) sprintf(out + i * 2, "%02x", b);
    out[64] = '\0';
}

static int build(unsigned char seed_byte, unsigned port, net_meshnode_t **out) {
    char seed[65];
    seed_hex(seed, seed_byte);
    char cfg[512];
    snprintf(cfg, sizeof cfg,
             "{\"bind_addr\":\"127.0.0.1:%u\",\"psk_hex\":\"%s\","
             "\"identity_seed_hex\":\"%s\"}",
             port, PSK_HEX, seed);
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
        /* A typed application refusal, not a transport error. The caller
         * decides to retry; the substrate never does it silently. */
        *out_err = dup_cstr("server_error: worker refused job 3");
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

int main(void) {
    net_meshnode_t *producer = NULL, *w1 = NULL, *w2 = NULL;
    if (build(0x71, 39031, &producer) != 0) { fprintf(stderr, "build producer failed\n"); return 1; }
    if (build(0x72, 39032, &w1) != 0) { fprintf(stderr, "build w1 failed\n"); return 1; }
    if (build(0x73, 39033, &w2) != 0) { fprintf(stderr, "build w2 failed\n"); return 1; }

    const char *producer_addr = "127.0.0.1:39031";
    const char *w1_addr = "127.0.0.1:39032";

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

    /* The queue: a local append-only log, one record per submitted job. */
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

    /* Dispatch round-robin; a refusal re-issues the same job to the other. */
    uint64_t targets[2] = {w1_id, w2_id};
    int retried = 0;
    for (unsigned id = 1; id <= JOBS; id++) {
        unsigned index = id - 1;
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
            if (err) net_rpc_free_cstring(err);
            /* The rust worker id 1 is the one that refuses job 3. */
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

    /* Reconcile from the logs, not from memory. A job id with one result
     * record ran exactly once. */
    int result_counts[JOBS + 2];
    for (int i = 0; i < JOBS + 2; i++) result_counts[i] = 0;

    char *results_json = NULL;
    size_t results_len = 0;
    if (net_redex_file_read_range(results, 0, net_redex_file_len(results),
                                  &results_json, &results_len) != 0) {
        fprintf(stderr, "read_range failed\n");
        return 1;
    }
    /* Each event JSON carries "payload_hex":"<hex of done:id:worker>". */
    const char *needle = "\"payload_hex\":\"";
    const char *p = results_json;
    while ((p = strstr(p, needle)) != NULL) {
        p += strlen(needle);
        const char *end = strchr(p, '"');
        if (!end) break;
        char text[128];
        size_t hlen = (size_t)(end - p);
        size_t o = 0;
        for (size_t i = 0; i + 1 < hlen && o < sizeof(text) - 1; i += 2) {
            char byte[3] = {p[i], p[i + 1], '\0'};
            text[o++] = (char)strtoul(byte, NULL, 16);
        }
        text[o] = '\0';
        if (strncmp(text, "done:", 5) == 0) {
            unsigned id = (unsigned)strtoul(text + 5, NULL, 10);
            if (id >= 1 && id <= JOBS) result_counts[id]++;
        }
        p = end + 1;
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
