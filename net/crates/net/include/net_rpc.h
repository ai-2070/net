/*
 * net_rpc.h — C SDK header for libnet_rpc (the nRPC C ABI).
 *
 * One header, one shared library. Mirrors the layout of `net.h` /
 * `net.go.h` next to it. Symbols live in the `libnet_rpc.{so,
 * dylib,dll}` cdylib built from `bindings/go/rpc-ffi`. The Go
 * binding's `bindings/go/net/mesh_rpc.go` cgo include block has
 * been the de-facto contract for non-Go consumers since v0.10;
 * this file is the canonical drop-in for C / C++ / Zig / Swift /
 * Java JNI / etc.
 *
 * # Build
 *
 *   cargo build --release -p net-rpc-ffi
 *
 *   Linux:   target/release/libnet_rpc.so
 *   macOS:   target/release/libnet_rpc.dylib
 *   Windows: target/release/net_rpc.dll
 *
 * # Link
 *
 *   gcc -o app app.c -L target/release -lnet_rpc -lpthread -ldl -lm
 *
 * # ABI versioning
 *
 * Call `net_rpc_abi_version()` at process init and refuse to
 * load on mismatch. Version `0x0001` covers Phase B5 (lifecycle
 * + unary call / call_service / serve / find_service_nodes) plus
 * Phase B6 (streaming + cancellation token).
 *
 * # Handle model
 *
 * Every Rust object that crosses the FFI is a heap-allocated
 * `Box` handed back as `*mut T`. The caller owns the pointer and
 * MUST call the matching `_free` exactly once. Idempotent on NULL.
 *
 * # Error model
 *
 * `int` return codes — `NET_RPC_OK` (0) on success, negative
 * on failure. Structured detail (an `<kind>: <message>` string,
 * e.g. `"timeout: deadline exceeded after 5000 ms"`) is written
 * to the `out_err` out-param when present. Caller frees the
 * message via `net_rpc_free_cstring`.
 *
 * Stable error-message kinds (the prefix before the first ':'):
 *
 *   no_route         — no node advertises the requested service
 *   timeout          — call exceeded its deadline
 *   server_error     — handler returned a typed error status
 *                      (`status=0xNNNN message=…`)
 *   transport        — wire-level failure (peer dropped, disconnect)
 *   codec_encode     — request body failed to encode
 *   codec_decode     — response body failed to decode
 *   cancelled        — call cancelled via net_rpc_cancel_call
 *
 * The Go binding's `parseRpcError` re-prefixes these with `nrpc:`
 * for end-user error strings; non-Go consumers SHOULD do the same
 * for cross-binding error-message parity.
 *
 * # Tokio runtime
 *
 * The crate owns a lazy `OnceLock<Arc<Runtime>>` for blocking
 * into the SDK's async surface. The Go consumer wraps each call
 * in a goroutine for concurrency; non-Go consumers pick whatever
 * threading discipline matches their language.
 */

#ifndef NET_RPC_H
#define NET_RPC_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* =========================================================================
 * ABI version
 * ========================================================================= */

/* ABI version stamp — bumped on any breaking change to this
 * surface (signature changes, error-code re-numbering, opaque-
 * struct layout shifts, semantic shifts in lifetime contracts).
 * Consumers SHOULD compare against their compiled-in expected
 * version at process init and refuse to load a mismatch.
 *
 *   0x0001 — initial release: lifecycle + unary call /
 *            call_service / find_service_nodes / serve / serve
 *            handle plus streaming (call_streaming, stream_next,
 *            stream_grant, stream_close, stream_free) and
 *            cancellation tokens. */
#define NET_RPC_ABI_VERSION 0x0001

uint32_t net_rpc_abi_version(void);

/* =========================================================================
 * Error codes
 * ========================================================================= */

#define NET_RPC_OK                  0
#define NET_RPC_ERR_NULL           -1
#define NET_RPC_ERR_CALL_FAILED    -2
#define NET_RPC_ERR_ALREADY_SERVING -3
#define NET_RPC_ERR_NO_DISPATCHER  -4
#define NET_RPC_ERR_INVALID_UTF8   -5
#define NET_RPC_ERR_STREAM_DONE    -6

/* =========================================================================
 * Opaque handle types
 * ========================================================================= */

typedef struct MeshRpcHandle      MeshRpcHandle;
typedef struct ServeHandleC       ServeHandleC;
typedef struct RpcStreamHandleC   RpcStreamHandleC;

/* =========================================================================
 * Free helpers
 * ========================================================================= */

/* Free a CString returned out-of-band (e.g. an `out_err` message
 * from `net_rpc_call`). Idempotent on NULL. */
void net_rpc_free_cstring(char* s);

/* Free a response-body buffer returned via `out_resp_ptr` /
 * `out_chunk_ptr` from `net_rpc_call` / `_call_service` /
 * `_stream_next`. Idempotent on NULL or zero-length.
 *
 * Layout invariant: every site that hands a buffer out does so
 * via `Box<[u8]>::into_raw`, whose layout is `(ptr, len)` with
 * `cap == len` baked in. Pass the SAME `len` you received. */
void net_rpc_response_free(uint8_t* ptr, size_t len);

/* Free an array of u64 node ids returned by
 * `net_rpc_find_service_nodes`. Idempotent on NULL or zero. */
void net_rpc_find_service_nodes_free(uint64_t* ptr, size_t count);

/* =========================================================================
 * Lifecycle
 * ========================================================================= */

/* Build a new MeshRpc from an `Arc<MeshNode>` cloned via
 * `net_mesh_arc_clone` (defined in the main `libnet` cdylib —
 * see `net.h` / `net.go.h`).
 *
 * Ownership: `node_arc` is CONSUMED on success — the MeshRpc
 * takes the Arc content via `Box::from_raw`. Caller MUST NOT
 * free `node_arc` after a successful call. On NULL input the
 * pointer is left intact. */
MeshRpcHandle* net_rpc_new(void* node_arc);

/* Free a MeshRpc handle. The underlying MeshNode stays alive so
 * long as another `Arc` to it is held. Idempotent on NULL. */
void net_rpc_free(MeshRpcHandle* handle);

/* Diagnostic accessor — monotonic id of this MeshRpc. */
uint64_t net_rpc_id(const MeshRpcHandle* handle);

/* =========================================================================
 * Cancellation
 *
 * Cancellation tokens are reserved BEFORE the call, then passed
 * to `net_rpc_call` / `_call_service`. A parallel
 * `net_rpc_cancel_call(token)` from another thread aborts the
 * in-flight future — the SDK fires CANCEL on the wire, the
 * call returns a `cancelled:` error. Reserving up-front closes
 * the "cancel arrives before registration" race: a token MUST
 * be reserved before the call starts, otherwise cancel is a
 * no-op.
 *
 * Pass `0` for `cancel_token` to opt out of cancellation. Tokens
 * are monotonic from 1 and never reused.
 * ========================================================================= */

uint64_t net_rpc_reserve_cancel_token(void);
void     net_rpc_cancel_call(uint64_t token);

/* =========================================================================
 * Handler dispatcher (consumer-side trampoline)
 *
 * Consumer registers ONE process-wide trampoline at init via
 * `net_rpc_set_handler_dispatcher`. Subsequent `net_rpc_serve`
 * calls allocate a `handler_id` (via `net_rpc_reserve_handler_id`)
 * and pass it to serve; on incoming requests Rust invokes the
 * trampoline with that id, the consumer looks up its callback
 * in its own registry, runs it, returns the response bytes.
 *
 * Pre-registration is load-bearing: the consumer MUST insert its
 * callback into its registry under the reserved id BEFORE
 * calling `net_rpc_serve`. A request landing between `serve`
 * returning and the consumer's `Store` would otherwise hit an
 * empty registry slot.
 *
 * Response-buffer ownership: the consumer allocates via
 * `malloc(3)`; Rust copies the bytes into its own `Bytes` and
 * frees the consumer's buffer via `free(3)`. Same for the
 * `out_err` CString.
 * ========================================================================= */

typedef int (*RpcHandlerFn)(
    uint64_t handler_id,
    const uint8_t* req_ptr,
    size_t req_len,
    uint8_t** out_resp_ptr,
    size_t* out_resp_len,
    char** out_err);

/* Idempotent first-call-wins. The Go binding calls this once in
 * its package init; non-Go consumers do the same at startup. */
void net_rpc_set_handler_dispatcher(RpcHandlerFn dispatcher);

/* Reserve the next monotonic handler id without registering
 * anything. The consumer stores its callback in its registry
 * under this id, THEN calls `net_rpc_serve` with the same id.
 * Unused reservations are harmless. */
uint64_t net_rpc_reserve_handler_id(void);

/* =========================================================================
 * Unary calls
 * ========================================================================= */

/* Direct-addressed unary call. Blocks the calling thread via the
 * shared tokio runtime; consumers that want concurrency wrap in
 * a thread / goroutine / etc.
 *
 * Inputs:
 *   - `handle`           — MeshRpc handle from net_rpc_new.
 *   - `target_node_id`   — explicit destination.
 *   - `service_ptr/_len` — UTF-8 service name (no NUL required).
 *   - `req_ptr/_len`     — request body bytes (NULL+0 = empty).
 *   - `deadline_ms`      — absolute-deadline cap; 0 = no deadline.
 *   - `cancel_token`     — token from net_rpc_reserve_cancel_token,
 *                          or 0 to opt out.
 *
 * On success: writes `(out_resp_ptr, out_resp_len)`, returns
 * NET_RPC_OK. Caller frees buffer via net_rpc_response_free.
 *
 * On failure: writes `<kind>: <message>` to `out_err` (caller
 * frees via net_rpc_free_cstring), returns
 * NET_RPC_ERR_CALL_FAILED. */
int net_rpc_call(
    MeshRpcHandle* handle,
    uint64_t target_node_id,
    const char* service_ptr, size_t service_len,
    const uint8_t* req_ptr, size_t req_len,
    uint64_t deadline_ms,
    uint64_t cancel_token,
    uint8_t** out_resp_ptr, size_t* out_resp_len,
    char** out_err);

/* Service-discovery unary call. Same semantics as net_rpc_call
 * but resolves `service` against the local capability index
 * instead of taking an explicit target. Returns `no_route` in
 * `out_err` when no node advertises the service. */
int net_rpc_call_service(
    MeshRpcHandle* handle,
    const char* service_ptr, size_t service_len,
    const uint8_t* req_ptr, size_t req_len,
    uint64_t deadline_ms,
    uint64_t cancel_token,
    uint8_t** out_resp_ptr, size_t* out_resp_len,
    char** out_err);

/* =========================================================================
 * Header-bearing call variants (Phase 9b end-to-end)
 *
 * The legacy net_rpc_call / _call_service / _call_streaming
 * don't take request headers. These three additive variants
 * accept a (headers, count) pair and forward it to the inner
 * `CallOptions::request_headers`. Predicate-pushdown via the
 * `cyberdeck-where:` header (built by `net_predicate_to_where_header`
 * in net.go.h) traverses the FFI through these variants.
 *
 * Header buffers are referenced for the call's duration only —
 * Rust copies into owned (String, Vec<u8>) before dispatching,
 * so the C consumer can release / reuse the memory once the
 * function returns. NULL headers_ptr with header_count=0 is
 * equivalent to the legacy variant.
 *
 * Header NAMES must be valid UTF-8 (the substrate uses lowercase
 * `cyberdeck-*` / `nrpc-*` convention but doesn't enforce a
 * format beyond the MAX_RPC_HEADER_NAME_LEN cap). VALUES are
 * opaque bytes — any encoding the receiving handler agrees on.
 * ========================================================================= */

/* FFI-side request-header descriptor. Consumer allocates an
 * array of these, fills each entry with slices it owns, and
 * passes (array, count) to a `_with_headers` variant. */
typedef struct {
    const char* name_ptr;
    size_t      name_len;
    const uint8_t* value_ptr;
    size_t      value_len;
} net_rpc_header_t;

/* `net_rpc_call` plus request headers. */
int net_rpc_call_with_headers(
    MeshRpcHandle* handle,
    uint64_t target_node_id,
    const char* service_ptr, size_t service_len,
    const uint8_t* req_ptr, size_t req_len,
    uint64_t deadline_ms,
    uint64_t cancel_token,
    const net_rpc_header_t* headers_ptr,
    size_t header_count,
    uint8_t** out_resp_ptr, size_t* out_resp_len,
    char** out_err);

/* `net_rpc_call_service` plus request headers. */
int net_rpc_call_service_with_headers(
    MeshRpcHandle* handle,
    const char* service_ptr, size_t service_len,
    const uint8_t* req_ptr, size_t req_len,
    uint64_t deadline_ms,
    uint64_t cancel_token,
    const net_rpc_header_t* headers_ptr,
    size_t header_count,
    uint8_t** out_resp_ptr, size_t* out_resp_len,
    char** out_err);

/* `net_rpc_call_streaming` plus request headers. */
int net_rpc_call_streaming_with_headers(
    MeshRpcHandle* handle,
    uint64_t target_node_id,
    const char* service_ptr, size_t service_len,
    const uint8_t* req_ptr, size_t req_len,
    uint64_t deadline_ms,
    uint32_t stream_window,
    const net_rpc_header_t* headers_ptr,
    size_t header_count,
    RpcStreamHandleC** out_stream,
    char** out_err);

/* N-16: cancellable variant of net_rpc_call_streaming. The
 * construction block_on (awaiting the peer's initial-frame ACK)
 * runs under a cancel_token-keyed AbortHandle, so a parallel
 * net_rpc_cancel_call(cancel_token) aborts mid-construction
 * rather than waiting for the stream handle to materialize. The
 * unary path got this discipline as CR-13; this is the streaming
 * sibling. cancel_token == 0 short-circuits to the original
 * non-cancellable path. */
int net_rpc_call_streaming_cancellable(
    MeshRpcHandle* handle,
    uint64_t target_node_id,
    const char* service_ptr, size_t service_len,
    const uint8_t* req_ptr, size_t req_len,
    uint64_t deadline_ms,
    uint32_t stream_window,
    uint64_t cancel_token,
    RpcStreamHandleC** out_stream,
    char** out_err);

/* N-16: cancellable variant of net_rpc_call_streaming_with_headers.
 * Same cancellation contract as net_rpc_call_streaming_cancellable. */
int net_rpc_call_streaming_with_headers_cancellable(
    MeshRpcHandle* handle,
    uint64_t target_node_id,
    const char* service_ptr, size_t service_len,
    const uint8_t* req_ptr, size_t req_len,
    uint64_t deadline_ms,
    uint32_t stream_window,
    uint64_t cancel_token,
    const net_rpc_header_t* headers_ptr,
    size_t header_count,
    RpcStreamHandleC** out_stream,
    char** out_err);

/* All node ids advertising `nrpc:<service>` in the local
 * capability index. On success writes a heap-allocated `u64`
 * array of length `*out_count` to `*out_ptr`; caller frees via
 * net_rpc_find_service_nodes_free. Empty result writes `(NULL,
 * 0)` and still returns NET_RPC_OK — only NULL-input or non-
 * UTF-8 service names produce a negative return. */
int net_rpc_find_service_nodes(
    MeshRpcHandle* handle,
    const char* service_ptr, size_t service_len,
    uint64_t** out_ptr, size_t* out_count,
    char** out_err);

/* =========================================================================
 * Serve (handler registration)
 * ========================================================================= */

/* Register a handler for `service`. Caller MUST have:
 *   1. Reserved `handler_id` via net_rpc_reserve_handler_id.
 *   2. Inserted the callback into its language-side registry
 *      under that id (so request-arrives-before-Store is
 *      impossible — see the dispatcher section above).
 *   3. Installed the trampoline via
 *      net_rpc_set_handler_dispatcher.
 *
 * `handler_timeout_ms` caps each handler invocation. `0` means
 * default 60 000 ms; `UINT64_MAX` effectively disables (not
 * recommended — a stuck handler holds a runtime worker).
 *
 * Returns: heap-allocated ServeHandle on success; NULL with an
 * error message on `out_err` on failure (e.g. service already
 * served by this MeshNode → message starts with
 * `already_serving:`). */
ServeHandleC* net_rpc_serve(
    MeshRpcHandle* handle,
    const char* service_ptr, size_t service_len,
    uint64_t handler_id,
    uint64_t handler_timeout_ms,
    char** out_err);

/* Diagnostic accessor — the handler_id this ServeHandle was
 * registered under. Returns 0 on NULL handle. */
uint64_t net_rpc_serve_handle_id(const ServeHandleC* handle);

/* Stop serving. Drops the inner SDK ServeHandle which
 * deregisters the handler. Idempotent: a second close is a
 * no-op. The handle struct itself is still valid until
 * net_rpc_serve_handle_free. */
void net_rpc_serve_handle_close(ServeHandleC* handle);

/* Free the handle struct. Implicitly closes if not already
 * closed. Idempotent on NULL. */
void net_rpc_serve_handle_free(ServeHandleC* handle);

/* =========================================================================
 * Streaming calls
 *
 * Construct via net_rpc_call_streaming, drain via net_rpc_stream_next,
 * grant credits via net_rpc_stream_grant, terminate via
 * net_rpc_stream_close, release via net_rpc_stream_free.
 *
 * Lifecycle invariants:
 *   - net_rpc_stream_close marks the stream done. Subsequent
 *     net_rpc_stream_next calls return NET_RPC_ERR_STREAM_DONE.
 *   - The stream auto-marks done on clean end (next returns
 *     NET_RPC_ERR_STREAM_DONE with NULL chunk) AND on mid-
 *     stream error (next returns NET_RPC_ERR_CALL_FAILED with
 *     out_err written; further calls return STREAM_DONE).
 *   - net_rpc_stream_free implicitly closes if not already
 *     closed. Always pair _new with _free.
 * ========================================================================= */

/* Direct-addressed streaming call. Constructs the underlying
 * `RpcStream` synchronously (block_on under the hood) and
 * writes an opaque handle to `*out_stream`.
 *
 * `stream_window` of 0 disables flow control (auto-grant only);
 * non-zero installs an initial credit window equal to that
 * value. */
int net_rpc_call_streaming(
    MeshRpcHandle* handle,
    uint64_t target_node_id,
    const char* service_ptr, size_t service_len,
    const uint8_t* req_ptr, size_t req_len,
    uint64_t deadline_ms,
    uint32_t stream_window,
    RpcStreamHandleC** out_stream,
    char** out_err);

/* Block until the next chunk arrives, the stream terminates,
 * OR a mid-stream error fires.
 *
 * Outcomes:
 *   - chunk available:    *out_chunk_ptr/_len set, returns
 *                         NET_RPC_OK. Caller frees buffer via
 *                         net_rpc_response_free.
 *   - clean end:          *out_chunk_ptr=NULL, *out_chunk_len=0,
 *                         returns NET_RPC_ERR_STREAM_DONE.
 *                         Subsequent calls return same code.
 *   - mid-stream error:   *out_err set with structured kind,
 *                         returns NET_RPC_ERR_CALL_FAILED. The
 *                         stream is also marked done.
 *   - close raced:        returns NET_RPC_ERR_STREAM_DONE
 *                         (close took ownership before us). */
int net_rpc_stream_next(
    RpcStreamHandleC* stream,
    uint8_t** out_chunk_ptr, size_t* out_chunk_len,
    char** out_err);

/* Explicitly grant `amount` more credits to the server's pump.
 * No-op when flow control wasn't enabled OR the stream is
 * already done. Returns NET_RPC_OK on no-op too. */
int net_rpc_stream_grant(RpcStreamHandleC* stream, uint32_t amount);

/* Diagnostic accessor — server-assigned call_id captured at
 * stream construction. Returns 0 on NULL handle. */
uint64_t net_rpc_stream_call_id(const RpcStreamHandleC* stream);

/* Cancel the stream. Sends best-effort CANCEL via the SDK's
 * Drop impl and latches the stream as done. Idempotent on NULL
 * or already-closed. */
void net_rpc_stream_close(RpcStreamHandleC* stream);

/* Free the stream handle. Implicitly closes if not already
 * closed. Idempotent on NULL. */
void net_rpc_stream_free(RpcStreamHandleC* stream);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* NET_RPC_H */
