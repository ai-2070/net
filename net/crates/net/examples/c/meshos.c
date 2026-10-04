/*
 * meshos.c — the MeshOS daemon-author SDK, from C (net_meshos.h).
 *
 * The SDK starts its own supervisor runtime. Two daemons register: a
 * lifecycle-only no-op daemon, and one with a C vtable registered through
 * the _v2 call, whose `destroy` tells this program when the library is
 * finished with its context. Checked against the header:
 *
 *   - handle accessors: the daemon id is the seed's origin hash and stable,
 *     the name is the one registered;
 *   - control receive: an empty channel is NET_MESHOS_CONTROL_NONE, and a
 *     bounded wait times out to NONE;
 *   - publish_log at every level; an unknown level is refused;
 *   - metadata: JSON naming this daemon, before and after a refresh;
 *   - publish_capabilities (a documented stub) accepts tags and a clear;
 *   - graceful shutdown consumes the handle: later calls are
 *     NET_MESHOS_ERR_ALREADY_SHUTDOWN, and metadata is NULL;
 *   - the vtable daemon's `destroy` runs exactly once, after teardown;
 *   - registration without a `process` callback is refused;
 *   - SDK shutdown consumes the SDK the same way, and the dropped-control
 *     counter reads UINT64_MAX after it;
 *   - every free accepts NULL.
 *
 * `process` and `snapshot` are not reached: net_meshos.h has no way to
 * deliver an event to a daemon from C.
 */

#include <stdio.h>
#include <string.h>

#include "net_meshos.h"

#include "consumer_util.h"
#include "loaded_module.h"

static cu_mutex* lock;
static int destroyed;
static int ctx_token = 42;

static int on_process(void* ctx, NetMeshOsProcessEmitCtx* emit, uint64_t origin, uint64_t seq,
                      const uint8_t* payload, size_t len) {
    (void)ctx, (void)origin, (void)seq;
    net_meshos_process_emit(emit, payload, len);
    return 0;
}

static int on_health(void* ctx) {
    (void)ctx;
    return NET_MESHOS_HEALTH_HEALTHY;
}

static void on_destroy(void* ctx) {
    cu_mutex_lock(lock);
    if (ctx == &ctx_token) {
        destroyed++;
    }
    cu_mutex_unlock(lock);
}

static int destroy_count(void) {
    int n;
    cu_mutex_lock(lock);
    n = destroyed;
    cu_mutex_unlock(lock);
    return n;
}

int main(void) {
    static const clm_fn_t used[] = {
        CLM_FN(net_meshos_sdk_start),          CLM_FN(net_meshos_sdk_shutdown),
        CLM_FN(net_meshos_sdk_free),           CLM_FN(net_meshos_register_daemon),
        CLM_FN(net_meshos_register_daemon_with_vtable_v2), CLM_FN(net_meshos_handle_daemon_id),
        CLM_FN(net_meshos_next_control),       CLM_FN(net_meshos_publish_log),
        CLM_FN(net_meshos_metadata),           CLM_FN(net_meshos_graceful_shutdown),
        CLM_FN(net_meshos_handle_free),        CLM_FN(net_meshos_last_error_kind),
        CLM_FN(net_meshos_free_string),
        CLM_FN(net_meshos_clear_last_error),
        CLM_FN(net_meshos_handle_daemon_name),
        CLM_FN(net_meshos_process_emit),
        CLM_FN(net_meshos_publish_capabilities),
        CLM_FN(net_meshos_refresh_metadata),
        CLM_FN(net_meshos_register_daemon_with_vtable),
        CLM_FN(net_meshos_sdk_dropped_control_events),
        CLM_FN(net_meshos_try_next_control),
    };
    uint8_t seed_a[32], seed_b[32];
    NetMeshOsSdk* sdk = NULL;
    NetMeshOsHandle *plain = NULL, *daemon = NULL, *refused = NULL;
    NetMeshOsDaemonVtable vt, no_process;
    NetMeshOsDaemonControl ctl;
    char want[64];
    char* meta;
    uint64_t id;
    int level, waited;

    if (clm_check_loaded_module(used, sizeof used / sizeof used[0]) != 0) {
        return 2;
    }
    lock = cu_mutex_new();
    CU_CHECK("a mutex for the destroy count", lock != NULL);
    memset(seed_a, 0x31, sizeof seed_a);
    memset(seed_b, 0x32, sizeof seed_b);

    CU_CHECK_RC("net_meshos_sdk_start: defaults", net_meshos_sdk_start(0, 50, 0, 0, 0, &sdk), NET_MESHOS_OK);
    CU_CHECK("net_meshos_sdk_dropped_control_events: 0", net_meshos_sdk_dropped_control_events(sdk) == 0);

    CU_CHECK_RC("net_meshos_register_daemon: the no-op daemon",
                net_meshos_register_daemon(sdk, "c-plain", 7, seed_a, &plain), NET_MESHOS_OK);
    id = net_meshos_handle_daemon_id(plain);
    CU_CHECK("net_meshos_handle_daemon_id: non-zero", id != 0);
    CU_CHECK("net_meshos_handle_daemon_name", net_meshos_handle_daemon_name(plain) != NULL &&
                                                   strcmp(net_meshos_handle_daemon_name(plain), "c-plain") == 0);

    memset(&vt, 0, sizeof vt);
    vt.process = on_process;
    vt.health = on_health;
    CU_CHECK_RC("net_meshos_register_daemon_with_vtable_v2: a C daemon with destroy",
                net_meshos_register_daemon_with_vtable_v2(sdk, "c-daemon", 8, seed_b, &vt, &ctx_token, on_destroy,
                                                          &daemon),
                NET_MESHOS_OK);
    CU_CHECK("vtable daemon: its own id", net_meshos_handle_daemon_id(daemon) != 0 &&
                                              net_meshos_handle_daemon_id(daemon) != id);
    memset(&no_process, 0, sizeof no_process);
    CU_CHECK("register: a vtable without process is refused",
             net_meshos_register_daemon_with_vtable(sdk, "c-bad", 5, seed_b, &no_process, NULL, &refused) !=
                 NET_MESHOS_OK);
    CU_CHECK("register refused: no handle", refused == NULL);

    memset(&ctl, 0xFF, sizeof ctl);
    CU_CHECK_RC("net_meshos_try_next_control", net_meshos_try_next_control(plain, &ctl), NET_MESHOS_OK);
    CU_CHECK_RC("try_next_control: an empty channel is NONE", ctl.kind, NET_MESHOS_CONTROL_NONE);
    memset(&ctl, 0xFF, sizeof ctl);
    CU_CHECK_RC("net_meshos_next_control: bounded wait", net_meshos_next_control(plain, 100, &ctl), NET_MESHOS_OK);
    CU_CHECK_RC("next_control: a timeout is NONE", ctl.kind, NET_MESHOS_CONTROL_NONE);

    for (level = NET_MESHOS_LOG_TRACE; level <= NET_MESHOS_LOG_ERROR; level++) {
        char label[64];
        snprintf(label, sizeof label, "net_meshos_publish_log: level %d", level);
        CU_CHECK_RC(label, net_meshos_publish_log(plain, level, "from C", 6), NET_MESHOS_OK);
    }
    CU_CHECK("net_meshos_publish_log: an unknown level is refused",
             net_meshos_publish_log(plain, 77, "x", 1) != NET_MESHOS_OK);
    CU_CHECK("the refusal sets the last error kind", net_meshos_last_error_kind() != NULL);
    net_meshos_clear_last_error();
    CU_CHECK("net_meshos_clear_last_error", net_meshos_last_error_kind() == NULL);

    snprintf(want, sizeof want, "\"daemon_id\":%llu", (unsigned long long)id);
    meta = net_meshos_metadata(plain);
    CU_CHECK("net_meshos_metadata: JSON naming this daemon",
             meta != NULL && strstr(meta, want) != NULL && strstr(meta, "\"daemon_name\":\"c-plain\"") != NULL);
    net_meshos_free_string(meta);
    meta = net_meshos_refresh_metadata(plain);
    CU_CHECK("net_meshos_refresh_metadata: the same daemon", meta != NULL && strstr(meta, want) != NULL);
    net_meshos_free_string(meta);
    CU_CHECK_RC("net_meshos_publish_capabilities: tags",
                net_meshos_publish_capabilities(plain, "[\"hardware.gpu\"]", 16), NET_MESHOS_OK);
    CU_CHECK_RC("net_meshos_publish_capabilities: clear", net_meshos_publish_capabilities(plain, NULL, 0),
                NET_MESHOS_OK);

    CU_CHECK_RC("net_meshos_graceful_shutdown", net_meshos_graceful_shutdown(plain, 200), NET_MESHOS_OK);
    CU_CHECK("after graceful shutdown: the id is stable", net_meshos_handle_daemon_id(plain) == id);
    CU_CHECK_RC("after graceful shutdown: publish_log is NET_MESHOS_ERR_ALREADY_SHUTDOWN",
                net_meshos_publish_log(plain, NET_MESHOS_LOG_INFO, "x", 1), NET_MESHOS_ERR_ALREADY_SHUTDOWN);
    CU_CHECK("after graceful shutdown: metadata is NULL", net_meshos_metadata(plain) == NULL);
    CU_CHECK_RC("graceful shutdown again: NET_MESHOS_ERR_ALREADY_SHUTDOWN", net_meshos_graceful_shutdown(plain, 0),
                NET_MESHOS_ERR_ALREADY_SHUTDOWN);
    net_meshos_handle_free(plain);

    CU_CHECK_RC("destroy: not before teardown", destroy_count(), 0);
    CU_CHECK_RC("graceful shutdown: the vtable daemon", net_meshos_graceful_shutdown(daemon, 200), NET_MESHOS_OK);
    net_meshos_handle_free(daemon);
    for (waited = 0; waited < 5000 && destroy_count() == 0; waited += 50) {
        cu_sleep_ms(50);
    }
    CU_CHECK_RC("destroy: ran exactly once, with this program's context", destroy_count(), 1);

    CU_CHECK_RC("net_meshos_sdk_shutdown", net_meshos_sdk_shutdown(sdk), NET_MESHOS_OK);
    CU_CHECK_RC("net_meshos_sdk_shutdown: again is NET_MESHOS_ERR_ALREADY_SHUTDOWN", net_meshos_sdk_shutdown(sdk),
                NET_MESHOS_ERR_ALREADY_SHUTDOWN);
    CU_CHECK("dropped_control_events: UINT64_MAX after shutdown",
             net_meshos_sdk_dropped_control_events(sdk) == UINT64_MAX);
    net_meshos_sdk_free(sdk);
    CU_CHECK_RC("destroy: still exactly once", destroy_count(), 1);

    net_meshos_sdk_free(NULL);
    net_meshos_handle_free(NULL);
    net_meshos_free_string(NULL);
    CU_CHECK("every free accepts NULL", 1);
    cu_mutex_free(lock);
    return cu_finish();
}
