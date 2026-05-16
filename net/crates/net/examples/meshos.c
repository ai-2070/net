/*
 * Net C SDK — MeshOS daemon-author example.
 *
 * Walks the canonical daemon lifecycle:
 *
 *   1. Start the supervisor SDK with default config.
 *   2. Register a daemon under a deterministic 32-byte seed.
 *   3. Publish a log line at INFO level.
 *   4. Poll the daemon's control channel non-blockingly (empty).
 *   5. Block for up to 100ms waiting for a control event (timeout).
 *   6. Graceful-shutdown the daemon with a 50ms grace period.
 *   7. Tear down the supervisor.
 *
 * Slice 1a caveat: the substrate-side daemon is an internal no-op —
 * the C consumer cannot yet plug in `process` / `snapshot` /
 * `restore` / `on_control` callbacks. The vtable bridge lands in
 * slice 1b. Everything else in this example is the permanent SDK
 * shape.
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

int main(void) {
    /* 1. Start the SDK with default config (all zeros). */
    NetMeshOsSdk* sdk = NULL;
    int rc = net_meshos_sdk_start(0, 0, 0, 0, 0, &sdk);
    if (rc != NET_MESHOS_OK || sdk == NULL) {
        print_last_error("sdk_start");
        return 1;
    }
    printf("started MeshOS SDK\n");

    /* 2. Register a daemon under a deterministic seed. The seed
     *    drives the daemon's substrate id (origin_hash). */
    uint8_t seed[32];
    memset(seed, 0x42, sizeof(seed));
    const char* name = "example-daemon";
    NetMeshOsHandle* handle = NULL;
    rc = net_meshos_register_daemon(
        sdk, name, strlen(name), seed, &handle);
    if (rc != NET_MESHOS_OK || handle == NULL) {
        print_last_error("register_daemon");
        net_meshos_sdk_free(sdk);
        return 1;
    }
    uint64_t daemon_id = net_meshos_handle_daemon_id(handle);
    const char* daemon_name = net_meshos_handle_daemon_name(handle);
    printf("registered daemon id=%#" PRIx64 " name=%s\n",
        daemon_id, daemon_name ? daemon_name : "(null)");

    /* 3. Publish a log line at INFO level. */
    const char* msg = "hello from the C SDK";
    rc = net_meshos_publish_log(
        handle, NET_MESHOS_LOG_INFO, msg, strlen(msg));
    if (rc != NET_MESHOS_OK) {
        print_last_error("publish_log");
    } else {
        printf("published log line\n");
    }

    /* 4. Non-blocking control receive — channel should be empty. */
    NetMeshOsDaemonControl ev = {0};
    rc = net_meshos_try_next_control(handle, &ev);
    if (rc != NET_MESHOS_OK) {
        print_last_error("try_next_control");
    } else {
        printf("try_next_control: kind=%s\n", control_kind_str(ev.kind));
    }

    /* 5. Block for up to 100ms — should time out, kind=None. */
    memset(&ev, 0, sizeof(ev));
    rc = net_meshos_next_control(handle, 100, &ev);
    if (rc != NET_MESHOS_OK) {
        print_last_error("next_control(timeout)");
    } else {
        printf("next_control(100ms): kind=%s\n", control_kind_str(ev.kind));
    }

    /* 6. Graceful-shutdown the daemon. */
    rc = net_meshos_graceful_shutdown(handle, 50);
    if (rc != NET_MESHOS_OK) {
        print_last_error("graceful_shutdown");
    } else {
        printf("graceful_shutdown complete\n");
    }
    net_meshos_handle_free(handle);

    /* 7. Tear down the SDK. */
    rc = net_meshos_sdk_shutdown(sdk);
    if (rc != NET_MESHOS_OK) {
        print_last_error("sdk_shutdown");
    } else {
        printf("sdk_shutdown complete\n");
    }
    net_meshos_sdk_free(sdk);

    return 0;
}
