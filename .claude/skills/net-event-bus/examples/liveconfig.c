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
 * so each node reserves an ephemeral loopback port for itself before binding
 * (see `reserve_addr`) rather than trusting a hard-coded one.
 *
 * Build: gcc liveconfig.c -lnet -lpthread -ldl -lm && ./a.out
 *
 * Expected final line: RESULT ok subscribers=2 applied=2 version=2
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

static const char *PSK_HEX =
    "4242424242424242424242424242424242424242424242424242424242424242";

#define DELIVER_TRIES 250
#define POLL_US 20000

/* Both subscribers are in the roster before the first publish, because
 * net_mesh_subscribe_channel blocks on the publisher's ack. */
#define SUBSCRIBERS 2

static void seed_hex(char *out, unsigned char b) {
    for (int i = 0; i < 32; i++) sprintf(out + i * 2, "%02x", b);
    out[64] = '\0';
}

/* How many times `build` re-reserves a port after losing the race
 * between releasing the reservation and the node binding it. Small:
 * the ephemeral range is large, so a repeated loss means something
 * is wrong rather than unlucky. */
#define BIND_ATTEMPTS 8

/* Ask the kernel for a free loopback port and report the address it handed
 * out. A hard-coded port fails the moment it is taken — including when two
 * copies of this example run at once. */
static int reserve_addr(char *out, size_t out_len) {
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0) return -1;
    struct sockaddr_in want;
    memset(&want, 0, sizeof want);
    want.sin_family = AF_INET;
    want.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    want.sin_port = 0;
    struct sockaddr_in got;
    socklen_t got_len = sizeof got;
    if (bind(fd, (struct sockaddr *)&want, sizeof want) != 0 ||
        getsockname(fd, (struct sockaddr *)&got, &got_len) != 0) {
        close(fd);
        return -1;
    }
    snprintf(out, out_len, "127.0.0.1:%u", (unsigned)ntohs(got.sin_port));
    close(fd);
    return 0;
}

/* The reserved address is reported back, so the address a node binds and the
 * address a peer connects to come from one place. */
static int build(unsigned char seed_byte, net_meshnode_t **out, char *addr_out,
                 size_t addr_len) {
    char seed[65];
    seed_hex(seed, seed_byte);
    /* Reserving a port and then closing it leaves a window: another
     * process can take it before net_mesh_new binds. The reservation
     * cannot simply be held, because the node needs to bind the port
     * itself, and the C ABI exposes no "what did you actually bind"
     * accessor to read back instead. So the window is closed by
     * retrying rather than by pretending it is not there — a lost race
     * yields a different free port on the next attempt. */
    for (int attempt = 0; attempt < BIND_ATTEMPTS; attempt++) {
        if (reserve_addr(addr_out, addr_len) != 0) return -1;
        char cfg[512];
        snprintf(cfg, sizeof cfg,
                 "{\"bind_addr\":\"%s\",\"psk_hex\":\"%s\","
                 "\"identity_seed_hex\":\"%s\"}",
                 addr_out, PSK_HEX, seed);
        if (net_mesh_new(cfg, out) == 0) return 0;
    }
    return -1;
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

/* Decode into `out`, writing at most `cap` bytes. Returns the number of bytes
 * written, or -1 if the payload would have overrun `out` — the encoded text
 * comes from a peer, so its decoded length is never assumed to fit. */
static long b64_decode(const char *s, size_t n, unsigned char *out, size_t cap) {
    size_t o = 0;
    uint32_t acc = 0;
    unsigned bits = 0; /* bits of the next output byte already in `acc` */
    for (size_t i = 0; i < n; i++) {
        int v = b64_val((unsigned char)s[i]);
        if (v < 0) continue; /* skips '=' padding and stray whitespace */
        /* Drop everything already emitted before shifting the new sextet in:
         * an unmasked accumulator shifts left once per character and runs off
         * the top of the type a few bytes into the payload. */
        acc = ((acc & ((1u << bits) - 1u)) << 6) | (uint32_t)v;
        bits += 6;
        if (bits >= 8) {
            bits -= 8;
            if (o == cap) return -1;
            out[o++] = (unsigned char)((acc >> bits) & 0xFFu);
        }
    }
    return (long)o;
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

/* The final line claims this subscriber applied both revisions, with the modes
 * the publisher sent. A revision that never arrived is a failure, not a
 * quieter success. */
static int check_applied(const char *who, const applied_t *ap) {
    if (!ap->has1 || strcmp(ap->m1, "blue") != 0 || !ap->has2 ||
        strcmp(ap->m2, "green") != 0) {
        fprintf(stderr,
                "subscriber %s did not apply both revisions: v1=%s v2=%s\n", who,
                ap->has1 ? ap->m1 : "-", ap->has2 ? ap->m2 : "-");
        return -1;
    }
    return 0;
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
        /* A revision is a handful of bytes, so anything that would not fit is
         * not one: a refused decode is ignored like any unreadable payload. */
        unsigned char raw[256];
        long raw_len = b64_decode(p, (size_t)(end - p), raw, sizeof raw);
        uint64_t version = 0;
        char mode[64];
        if (raw_len > 0 &&
            parse_revision(raw, (size_t)raw_len, &version, mode, sizeof mode)) {
            apply(ap, version, mode);
        }
        count++;
        p = end + 1;
    }
    return count;
}

/* Drain every shard the bus could have routed a channel event to. Returns the
 * number of events seen, or -1 if a receive failed.
 *
 * A quiet poll is not the end of the stream: two revisions published
 * back-to-back can land one poll apart, so the only reason to stop early is
 * holding both of them. */
static int drain(net_meshnode_t *node, applied_t *ap) {
    int seen = 0;
    for (int i = 0; i < DELIVER_TRIES; i++) {
        for (uint16_t shard = 0; shard < 4; shard++) {
            char *json = NULL;
            size_t len = 0;
            /* A failed receive is not an empty shard. Swallowing it would make
             * "nothing was delivered" and "we never looked" the same answer. */
            int rc = net_mesh_recv_shard(node, shard, 64, &json, &len);
            if (rc != 0) {
                fprintf(stderr, "recv_shard %u failed: %d\n", shard, rc);
                if (json) net_free_string(json);
                return -1;
            }
            if (json && len > 0) seen += consume_shard_json(json, ap);
            if (json) net_free_string(json);
        }
        if (ap->has1 && ap->has2) break;
        usleep(POLL_US);
    }
    return seen;
}

/* The publish report is JSON: {"attempted":N,"delivered":N,"errors":[...]}. */
static int report_int(const char *json, const char *key) {
    char needle[32];
    snprintf(needle, sizeof needle, "\"%s\":", key);
    const char *p = json ? strstr(json, needle) : NULL;
    return p ? atoi(p + strlen(needle)) : -1;
}

static int report_has_errors(const char *json) {
    const char *p = json ? strstr(json, "\"errors\":") : NULL;
    if (!p) return 1; /* nothing to read is not evidence of success */
    p += strlen("\"errors\":");
    while (*p == ' ') p++;
    return !(p[0] == '[' && p[1] == ']');
}

/* A revision that did not reach every subscriber is not a config update. The
 * publisher's own report is the evidence, so read it instead of assuming the
 * fan-out worked. */
static int check_delivery(unsigned version, const char *json) {
    int attempted = report_int(json, "attempted");
    int delivered = report_int(json, "delivered");
    if (attempted != SUBSCRIBERS || delivered != attempted ||
        report_has_errors(json)) {
        fprintf(stderr, "v%u not delivered to every subscriber: %s\n", version,
                json ? json : "(no report)");
        return -1;
    }
    return 0;
}

int main(void) {
    net_meshnode_t *publisher = NULL, *s1 = NULL, *s2 = NULL;
    char publisher_addr[32], s1_addr[32], s2_addr[32];
    if (build(0xF1, &publisher, publisher_addr, sizeof publisher_addr) != 0) { fprintf(stderr, "build publisher failed\n"); return 1; }
    if (build(0xF2, &s1, s1_addr, sizeof s1_addr) != 0) { fprintf(stderr, "build s1 failed\n"); return 1; }
    if (build(0xF3, &s2, s2_addr, sizeof s2_addr) != 0) { fprintf(stderr, "build s2 failed\n"); return 1; }

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
    printf("published v1 to %d of %d subscribers\n", report_int(report, "delivered"),
           report_int(report, "attempted"));
    int v1_ok = check_delivery(1, report);
    if (report) { net_free_string(report); report = NULL; }
    if (v1_ok != 0) return 1;

    if (net_mesh_publish(publisher, "config/edge", (const uint8_t *)v2,
                         strlen(v2), "{\"reliability\":\"reliable\"}",
                         &report, &report_len) != 0) {
        fprintf(stderr, "publish v2 failed\n");
        return 1;
    }
    int subscribers = report_int(report, "attempted");
    printf("published v2 to %d of %d subscribers\n", report_int(report, "delivered"),
           subscribers);
    int v2_ok = check_delivery(2, report);
    if (report) { net_free_string(report); report = NULL; }
    if (v2_ok != 0) return 1;

    /* Both revisions are expected on both subscribers, so drain until each has
     * them rather than until a poll comes back quiet. */
    applied_t one = {0}, two = {0};
    if (drain(s1, &one) < 0 || drain(s2, &two) < 0) return 1;

    printf("subscriber one applied: v1=%s v2=%s\n",
           one.has1 ? one.m1 : "-", one.has2 ? one.m2 : "-");
    printf("subscriber two applied: v1=%s v2=%s\n",
           two.has1 ? two.m1 : "-", two.has2 ? two.m2 : "-");

    if (check_applied("one", &one) != 0 || check_applied("two", &two) != 0) return 1;

    int applied = (one.has2 ? 1 : 0) + (two.has2 ? 1 : 0);

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
