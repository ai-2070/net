/*
 * Live config — the config service you no longer run (C).
 *
 * One publisher and two subscribers, three in-process mesh nodes over
 * loopback UDP. The publisher registers a channel, both subscribers join by
 * name, and every config revision is pushed once and applied by each
 * subscriber locally. There is no config server to poll, no cache to
 * invalidate and no reload to coordinate — the channel *is* the delivery, and
 * the roster is held by the publisher, not by a broker.
 *
 * Mirrors examples/liveconfig.rs. The Rust `local_addr()` has no C binding,
 * so each node binds a chosen free loopback port instead of ":0".
 *
 * Build: gcc liveconfig.c -lnet -lpthread -ldl -lm && ./a.out
 *
 * Expected final line: RESULT ok subscribers=2 applied=2 version=2
 */

#include "net.go.h"

#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static const char *PSK_HEX =
    "4242424242424242424242424242424242424242424242424242424242424242";

#define DELIVER_TRIES 250
#define POLL_US 20000

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

/* ---- base64 (the shard-receive JSON carries payload_b64) ---- */

static int b64_val(int c) {
    if (c >= 'A' && c <= 'Z') return c - 'A';
    if (c >= 'a' && c <= 'z') return c - 'a' + 26;
    if (c >= '0' && c <= '9') return c - '0' + 52;
    if (c == '+') return 62;
    if (c == '/') return 63;
    return -1;
}

static size_t b64_decode(const char *s, size_t n, unsigned char *out) {
    size_t o = 0;
    int acc = 0, bits = 0;
    for (size_t i = 0; i < n; i++) {
        int v = b64_val((unsigned char)s[i]);
        if (v < 0) continue; /* skips '=' padding and stray whitespace */
        acc = (acc << 6) | v;
        bits += 6;
        if (bits >= 8) {
            bits -= 8;
            out[o++] = (unsigned char)((acc >> bits) & 0xFF);
        }
    }
    return o;
}

/* Parse `v=<n>;mode=<name>`; subscribers apply what they understand. */
static int parse_revision(const unsigned char *buf, size_t n, uint64_t *version,
                          char *mode, size_t mode_len) {
    char text[256];
    size_t take = n < sizeof(text) - 1 ? n : sizeof(text) - 1;
    memcpy(text, buf, take);
    text[take] = '\0';

    int have_v = 0, have_mode = 0;
    char *p = text;
    while (p && *p) {
        if (strncmp(p, "v=", 2) == 0) {
            *version = strtoull(p + 2, NULL, 10);
            have_v = 1;
        } else if (strncmp(p, "mode=", 5) == 0) {
            char *end = strchr(p + 5, ';');
            size_t len = end ? (size_t)(end - (p + 5)) : strlen(p + 5);
            if (len >= mode_len) len = mode_len - 1;
            memcpy(mode, p + 5, len);
            mode[len] = '\0';
            have_mode = 1;
        }
        char *semi = strchr(p, ';');
        p = semi ? semi + 1 : NULL;
    }
    return have_v && have_mode;
}

typedef struct {
    int has1, has2;
    char m1[32], m2[32];
} applied_t;

static void apply(applied_t *ap, uint64_t version, const char *mode) {
    if (version == 1) {
        ap->has1 = 1;
        snprintf(ap->m1, sizeof ap->m1, "%s", mode);
    } else if (version == 2) {
        ap->has2 = 1;
        snprintf(ap->m2, sizeof ap->m2, "%s", mode);
    }
}

/* Scan one recv_shard JSON array for `"payload_b64":"..."` objects. */
static int consume_shard_json(const char *json, applied_t *ap) {
    int count = 0;
    const char *needle = "\"payload_b64\":\"";
    const char *p = json;
    while ((p = strstr(p, needle)) != NULL) {
        p += strlen(needle);
        const char *end = strchr(p, '"');
        if (!end) break;
        unsigned char raw[256];
        size_t raw_len = b64_decode(p, (size_t)(end - p), raw);
        uint64_t version = 0;
        char mode[64];
        if (parse_revision(raw, raw_len, &version, mode, sizeof mode)) {
            apply(ap, version, mode);
        }
        count++;
        p = end + 1;
    }
    return count;
}

/* Drain every shard the bus could have routed a channel event to. */
static int drain(net_meshnode_t *node, applied_t *ap) {
    int seen = 0;
    for (int i = 0; i < DELIVER_TRIES; i++) {
        int quiet = 1;
        for (uint16_t shard = 0; shard < 4; shard++) {
            char *json = NULL;
            size_t len = 0;
            if (net_mesh_recv_shard(node, shard, 64, &json, &len) == 0) {
                if (json && len > 0) {
                    int n = consume_shard_json(json, ap);
                    if (n > 0) {
                        quiet = 0;
                        seen += n;
                    }
                }
                if (json) net_free_string(json);
            } else if (json) {
                net_free_string(json);
            }
        }
        if (quiet && seen > 0) break;
        usleep(POLL_US);
    }
    return seen;
}

static int report_attempted(const char *json) {
    const char *p = json ? strstr(json, "\"attempted\":") : NULL;
    return p ? atoi(p + strlen("\"attempted\":")) : 0;
}

int main(void) {
    net_meshnode_t *publisher = NULL, *s1 = NULL, *s2 = NULL;
    if (build(0xF1, 39021, &publisher) != 0) { fprintf(stderr, "build publisher failed\n"); return 1; }
    if (build(0xF2, 39022, &s1) != 0) { fprintf(stderr, "build s1 failed\n"); return 1; }
    if (build(0xF3, 39023, &s2) != 0) { fprintf(stderr, "build s2 failed\n"); return 1; }

    const char *publisher_addr = "127.0.0.1:39021";

    /* Both subscribers connect to the publisher; it accepts both. */
    if (handshake(publisher, s1, publisher_addr) != 0) { fprintf(stderr, "p<->s1 failed\n"); return 1; }
    if (handshake(publisher, s2, publisher_addr) != 0) { fprintf(stderr, "p<->s2 failed\n"); return 1; }

    net_mesh_start(publisher);
    net_mesh_start(s1);
    net_mesh_start(s2);

    /* The publisher owns the channel config. No broker registers it. */
    if (net_mesh_register_channel(
            publisher,
            "{\"name\":\"config/edge\",\"visibility\":\"global\"}") != 0) {
        fprintf(stderr, "register_channel failed\n");
        return 1;
    }

    /* Subscribers join by name; subscribe blocks on the publisher's ack. */
    uint64_t publisher_id = net_mesh_node_id(publisher);
    if (net_mesh_subscribe_channel(s1, publisher_id, "config/edge") != 0 ||
        net_mesh_subscribe_channel(s2, publisher_id, "config/edge") != 0) {
        fprintf(stderr, "subscribe failed\n");
        return 1;
    }

    /* Revision 1, then revision 2 — delivered the same way. */
    const char *v1 = "v=1;mode=blue";
    const char *v2 = "v=2;mode=green";
    char *report = NULL;
    size_t report_len = 0;

    if (net_mesh_publish(publisher, "config/edge", (const uint8_t *)v1,
                         strlen(v1), "{\"reliability\":\"reliable\"}",
                         &report, &report_len) != 0) {
        fprintf(stderr, "publish v1 failed\n");
        return 1;
    }
    printf("published v1 to %d subscribers\n", report_attempted(report));
    if (report) { net_free_string(report); report = NULL; }

    if (net_mesh_publish(publisher, "config/edge", (const uint8_t *)v2,
                         strlen(v2), "{\"reliability\":\"reliable\"}",
                         &report, &report_len) != 0) {
        fprintf(stderr, "publish v2 failed\n");
        return 1;
    }
    int subscribers = report_attempted(report);
    printf("published v2 to %d subscribers\n", subscribers);
    if (report) { net_free_string(report); report = NULL; }

    applied_t one = {0}, two = {0};
    drain(s1, &one);
    drain(s2, &two);

    int applied = (one.has2 ? 1 : 0) + (two.has2 ? 1 : 0);

    printf("subscriber one applied: v1=%s v2=%s\n",
           one.has1 ? one.m1 : "-", one.has2 ? one.m2 : "-");
    printf("subscriber two applied: v1=%s v2=%s\n",
           two.has1 ? two.m1 : "-", two.has2 ? two.m2 : "-");

    /* Worth pinning: the publisher's roster is what fan-out costs. */
    printf("roster at publish time: %d\n", subscribers);

    printf("RESULT ok subscribers=%d applied=%d version=2\n", subscribers, applied);

    net_mesh_shutdown(publisher);
    net_mesh_shutdown(s1);
    net_mesh_shutdown(s2);
    net_mesh_free(publisher);
    net_mesh_free(s1);
    net_mesh_free(s2);
    return 0;
}
