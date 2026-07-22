/*
 * net_org.h — C SDK header for libnet_org (the organization
 * capability-auth C ABI).
 *
 * One header, one shared library. Mirrors the layout of `net_rpc.h`
 * next to it. Symbols live in the `libnet_org.{so,dylib,dll}` cdylib
 * built from `bindings/go/org-ffi`. The Go binding's `go/org.go` cgo
 * include block is the de-facto contract for non-Go consumers; this
 * file is the canonical drop-in for C / C++ / Zig / Swift / Java JNI.
 *
 * This is the organization verb facade — the same two verbs (call,
 * serve), five concepts, and four error domains the Rust, Node, and
 * Python SDKs expose. It wraps `net_sdk::org`; every authority
 * decision already happened in Rust. C code here is marshaling.
 *
 * # Build
 *
 *   cargo build --release -p net-org-ffi
 *
 *   Linux:   target/release/libnet_org.so
 *   macOS:   target/release/libnet_org.dylib
 *   Windows: target/release/net_org.dll
 *
 * The cdylib wraps `Arc<MeshNode>` handles minted by the base
 * `libnet` (via `net_mesh_arc_clone`), so link both:
 *
 *   gcc -o app app.c -L target/release -lnet_org -lnet -lpthread -ldl -lm
 *
 * # ABI versioning
 *
 * Call `net_org_check_abi_version(NET_ORG_ABI_VERSION)` at process
 * init and refuse to load on a negative return. The org surface
 * carries its OWN ABI stamp, versioned independently of net_rpc's,
 * starting at `0x0001`.
 *
 * # Handle model
 *
 * Every Rust object that crosses the FFI is a heap-allocated `Box`
 * handed back as `*mut T`. The caller owns the pointer and MUST call
 * the matching `_free` exactly once. The frees take a DOUBLE pointer
 * (`T**`) and NULL the caller's slot, so a language finalizer racing
 * an explicit close cannot double-free.
 *
 * # The secret asymmetry (the one rule that shapes this header)
 *
 * Public signed credentials (membership, dispatcher grant, capability
 * grants) cross as wire BYTES — they are designed to transit. The
 * audience secret — the raw 32-byte discovery key — crosses ONLY as
 * a file PATH. There is deliberately no bytes variant, in any
 * function, ever: a discovery key must never enter a garbage-collected
 * runtime's heap. Rust loads each secret through a checked loader
 * (validates the opened object, zeroizes on drop) and never returns
 * one.
 *
 * # Error model
 *
 * `int` return codes — `NET_ORG_OK` (0) on success, negative on
 * failure. The four CALL domains map to distinct codes
 * (CREDENTIALS / DISCOVERY / ADMISSION_DENIED / RPC) so a caller can
 * branch without parsing; the full `org:<domain>:<kind>` wire string
 * is written to the `out_err` out-param. Caller frees the message via
 * `net_org_free_cstring`.
 *
 *   org:credentials:<kind>       LOCAL — nothing was sent
 *   org:discovery:<kind>         LOCAL — nothing was sent
 *   org:admission_denied:<coarse> REMOTE — the provider's engine refused
 *                                 (denied / not_supported / unavailable —
 *                                  a precise reason would be a credential
 *                                  oracle, so the bucket is deliberately coarse)
 *   org:rpc:<nrpc-kind>: <detail> transport / non-admission server error
 *
 * A binding that cannot classify a wire string reports `unknown`
 * (NET_ORG_ERR_UNCLASSIFIED) — never one of the four canonical
 * domains it could not establish, and never a success.
 *
 * Provisioning failures (NET_ORG_ERR_PROVISION) are their own class:
 * a node either starts correctly or it does not, so they are NOT a
 * call-domain result and carry a plain message, not the org: wire.
 *
 * # Disposal, and its security consequence
 *
 * Close every client. While an OrgClient is un-closed its
 * consumer-audience lease stays installed, so the node keeps ingest
 * authority for those grants — it can still open and store inbound
 * private announcements for a credential set the application has
 * logically finished with. Closing is not hygiene; it is the
 * withdrawal step.
 */

#ifndef NET_ORG_H
#define NET_ORG_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ======================================================================
 * ABI version.
 * ==================================================================== */

/* The ABI this header describes. Independent of NET_RPC_ABI_VERSION. */
#define NET_ORG_ABI_VERSION 0x0001

/* Returns the ABI version the loaded library was built with. */
uint32_t net_org_abi_version(void);

/* Returns NET_ORG_OK iff the loaded library is at least `expected`
 * (a newer library satisfies an older header). Returns
 * NET_ORG_ERR_NULL if the loaded library is older. Pin
 * NET_ORG_ABI_VERSION at init and hard-fail on a negative return. */
int net_org_check_abi_version(uint32_t expected);

/* ======================================================================
 * Error codes.
 *
 * libnet_org uses its own small negative namespace starting at -1
 * (the standalone-cdylib house convention that net_rpc.h / net_deck.h
 * / net_meshos.h follow), NOT the base surface's shared net_error_t
 * enum. Kept in sync with the Rust `pub const NET_ORG_*` in
 * bindings/go/org-ffi/src/lib.rs.
 * ==================================================================== */

#define NET_ORG_OK                     0
#define NET_ORG_ERR_NULL              -1   /* null/invalid pointer or buffer */
#define NET_ORG_ERR_INVALID_UTF8      -2   /* a (ptr,len) string was non-UTF-8 */
#define NET_ORG_ERR_CREDENTIALS       -3   /* CALL domain: local credential refusal */
#define NET_ORG_ERR_DISCOVERY         -4   /* CALL domain: no authorized provider */
#define NET_ORG_ERR_ADMISSION_DENIED  -5   /* CALL domain: provider engine refused */
#define NET_ORG_ERR_RPC               -6   /* CALL domain: transport / server error */
#define NET_ORG_ERR_CLOSED            -7   /* client handle is closed (binding sentinel) */
#define NET_ORG_ERR_UNCLASSIFIED      -8   /* parser/ABI fallback (§D5a) */
#define NET_ORG_ERR_NO_DISPATCHER     -9   /* serve before set_handler_dispatcher */
#define NET_ORG_ERR_ALREADY_SERVING  -10   /* serve: service already served on this node */
#define NET_ORG_ERR_SERVE            -11   /* serve: other failure (bad name, no authority) */
#define NET_ORG_ERR_PROVISION        -12   /* provisioning (§D9) failed — NOT a call domain */

/* ======================================================================
 * Access modes (who may call, and how the service is announced).
 * ==================================================================== */

#define NET_ORG_ACCESS_SAME_ORG 0  /* this node's own organization */
#define NET_ORG_ACCESS_GRANTED  1  /* another org holding a capability grant */

/* ======================================================================
 * Opaque handle types.
 * ==================================================================== */

typedef struct NetOrgCredentials NetOrgCredentials;
typedef struct NetOrgClient      NetOrgClient;
typedef struct NetOrgServeHandle NetOrgServeHandle;

/* The Arc<MeshNode> handle minted by the base libnet. Declared there
 * as `net_compute_mesh_arc_t`; forward-declared here so this header is
 * self-contained. The same underlying pointer. */
typedef struct net_compute_mesh_arc_t net_compute_mesh_arc_t;

/* ======================================================================
 * net_org_caller_t — the provider-verified facts about an admitted call.
 *
 * Five 32-byte ids, an exact projection of the Rust `OrgCaller`. Every
 * field was verified by the admission engine before the handler ran;
 * none is caller-claimed. Layout is part of the ABI (guarded by a Rust
 * offset test) — 160 bytes, no padding.
 * ==================================================================== */

typedef struct {
    uint8_t caller[32];        /* the acting entity S */
    uint8_t acting_org[32];    /* the organization S acted for */
    uint8_t provider_org[32];  /* this provider's owner organization */
    uint8_t provider[32];      /* this exact provider node */
    uint8_t capability[32];    /* the capability that was invoked */
} net_org_caller_t;

/* ======================================================================
 * Out-of-band buffer frees.
 * ==================================================================== */

/* Free a CString returned via an `out_err` out-param. Idempotent on NULL. */
void net_org_free_cstring(char* s);

/* Free a response body returned via `out_resp_ptr` (from net_org_call).
 * Pass the SAME `len` you received. Idempotent on NULL or zero length. */
void net_org_response_free(uint8_t* ptr, size_t len);

/* ======================================================================
 * OrgCredentials — public bytes + audience-secret PATHS.
 * ==================================================================== */

/* Build a validated credential set. Public credentials cross as bytes;
 * audience secrets cross ONLY as file paths (there is no bytes variant).
 *
 * `grant_ptrs` / `grant_lens` are parallel arrays of length `grant_count`
 * (may be 0). `audience_secret_paths` is an array of NUL-terminated path
 * strings of length `audience_secret_count` (may be 0).
 *
 * On success `*out_creds` receives an owned handle. On failure returns a
 * negative code (typically NET_ORG_ERR_CREDENTIALS) and writes the
 * `org:credentials:<kind>` wire to `*out_err`. Signature verification and
 * binding checks run in Rust; the caller never sees a key. */
int net_org_credentials_new(
    const uint8_t* membership_ptr, size_t membership_len,
    const uint8_t* dispatcher_ptr, size_t dispatcher_len,
    const uint8_t* const* grant_ptrs, const size_t* grant_lens, size_t grant_count,
    const char* const* audience_secret_paths, size_t audience_secret_count,
    NetOrgCredentials** out_creds, char** out_err);

/* Free an unconsumed credential handle and NULL `*credentials`.
 * Idempotent on NULL or a pointer to NULL. A handle consumed by
 * net_org_bind is already NULL, so this is then a no-op. */
void net_org_credentials_free(NetOrgCredentials** credentials);

/* ======================================================================
 * OrgClient — bind, call, cancel, close.
 * ==================================================================== */

/* Bind a credential set to a mesh node, producing a client.
 *
 * `mesh_arc` MUST come from net_mesh_arc_clone; it is CONSUMED here (the
 * same ownership transfer net_rpc_new makes) — mint a fresh clone per bind
 * and do NOT free it. The node itself lives on via the Go MeshNode's own
 * Arc; a failed bind simply drops this clone.
 *
 * `credentials` is CONSUMED unconditionally — the bind takes the set by
 * value, so `*credentials` is set to NULL on BOTH success and failure. A
 * failed bind means the credentials do not match this node's identity or
 * authority; retrying with the same set would not succeed.
 *
 * On success `*out_client` receives an owned handle. On failure returns
 * NET_ORG_ERR_CREDENTIALS and writes the `org:credentials:<kind>` wire to
 * `*out_err`. Requires an installed node authority (see
 * net_org_install_authority). */
int net_org_bind(net_compute_mesh_arc_t* mesh_arc,
                 NetOrgCredentials** credentials,
                 NetOrgClient** out_client, char** out_err);

/* Close the client: releases the consumer-audience lease, frees the
 * handle, and NULLs `*client`. Idempotent on NULL or a pointer to NULL.
 * Every non-NULL handle must be freed exactly once — the double pointer
 * makes a finalizer/close double-free unrepresentable. */
void net_org_client_free(NetOrgClient** client);

/* Call a protected service. Bytes in, bytes out — the caller marshals.
 *
 * `deadline_ms == 0` means the facade's default; a positive value is a
 * hard deadline. `cancel_token == 0` means uncancellable; a non-zero
 * token (from net_org_reserve_cancel_token) lets a concurrent
 * net_org_cancel_call drop the one in-flight future. Deadline and cancel
 * are execution control — they select no provider, no grant, and no
 * authority.
 *
 * On success writes `(*out_resp_ptr, *out_resp_len)` (free with
 * net_org_response_free) and returns NET_ORG_OK. On failure returns the
 * domain code and writes the `org:<domain>:<kind>` wire to `*out_err`.
 * A signed proof is never resent: the facade never retries. */
int net_org_call(NetOrgClient* client,
                 const char* service_ptr, size_t service_len,
                 const uint8_t* req_ptr, size_t req_len,
                 uint64_t deadline_ms, uint64_t cancel_token,
                 uint8_t** out_resp_ptr, size_t* out_resp_len, char** out_err);

/* Reserve a cancel token scoped to this client's node, for a subsequent
 * cancellable net_org_call. Reserve BEFORE the call so a cancel that
 * races registration is still delivered. Returns 0 if `client` is NULL. */
uint64_t net_org_reserve_cancel_token(NetOrgClient* client);

/* Drop the ONE in-flight call bound to `cancel_token`. Idempotent; a
 * no-op on token 0, a NULL client, or a token no call reserved. It never
 * launches a second attempt (the no-retry rule). */
int net_org_cancel_call(NetOrgClient* client, uint64_t cancel_token);

/* ======================================================================
 * serve — register a protected handler.
 * ==================================================================== */

/* The process-wide handler dispatcher Rust calls when an admitted request
 * lands. It receives the reserved `handler_id`, the verified caller, and
 * the request bytes; on success it returns a response the caller allocated
 * with malloc via `(*out_resp_ptr, *out_resp_len)` and returns
 * NET_ORG_OK; on failure it writes `*out_err` and returns non-zero. Rust
 * copies the response and frees it with the C runtime's free. To signal a
 * typed application status, write an "nrpc:app_error:0x<code>:<body>"
 * message to `*out_err`. */
typedef int (*NetOrgHandlerFn)(
    uint64_t handler_id, const net_org_caller_t* caller,
    const uint8_t* req_ptr, size_t req_len,
    uint8_t** out_resp_ptr, size_t* out_resp_len, char** out_err);

/* Register the process-wide dispatcher. First call wins; later calls are
 * no-ops. Call once at init before net_org_serve. */
void net_org_set_handler_dispatcher(NetOrgHandlerFn dispatcher);

/* Reserve a fresh handler id. Reserve the id, store the callable under it
 * in the language-side registry, THEN call net_org_serve — pre-registration
 * closes the request-arrives-before-store race. */
uint64_t net_org_reserve_handler_id(void);

/* Register a protected service. `mesh_arc` is CONSUMED (mint a fresh clone
 * per call; do NOT free it — the node lives on via the Go MeshNode).
 * `access` is NET_ORG_ACCESS_SAME_ORG / _GRANTED.
 * `handler_id` MUST already be reserved AND stored in the language registry.
 *
 * On success `*out_handle` receives an owned handle. On failure returns a
 * negative code (NET_ORG_ERR_ALREADY_SERVING / _NO_DISPATCHER / _SERVE) and
 * writes a message to `*out_err`. Requires an installed node authority. */
int net_org_serve(net_compute_mesh_arc_t* mesh_arc,
                  const char* service_ptr, size_t service_len,
                  int access, uint64_t handler_id,
                  NetOrgServeHandle** out_handle, char** out_err);

/* Diagnostic: the handler_id of this ServeHandle. 0 on NULL. */
uint64_t net_org_serve_handle_id(const NetOrgServeHandle* handle);

/* Unregister the service. Idempotent; in-flight handlers continue but no
 * new requests are dispatched. No-op on NULL. */
void net_org_serve_handle_close(NetOrgServeHandle* handle);

/* Free the ServeHandle (implicitly closing) and NULL `*handle`. Idempotent
 * on NULL or a pointer to NULL. */
void net_org_serve_handle_free(NetOrgServeHandle** handle);

/* ======================================================================
 * Provisioning (§D9) — node startup, distinct from adoption / issuance.
 * ==================================================================== */

/* Install an adopted node authority from the directory `net node adopt`
 * wrote. Required before net_org_bind can succeed or a granted service can
 * serve. `mesh_arc` is CONSUMED (mint a fresh clone per call; do NOT free
 * it — the install mutates the node's shared state, visible via the Go
 * MeshNode afterward). On failure returns NET_ORG_ERR_PROVISION with a
 * plain message on `*out_err` (NOT a call-domain result). */
int net_org_install_authority(net_compute_mesh_arc_t* mesh_arc,
                              const char* dir_ptr, size_t dir_len,
                              char** out_err);

/* Install a PROVIDER grant audience so a granted service can seal
 * envelopes. The grant crosses as wire bytes; its secret crosses as a
 * PATH, never bytes. `mesh_arc` is CONSUMED (mint a fresh clone per call;
 * do NOT free it). A SameOrg provider does not need this; only a Granted
 * provider does. On failure returns NET_ORG_ERR_PROVISION with a plain
 * message. */
int net_org_install_provider_grant_audience(
    net_compute_mesh_arc_t* mesh_arc,
    const uint8_t* grant_ptr, size_t grant_len,
    const char* secret_path_ptr, size_t secret_path_len,
    char** out_err);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* NET_ORG_H */
