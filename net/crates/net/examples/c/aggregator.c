/*
 * aggregator.c — the aggregator clients from C: the registry client and
 * the fold-query client, and the channel-visibility setter (net.h,
 * "Aggregator").
 *
 * This translation unit includes net.h ONLY. net.h and net.go.h share the
 * NET_SDK_H guard and net_mesh_new lives in net.go.h, so the nodes come
 * from support/mesh_opaque.c, a separate unit, as `void*` — the split the
 * C docs prescribe. (It cannot use consumer_util.h either, which includes
 * net.go.h, so it carries its own check macros.)
 *
 * Both clients are client-side only: serving them is the aggregator
 * daemon's job (net-aggregator-daemon), which no C program can stand up.
 * So this checks the clients' contract against a connected peer that
 * serves neither service: each operation returns NULL / -1 with a typed
 * error kind, and the handle's last-error detail explains it. A
 * successful query is not exercised from C (the matrix says so).
 *
 * Also: net_register_channel's visibility tiers, the clients' NULL-mesh
 * refusal, the cache controls, and every free on NULL.
 */

#include <stdio.h>
#include <string.h>

#include "net.h"

#include "loaded_module.h"
#include "mesh_opaque.h"

static int checks = 0;

#define CHECK(name, cond)                                                     \
    do {                                                                      \
        if (!(cond)) {                                                        \
            printf("FAIL %s (%s:%d)\n", name, __FILE__, __LINE__);            \
            fflush(stdout);                                                   \
            return 1;                                                         \
        }                                                                     \
        printf("ok %s\n", name);                                              \
        checks++;                                                             \
    } while (0)

int main(void) {
    static const clm_fn_t used[] = {
        CLM_FN(net_registry_client_new),     CLM_FN(net_registry_client_list),
        CLM_FN(net_registry_client_spawn),   CLM_FN(net_registry_client_unregister),
        CLM_FN(net_registry_last_error_detail), CLM_FN(net_registry_client_free),
        CLM_FN(net_fold_query_client_new),   CLM_FN(net_fold_query_client_query_latest),
        CLM_FN(net_fold_query_client_query_summarize_now), CLM_FN(net_fold_query_last_error_detail),
        CLM_FN(net_fold_query_client_free),  CLM_FN(net_register_channel),
        CLM_FN(net_free_string),
        CLM_FN(net_fold_query_client_invalidate_cache),
        CLM_FN(net_fold_query_client_invalidate_target),
        CLM_FN(net_fold_query_client_set_deadline),
        CLM_FN(net_fold_query_client_set_ttl),
        CLM_FN(net_registry_client_set_deadline),
    };
    void *a = NULL, *b = NULL;
    net_registry_client_handle_t* reg;
    net_fold_query_client_handle_t* fq;
    uint64_t b_id;
    char* out;
    char* detail;
    int kind;

    if (clm_check_loaded_module(used, sizeof used / sizeof used[0]) != 0) {
        return 2;
    }
    CHECK("net_registry_client_new: NULL mesh is NULL", net_registry_client_new(NULL) == NULL);
    CHECK("net_fold_query_client_new: NULL mesh is NULL", net_fold_query_client_new(NULL) == NULL);
    CHECK("bring-up: two connected, started nodes (in another translation unit)", cu_opaque_pair(0xC7, 0xC8, &a, &b) == 0);
    b_id = cu_opaque_node_id(b);

    reg = net_registry_client_new(a);
    CHECK("net_registry_client_new", reg != NULL);
    net_registry_client_set_deadline(reg, 2000);
    kind = -1;
    out = net_registry_client_list(reg, b_id, &kind);
    CHECK("net_registry_client_list: a peer serving no registry is NULL", out == NULL);
    CHECK("net_registry_client_list: kind NET_REGISTRY_ERR_TRANSPORT", kind == NET_REGISTRY_ERR_TRANSPORT);
    detail = net_registry_last_error_detail(reg);
    CHECK("net_registry_last_error_detail: says why (caller-owned)", detail != NULL && strlen(detail) > 0);
    net_free_string(detail);
    kind = -1;
    out = net_registry_client_spawn(reg, b_id, "tpl", "grp", 1, &kind);
    CHECK("net_registry_client_spawn: NULL, NET_REGISTRY_ERR_TRANSPORT", out == NULL && kind == NET_REGISTRY_ERR_TRANSPORT);
    kind = -1;
    CHECK("net_registry_client_unregister: -1", net_registry_client_unregister(reg, b_id, "grp", &kind) == -1);
    CHECK("net_registry_client_unregister: kind NET_REGISTRY_ERR_TRANSPORT", kind == NET_REGISTRY_ERR_TRANSPORT);

    fq = net_fold_query_client_new(a);
    CHECK("net_fold_query_client_new", fq != NULL);
    net_fold_query_client_set_deadline(fq, 2000);
    net_fold_query_client_set_ttl(fq, 1000);
    kind = -1;
    out = net_fold_query_client_query_latest(fq, b_id, 1, &kind);
    CHECK("net_fold_query_client_query_latest: a peer serving no folds is NULL", out == NULL);
    CHECK("net_fold_query_client_query_latest: kind NET_REGISTRY_ERR_TRANSPORT", kind == NET_REGISTRY_ERR_TRANSPORT);
    detail = net_fold_query_last_error_detail(fq);
    CHECK("net_fold_query_last_error_detail: says why (caller-owned)", detail != NULL && strlen(detail) > 0);
    net_free_string(detail);
    kind = -1;
    out = net_fold_query_client_query_summarize_now(fq, b_id, 1, &kind);
    CHECK("net_fold_query_client_query_summarize_now: NULL, NET_REGISTRY_ERR_TRANSPORT",
          out == NULL && kind == NET_REGISTRY_ERR_TRANSPORT);

    CHECK("net_register_channel: global", net_register_channel(a, "c/agg-global", NET_VISIBILITY_GLOBAL) == NET_REGISTRY_OK);
    CHECK("net_register_channel: subnet-local",
          net_register_channel(a, "c/agg-local", NET_VISIBILITY_SUBNET_LOCAL) == NET_REGISTRY_OK);
    CHECK("net_register_channel: an unknown visibility is NET_REGISTRY_ERR_INVALID_ARGS",
          net_register_channel(a, "c/agg-bad", 42) == NET_REGISTRY_ERR_INVALID_ARGS);
    CHECK("net_register_channel: NULL mesh is NET_REGISTRY_ERR_INVALID_ARGS",
          net_register_channel(NULL, "c/agg-null", NET_VISIBILITY_GLOBAL) == NET_REGISTRY_ERR_INVALID_ARGS);

    net_fold_query_client_invalidate_target(fq, b_id);
    net_fold_query_client_invalidate_cache(fq);
    net_fold_query_client_free(fq);
    net_registry_client_free(reg);
    net_fold_query_client_free(NULL);
    net_registry_client_free(NULL);
    CHECK("both clients' free accepts NULL", 1);
    CHECK("teardown A", cu_opaque_teardown(a) == 0);
    CHECK("teardown B", cu_opaque_teardown(b) == 0);
    printf("NET-CHECKS: %d\n", checks);
    /* LeakSanitizer reports at exit and leaves through _exit, which does not
     * flush stdio: without this, the sanitizer lane sees no NET-CHECKS. */
    fflush(stdout);
    return 0;
}
