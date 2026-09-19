/*
 * The sidecar you no longer run: call a service, not a host (C).
 *
 * Two providers serve the same service name. A caller addresses the *service*
 * — never a node id — and when the provider that answered first goes away, the
 * next call lands on the survivor, with a bounded retry absorbing the transient
 * while the dead provider is still in the roster.
 *
 * Rust has a retry helper that does this for you; this binding has none, so the
 * loop below is written by hand. That is the only difference from the Rust
 * sibling.
 *
 * Mirrors examples/failover.rs. The Rust `local_addr()` has no C binding, so
 * each node binds a chosen free loopback port instead of ":0".
 *
 * Build: gcc failover.c -lnet -lpthread -ldl -lm && ./a.out
 *
 * Expected final line: RESULT ok providers=2 moved=1 served=2
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

/* The roster still lists a dead provider until the capability fold converges. */
enum { RETRY_ATTEMPTS = 6, RETRY_INTERVAL_MS = 250 };

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
    char served_by[24];
} handler_slot;

static handler_slot g_handlers[4];
static int g_handler_count = 0;

static handler_slot *find_handler(uint64_t handler_id) {
    for (int i = 0; i < g_handler_count; i++) {
        if (g_handlers[i].handler_id == handler_id) return &g_handlers[i];
    }
    return NULL;
}

/* Pull `"units":` out of a request or response. Our own format. */
static unsigned parse_units(const uint8_t *req, size_t len) {
    char buf[256];
    size_t take = len < sizeof(buf) - 1 ? len : sizeof(buf) - 1;
    if (req) memcpy(buf, req, take);
    buf[take] = '\0';
    const char *p = strstr(buf, "\"units\":");
    return p ? (unsigned)strtoul(p + 8, NULL, 10) : 0;
}

/* Pull the string after `"served_by":"` — ours, and always quoted. */
static void parse_served_by(const uint8_t *resp, size_t len, char *out,
                            size_t out_len) {
    out[0] = '\0';
    char buf[512];
    size_t take = len < sizeof(buf) - 1 ? len : sizeof(buf) - 1;
    if (resp) memcpy(buf, resp, take);
    buf[take] = '\0';
    const char *p = strstr(buf, "\"served_by\":\"");
    if (!p) return;
    p += strlen("\"served_by\":\"");
    const char *end = strchr(p, '"');
    if (!end) return;
    size_t n = (size_t)(end - p);
    if (n >= out_len) n = out_len - 1;
    memcpy(out, p, n);
    out[n] = '\0';
}

static void c_free(void *p) { free(p); }

static int rpc_dispatch(uint64_t handler_id, const uint8_t *req_ptr,
                        size_t req_len, uint8_t **out_resp_ptr,
                        size_t *out_resp_len, char **out_err) {
    handler_slot *slot = find_handler(handler_id);
    if (!slot) {
        *out_err = dup_cstr("server_error: unknown handler id");
        return -1;
    }
    unsigned units = parse_units(req_ptr, req_len);
    char body[160];
    int n = snprintf(body, sizeof body,
                     "{\"served_by\":\"%s\",\"units\":%u}", slot->served_by,
                     units);
    uint8_t *resp = (uint8_t *)malloc((size_t)n);
    memcpy(resp, body, (size_t)n);
    *out_resp_ptr = resp;
    *out_resp_len = (size_t)n;
    return 0;
}

/* One call to the service by NAME. Returns 0 and fills `served_by` on success. */
static int call_service(MeshRpcHandle *rpc, unsigned units, uint64_t deadline_ms,
                        char *served_by, size_t served_by_len, unsigned *out_units) {
    char body[32];
    int n = snprintf(body, sizeof body, "{\"units\":%u}", units);

    uint8_t *resp = NULL;
    size_t resp_len = 0;
    char *err = NULL;
    int rc = net_rpc_call_service(rpc, "work", 4, (const uint8_t *)body,
                                  (size_t)n, deadline_ms, 0, &resp, &resp_len,
                                  &err);
    if (err) net_rpc_free_cstring(err);
    if (rc != 0) {
        if (resp) net_rpc_response_free(resp, resp_len);
        return -1;
    }
    parse_served_by(resp, resp_len, served_by, served_by_len);
    if (out_units) *out_units = parse_units(resp, resp_len);
    if (resp) net_rpc_response_free(resp, resp_len);
    return 0;
}

int main(void) {
    net_meshnode_t *caller = NULL, *p1 = NULL, *p2 = NULL;
    if (build(0xC1, 39061, &caller) != 0) { fprintf(stderr, "build caller failed\n"); return 1; }
    if (build(0xC2, 39062, &p1) != 0) { fprintf(stderr, "build p1 failed\n"); return 1; }
    if (build(0xC3, 39063, &p2) != 0) { fprintf(stderr, "build p2 failed\n"); return 1; }

    const char *caller_addr = "127.0.0.1:39061";

    if (handshake(caller, p1, caller_addr) != 0) { fprintf(stderr, "c<->p1 failed\n"); return 1; }
    if (handshake(caller, p2, caller_addr) != 0) { fprintf(stderr, "c<->p2 failed\n"); return 1; }

    net_mesh_start(caller);
    net_mesh_start(p1);
    net_mesh_start(p2);

    /* nRPC setup: one process-wide dispatcher, a handler id per provider. */
    if (net_rpc_set_callback_free(c_free) != 0) {
        fprintf(stderr, "set_callback_free failed\n");
        return 1;
    }
    if (net_rpc_set_handler_dispatcher(rpc_dispatch) != 0) {
        fprintf(stderr, "set_handler_dispatcher failed\n");
        return 1;
    }

    MeshRpcHandle *rpc_caller = net_rpc_new((void *)net_mesh_arc_clone(caller));
    MeshRpcHandle *rpc_p1 = net_rpc_new((void *)net_mesh_arc_clone(p1));
    MeshRpcHandle *rpc_p2 = net_rpc_new((void *)net_mesh_arc_clone(p2));
    if (!rpc_caller || !rpc_p1 || !rpc_p2) {
        fprintf(stderr, "net_rpc_new failed\n");
        return 1;
    }

    /* Two providers, one service name. Each echoes its own node id so the
     * caller can prove which one answered. */
    char p1_hex[24], p2_hex[24];
    snprintf(p1_hex, sizeof p1_hex, "0x%llx", (unsigned long long)net_mesh_node_id(p1));
    snprintf(p2_hex, sizeof p2_hex, "0x%llx", (unsigned long long)net_mesh_node_id(p2));

    uint64_t hid1 = net_rpc_reserve_handler_id();
    g_handlers[g_handler_count].handler_id = hid1;
    snprintf(g_handlers[g_handler_count].served_by, 24, "%s", p1_hex);
    g_handler_count++;

    uint64_t hid2 = net_rpc_reserve_handler_id();
    g_handlers[g_handler_count].handler_id = hid2;
    snprintf(g_handlers[g_handler_count].served_by, 24, "%s", p2_hex);
    g_handler_count++;

    char *serve_err = NULL;
    ServeHandleC *serve1 = net_rpc_serve(rpc_p1, "work", 4, hid1, 60000, &serve_err);
    if (!serve1) {
        fprintf(stderr, "serve p1 failed: %s\n", serve_err ? serve_err : "?");
        if (serve_err) net_rpc_free_cstring(serve_err);
        return 1;
    }
    serve_err = NULL;
    ServeHandleC *serve2 = net_rpc_serve(rpc_p2, "work", 4, hid2, 60000, &serve_err);
    if (!serve2) {
        fprintf(stderr, "serve p2 failed: %s\n", serve_err ? serve_err : "?");
        if (serve_err) net_rpc_free_cstring(serve_err);
        return 1;
    }

    /* The caller discovers the service by name. It learns who can serve it,
     * never whom to prefer — that is what makes the next part work. */
    size_t providers = 0;
    for (int attempt = 0; attempt < 200 && providers < 2; attempt++) {
        uint64_t *ids = NULL;
        size_t count = 0;
        char *err = NULL;
        if (net_rpc_find_service_nodes(rpc_caller, "work", 4, &ids, &count, &err) == 0) {
            providers = count;
        }
        if (err) net_rpc_free_cstring(err);
        net_rpc_find_service_nodes_free(ids, count);
        if (providers < 2) usleep(25000);
    }
    printf("providers advertising `work`: %zu\n", providers);

    /* Call the service, not a host. */
    char first_by[24];
    unsigned first_units = 0;
    if (call_service(rpc_caller, 2, 1000, first_by, sizeof first_by, &first_units) != 0) {
        fprintf(stderr, "first call failed\n");
        return 1;
    }
    printf("first call served by:  %s\n", first_by);

    /* Take that provider out of the mesh entirely — the hard version of a
     * deploy: not a drain, a death. Its RPC handles retire first. */
    int doomed_is_p1 = strcmp(first_by, p1_hex) == 0;
    ServeHandleC *doomed_serve = doomed_is_p1 ? serve1 : serve2;
    ServeHandleC *survivor_serve = doomed_is_p1 ? serve2 : serve1;
    MeshRpcHandle *doomed_rpc = doomed_is_p1 ? rpc_p1 : rpc_p2;
    net_meshnode_t *doomed_node = doomed_is_p1 ? p1 : p2;

    net_rpc_serve_handle_close(doomed_serve);
    net_rpc_serve_handle_free(doomed_serve);
    net_rpc_free(doomed_rpc);
    net_mesh_shutdown(doomed_node);
    printf("took %s out\n", first_by);

    /* The roster still lists the dead provider until the fold converges, so
     * the first attempt may be spent on it. This loop is what Rust's retry
     * helper would do for you. */
    char second_by[24];
    unsigned second_units = 0;
    int answered = 0;
    for (int attempt = 0; attempt < RETRY_ATTEMPTS && !answered; attempt++) {
        if (call_service(rpc_caller, 5, 500, second_by, sizeof second_by, &second_units) == 0) {
            answered = 1;
            break;
        }
        usleep(RETRY_INTERVAL_MS * 1000);
    }
    if (!answered) {
        fprintf(stderr, "no provider answered after the death\n");
        return 1;
    }
    printf("after the death served by: %s\n", second_by);

    int moved = strcmp(second_by, first_by) != 0 ? 1 : 0;
    int served = (first_units == 2 ? 1 : 0) + (second_units == 5 ? 1 : 0);
    printf("RESULT ok providers=%zu moved=%d served=%d\n", providers, moved,
           served);

    net_rpc_serve_handle_close(survivor_serve);
    net_rpc_serve_handle_free(survivor_serve);
    net_rpc_free(rpc_caller);
    net_mesh_shutdown(caller);
    net_mesh_shutdown(doomed_is_p1 ? p2 : p1);
    return 0;
}