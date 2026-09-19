/*
 * The service registry you no longer run (C).
 *
 * Four in-process mesh nodes over loopback UDP: two providers announce the
 * same capability, a caller discovers them and ranks them locally, a third
 * provider appears, and the caller's next lookup ranks it first — with no
 * registry process, no health-check poller, no config reload and no
 * announcement to any address.
 *
 * Mirrors examples/registry.rs. The Rust `local_addr()` has no C binding, so
 * each node reserves an ephemeral loopback port for itself before binding
 * (see `reserve_addr`) rather than trusting a hard-coded one.
 *
 * Build: gcc registry.c -lnet -lpthread -ldl -lm && ./a.out
 *
 * Expected final line: RESULT ok providers=3 joined=1 best_moved=1
 */

#include "net.go.h"

#include <arpa/inet.h>
#include <netinet/in.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

/* 32 bytes exactly — a PSK, not a passphrase. Every node in a mesh shares it. */
static const char *PSK_HEX =
    "4242424242424242424242424242424242424242424242424242424242424242";

/* How long we wait for an announcement to reach the caller's local fold
 * (5s at 25ms per poll). */
#define CONVERGE_TRIES 200
#define POLL_US 25000

/* Fill `out[65]` with a 32-byte seed whose every byte is `b`, hex-encoded. */
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

/* Bind a node to a freshly reserved loopback port with a distinct identity
 * seed, and report the address back for the handshakes below. */
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

/*
 * One side connects, the other accepts. `net_mesh_accept` blocks until the
 * initiator's handshake lands, so the responder's accept runs on a helper
 * thread while the initiator connects — joining them is the
 * wait-until-connected primitive.
 */
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
    /* Absorbs the accept/connect race; both sides retry with backoff. */
    usleep(50000);
    int rc = net_mesh_connect(initiator, responder_addr, pub,
                              net_mesh_node_id(responder));
    pthread_join(t, NULL);
    net_free_string(pub);
    return (rc == 0 && a.rc == 0) ? 0 : -1;
}

/* Count entries in a JSON array the SDK returns (here: `[node_id, ...]`). */
static int json_array_count(const char *s) {
    if (!s) return 0;
    const char *open = strchr(s, '[');
    if (!open) return 0;
    int count = 0;
    int have_number = 0;
    for (const char *p = open + 1; *p; p++) {
        if (*p == ']') break;
        if (*p >= '0' && *p <= '9') {
            if (!have_number) {
                count++;
                have_number = 1;
            }
        } else if (*p == ',' || *p == ' ' || *p == '\t' || *p == '\n') {
            have_number = 0;
        }
    }
    return count;
}

/* Poll the caller's local capability fold until `want` providers match. */
static int until_found(net_meshnode_t *caller, const char *filter, int want) {
    for (int i = 0; i < CONVERGE_TRIES; i++) {
        char *json = NULL;
        size_t len = 0;
        if (net_mesh_find_nodes(caller, filter, &json, &len) == 0) {
            int n = json_array_count(json);
            net_free_string(json);
            if (n == want) return 1;
        } else if (json) {
            net_free_string(json);
        }
        usleep(POLL_US);
    }
    return 0;
}

int main(void) {
    const char *api_filter = "{\"require_tags\":[\"api\"]}";
    const char *requirement =
        "{\"filter\":{\"require_tags\":[\"api\"]},\"prefer_more_memory\":1.0}";
    const char *caps_a = "{\"hardware\":{\"memory_gb\":16},\"tags\":[\"api\"]}";
    const char *caps_b = "{\"hardware\":{\"memory_gb\":64},\"tags\":[\"api\"]}";
    const char *caps_c = "{\"hardware\":{\"memory_gb\":256},\"tags\":[\"api\"]}";

    net_meshnode_t *a = NULL, *b = NULL, *c = NULL, *caller = NULL;
    char addr_a[32], addr_b[32], addr_c[32], addr_caller[32];
    if (build(0xA1, &a, addr_a, sizeof addr_a) != 0) { fprintf(stderr, "build a failed\n"); return 1; }
    if (build(0xB2, &b, addr_b, sizeof addr_b) != 0) { fprintf(stderr, "build b failed\n"); return 1; }
    if (build(0xC3, &c, addr_c, sizeof addr_c) != 0) { fprintf(stderr, "build c failed\n"); return 1; }
    if (build(0xD4, &caller, addr_caller, sizeof addr_caller) != 0) {
        fprintf(stderr, "build caller failed\n");
        return 1;
    }

    /* Every accept() completes before any start() — the dispatch loop races
     * the responder handshake otherwise. */
    if (handshake(b, a, addr_b) != 0) { fprintf(stderr, "b<->a failed\n"); return 1; }
    if (handshake(a, caller, addr_a) != 0) { fprintf(stderr, "a<->caller failed\n"); return 1; }
    if (handshake(caller, b, addr_caller) != 0) { fprintf(stderr, "caller<->b failed\n"); return 1; }
    if (handshake(caller, c, addr_caller) != 0) { fprintf(stderr, "caller<->c failed\n"); return 1; }

    net_mesh_start(a);
    net_mesh_start(b);
    net_mesh_start(c);
    net_mesh_start(caller);

    /* The two original providers announce. That is the entire registration. */
    if (net_mesh_announce_capabilities(a, caps_a) != 0 ||
        net_mesh_announce_capabilities(b, caps_b) != 0) {
        fprintf(stderr, "announce failed\n");
        return 1;
    }

    if (!until_found(caller, api_filter, 2)) {
        fprintf(stderr, "providers never converged\n");
        return 1;
    }

    /* Ranking is local and free. Prefer more memory, so the bigger machine
     * wins without any scheduler deciding it centrally. */
    uint64_t first = 0;
    int first_has = 0;
    if (net_mesh_find_best_node(caller, requirement, &first, &first_has) != 0 ||
        !first_has) {
        fprintf(stderr, "no provider matched\n");
        return 1;
    }

    /* A new provider appears. It connects, announces once, and is
     * immediately addressable — no registry entry, no reload. */
    if (net_mesh_announce_capabilities(c, caps_c) != 0) {
        fprintf(stderr, "announce c failed\n");
        return 1;
    }
    if (!until_found(caller, api_filter, 3)) {
        fprintf(stderr, "the new provider never converged\n");
        return 1;
    }

    uint64_t best = 0;
    int best_has = 0;
    if (net_mesh_find_best_node(caller, requirement, &best, &best_has) != 0 ||
        !best_has) {
        fprintf(stderr, "no provider matched after the join\n");
        return 1;
    }
    int moved = (first != best) ? 1 : 0;
    int providers = 3;

    printf("providers found at first lookup: 2\n");
    printf("ranked best:                     0x%llx\n", (unsigned long long)first);
    printf("providers after the join:        %d\n", providers);
    printf("ranked best:                     0x%llx\n", (unsigned long long)best);

    printf("RESULT ok providers=%d joined=1 best_moved=%d\n", providers, moved);

    net_mesh_shutdown(a);
    net_mesh_shutdown(b);
    net_mesh_shutdown(c);
    net_mesh_shutdown(caller);
    net_mesh_free(a);
    net_mesh_free(b);
    net_mesh_free(c);
    net_mesh_free(caller);
    return 0;
}
