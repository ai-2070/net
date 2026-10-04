/*
 * rpc_callbacks.c — nRPC handlers written in C, and who releases their
 * buffers (C SDK plan, C4).
 *
 * The contract (net_rpc.h): a handler returns its response, and on failure
 * an error string, in memory it allocated with its own allocator. The
 * library copies the bytes, then releases the buffer through the
 * deallocator registered with net_rpc_set_callback_free, never with a
 * free() of its own. That registration must precede the dispatcher's.
 *
 * So this program registers a counting deallocator first, serves two
 * handlers on one node and calls them from another:
 *
 *   - "echo" returns a malloc'd, non-empty response;
 *   - "fail" returns a malloc'd error string and a non-zero code.
 *
 * Every buffer a handler allocated must be released exactly once, through
 * the registered deallocator: the counts of buffers allocated and released
 * are equal, and no pointer is released twice or released that was never
 * handed out. The refusal of a dispatcher registered without a deallocator
 * is rpc_no_free.c's: registration is first-call-wins per process.
 *
 * Built and run by .github/scripts/run-c-consumers.py.
 */

#include <stdlib.h>
#include <string.h>

#include "net.go.h"
#include "net_rpc.h"

#include "consumer_util.h"
#include "loaded_module.h"

#define MAX_BUFS 64

static cu_mutex* mu;
static void* handed[MAX_BUFS];
static int n_handed, n_released, n_bad_release;
static uint64_t echo_id, fail_id;

static void* track(void* p) {
    cu_mutex_lock(mu);
    if (p != NULL && n_handed < MAX_BUFS) {
        handed[n_handed++] = p;
    }
    cu_mutex_unlock(mu);
    return p;
}

/* The deallocator the library releases handler buffers through. */
static void counting_free(void* p) {
    int i, found = 0;
    cu_mutex_lock(mu);
    for (i = 0; i < n_handed; i++) {
        if (handed[i] == p) {
            handed[i] = NULL; /* a second release of p is then "bad" */
            found = 1;
            break;
        }
    }
    if (found) {
        n_released++;
    } else {
        n_bad_release++;
    }
    cu_mutex_unlock(mu);
    free(p);
}

static int dispatch(uint64_t handler_id, const uint8_t* req, size_t req_len, uint8_t** out_resp,
                    size_t* out_resp_len, char** out_err) {
    if (handler_id == echo_id) {
        static const char prefix[] = "echo:";
        size_t n = sizeof prefix - 1 + req_len;
        uint8_t* resp = (uint8_t*)track(malloc(n));
        if (resp == NULL) {
            return 1;
        }
        memcpy(resp, prefix, sizeof prefix - 1);
        memcpy(resp + sizeof prefix - 1, req, req_len);
        *out_resp = resp;
        *out_resp_len = n;
        return NET_RPC_OK;
    }
    if (handler_id == fail_id) {
        static const char msg[] = "the C handler refused";
        char* err = (char*)track(malloc(sizeof msg));
        if (err != NULL) {
            memcpy(err, msg, sizeof msg);
        }
        *out_err = err;
        return 1;
    }
    return 2;
}

static int counts(int* handed_out, int* released, int* bad) {
    cu_mutex_lock(mu);
    *handed_out = n_handed;
    *released = n_released;
    *bad = n_bad_release;
    cu_mutex_unlock(mu);
    return 0;
}

static int run(void) {
    net_meshnode_t *prov = NULL, *caller = NULL;
    char prov_addr[32], caller_addr[32];
    MeshRpcHandle *prov_rpc, *caller_rpc;
    ServeHandleC *echo_serve, *fail_serve;
    char* err = NULL;
    uint8_t* resp = NULL;
    size_t resp_len = 0;
    int handed_out, released, bad, i;

    mu = cu_mutex_new();
    CU_CHECK("mutex", mu != NULL);

    /* The deallocator first: the dispatcher refuses without it. */
    CU_CHECK_RC("net_rpc_set_callback_free: NULL is -1", net_rpc_set_callback_free(NULL), -1);
    CU_CHECK_RC("net_rpc_set_callback_free: the counting deallocator",
                net_rpc_set_callback_free(counting_free), 0);
    CU_CHECK_RC("net_rpc_set_handler_dispatcher", net_rpc_set_handler_dispatcher(dispatch), 0);

    CU_CHECK_RC("bring-up: provider", cu_mesh_build(0xD1, &prov, prov_addr, sizeof prov_addr), 0);
    CU_CHECK_RC("bring-up: caller", cu_mesh_build(0xD2, &caller, caller_addr, sizeof caller_addr), 0);
    CU_CHECK_RC("bring-up: handshake", cu_mesh_handshake(prov, caller, prov_addr), 0);
    CU_CHECK_RC("bring-up: start provider", net_mesh_start(prov), 0);
    CU_CHECK_RC("bring-up: start caller", net_mesh_start(caller), 0);
    prov_rpc = net_rpc_new(net_mesh_arc_clone(prov));
    caller_rpc = net_rpc_new(net_mesh_arc_clone(caller));
    CU_CHECK("net_rpc_new: both nodes", prov_rpc != NULL && caller_rpc != NULL);

    /* Reserve, store in this program's registry (echo_id / fail_id), then
     * serve: the order net_rpc.h requires. */
    echo_id = net_rpc_reserve_handler_id();
    fail_id = net_rpc_reserve_handler_id();
    CU_CHECK("reserve_handler_id: two distinct ids", echo_id != 0 && fail_id != 0 && echo_id != fail_id);
    echo_serve = net_rpc_serve(prov_rpc, "c4.echo", 7, echo_id, 5000, &err);
    CU_CHECK("net_rpc_serve: echo", echo_serve != NULL);
    fail_serve = net_rpc_serve(prov_rpc, "c4.fail", 7, fail_id, 5000, &err);
    CU_CHECK("net_rpc_serve: fail", fail_serve != NULL);

    for (i = 0; i < 3; i++) {
        CU_CHECK_RC("net_rpc_call: echo",
                    net_rpc_call(caller_rpc, net_mesh_node_id(prov), "c4.echo", 7, (const uint8_t*)"ping", 4,
                                 5000, 0, &resp, &resp_len, &err),
                    NET_RPC_OK);
        CU_CHECK("echo: the C handler's response", resp_len == 9 && memcmp(resp, "echo:ping", 9) == 0);
        net_rpc_response_free(resp, resp_len);
        resp = NULL;
    }
    CU_CHECK_RC("net_rpc_call: fail is NET_RPC_ERR_CALL_FAILED",
                net_rpc_call(caller_rpc, net_mesh_node_id(prov), "c4.fail", 7, (const uint8_t*)"x", 1, 5000,
                             0, &resp, &resp_len, &err),
                NET_RPC_ERR_CALL_FAILED);
    CU_CHECK("fail: the caller sees the handler's error text",
             err != NULL && strstr(err, "the C handler refused") != NULL);
    net_rpc_free_cstring(err);
    err = NULL;

    /* Releases happen on the handler thread before the reply is sent. */
    counts(&handed_out, &released, &bad);
    CU_CHECK_RC("buffers: three responses and one error string were handed out", handed_out, 4);
    CU_CHECK_RC("buffers: every one released through the registered deallocator", released, handed_out);
    CU_CHECK_RC("buffers: nothing released twice or never handed out", bad, 0);

    net_rpc_serve_handle_free(echo_serve);
    net_rpc_serve_handle_free(fail_serve);
    net_rpc_free(prov_rpc);
    net_rpc_free(caller_rpc);
    CU_CHECK_RC("net_mesh_shutdown: provider", net_mesh_shutdown(prov), 0);
    CU_CHECK_RC("net_mesh_shutdown: caller", net_mesh_shutdown(caller), 0);
    net_mesh_free(prov);
    net_mesh_free(caller);
    /* No handler can run any more, so nothing can reach counting_free. */
    cu_mutex_free(mu);
    mu = NULL;
    return 0;
}

int main(void) {
    static const clm_fn_t used[] = {
        CU_MESH_FNS,
        CLM_FN(net_mesh_start),
        CLM_FN(net_mesh_shutdown),
        CLM_FN(net_mesh_free),
        CLM_FN(net_mesh_arc_clone),
        CLM_FN(net_rpc_set_callback_free),
        CLM_FN(net_rpc_set_handler_dispatcher),
        CLM_FN(net_rpc_new),
        CLM_FN(net_rpc_free),
        CLM_FN(net_rpc_reserve_handler_id),
        CLM_FN(net_rpc_serve),
        CLM_FN(net_rpc_serve_handle_free),
        CLM_FN(net_rpc_call),
        CLM_FN(net_rpc_response_free),
        CLM_FN(net_rpc_free_cstring),
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
