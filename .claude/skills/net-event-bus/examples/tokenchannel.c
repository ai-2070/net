/*
 * Scoped credentials, not an open channel (C).
 *
 * Two nodes. The publisher owns a channel whose subscriber ACL is rooted at
 * its own entity id; a subscriber holding a token minted for *its* entity id,
 * for *this* channel, with the subscribe scope is admitted. The same
 * subscriber asking without the token is refused.
 *
 * This is the shape a broker makes you build out of ACL files and a separate
 * auth service: there is one identity, one token, one place the decision is
 * made, and the credential is presented per subscribe rather than cached by a
 * connection.
 *
 * Mirrors examples/tokenchannel.rs. The Rust `local_addr()` has no C binding,
 * so each node binds a chosen free loopback port instead of ":0".
 *
 * Build: gcc tokenchannel.c -lnet -lpthread -ldl -lm && ./a.out
 *
 * Expected final line: RESULT ok granted=1 refused=1
 */

#include "net.go.h"

#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

/* 32 bytes exactly — a PSK, not a passphrase. Every node in a mesh shares it. */
static const char *PSK_HEX =
    "4242424242424242424242424242424242424242424242424242424242424242";

/* The token's lifetime. Long enough for the example, short enough that a
 * minted credential is never a standing secret. */
#define TOKEN_TTL_SECONDS 300

static void seed_bytes(uint8_t out[32], unsigned char b) {
    memset(out, b, 32);
}

static void hex_of(const uint8_t *bytes, size_t n, char *out) {
    for (size_t i = 0; i < n; i++) sprintf(out + i * 2, "%02x", bytes[i]);
    out[n * 2] = '\0';
}

/* Bind a node to a chosen loopback port with a distinct identity seed. */
static int build(const uint8_t seed[32], unsigned port, net_meshnode_t **out) {
    char seed_hex[65];
    hex_of(seed, 32, seed_hex);
    char cfg[512];
    snprintf(cfg, sizeof cfg,
             "{\"bind_addr\":\"127.0.0.1:%u\",\"psk_hex\":\"%s\","
             "\"identity_seed_hex\":\"%s\"}",
             port, PSK_HEX, seed_hex);
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

int main(void) {
    const char *channel = "config/gated";
    const unsigned PORT_PUBLISHER = 39051, PORT_SUBSCRIBER = 39052;
    const char *publisher_addr = "127.0.0.1:39051";

    uint8_t publisher_seed[32], subscriber_seed[32];
    seed_bytes(publisher_seed, 0xE1);
    seed_bytes(subscriber_seed, 0xE2);

    net_identity_t *publisher_identity = NULL, *subscriber_identity = NULL;
    if (net_identity_from_seed(publisher_seed, 32, &publisher_identity) != 0) {
        fprintf(stderr, "publisher identity failed\n");
        return 1;
    }
    if (net_identity_from_seed(subscriber_seed, 32, &subscriber_identity) != 0) {
        fprintf(stderr, "subscriber identity failed\n");
        return 1;
    }

    net_meshnode_t *publisher = NULL, *subscriber = NULL;
    if (build(publisher_seed, PORT_PUBLISHER, &publisher) != 0) {
        fprintf(stderr, "build publisher failed\n");
        return 1;
    }
    if (build(subscriber_seed, PORT_SUBSCRIBER, &subscriber) != 0) {
        fprintf(stderr, "build subscriber failed\n");
        return 1;
    }

    if (handshake(publisher, subscriber, publisher_addr) != 0) {
        fprintf(stderr, "publisher<->subscriber failed\n");
        return 1;
    }
    net_mesh_start(publisher);
    net_mesh_start(subscriber);

    /* Nothing else to set up. A token's leaf binds to the subscribing peer's
     * EntityId, and the runtime establishes that binding as part of the
     * token-bearing subscribe itself — a bounded, session-bound identity
     * proof over the encrypted session. The subscriber advertises no
     * capabilities and queries no discovery index; a consumer should not
     * have to publish services to use a credential issued to it. */

    /* The channel's subscriber ACL is rooted at the publisher's own entity id.
     * `token_roots` is what turns token enforcement on, so this is one
     * declaration rather than a flag plus a trust anchor that can disagree. */
    uint8_t publisher_entity[32];
    if (net_identity_entity_id(publisher_identity, publisher_entity) != 0) {
        fprintf(stderr, "publisher entity id failed\n");
        return 1;
    }
    char publisher_entity_hex[65];
    hex_of(publisher_entity, 32, publisher_entity_hex);

    char channel_cfg[256];
    snprintf(channel_cfg, sizeof channel_cfg,
             "{\"name\":\"%s\",\"visibility\":\"global\","
             "\"token_roots\":[\"%s\"]}",
             channel, publisher_entity_hex);
    if (net_mesh_register_channel(publisher, channel_cfg) != 0) {
        fprintf(stderr, "register_channel failed\n");
        return 1;
    }
    printf("channel gated on a token rooted at 0x%llx\n",
           (unsigned long long)net_identity_origin_hash(publisher_identity));

    /* A credential scoped three ways: to this subscriber's entity id, to this
     * channel, and to the subscribe action alone. It cannot publish, and it is
     * useless to any other node. */
    uint8_t subscriber_entity[32];
    if (net_identity_entity_id(subscriber_identity, subscriber_entity) != 0) {
        fprintf(stderr, "subscriber entity id failed\n");
        return 1;
    }
    uint8_t *token = NULL;
    size_t token_len = 0;
    if (net_identity_issue_token(publisher_identity, subscriber_entity, 32,
                                 "[\"subscribe\"]", channel,
                                 TOKEN_TTL_SECONDS, 0, &token, &token_len) != 0 ||
        !token) {
        fprintf(stderr, "issue_token failed\n");
        return 1;
    }
    printf("issued a subscribe-only token to the subscriber\n");

    uint64_t publisher_node_id = net_mesh_node_id(publisher);

    /* Without it: refused. The publisher answers and says no, which is a
     * different outcome from "the publisher never answered". */
    int refused =
        net_mesh_subscribe_channel(subscriber, publisher_node_id, channel) != 0;
    printf("bare subscribe refused:            %s\n",
           refused ? "true" : "false");

    /* With it: admitted. The credential is presented on the subscribe request
     * itself, not negotiated once per connection. */
    int granted = net_mesh_subscribe_channel_with_token(
                      subscriber, publisher_node_id, channel, token,
                      token_len) == 0;
    printf("token-carrying subscribe admitted: %s\n",
           granted ? "true" : "false");

    printf("RESULT ok granted=%d refused=%d\n", granted, refused);

    net_free_bytes(token, token_len);
    net_mesh_shutdown(publisher);
    net_mesh_shutdown(subscriber);
    net_mesh_free(publisher);
    net_mesh_free(subscriber);
    net_identity_free(publisher_identity);
    net_identity_free(subscriber_identity);
    return 0;
}
