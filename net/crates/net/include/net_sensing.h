/*
 * net_sensing.h — C SDK header for capability sensing
 * (SENSING_OLB_COMPLETION_PLAN.md B2).
 *
 * These symbols sit beside net_org.h's, in the org-ffi rlib that is linked
 * into `libnet`. They need `net-mesh-sdk`, which the base FFI inside the
 * `net` crate cannot depend on and the org surface already does. Like every
 * other header here, this one resolves out of one `libnet`:
 *
 *   cargo build --release -p net-ffi
 *   gcc -o app app.c -L target/release -lnet -lpthread -ldl -lm
 *
 * # The surface, in one paragraph
 *
 * A PROVIDER serves readiness for a capability as a stored VALUE
 * (net_sensing_provide / net_sensing_provider_set). Rust signs whatever value
 * is current on each attestation beat, so no callback crosses this boundary
 * per beat. A CONSUMER watches its own organization's authorized providers of
 * a capability (net_sensing_watch). It takes snapshots, and blocks in
 * net_sensing_watch_changed until something that can change the answer moved.
 * A snapshot can steer a service call (net_sensing_call_service): the
 * candidates are authorized exactly as an unsensed call authorizes them,
 * viable providers lead, not-ready ones go last, nothing is removed, and the
 * target is invoked once.
 *
 * # Enabling sensing on a node
 *
 * Config-time state, in the JSON `net_mesh_new` already takes:
 *
 *   uint64_t incarnation;
 *   net_sensing_next_incarnation(path, strlen(path), &incarnation, &err);
 *   // {"bind_addr": "...", "psk_hex": "...", "identity_seed_hex": "...",
 *   //  "enable_sensing": true, "sensing_incarnation": <incarnation>}
 *
 * net_sensing_next_incarnation reads, increments and DURABLY persists the boot
 * counter (temp file, sync, rename) before returning it. Pass only a value
 * obtained that way: a value that was never persisted could be reissued by a
 * later boot. Providing readiness also needs a seeded identity.
 *
 * # Error model
 *
 * `int` returns. A sensing refusal is NET_SENSING_ERR_SENSING, with the Rust
 * SDK's `sensing:<kind>: <message>` wire string in `out_err`.
 * net_sensing_error_class returns Rust's kind and terminal flag for it, so no
 * caller keeps a classification table of its own. The shared fixture
 * tests/cross_lang_sensing/error_vectors.json lists every kind.
 *
 * # Threads
 *
 * net_sensing_watch_changed blocks the calling thread. net_sensing_watch_close
 * may run on ANOTHER thread while it is blocked; the blocked call then returns
 * NET_SENSING_ERR_SENSING with `sensing:watch_closed:`. A timeout returns
 * NET_SENSING_TIMEOUT and loses no edge. Do not free a watch while a call on
 * it is running: close it first.
 *
 * # Ownership
 *
 * `void* mesh_arc` comes from net_mesh_arc_clone and is CONSUMED on every
 * path. Handles are freed through their _free with a double pointer that is
 * NULLed. Strings (out_err, out_kind, out_json) are freed with
 * net_org_free_cstring, and response bodies with net_org_response_free.
 */

#ifndef NET_SENSING_H
#define NET_SENSING_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define NET_SENSING_OK                 0
#define NET_SENSING_ERR_NULL          -1
#define NET_SENSING_ERR_INVALID_UTF8  -2
#define NET_SENSING_ERR_SENSING       -3
#define NET_SENSING_TIMEOUT           -4
#define NET_SENSING_ERR_RPC           -5
#define NET_SENSING_ERR_INCARNATION   -6
#define NET_SENSING_ERR_INVALID_ARG   -7

#define NET_SENSING_STATE_READY        0
#define NET_SENSING_STATE_NOT_READY    1
#define NET_SENSING_STATE_UNEVALUABLE  2

typedef struct NetSensingProvider NetSensingProvider;
typedef struct NetSensingWatch    NetSensingWatch;
typedef struct NetSensingSnapshot NetSensingSnapshot;

uint32_t net_sensing_abi_version(void);

int net_sensing_next_incarnation(const char* path_ptr, size_t path_len,
                                 uint64_t* out_incarnation, char** out_err);

int net_sensing_error_class(const char* wire_ptr, size_t wire_len,
                            char** out_kind, int* out_terminal);

/* estimated_start_ms < 0 means no estimate; reason applies to NOT_READY. */
int  net_sensing_provide(void* mesh_arc,
                         const char* capability_ptr, size_t capability_len,
                         int state, int64_t estimated_start_ms, uint32_t reason,
                         NetSensingProvider** out_provider, char** out_err);
int  net_sensing_provider_set(const NetSensingProvider* provider,
                              int state, int64_t estimated_start_ms,
                              uint32_t reason, int* out_moved);
int  net_sensing_provider_close(const NetSensingProvider* provider);
void net_sensing_provider_free(NetSensingProvider** provider);

/* within_ms / start_within_ms < 0 mean "not set". */
int  net_sensing_watch(void* mesh_arc,
                       const char* capability_ptr, size_t capability_len,
                       int64_t within_ms, int64_t start_within_ms,
                       NetSensingWatch** out_watch, char** out_err);
int  net_sensing_watch_snapshot(const NetSensingWatch* watch,
                                NetSensingSnapshot** out_snapshot, char** out_err);
/* timeout_ms < 0 waits without a bound. */
int  net_sensing_watch_changed(const NetSensingWatch* watch, int64_t timeout_ms,
                               char** out_err);
int  net_sensing_watch_close(const NetSensingWatch* watch);
void net_sensing_watch_free(NetSensingWatch** watch);

int  net_sensing_snapshot_json(const NetSensingSnapshot* snapshot, char** out_json);
void net_sensing_snapshot_free(NetSensingSnapshot** snapshot);

/* timeout_ms == 0 uses the default call deadline. */
int net_sensing_call_service(void* mesh_arc,
                             const char* service_ptr, size_t service_len,
                             const uint8_t* req_ptr, size_t req_len,
                             const NetSensingSnapshot* snapshot, uint64_t timeout_ms,
                             uint8_t** out_resp, size_t* out_len, char** out_err);

#ifdef __cplusplus
}
#endif

#endif /* NET_SENSING_H */
