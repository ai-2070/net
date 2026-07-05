/*
 * net_mcp.h — C SDK header for libnet_mcp_ffi (the MCP bridge pure
 * helpers + the graduated consent / pin surface, C ABI).
 *
 * One header, one shared library. Mirrors the layout of `net.h` /
 * `net_meshdb.h` next to it. Symbols live in the
 * `libnet_mcp_ffi.{so,dylib}` / `net_mcp_ffi.dll` cdylib built from
 * `bindings/go/mcp-ffi`. This is the C face of exactly what the Python
 * (`bindings/python/src/{consent,mcp_helpers}.rs`) and Node
 * (`bindings/node/src/{consent,mcp_helpers}.rs`) bindings expose — one
 * Rust implementation, three faces. The Go binding's `go/mcp.go` cgo
 * block is the de-facto contract; this file is the canonical drop-in for
 * C / C++ / Zig / Swift / Java JNI / etc.
 *
 * # Build
 *
 *   cargo build --release -p net-mcp-ffi
 *
 *   Linux:   target/release/libnet_mcp_ffi.so
 *   macOS:   target/release/libnet_mcp_ffi.dylib
 *   Windows: target/release/net_mcp_ffi.dll
 *
 * # Link
 *
 *   gcc -o app app.c -L target/release -lnet_mcp_ffi -lpthread -ldl -lm
 *
 * # Scope
 *
 * Pure helpers (no mesh, no process, no secret ever crosses — the
 * bridge's forwarding / keychain internals are NOT bound):
 *
 *   net_mcp_classify      — credential-risk score a wrapped server, for
 *                           DISPLAY before publishing. Only env KEYS drive
 *                           detection; values never appear in the result.
 *   net_mcp_lower_tool    — lower an MCP tools/list entry to the Net
 *                           ToolDescriptor + bridge metadata (JSON DTO).
 *
 * Consent gate:
 *
 *   net_mcp_credential_requires_consent — the wire-"none"-is-never-trusted
 *                           boundary (a discovered capability can only ever
 *                           over-gate, never bypass consent).
 *   net_mcp_cap_id_canonicalize — canonical provider/capability id.
 *   ConsentPolicy (opaque) — the allowlist / pin gate.
 *
 * Pin store (path-scoped, cross-process-locked): every mutation runs the
 * core's full locked load->apply->save transaction, so the same file the
 * `net mcp pin` CLI and a running `net mcp serve` shim use is honored
 * bidirectionally. The store file is never opened here directly.
 *
 * # Memory model
 *
 * A `char*` returned by any function is heap-owned by the caller and MUST
 * be released with net_mcp_free_string exactly once (idempotent on NULL).
 * The one opaque handle, ConsentPolicy, is freed with
 * net_mcp_consent_policy_free (idempotent on NULL).
 *
 * # Error model
 *
 * Functions returning `char*` yield NULL on error. net_mcp_pin_state also
 * returns an empty string "" for "no record" (states are never empty, so
 * it is unambiguous). Functions returning `int` use -1 for error; a
 * non-negative value is the result (0/1 for a bool) — except
 * net_mcp_credential_requires_consent, which has no error return and gates
 * (returns 1) if a runtime panic is trapped, so a failure never under-gates.
 *
 * Detail for the most recent failure is available per-thread via
 * net_mcp_last_error_message (human-readable) and net_mcp_last_error_kind
 * (a stable tag: "invalid_arg", "classify_error", "pins_error",
 * "encode_error", "runtime_panic"). Both return NULL when no error has
 * been recorded on the calling thread; the returned pointers are valid
 * until the next FFI call on the same thread touches the thread-local and
 * must NOT be freed. Every entry point clears the last-error at the top,
 * so a NULL / -1 with no last-error set means "not an error" (e.g. an
 * absent pin record). Use net_mcp_clear_last_error to reset.
 *
 * # Threading
 *
 * Every function is safe to call from any thread. The pin-store functions
 * are serialized across processes by the store's own advisory file lock;
 * a ConsentPolicy handle is NOT internally synchronized — do not share one
 * handle across threads without external synchronization.
 */

#ifndef NET_MCP_H
#define NET_MCP_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Opaque handle to a consent policy (allowlist + pinned set). */
typedef struct ConsentPolicyHandle ConsentPolicyHandle;

/* ---- Last-error (thread-local) + free ---------------------------------- */

const char *net_mcp_last_error_message(void);
const char *net_mcp_last_error_kind(void);
void net_mcp_clear_last_error(void);

/* Free a char* returned by any net_mcp_* function. No-op on NULL. */
void net_mcp_free_string(char *s);

/* ---- Pure helpers ------------------------------------------------------ */

/*
 * Classify a wrapped MCP server's credential exposure. Returns the status
 * label ("credentialed" / "external_api" / "unknown" / "none") as an owned
 * C string, or NULL on error.
 *
 * args_json:  JSON array of strings (or NULL/empty for none).
 * envs_json:  JSON object { "KEY": "VALUE" } of env additions (only the
 *             KEYS drive detection; values never appear in the result).
 * credential_override: "detect" (or NULL) / "credentialed" /
 *             "no-credentials".
 * force:      non-zero confirms a downward override (net wrap
 *             --no-credentials --force).
 */
char *net_mcp_classify(const char *program, const char *args_json,
                       const char *envs_json, const char *credential_override,
                       int force);

/*
 * Lower one MCP tools/list entry (its JSON object) to the Net discovery
 * shape. Returns the DTO
 *   {"tool_id","mcp_name","descriptor":{...},"bridge_metadata":{...}}
 * as an owned JSON C string, or NULL on error.
 *
 * credential_status:  the exact label the classifier produced (trusted
 *             local input — "none" is accepted verbatim).
 * substitutability:   "provider_local" (or NULL) / "provider_equivalent".
 */
char *net_mcp_lower_tool(const char *tool_json, const char *server_version,
                         const char *credential_status,
                         const char *substitutability);

/* ---- Consent gate ------------------------------------------------------ */

/*
 * Does a wire-declared credential status require local consent? 1 if it
 * does, 0 if not. A wire "none" is NOT trusted (gates like "unknown"), so
 * "none" / NULL / anything unrecognised returns 1.
 */
int net_mcp_credential_requires_consent(const char *status);

/*
 * Canonicalize a provider/capability id (trims whitespace, rewrites a
 * 0x-hex node id to decimal). Returns the canonical display as an owned C
 * string, or NULL on a parse error. Free with net_mcp_free_string.
 */
char *net_mcp_cap_id_canonicalize(const char *display);

/* Allocate an empty consent policy. Free with net_mcp_consent_policy_free. */
ConsentPolicyHandle *net_mcp_consent_policy_new(void);
void net_mcp_consent_policy_free(ConsentPolicyHandle *policy);

/* Mutators: 0 on success, -1 on error. */
int net_mcp_consent_policy_allow(ConsentPolicyHandle *policy,
                                 const char *cap_id);
int net_mcp_consent_policy_pin(ConsentPolicyHandle *policy, const char *cap_id);
int net_mcp_consent_policy_unpin(ConsentPolicyHandle *policy,
                                 const char *cap_id);
/* 1 yes, 0 no, -1 error. */
int net_mcp_consent_policy_is_pinned(ConsentPolicyHandle *policy,
                                     const char *cap_id);
/*
 * Decide: returns "allowed" / "requires_approval" (owned C string; free
 * with net_mcp_free_string), or NULL on error.
 */
char *net_mcp_consent_policy_decide(ConsentPolicyHandle *policy,
                                    const char *cap_id,
                                    const char *credential_status);
/* Pinned display ids as an owned JSON array string (sorted), or NULL. */
char *net_mcp_consent_policy_pinned(ConsentPolicyHandle *policy);

/* ---- Pin store (path-scoped, cross-process-locked) --------------------- */

/*
 * request: model-callable verb; writes "pending" if absent, else leaves
 * the record untouched. Returns the resulting state ("pending"/"approved")
 * as an owned C string, or NULL on error.
 */
char *net_mcp_pin_request(const char *store_path, const char *cap_id);
/* approve/reject: operator verbs. 1 if it changed the store, 0 if not, -1 error. */
int net_mcp_pin_approve(const char *store_path, const char *cap_id);
int net_mcp_pin_reject(const char *store_path, const char *cap_id);
/* is_approved: 1 yes, 0 no, -1 error. */
int net_mcp_pin_is_approved(const char *store_path, const char *cap_id);
/*
 * state: "pending"/"approved" (owned C string), an empty string "" when
 * there is no record, or NULL on error.
 */
char *net_mcp_pin_state(const char *store_path, const char *cap_id);
/*
 * list: all records as an owned JSON-array string of {"cap_id","state"}
 * (sorted by cap_id), or NULL on error.
 */
char *net_mcp_pin_list(const char *store_path);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* NET_MCP_H */
