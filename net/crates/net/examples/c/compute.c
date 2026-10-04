/*
 * compute.c — daemons written in C, run by the compute runtime, and the
 * groups built on it (net.go.h, "Compute" and "Groups").
 *
 * The daemon is a counter: each event increments its count and emits
 * "n=<count>:<payload>". Its code lives here, behind the dispatcher the
 * runtime calls back into (process / snapshot / restore / free / factory),
 * so this checks the callback contract the Go binding relies on:
 *
 *   - the callback deallocator is registered first
 *     (net_compute_set_callback_free); the dispatcher needs it;
 *   - a spawned daemon's events reach `process`, and its outputs come
 *     back through net_compute_runtime_deliver;
 *   - a snapshot is taken through `snapshot` (a buffer this program
 *     malloc'd, released by the library through the registered free);
 *   - spawn-from-snapshot hands the state to `restore` for the new
 *     daemon, which continues the count;
 *   - a factory-registered kind is reconstructed through `factory`;
 *   - `free` runs when a daemon is dropped.
 *
 * Then the runtime's documented refusals (not ready before start, a
 * duplicate kind, an unknown origin, corrupt snapshot bytes), and a fork
 * group and a replica group of the C daemon on this one node: counts,
 * parent origin, fork sequence, lineage, routing to a live member. Every
 * err_out string is freed with net_compute_free_cstring.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "net.go.h"

#include "consumer_util.h"
#include "loaded_module.h"

#define MAX_DAEMONS 64

static cu_mutex* lock;
static uint64_t counts[MAX_DAEMONS];
static int live[MAX_DAEMONS];
static uint64_t next_daemon_id = 1;
static int frees, restores, factory_calls;
static unsigned long callback_frees;

static void callback_free(void* p) {
    callback_frees++;
    free(p);
}

static int on_process(uint64_t daemon_id, uint64_t origin, uint64_t seq, const uint8_t* payload,
                      size_t payload_len, net_compute_outputs_t* outputs) {
    char prefix[32];
    unsigned char* out;
    int n, rc;
    uint64_t count;
    (void)origin, (void)seq;
    if (daemon_id >= MAX_DAEMONS) {
        return -1;
    }
    cu_mutex_lock(lock);
    count = ++counts[daemon_id];
    cu_mutex_unlock(lock);
    /* "n=<count>:" then the payload, sized exactly: a payload of any length
     * is emitted whole, and nothing is read past a buffer. */
    n = snprintf(prefix, sizeof prefix, "n=%llu:", (unsigned long long)count);
    if (n < 0 || (size_t)n >= sizeof prefix) {
        return -1;
    }
    out = (unsigned char*)malloc((size_t)n + payload_len + 1);
    if (!out) {
        return -1;
    }
    memcpy(out, prefix, (size_t)n);
    if (payload_len) {
        memcpy(out + n, payload, payload_len);
    }
    rc = net_compute_outputs_push(outputs, out, (size_t)n + payload_len);
    free(out);
    return rc;
}

static int on_snapshot(uint64_t daemon_id, uint8_t** out_ptr, size_t* out_len) {
    uint8_t* buf;
    if (daemon_id >= MAX_DAEMONS) {
        return -1;
    }
    buf = (uint8_t*)malloc(sizeof(uint64_t));
    if (!buf) {
        return -1;
    }
    cu_mutex_lock(lock);
    memcpy(buf, &counts[daemon_id], sizeof(uint64_t));
    cu_mutex_unlock(lock);
    *out_ptr = buf;
    *out_len = sizeof(uint64_t);
    return 0;
}

static int on_restore(uint64_t daemon_id, const uint8_t* state, size_t state_len) {
    if (daemon_id >= MAX_DAEMONS || state_len != sizeof(uint64_t)) {
        return -1;
    }
    cu_mutex_lock(lock);
    memcpy(&counts[daemon_id], state, sizeof(uint64_t));
    restores++;
    cu_mutex_unlock(lock);
    return 0;
}

static void on_free(uint64_t daemon_id) {
    cu_mutex_lock(lock);
    if (daemon_id < MAX_DAEMONS) {
        live[daemon_id] = 0;
    }
    frees++;
    cu_mutex_unlock(lock);
}

static uint64_t new_daemon(void) {
    uint64_t id;
    cu_mutex_lock(lock);
    id = next_daemon_id++;
    if (id < MAX_DAEMONS) {
        live[id] = 1;
        counts[id] = 0;
    }
    cu_mutex_unlock(lock);
    return id;
}

static int on_factory(uint64_t runtime_id, const char* kind, size_t kind_len, uint64_t* out_daemon_id) {
    (void)runtime_id;
    if (!out_daemon_id || kind_len != 7 || memcmp(kind, "counter", 7) != 0) {
        return -1;
    }
    *out_daemon_id = new_daemon();
    cu_mutex_lock(lock);
    factory_calls++;
    cu_mutex_unlock(lock);
    return *out_daemon_id < MAX_DAEMONS ? 0 : -1;
}

/* Deliver one event; 0 and the single output copied to `out` on success. */
static int deliver(net_compute_runtime_t* rt, uint64_t origin, uint64_t seq, const char* payload,
                   char* out, size_t out_len) {
    net_compute_outputs_t* outputs = NULL;
    char* err = NULL;
    const uint8_t* p = NULL;
    size_t len = 0;
    int rc = net_compute_runtime_deliver(rt, origin, 0x5EED, seq, (const uint8_t*)payload, strlen(payload),
                                         &outputs, &err);
    net_compute_free_cstring(err);
    if (rc != 0) {
        return rc;
    }
    if (net_compute_outputs_len(outputs) != 1 || net_compute_outputs_at(outputs, 0, &p, &len) != 0 ||
        len >= out_len) {
        net_compute_outputs_free(outputs);
        return -100;
    }
    memcpy(out, p, len);
    out[len] = '\0';
    net_compute_outputs_free(outputs);
    return 0;
}

static int error_mentions(int rc, char* err, const char* needle) {
    int ok = rc != 0 && err != NULL && strstr(err, needle) != NULL;
    if (!ok) {
        printf("  (rc %d, err %s)\n", rc, err ? err : "(null)");
    }
    net_compute_free_cstring(err);
    return ok;
}

static int daemons(net_compute_runtime_t* rt) {
    static const uint8_t seed[32] = {7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
                                     7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7};
    net_compute_daemon_handle_t* h = NULL;
    net_compute_daemon_handle_t* restored = NULL;
    net_compute_outputs_t* snap = NULL;
    char* err = NULL;
    char out[256];
    uint8_t entity[32];
    uint64_t origin, origin2, d1, d2;
    const uint8_t* snap_ptr = NULL;
    size_t snap_len = 0;
    uint8_t* snap_copy;
    int rc;

    d1 = new_daemon();
    rc = net_compute_spawn(rt, "counter", 7, seed, d1, 0, 0, &h, &err);
    net_compute_free_cstring(err);
    err = NULL;
    CU_CHECK_RC("net_compute_spawn: the C daemon", rc, 0);
    origin = net_compute_daemon_handle_origin_hash(h);
    CU_CHECK("net_compute_daemon_handle_origin_hash: non-zero", origin != 0);
    CU_CHECK_RC("net_compute_daemon_handle_entity_id", net_compute_daemon_handle_entity_id(h, entity), 0);
    CU_CHECK_RC("net_compute_runtime_daemon_count: 1", net_compute_runtime_daemon_count(rt), 1);

    CU_CHECK_RC("deliver 1: reaches process", deliver(rt, origin, 1, "alpha", out, sizeof out), 0);
    CU_CHECK("deliver 1: the daemon's output", strcmp(out, "n=1:alpha") == 0);
    CU_CHECK_RC("deliver 2", deliver(rt, origin, 2, "beta", out, sizeof out), 0);
    CU_CHECK_RC("deliver 3", deliver(rt, origin, 3, "gamma", out, sizeof out), 0);
    CU_CHECK("deliver 3: the count carried across events", strcmp(out, "n=3:gamma") == 0);
    CU_CHECK("deliver: an unknown origin is refused",
             deliver(rt, 0xDEADBEEF, 1, "x", out, sizeof out) != 0);

    rc = net_compute_runtime_snapshot(rt, origin, &snap, &err);
    net_compute_free_cstring(err);
    err = NULL;
    CU_CHECK_RC("net_compute_runtime_snapshot", rc, 0);
    CU_CHECK("snapshot: one serialized StateSnapshot",
             net_compute_outputs_len(snap) == 1 && net_compute_outputs_at(snap, 0, &snap_ptr, &snap_len) == 0 &&
                 snap_len > sizeof(uint64_t));
    CU_CHECK("snapshot: the library released this program's state buffer through the registered free",
             callback_frees >= 1);
    snap_copy = (uint8_t*)malloc(snap_len);
    CU_CHECK("snapshot: copied", snap_copy != NULL);
    memcpy(snap_copy, snap_ptr, snap_len);
    net_compute_outputs_free(snap);

    rc = net_compute_runtime_stop(rt, origin, &err);
    net_compute_free_cstring(err);
    err = NULL;
    CU_CHECK_RC("net_compute_runtime_stop", rc, 0);
    net_compute_daemon_handle_free(h);
    CU_CHECK_RC("net_compute_runtime_daemon_count: 0 after stop", net_compute_runtime_daemon_count(rt), 0);
    CU_CHECK("free: the dropped daemon's free callback ran", frees >= 1 && !live[d1]);

    d2 = new_daemon();
    rc = net_compute_spawn_from_snapshot(rt, "counter", 7, seed, snap_copy, snap_len, d2, 0, 0, &restored, &err);
    net_compute_free_cstring(err);
    err = NULL;
    CU_CHECK_RC("net_compute_spawn_from_snapshot", rc, 0);
    CU_CHECK("spawn_from_snapshot: restore received the state", restores == 1 && counts[d2] == 3);
    origin2 = net_compute_daemon_handle_origin_hash(restored);
    CU_CHECK("spawn_from_snapshot: the same identity, the same origin", origin2 == origin);
    CU_CHECK_RC("deliver after restore", deliver(rt, origin2, 4, "delta", out, sizeof out), 0);
    CU_CHECK("deliver after restore: the count continues", strcmp(out, "n=4:delta") == 0);
    rc = net_compute_runtime_stop(rt, origin2, &err);
    net_compute_free_cstring(err);
    err = NULL;
    CU_CHECK_RC("stop the restored daemon", rc, 0);
    net_compute_daemon_handle_free(restored);

    snap_copy[snap_len / 2] ^= 0xFF;
    snap_copy[0] ^= 0xFF;
    restored = NULL;
    d2 = new_daemon();
    rc = net_compute_spawn_from_snapshot(rt, "counter", 7, seed, snap_copy, snap_len, d2, 0, 0, &restored, &err);
    CU_CHECK("spawn_from_snapshot: corrupt bytes are refused", rc != 0 && restored == NULL);
    if (d2 < MAX_DAEMONS) {
        cu_mutex_lock(lock);
        live[d2] = 0; /* the refused spawn never ran: retire its id */
        cu_mutex_unlock(lock);
    }
    net_compute_free_cstring(err);
    free(snap_copy);
    return 0;
}

static int groups(net_compute_runtime_t* rt) {
    static const uint8_t group_seed[32] = {0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
                                           0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
                                           0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
                                           0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11};
    net_compute_fork_group_t* fork = NULL;
    net_compute_replica_group_t* replica = NULL;
    char* err = NULL;
    char* members;
    char needle[64];
    uint64_t routed = 0;
    int status = -1, before = factory_calls, rc;
    uint32_t healthy = 0, total = 0;

    rc = net_compute_fork_group_spawn(rt, "counter", 7, 0xABCDEF01, 42, 1, "round-robin", 11, 0, 0, &fork, &err);
    net_compute_free_cstring(err);
    err = NULL;
    CU_CHECK_RC("net_compute_fork_group_spawn", rc, 0);
    CU_CHECK_RC("net_compute_fork_group_fork_count", net_compute_fork_group_fork_count(fork), 1);
    CU_CHECK_RC("net_compute_fork_group_healthy_count", net_compute_fork_group_healthy_count(fork), 1);
    CU_CHECK("net_compute_fork_group_parent_origin", net_compute_fork_group_parent_origin(fork) == 0xABCDEF01);
    CU_CHECK("net_compute_fork_group_fork_seq", net_compute_fork_group_fork_seq(fork) == 42);
    CU_CHECK_RC("net_compute_fork_group_verify_lineage", net_compute_fork_group_verify_lineage(fork), 1);
    CU_CHECK("fork group: its member was built by this program's factory", factory_calls > before);
    net_compute_fork_group_free(fork);

    rc = net_compute_replica_group_spawn(rt, "counter", 7, 1, group_seed, "consistent-hash", 15, 0, 0, &replica,
                                         &err);
    net_compute_free_cstring(err);
    err = NULL;
    CU_CHECK_RC("net_compute_replica_group_spawn", rc, 0);
    CU_CHECK_RC("net_compute_replica_group_replica_count", net_compute_replica_group_replica_count(replica), 1);
    CU_CHECK_RC("net_compute_replica_group_health", net_compute_replica_group_health(replica, &status, &healthy, &total),
                0);
    /* net.go.h: healthy and total are filled only when degraded. */
    CU_CHECK("replica group: healthy (status 0; no degraded counts)", status == 0 && healthy == 0 && total == 0);
    CU_CHECK_RC("net_compute_replica_group_healthy_count: 1", net_compute_replica_group_healthy_count(replica), 1);
    rc = net_compute_replica_group_route_event(replica, "req-a", 5, &routed, &err);
    net_compute_free_cstring(err);
    err = NULL;
    CU_CHECK_RC("net_compute_replica_group_route_event", rc, 0);
    members = net_compute_replica_group_members_json(replica);
    snprintf(needle, sizeof needle, "%llu", (unsigned long long)routed);
    CU_CHECK("route_event: the origin of a live member", members != NULL && routed != 0 && strstr(members, needle));
    net_compute_free_cstring(members);
    net_compute_replica_group_free(replica);

    rc = net_compute_fork_group_spawn(rt, "nothing", 7, 1, 1, 1, "round-robin", 11, 0, 0, &fork, &err);
    CU_CHECK("fork group: an unknown kind is group: factory-not-found",
             error_mentions(rc, err, "factory-not-found"));
    return 0;
}

int main(void) {
    static const clm_fn_t used[] = {
        CU_MESH_FNS,
        CLM_FN(net_mesh_start),
        CLM_FN(net_mesh_shutdown),
        CLM_FN(net_mesh_free),
        CLM_FN(net_mesh_arc_clone),
        CLM_FN(net_mesh_channel_configs_arc_clone),
        CLM_FN(net_compute_runtime_new),
        CLM_FN(net_compute_runtime_start),
        CLM_FN(net_compute_set_callback_free),
        CLM_FN(net_compute_set_dispatcher),
        CLM_FN(net_compute_spawn),
        CLM_FN(net_compute_runtime_deliver),
        CLM_FN(net_compute_runtime_snapshot),
        CLM_FN(net_compute_spawn_from_snapshot),
        CLM_FN(net_compute_fork_group_spawn),
        CLM_FN(net_compute_fork_group_fork_count),
        CLM_FN(net_compute_replica_group_spawn),
        CLM_FN(net_compute_free_cstring),
        CLM_FN(net_compute_daemon_handle_entity_id),
        CLM_FN(net_compute_daemon_handle_free),
        CLM_FN(net_compute_daemon_handle_origin_hash),
        CLM_FN(net_compute_fork_group_fork_seq),
        CLM_FN(net_compute_fork_group_free),
        CLM_FN(net_compute_fork_group_healthy_count),
        CLM_FN(net_compute_fork_group_parent_origin),
        CLM_FN(net_compute_fork_group_verify_lineage),
        CLM_FN(net_compute_outputs_at),
        CLM_FN(net_compute_outputs_free),
        CLM_FN(net_compute_outputs_len),
        CLM_FN(net_compute_outputs_push),
        CLM_FN(net_compute_register_factory_with_func),
        CLM_FN(net_compute_replica_group_free),
        CLM_FN(net_compute_replica_group_health),
        CLM_FN(net_compute_replica_group_healthy_count),
        CLM_FN(net_compute_replica_group_members_json),
        CLM_FN(net_compute_replica_group_replica_count),
        CLM_FN(net_compute_replica_group_route_event),
        CLM_FN(net_compute_runtime_daemon_count),
        CLM_FN(net_compute_runtime_free),
        CLM_FN(net_compute_runtime_id),
        CLM_FN(net_compute_runtime_is_ready),
        CLM_FN(net_compute_runtime_shutdown),
        CLM_FN(net_compute_runtime_stop),
    };
    net_meshnode_t* node = NULL;
    net_compute_runtime_t* rt;
    net_compute_fork_group_t* fork = NULL;
    char addr[32];
    char* err = NULL;
    int rc;

    if (clm_check_loaded_module(used, sizeof used / sizeof used[0]) != 0) {
        return 2;
    }
    if (cu_net_init() != 0) {
        printf("FAIL setup: cu_net_init\n");
        return 1;
    }
    lock = cu_mutex_new();
    CU_CHECK("a mutex for the daemon state", lock != NULL);
    CU_CHECK_RC("bring-up: node", cu_mesh_build(0xE1, &node, addr, sizeof addr), 0);
    CU_CHECK_RC("bring-up: start", net_mesh_start(node), 0);

    rt = net_compute_runtime_new(net_mesh_arc_clone(node), net_mesh_channel_configs_arc_clone(node));
    CU_CHECK("net_compute_runtime_new", rt != NULL);
    CU_CHECK("net_compute_runtime_new: NULL arcs are NULL", net_compute_runtime_new(NULL, NULL) == NULL);
    CU_CHECK("net_compute_runtime_id: non-zero", net_compute_runtime_id(rt) != 0);
    CU_CHECK_RC("net_compute_runtime_is_ready: not before start", net_compute_runtime_is_ready(rt), 0);
    CU_CHECK_RC("net_compute_runtime_is_ready: NULL is NET_COMPUTE_ERR_NULL", net_compute_runtime_is_ready(NULL),
                NET_COMPUTE_ERR_NULL);

    /* net.go.h: the deallocator "MUST be called before any dispatcher
     * registration"; the dispatcher is refused without it. */
    CU_CHECK_RC("net_compute_set_dispatcher: refused before the deallocator",
                net_compute_set_dispatcher(on_process, on_snapshot, on_restore, on_free, on_factory),
                NET_COMPUTE_ERR_NULL);
    CU_CHECK_RC("net_compute_set_callback_free: NULL is -1", net_compute_set_callback_free(NULL), -1);
    CU_CHECK_RC("net_compute_set_callback_free", net_compute_set_callback_free(callback_free), 0);
    CU_CHECK_RC("net_compute_set_dispatcher", net_compute_set_dispatcher(on_process, on_snapshot, on_restore, on_free,
                                                                        on_factory),
                0);
    CU_CHECK_RC("net_compute_register_factory_with_func: counter",
                net_compute_register_factory_with_func(rt, "counter", 7), 0);
    CU_CHECK_RC("net_compute_register_factory_with_func: counter again is NET_COMPUTE_ERR_DUPLICATE_KIND",
                net_compute_register_factory_with_func(rt, "counter", 7), NET_COMPUTE_ERR_DUPLICATE_KIND);

    rc = net_compute_fork_group_spawn(rt, "counter", 7, 1, 1, 1, "round-robin", 11, 0, 0, &fork, &err);
    CU_CHECK("fork group before start: group: not-ready", error_mentions(rc, err, "not-ready"));
    err = NULL;

    rc = net_compute_runtime_start(rt, &err);
    net_compute_free_cstring(err);
    err = NULL;
    CU_CHECK_RC("net_compute_runtime_start", rc, 0);
    CU_CHECK_RC("net_compute_runtime_is_ready: after start", net_compute_runtime_is_ready(rt), 1);

    if (daemons(rt) || groups(rt)) {
        return 1;
    }

    rc = net_compute_runtime_shutdown(rt, &err);
    net_compute_free_cstring(err);
    CU_CHECK_RC("net_compute_runtime_shutdown", rc, 0);
    net_compute_runtime_free(rt);
    net_compute_runtime_free(NULL);
    CU_CHECK_RC("net_mesh_shutdown", net_mesh_shutdown(node), 0);
    net_mesh_free(node);
    cu_mutex_free(lock);
    return cu_finish();
}
