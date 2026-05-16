/*
 * net_meshos.h — C SDK header for libnet_meshos (the MeshOS
 * daemon-author SDK C ABI).
 *
 * One header, one shared library. Mirrors the layout of `net.h` /
 * `net_meshdb.h` / `net_rpc.h` next to it. Symbols live in the
 * `libnet_meshos.{so,dylib,dll}` cdylib built from
 * `bindings/go/meshos-ffi`. The Go binding's
 * `bindings/go/net/meshos.go` cgo include block is the
 * sister-consumer contract; this file is the canonical drop-in
 * for C / C++ / Zig / Swift / Java JNI / etc.
 *
 * Companion to `MESHOS_SDK_PLAN.md` Phase 5; consumes the cdylib
 * built for Phase 4 (Go) without modification.
 *
 * # Scope (slice 1a)
 *
 * SDK + handle lifecycle, control-event RX, log emission, graceful
 * shutdown. The substrate-side daemon registered by
 * `net_meshos_register_daemon` is an internal no-op `MeshDaemon`
 * impl in this slice — the C consumer cannot yet plug in
 * `process` / `snapshot` / `restore` / `on_control` callbacks.
 * The vtable-based callback bridge (slice 1b) is the next slice
 * and is the only thing keeping this header from being the full
 * daemon-author contract. Everything else — lifecycle, control
 * event delivery, log emission, graceful shutdown, error envelope —
 * is permanent SDK shape.
 *
 * # Build
 *
 *   cargo build --release -p net-meshos-ffi
 *
 *   Linux:   target/release/libnet_meshos.so
 *   macOS:   target/release/libnet_meshos.dylib
 *   Windows: target/release/net_meshos.dll
 *
 * # Link
 *
 *   gcc -o app app.c -L target/release -lnet_meshos -lpthread -ldl -lm
 *
 * # Handle model
 *
 * Two opaque heap-allocated handles cross the FFI:
 *
 *   NetMeshOsSdk    — the supervisor runtime + control router.
 *   NetMeshOsHandle — a registered daemon's lifecycle handle.
 *
 * Caller owns every returned pointer and MUST call the matching
 * `_free` exactly once. Each `_free` is idempotent on NULL.
 *
 * Calling `net_meshos_sdk_shutdown` consumes the inner SDK by
 * value; subsequent operations on that handle return
 * `NET_MESHOS_ERR_ALREADY_SHUTDOWN`. The outer pointer still
 * needs `net_meshos_sdk_free` to release. Same shape for
 * `net_meshos_graceful_shutdown` + `net_meshos_handle_free`.
 *
 * # Error model
 *
 * Status-code functions return `int`:
 *
 *   NET_MESHOS_OK                  (0)  — success.
 *   NET_MESHOS_ERR_NULL           (-1)  — NULL handle.
 *   NET_MESHOS_ERR_CALL_FAILED    (-2)  — substrate-side failure;
 *                                          see last-error pair.
 *   NET_MESHOS_ERR_INVALID_ARG    (-3)  — NULL pointer / bad input.
 *   NET_MESHOS_ERR_ALREADY_SHUTDOWN (-4) — SDK / handle consumed.
 *
 * Detail flows through a per-thread last-error pair. After any
 * non-OK status, call `net_meshos_last_error_message` for the
 * human-readable text and `net_meshos_last_error_kind` for the
 * stable substrate discriminator (e.g. `"register_failed"`,
 * `"queue_full"`, `"loop_closed"`, `"invalid_log_level"`,
 * `"already_shutdown"`, `"shutdown_failed"`, `"runtime_panic"`).
 * Both return NULL when no error has been recorded on the
 * calling thread. Returned pointers are valid until the next
 * FFI call on the same thread touches the thread-local; callers
 * must NOT free them. Use `net_meshos_clear_last_error` to reset.
 *
 * The substrate-side envelope is `<<meshos-sdk-kind:KIND>>MSG` —
 * the C header surfaces the KIND verbatim through
 * `net_meshos_last_error_kind` and the MSG body through
 * `net_meshos_last_error_message`. Cross-language consumers
 * (Python / Node / Go) parse the same envelope.
 *
 * Panics from substrate calls are trapped by `catch_unwind` at
 * every FFI entry point; instead of unwinding across the C ABI
 * (UB), the call returns the appropriate error status and
 * populates the last-error pair with kind `"runtime_panic"`.
 *
 * # Threading
 *
 * The cdylib owns one process-global Tokio multi-thread runtime.
 * `net_meshos_next_control` blocks the caller's thread; everything
 * else is non-blocking on the substrate side. Handles are safe
 * to MOVE across threads (Send-equivalent). Concurrent calls
 * from multiple threads on the SAME handle are NOT supported in
 * this slice — guard with external synchronisation if you need it.
 * The thread-local last-error pair behaves like POSIX errno: each
 * calling thread sees its own most-recent error.
 *
 * # Wire format
 *
 * Control events cross the FFI as `NetMeshOsDaemonControl`
 * (tagged struct, see below). The `kind` integer discriminator
 * is stable across language bindings — Python emits the same
 * variant strings ("Shutdown", "DrainStart", …), Node emits the
 * same kind strings, Go emits the same constants.
 */

#ifndef NET_MESHOS_H
#define NET_MESHOS_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* =========================================================================
 * Status codes
 * ========================================================================= */

#define NET_MESHOS_OK                     0
#define NET_MESHOS_ERR_NULL              -1
#define NET_MESHOS_ERR_CALL_FAILED       -2
#define NET_MESHOS_ERR_INVALID_ARG       -3
#define NET_MESHOS_ERR_ALREADY_SHUTDOWN  -4

/* =========================================================================
 * DaemonControl wire form
 *
 * `kind` discriminator → which payload fields are valid:
 *   NONE             — channel empty / timeout / shutdown.
 *   SHUTDOWN         — grace_period_ms.
 *   DRAIN_START      — grace_period_ms.
 *   DRAIN_FINISH     — no payload.
 *   BACKPRESSURE_ON  — level (range [0.0, 1.0]).
 *   BACKPRESSURE_OFF — no payload.
 *   UNKNOWN          — fallback for substrate-side variants the
 *                      header hasn't been rebuilt against. Tolerate
 *                      this; binding ABIs are forward-compatible.
 * ========================================================================= */

#define NET_MESHOS_CONTROL_NONE             0
#define NET_MESHOS_CONTROL_SHUTDOWN         1
#define NET_MESHOS_CONTROL_DRAIN_START      2
#define NET_MESHOS_CONTROL_DRAIN_FINISH     3
#define NET_MESHOS_CONTROL_BACKPRESSURE_ON  4
#define NET_MESHOS_CONTROL_BACKPRESSURE_OFF 5
#define NET_MESHOS_CONTROL_UNKNOWN          99

typedef struct {
    int kind;
    uint64_t grace_period_ms;
    float level;
} NetMeshOsDaemonControl;

/* =========================================================================
 * LogLevel constants — match the substrate's `LogLevel` enum.
 * ========================================================================= */

#define NET_MESHOS_LOG_TRACE 0
#define NET_MESHOS_LOG_DEBUG 1
#define NET_MESHOS_LOG_INFO  2
#define NET_MESHOS_LOG_WARN  3
#define NET_MESHOS_LOG_ERROR 4

/* =========================================================================
 * Opaque handle types
 * ========================================================================= */

typedef struct NetMeshOsSdk    NetMeshOsSdk;
typedef struct NetMeshOsHandle NetMeshOsHandle;

/* =========================================================================
 * SDK lifecycle
 * ========================================================================= */

/* Start the MeshOS SDK. Every config field accepts 0 to pick the
 * substrate default:
 *
 *   this_node              — local node id. Default 0.
 *   tick_interval_ms       — supervisor reconcile cadence. Default 500.
 *   event_queue_capacity   — event-source mpsc capacity. Default 1024.
 *   action_queue_capacity  — executor mpsc capacity. Default 1024.
 *   control_capacity       — per-daemon control channel capacity.
 *                            Default 8.
 *
 * On success, writes a heap-allocated handle to `*out` and returns
 * NET_MESHOS_OK. On failure, populates the thread-local last-error
 * pair and returns a non-OK status. */
int net_meshos_sdk_start(
    uint64_t this_node,
    uint64_t tick_interval_ms,
    size_t event_queue_capacity,
    size_t action_queue_capacity,
    size_t control_capacity,
    NetMeshOsSdk** out
);

/* Free an SDK handle without graceful shutdown. The wrapped Tokio
 * runtime stays alive on its tasks until they finish naturally;
 * for orderly teardown call `net_meshos_sdk_shutdown` first.
 * Idempotent on NULL. */
void net_meshos_sdk_free(NetMeshOsSdk* sdk);

/* Drive a clean shutdown of the wrapped runtime. Consumes the
 * inner SDK by value — subsequent calls on this handle return
 * NET_MESHOS_ERR_ALREADY_SHUTDOWN. Caller still must
 * `net_meshos_sdk_free` to release the outer handle. */
int net_meshos_sdk_shutdown(NetMeshOsSdk* sdk);

/* Diagnostic counter — total control events the router dropped
 * across every registered daemon because a daemon's channel was
 * full. Returns UINT64_MAX on NULL or already-shutdown SDK. */
uint64_t net_meshos_sdk_dropped_control_events(NetMeshOsSdk* sdk);

/* =========================================================================
 * Daemon registration (slice 1a — internal no-op daemon)
 * ========================================================================= */

/* Register a daemon under the supplied identity.
 *
 *   name_ptr / name_len — UTF-8 daemon name (NOT NUL-terminated;
 *                         length-explicit).
 *   seed_ptr            — pointer to exactly 32 bytes of ed25519
 *                         seed material. The substrate constructs
 *                         the `EntityKeypair` from this seed and
 *                         uses its `origin_hash` as the daemon's
 *                         substrate id.
 *
 * **Slice 1a caveat:** the substrate-side daemon registered by
 * this call is an internal no-op `MeshDaemon` impl. Slice 1b
 * lands the vtable-based callback bridge (`process` / `snapshot`
 * / `restore` / `on_control` function pointers) that lets C
 * consumers implement real daemons. The supervisor lifecycle —
 * control events, log emission, graceful shutdown — works
 * end-to-end against today's no-op daemon.
 *
 * On success, writes a heap-allocated handle to `*out` and returns
 * NET_MESHOS_OK. On failure, populates the thread-local last-error
 * pair and returns a non-OK status. */
int net_meshos_register_daemon(
    NetMeshOsSdk* sdk,
    const char* name_ptr,
    size_t name_len,
    const uint8_t* seed_ptr,
    NetMeshOsHandle** out
);

/* Free a daemon handle. If the substrate-side handle is still
 * present (graceful shutdown wasn't called), the Rust-side `Drop`
 * impl still cleans up the registry slot. Idempotent on NULL. */
void net_meshos_handle_free(NetMeshOsHandle* handle);

/* Substrate identifier (the keypair's `origin_hash`). Stable
 * across the handle's lifetime, including after graceful shutdown.
 * Returns 0 on NULL. */
uint64_t net_meshos_handle_daemon_id(const NetMeshOsHandle* handle);

/* Daemon name (NUL-terminated). Pointer valid for the handle's
 * lifetime. Returns NULL on NULL handle. Callers must NOT free. */
const char* net_meshos_handle_daemon_name(const NetMeshOsHandle* handle);

/* =========================================================================
 * Control event RX
 * ========================================================================= */

/* Non-blocking control-event receive. Writes the next event to
 * `*out` and returns NET_MESHOS_OK. If the channel is empty,
 * writes `kind = NET_MESHOS_CONTROL_NONE` and still returns OK
 * — callers branch on `out->kind`. */
int net_meshos_try_next_control(
    NetMeshOsHandle* handle,
    NetMeshOsDaemonControl* out
);

/* Block until the next control event arrives, the runtime shuts
 * down, or `timeout_ms` elapses. Pass `0` for an unbounded wait
 * (matches the substrate's `next_control().await` semantics). On
 * timeout or shutdown, writes `kind = NET_MESHOS_CONTROL_NONE`
 * and returns OK. */
int net_meshos_next_control(
    NetMeshOsHandle* handle,
    uint64_t timeout_ms,
    NetMeshOsDaemonControl* out
);

/* =========================================================================
 * Log emission
 * ========================================================================= */

/* Publish a log line tagged with this daemon's id. Non-blocking
 * on the substrate side; on a saturated log ring the call returns
 * NET_MESHOS_ERR_CALL_FAILED with kind `"queue_full"` or
 * `"loop_closed"` on the last-error pair.
 *
 *   level                — one of the NET_MESHOS_LOG_* constants.
 *   message_ptr / _len   — UTF-8 message (NOT NUL-terminated). */
int net_meshos_publish_log(
    NetMeshOsHandle* handle,
    int level,
    const char* message_ptr,
    size_t message_len
);

/* =========================================================================
 * Graceful shutdown
 * ========================================================================= */

/* Drive a graceful shutdown on the handle. Sends
 * `Shutdown { grace_period_ms }` on the daemon's control channel,
 * parks for `grace_ms`, then unregisters. Consumes the inner
 * handle — subsequent ops return NET_MESHOS_ERR_ALREADY_SHUTDOWN.
 * Caller still must `net_meshos_handle_free` to release the
 * outer handle.
 *
 * Pass `0` for `grace_ms` to use the substrate's default
 * (DEFAULT_GRACEFUL_SHUTDOWN, currently 5 seconds). */
int net_meshos_graceful_shutdown(
    NetMeshOsHandle* handle,
    uint64_t grace_ms
);

/* =========================================================================
 * Last-error trio (thread-local)
 * ========================================================================= */

/* Most recent error message on the calling thread, or NULL if no
 * error has been recorded. Pointer valid until the next FFI call
 * on the same thread that touches the thread-local. */
const char* net_meshos_last_error_message(void);

/* Most recent error kind on the calling thread (the substrate's
 * stable discriminator), or NULL. Same lifetime as
 * `net_meshos_last_error_message`. */
const char* net_meshos_last_error_kind(void);

/* Clear the thread-local last-error pair. */
void net_meshos_clear_last_error(void);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* NET_MESHOS_H */
