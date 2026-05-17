/*
 * Net C SDK — MeshOS daemon-author example.
 *
 * Walks the canonical daemon lifecycle with a real vtable-based
 * daemon:
 *
 *   1. Start the supervisor SDK with default config.
 *   2. Build a NetMeshOsDaemonVtable with consumer callbacks
 *      (`process` + `snapshot` + `restore` + `on_control` +
 *      `health` + `saturation`).
 *   3. Register the daemon under a deterministic 32-byte seed.
 *   4. Publish a log line at INFO level.
 *   5. Poll the daemon's control channel non-blockingly (empty).
 *   6. Block for up to 100ms waiting for a control event
 *      (timeout).
 *   7. Graceful-shutdown the daemon with a 50ms grace period.
 *   8. Tear down the supervisor.
 *
 * Build:
 *   cargo build --release -p net-meshos-ffi
 *   gcc -o meshos meshos.c -L ../target/release -lnet_meshos \
 *       -lpthread -ldl -lm
 *
 * Run:
 *   LD_LIBRARY_PATH=../target/release ./meshos     (Linux)
 *   DYLD_LIBRARY_PATH=../target/release ./meshos   (macOS)
 */

#include "../include/net_meshos.h"
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* ---------------------------------------------------------------
 * Daemon implementation — a tiny "echo" daemon that emits each
 * received payload back out as a process output, plus a stateful
 * counter saved to its snapshot.
 * --------------------------------------------------------------- */

typedef struct {
    uint64_t process_count;
    uint64_t restore_count;
} EchoDaemonState;

static int echo_process(
    void* user_ctx,
    NetMeshOsProcessEmitCtx* emit_ctx,
    uint64_t origin_hash,
    uint64_t sequence,
    const uint8_t* payload_ptr,
    size_t payload_len
) {
    EchoDaemonState* state = (EchoDaemonState*)user_ctx;
    state->process_count++;
    /* Echo the input back as one output buffer. */
    net_meshos_process_emit(emit_ctx, payload_ptr, payload_len);
    /* Suppress unused-parameter warnings without compiler-specific
     * attributes. */
    (void)origin_hash;
    (void)sequence;
    return 0;
}

static void echo_snapshot(
    void* user_ctx,
    NetMeshOsSnapshotEmitCtx* emit_ctx
) {
    EchoDaemonState* state = (EchoDaemonState*)user_ctx;
    /* Snapshot is the raw two counters — opaque to the supervisor. */
    net_meshos_snapshot_emit(emit_ctx, (const uint8_t*)state, sizeof(*state));
}

static int echo_restore(
    void* user_ctx,
    const uint8_t* payload_ptr,
    size_t payload_len
) {
    if (payload_len != sizeof(EchoDaemonState)) {
        return 1; /* RestoreFailed */
    }
    EchoDaemonState* state = (EchoDaemonState*)user_ctx;
    memcpy(state, payload_ptr, sizeof(*state));
    state->restore_count++;
    return 0;
}

static void echo_on_control(
    void* user_ctx,
    int kind,
    uint64_t grace_period_ms,
    float level
) {
    (void)user_ctx;
    (void)grace_period_ms;
    (void)level;
    fprintf(stderr, "[echo] on_control kind=%d\n", kind);
}

static int echo_health(void* user_ctx) {
    (void)user_ctx;
    return NET_MESHOS_HEALTH_HEALTHY;
}

static float echo_saturation(void* user_ctx) {
    (void)user_ctx;
    return 0.0f;
}

/* ---------------------------------------------------------------
 * Helpers
 * --------------------------------------------------------------- */

static const char* control_kind_str(int kind) {
    switch (kind) {
        case NET_MESHOS_CONTROL_NONE:             return "None";
        case NET_MESHOS_CONTROL_SHUTDOWN:         return "Shutdown";
        case NET_MESHOS_CONTROL_DRAIN_START:      return "DrainStart";
        case NET_MESHOS_CONTROL_DRAIN_FINISH:     return "DrainFinish";
        case NET_MESHOS_CONTROL_BACKPRESSURE_ON:  return "BackpressureOn";
        case NET_MESHOS_CONTROL_BACKPRESSURE_OFF: return "BackpressureOff";
        default:                                  return "Unknown";
    }
}

static void print_last_error(const char* context) {
    const char* kind = net_meshos_last_error_kind();
    const char* msg = net_meshos_last_error_message();
    fprintf(stderr, "[%s] kind=%s message=%s\n",
        context,
        kind ? kind : "(none)",
        msg ? msg : "(none)");
    net_meshos_clear_last_error();
}

/* ---------------------------------------------------------------
 * Main
 * --------------------------------------------------------------- */

int main(void) {
    /* 1. Start the SDK with default config. */
    NetMeshOsSdk* sdk = NULL;
    int rc = net_meshos_sdk_start(0, 0, 0, 0, 0, &sdk);
    if (rc != NET_MESHOS_OK || sdk == NULL) {
        print_last_error("sdk_start");
        return 1;
    }
    printf("started MeshOS SDK\n");

    /* 2. Build the vtable. */
    NetMeshOsDaemonVtable vt = {
        .process    = echo_process,
        .snapshot   = echo_snapshot,
        .restore    = echo_restore,
        .on_control = echo_on_control,
        .health     = echo_health,
        .saturation = echo_saturation,
    };

    /* 3. Daemon state + register. */
    EchoDaemonState state = { .process_count = 0, .restore_count = 0 };
    uint8_t seed[32];
    memset(seed, 0x42, sizeof(seed));
    const char* name = "echo";
    NetMeshOsHandle* handle = NULL;
    rc = net_meshos_register_daemon_with_vtable(
        sdk, name, strlen(name), seed, &vt, &state, &handle
    );
    if (rc != NET_MESHOS_OK || handle == NULL) {
        print_last_error("register_daemon_with_vtable");
        net_meshos_sdk_free(sdk);
        return 1;
    }
    uint64_t daemon_id = net_meshos_handle_daemon_id(handle);
    const char* daemon_name = net_meshos_handle_daemon_name(handle);
    printf("registered daemon id=%#" PRIx64 " name=%s\n",
        daemon_id, daemon_name ? daemon_name : "(null)");

    /* 4. Publish a log line. */
    const char* msg = "echo daemon up";
    rc = net_meshos_publish_log(handle, NET_MESHOS_LOG_INFO, msg, strlen(msg));
    if (rc != NET_MESHOS_OK) {
        print_last_error("publish_log");
    } else {
        printf("published log line\n");
    }

    /* 5. Non-blocking control receive — channel should be empty. */
    NetMeshOsDaemonControl ev = {0};
    rc = net_meshos_try_next_control(handle, &ev);
    if (rc != NET_MESHOS_OK) {
        print_last_error("try_next_control");
    } else {
        printf("try_next_control: kind=%s\n", control_kind_str(ev.kind));
    }

    /* 6. Block for up to 100ms — should time out, kind=None. */
    memset(&ev, 0, sizeof(ev));
    rc = net_meshos_next_control(handle, 100, &ev);
    if (rc != NET_MESHOS_OK) {
        print_last_error("next_control(timeout)");
    } else {
        printf("next_control(100ms): kind=%s\n", control_kind_str(ev.kind));
    }

    /* 7. Graceful-shutdown the daemon. */
    rc = net_meshos_graceful_shutdown(handle, 50);
    if (rc != NET_MESHOS_OK) {
        print_last_error("graceful_shutdown");
    } else {
        printf("graceful_shutdown complete (process_count=%" PRIu64 ", restore_count=%" PRIu64 ")\n",
            state.process_count, state.restore_count);
    }
    net_meshos_handle_free(handle);

    /* 8. Tear down the SDK. */
    rc = net_meshos_sdk_shutdown(sdk);
    if (rc != NET_MESHOS_OK) {
        print_last_error("sdk_shutdown");
    } else {
        printf("sdk_shutdown complete\n");
    }
    net_meshos_sdk_free(sdk);

    return 0;
}
