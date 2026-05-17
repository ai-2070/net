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
 * # Scope (slice 1b)
 *
 * Full daemon-author surface. `net_meshos_register_daemon_with_vtable`
 * accepts a `NetMeshOsDaemonVtable` struct of consumer-supplied
 * C function pointers (`process` / `snapshot` / `restore` /
 * `on_control` / `health` / `saturation`); the cdylib bridges
 * each into the substrate's `MeshDaemon` trait. `process` and
 * `snapshot` callbacks emit output buffers via the
 * `net_meshos_process_emit` / `_snapshot_emit` helpers — the
 * bridge copies the bytes immediately so the caller may free the
 * source buffer as soon as the emit returns.
 *
 * The slice-1a `net_meshos_register_daemon` (which registers an
 * internal no-op daemon) is kept for lifecycle-only consumers
 * that want to drive control / log / shutdown surfaces without
 * plugging in process logic.
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
 * every FFI entry point that calls into the substrate; instead
 * of unwinding across the C ABI (UB), the call returns the
 * appropriate error status and populates the last-error pair
 * with kind `"runtime_panic"`. Trivial accessors / emit helpers
 * that only tag a pointer and copy bytes skip the trap — they
 * have no panic surface, so wrapping would only add `catch_unwind`
 * overhead.
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
 * Daemon registration — vtable callbacks (slice 1b)
 *
 * Consumer supplies a `NetMeshOsDaemonVtable` of C function
 * pointers. Each callback receives the consumer's `user_ctx`.
 * Callbacks fire from tokio worker threads — consumer is
 * responsible for the thread-safety of any shared state.
 *
 * `process` is required; every other field may be NULL to take
 * the substrate default. The vtable struct is copied during
 * registration; the caller may free or reuse the struct as soon
 * as `net_meshos_register_daemon_with_vtable` returns. Each
 * non-NULL function pointer must remain valid until the handle
 * is freed.
 * ========================================================================= */

/* Health discriminator returned by the vtable's `health` callback. */
#define NET_MESHOS_HEALTH_HEALTHY   0
#define NET_MESHOS_HEALTH_DEGRADED  1
#define NET_MESHOS_HEALTH_UNHEALTHY 2

/* Opaque emit contexts. Handed to `process` / `snapshot`
 * callbacks. Consumers must NOT free; the bridge owns the
 * underlying buffer. */
typedef struct NetMeshOsProcessEmitCtx  NetMeshOsProcessEmitCtx;
typedef struct NetMeshOsSnapshotEmitCtx NetMeshOsSnapshotEmitCtx;

/* Vtable of consumer-supplied callbacks. */
typedef struct {
    /* Required. Return 0 on success; non-zero surfaces as
     * substrate `ProcessFailed`. Emit zero or more output buffers
     * via `net_meshos_process_emit(emit_ctx, ptr, len)`. */
    int (*process)(
        void* user_ctx,
        NetMeshOsProcessEmitCtx* emit_ctx,
        uint64_t origin_hash,
        uint64_t sequence,
        const uint8_t* payload_ptr,
        size_t payload_len
    );
    /* Optional. Stateless daemons set NULL. Emit at most one
     * snapshot buffer via `net_meshos_snapshot_emit(emit_ctx,
     * ptr, len)`; subsequent emits in the same callback are
     * ignored. */
    void (*snapshot)(
        void* user_ctx,
        NetMeshOsSnapshotEmitCtx* emit_ctx
    );
    /* Optional. Return 0 on success; non-zero surfaces as
     * substrate `RestoreFailed`. */
    int (*restore)(
        void* user_ctx,
        const uint8_t* payload_ptr,
        size_t payload_len
    );
    /* Optional. Fires when the supervisor routes a daemon-
     * targeted action. Same wire form as `next_control` — `kind`
     * is one of `NET_MESHOS_CONTROL_*`. */
    void (*on_control)(
        void* user_ctx,
        int kind,
        uint64_t grace_period_ms,
        float level
    );
    /* Optional. Returns one of `NET_MESHOS_HEALTH_*`. NULL = always
     * `Healthy`. */
    int (*health)(void* user_ctx);
    /* Optional. Returns a value in `[0.0, 1.0]`. NULL = `0.0`. */
    float (*saturation)(void* user_ctx);
} NetMeshOsDaemonVtable;

/* Emit a process-output buffer. Bytes are copied immediately;
 * the source buffer may be freed as soon as this returns. Safe
 * to call multiple times per `process` invocation. */
void net_meshos_process_emit(
    NetMeshOsProcessEmitCtx* ctx,
    const uint8_t* payload_ptr,
    size_t payload_len
);

/* Emit the daemon's snapshot buffer. Calling more than once per
 * snapshot callback is a no-op for subsequent calls. */
void net_meshos_snapshot_emit(
    NetMeshOsSnapshotEmitCtx* ctx,
    const uint8_t* payload_ptr,
    size_t payload_len
);

/* Register a daemon with consumer-supplied callbacks.
 *
 *   sdk             — running SDK handle.
 *   name_ptr/_len   — UTF-8 daemon name (NOT NUL-terminated).
 *   seed_ptr        — 32 bytes of ed25519 seed (operator id =
 *                     keypair's origin hash).
 *   vtable_ptr      — vtable; `process` field is required.
 *   user_ctx        — opaque pointer passed verbatim to every
 *                     callback. Consumer owns the lifetime —
 *                     must outlive the handle.
 *   out             — receives the registered handle.
 *
 * Returns NET_MESHOS_OK on success. Caller MUST free the handle
 * via `net_meshos_handle_free` (or `net_meshos_graceful_shutdown`
 * followed by `_free`). */
int net_meshos_register_daemon_with_vtable(
    NetMeshOsSdk* sdk,
    const char* name_ptr,
    size_t name_len,
    const uint8_t* seed_ptr,
    const NetMeshOsDaemonVtable* vtable_ptr,
    void* user_ctx,
    NetMeshOsHandle** out
);

/* =========================================================================
 * Daemon registration (slice 1a — internal no-op daemon)
 *
 * Kept for lifecycle-only consumers. Real daemons use
 * `_with_vtable` above.
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
 * **No-op daemon:** the substrate-side daemon registered by
 * this call is an internal no-op `MeshDaemon` impl. Use
 * `net_meshos_register_daemon_with_vtable` for daemons that plug
 * in `process` / `snapshot` / `restore` / `on_control` callbacks.
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
 * Metadata + capability advertisement (slice 2)
 * ========================================================================= */

/* Return a heap-allocated JSON CString rendering of the daemon's
 * `MetadataView`. Shape:
 *
 *   {
 *     "node_id": <u64>,
 *     "daemon_id": <u64>,
 *     "daemon_name": <string>,
 *     "maintenance_state": {
 *       "kind": "active"
 *             | "entering_maintenance" (since_ms, deadline_remaining_ms?)
 *             | "maintenance" (since_ms)
 *             | "exiting_maintenance" (since_ms)
 *             | "drain_failed" (since_ms, reason)
 *             | "recovery" (since_ms)
 *             | "unknown",
 *       ... discriminator-specific fields ...
 *     },
 *     "peers": [ { "node_id", "rtt_ms", "health", "maintenance",
 *                  "cpu_load_1m", "mem_used_bytes", ...
 *                  "capability_set": [tag, ...] }, ... ]
 *   }
 *
 * Caller MUST release the buffer via `net_meshos_free_string`.
 * Returns NULL on NULL handle or after `graceful_shutdown`; the
 * thread-local last-error pair carries the kind. */
char* net_meshos_metadata(const NetMeshOsHandle* handle);

/* Refresh the metadata cache from the runtime's latest snapshot
 * and return the freshly-rendered JSON. Same ownership +
 * lifetime contract as `net_meshos_metadata`. */
char* net_meshos_refresh_metadata(NetMeshOsHandle* handle);

/* Free a heap-allocated C string returned by this crate (e.g.
 * from `net_meshos_metadata` / `net_meshos_refresh_metadata`).
 * Idempotent on NULL. */
void net_meshos_free_string(char* s);

/* Publish a capability advertisement update for this daemon.
 * `tags_json_ptr` / `tags_json_len` carry a UTF-8 JSON array of
 * tag strings, e.g.
 * `["hardware.gpu", "software.model.foo=llama-3.1-70b"]`. Pass
 * NULL / 0 to clear the advertisement.
 *
 * Stub today — the substrate's
 * `MeshOsDaemonHandle::publish_capabilities` returns `Ok(())`
 * without committing to the capability chain. Every binding
 * surfaces the same stub semantics so consumers don't write
 * code against a contract the substrate doesn't yet honor.
 * Cuts over transparently when the substrate's chain commit
 * lands. */
int net_meshos_publish_capabilities(
    NetMeshOsHandle* handle,
    const char* tags_json_ptr,
    size_t tags_json_len
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
