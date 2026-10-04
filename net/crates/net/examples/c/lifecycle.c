/*
 * lifecycle.c — a mesh node's lifecycle, driven from C (C SDK plan, C2).
 *
 * Two in-process nodes go through net_mesh_new, the handshake
 * (net_mesh_accept / net_mesh_connect), net_mesh_start, net_mesh_shutdown and
 * net_mesh_free. Each check asserts the contract its function actually has,
 * cited beside it; there is no blanket rule for failures.
 *
 * Shutdown and free are different operations. net_mesh_shutdown stops the
 * node's tasks but leaves the handle usable (src/ffi/mesh.rs: shutdown runs
 * inside the handle's guard, which only net_mesh_free closes), so after it
 * the identity accessors still answer. Nothing is called on a handle after
 * net_mesh_free: no header documents that as safe, so this program does not
 * test it.
 *
 * Built and run by .github/scripts/run-c-consumers.py.
 */

#include <string.h>

#include "net.go.h"

#include "consumer_util.h"
#include "loaded_module.h"

static int run(void) {
    net_meshnode_t *a = NULL, *b = NULL, *untouched = NULL;
    char a_addr[32], b_addr[32];
    char *key = NULL, *key_after = NULL;
    size_t key_len = 0, key_after_len = 0;
    uint64_t a_id, b_id;
    char cfg[256];

    /* ---- refused configurations (src/ffi/mesh.rs, net_mesh_new) ---- */

    CU_CHECK_RC("net_mesh_new: NULL config is NET_ERR_NULL_POINTER",
                net_mesh_new(NULL, &untouched), NET_ERR_NULL_POINTER);
    CU_CHECK_RC("net_mesh_new: malformed JSON is NET_ERR_INVALID_JSON",
                net_mesh_new("{not json", &untouched), NET_ERR_INVALID_JSON);
    snprintf(cfg, sizeof cfg, "{\"bind_addr\":\"127.0.0.1:0\",\"psk_hex\":\"%s\",\"heartbeat_ms\":0}",
             CU_PSK_HEX);
    CU_CHECK_RC("net_mesh_new: a zero heartbeat is NET_ERR_INVALID_JSON",
                net_mesh_new(cfg, &untouched), NET_ERR_INVALID_JSON);
    CU_CHECK_RC("net_mesh_new: a PSK that is not 32 bytes is NET_ERR_MESH_INIT",
                net_mesh_new("{\"bind_addr\":\"127.0.0.1:0\",\"psk_hex\":\"4242\"}", &untouched),
                NET_ERR_MESH_INIT);
    CU_CHECK_RC("net_mesh_new: an unparseable bind address is NET_ERR_MESH_INIT",
                net_mesh_new("{\"bind_addr\":\"not-an-address\",\"psk_hex\":"
                             "\"4242424242424242424242424242424242424242424242424242424242424242\"}",
                             &untouched),
                NET_ERR_MESH_INIT);
    CU_CHECK("net_mesh_new: no refused call produced a handle", untouched == NULL);

    /* ---- bring-up ---- */

    CU_CHECK_RC("net_mesh_new: node A", cu_mesh_build(0xA1, &a, a_addr, sizeof a_addr), 0);
    CU_CHECK_RC("net_mesh_new: node B", cu_mesh_build(0xB2, &b, b_addr, sizeof b_addr), 0);
    a_id = net_mesh_node_id(a);
    b_id = net_mesh_node_id(b);
    CU_CHECK("net_mesh_node_id: both non-zero and distinct", a_id != 0 && b_id != 0 && a_id != b_id);
    CU_CHECK_RC("net_mesh_public_key_hex: returns 0", net_mesh_public_key_hex(a, &key, &key_len), 0);
    CU_CHECK("net_mesh_public_key_hex: 64 hex characters, length matches",
             key != NULL && key_len == 64 && strlen(key) == 64);
    CU_CHECK_RC("handshake: B connects to A, A accepts", cu_mesh_handshake(a, b, a_addr), 0);
    CU_CHECK_RC("net_mesh_start: A", net_mesh_start(a), 0);
    CU_CHECK_RC("net_mesh_start: B", net_mesh_start(b), 0);

    /* ---- argument checks (src/ffi/mesh.rs) ---- */

    CU_CHECK_RC("net_mesh_start: NULL is NET_ERR_NULL_POINTER", net_mesh_start(NULL),
                NET_ERR_NULL_POINTER);
    CU_CHECK_RC("net_mesh_shutdown: NULL is NET_ERR_NULL_POINTER", net_mesh_shutdown(NULL),
                NET_ERR_NULL_POINTER);
    CU_CHECK_RC("net_mesh_node_id: NULL is 0", (long long)net_mesh_node_id(NULL), 0);
    CU_CHECK_RC("net_mesh_public_key_hex: NULL output is NET_ERR_NULL_POINTER",
                net_mesh_public_key_hex(a, NULL, &key_after_len), NET_ERR_NULL_POINTER);

    /* ---- shutdown, then the operations with a defined result after it ---- */

    CU_CHECK_RC("net_mesh_shutdown: A", net_mesh_shutdown(a), 0);
    CU_CHECK_RC("net_mesh_shutdown: A again (idempotent, src/ffi/mesh.rs)", net_mesh_shutdown(a), 0);
    CU_CHECK_RC("after shutdown: net_mesh_node_id still names A", (long long)net_mesh_node_id(a),
                (long long)a_id);
    CU_CHECK_RC("after shutdown: net_mesh_public_key_hex still answers",
                net_mesh_public_key_hex(a, &key_after, &key_after_len), 0);
    CU_CHECK("after shutdown: the same public key",
             key_after != NULL && key_after_len == key_len && strcmp(key, key_after) == 0);
    CU_CHECK_RC("net_mesh_shutdown: B", net_mesh_shutdown(b), 0);

    /* ---- free ---- */

    net_free_string(key);
    net_free_string(key_after);
    /* net.go.h: "String to free (may be NULL)". */
    net_free_string(NULL);
    CU_CHECK("net_free_string: NULL is accepted", 1);
    net_mesh_free(a);
    net_mesh_free(b);
    CU_CHECK("net_mesh_free: both nodes freed after shutdown", 1);
    return 0;
}

int main(void) {
    static const clm_fn_t used[] = {
        CU_MESH_FNS,
        CLM_FN(net_mesh_start),
        CLM_FN(net_mesh_shutdown),
        CLM_FN(net_mesh_free),
    };
    if (clm_check_loaded_module(used, sizeof used / sizeof used[0]) != 0) {
        return 2;
    }
    if (cu_net_init() != 0) {
        printf("FAIL socket layer init\n");
        return 1;
    }
    if (run() != 0) {
        return 1;
    }
    return cu_finish();
}
