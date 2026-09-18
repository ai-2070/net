/*
 * The object store you no longer run (C).
 *
 * Two in-process mesh nodes over loopback UDP and a content-addressed blob
 * store: a producer stores bytes and mints an address, a second node fetches
 * them by that address, and storing the same bytes again produces the same
 * address. There is no bucket to create, no region to pick and no replication
 * factor to configure — the address *is* the data.
 *
 * Mirrors examples/objectstore.rs. The Rust `local_addr()` has no C binding,
 * so each node binds a chosen free loopback port instead of ":0".
 *
 * Build: gcc objectstore.c -lnet -lpthread -ldl -lm && ./a.out
 *
 * Expected final line: RESULT ok dedup=1 readback=1 bytes=64
 */

#include "net.go.h"
#include "net_transport.h"

#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static const char *PSK_HEX =
    "4242424242424242424242424242424242424242424242424242424242424242";

/* A fixed-size payload, so the byte count in the result line is deterministic. */
static const unsigned char PAYLOAD[64] = {
    0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A,
    0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A,
    0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A,
    0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A,
    0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A, 0x5A,
    0x5A, 0x5A, 0x5A, 0x5A};

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

static void print_hex(const char *label, const uint8_t *bytes, size_t n) {
    printf("%s", label);
    for (size_t i = 0; i < n; i++) printf("%02x", bytes[i]);
    printf("\n");
}

int main(void) {
    net_meshnode_t *holder = NULL, *reader = NULL;
    if (build(0x91, 39041, &holder) != 0) { fprintf(stderr, "build holder failed\n"); return 1; }
    if (build(0x92, 39042, &reader) != 0) { fprintf(stderr, "build reader failed\n"); return 1; }

    const char *holder_addr = "127.0.0.1:39041";
    if (handshake(holder, reader, holder_addr) != 0) {
        fprintf(stderr, "holder<->reader failed\n");
        return 1;
    }

    net_mesh_start(holder);
    net_mesh_start(reader);

    uint64_t holder_id = net_mesh_node_id(holder);

    /* A blob store backed by a local log. Each node that serves or fetches
     * blobs installs the transfer subprotocol over its own adapter. */
    net_redex_t *redex_h = net_redex_new(NULL);
    net_redex_t *redex_r = net_redex_new(NULL);
    net_mesh_blob_adapter_t *store_h =
        net_mesh_blob_adapter_new(redex_h, "objects", 0, NULL);
    net_mesh_blob_adapter_t *store_r =
        net_mesh_blob_adapter_new(redex_r, "objects", 0, NULL);
    if (!redex_h || !redex_r || !store_h || !store_r) {
        fprintf(stderr, "blob adapter setup failed\n");
        return 1;
    }

    /* Install the blob-transfer engine on both nodes before any store or
     * fetch. A fetch needs it just as much as a serve does. */
    if (net_serve_blob_transfer(holder, store_h) != NET_TRANSPORT_OK ||
        net_serve_blob_transfer(reader, store_r) != NET_TRANSPORT_OK) {
        fprintf(stderr, "serve_blob_transfer failed\n");
        return 1;
    }

    /* Store the bytes and mint the address. The address is a BLAKE3 hash of
     * the content — the producer never names a bucket, a key or a path. */
    const char *uri = "mesh:orders/2026-09/payload";
    uint8_t *ref = NULL;
    size_t ref_len = 0;
    if (net_mesh_blob_adapter_publish(store_h, (const uint8_t *)uri, strlen(uri),
                                      PAYLOAD, sizeof PAYLOAD, &ref, &ref_len) != 0) {
        fprintf(stderr, "publish failed\n");
        return 1;
    }
    printf("stored %zu bytes\n", sizeof PAYLOAD);

    uint8_t address[32];
    if (net_blob_ref_hash(ref, ref_len, address) != 0) {
        fprintf(stderr, "net_blob_ref_hash failed\n");
        return 1;
    }
    print_hex("minted address:           ", address, sizeof address);

    /* Read your own write. The caller does not have to know which node the
     * bytes settled on — the address resolves through the mesh. */
    uint8_t *out = NULL;
    size_t out_len = 0;
    if (net_fetch_blob(reader, holder_id, address, &out, &out_len) !=
        NET_TRANSPORT_OK) {
        fprintf(stderr, "fetch_blob failed\n");
        return 1;
    }
    int same = (out_len == sizeof PAYLOAD && out &&
                memcmp(out, PAYLOAD, sizeof PAYLOAD) == 0);

    /* Content addressing means the second store is a no-op that produces the
     * same address: identical bytes cannot occupy two identities.
     *
     * Note: the URI is part of the encoded ref, so the address identity here
     * is "same uri + same bytes => same ref". */
    uint8_t *ref2 = NULL;
    size_t ref2_len = 0;
    if (net_mesh_blob_adapter_publish(store_r, (const uint8_t *)uri, strlen(uri),
                                      PAYLOAD, sizeof PAYLOAD, &ref2,
                                      &ref2_len) != 0) {
        fprintf(stderr, "second publish failed\n");
        return 1;
    }
    int dedup = (ref_len == ref2_len && memcmp(ref, ref2, ref_len) == 0) ? 1 : 0;

    printf("read back from the mesh:  %zu bytes\n", out_len);
    printf("same bytes:               %s\n", same ? "true" : "false");
    printf("storing them again:       same address = %s\n",
           dedup == 1 ? "true" : "false");

    printf("RESULT ok dedup=%d readback=%d bytes=%zu\n", dedup, same,
           sizeof PAYLOAD);

    net_transport_free_buffer(out, out_len);
    net_blob_free_buffer(ref, ref_len);
    net_blob_free_buffer(ref2, ref2_len);
    net_mesh_blob_adapter_free(store_h);
    net_mesh_blob_adapter_free(store_r);
    net_redex_free(redex_h);
    net_redex_free(redex_r);

    net_mesh_shutdown(holder);
    net_mesh_shutdown(reader);
    net_mesh_free(holder);
    net_mesh_free(reader);
    return 0;
}
