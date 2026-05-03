/*
 * Net C API Header
 *
 * High-performance event bus for AI runtime workloads.
 * This header provides C-compatible bindings for use with CGO.
 */

#ifndef NET_SDK_H
#define NET_SDK_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Opaque handle to the event bus */
typedef void* net_handle_t;

/*
 * Error codes.
 *
 * Kept in sync with the Rust-side `NetError` enum and the
 * canonical `include/net.h`. The library has a regression test
 * that scans both headers to detect drift.
 */
typedef enum {
    NET_SUCCESS = 0,
    NET_ERR_NULL_POINTER = -1,
    NET_ERR_INVALID_UTF8 = -2,
    NET_ERR_INVALID_JSON = -3,
    NET_ERR_INIT_FAILED = -4,
    NET_ERR_INGESTION_FAILED = -5,
    NET_ERR_POLL_FAILED = -6,
    NET_ERR_BUFFER_TOO_SMALL = -7,
    NET_ERR_SHUTTING_DOWN = -8,
    /* Response byte count exceeds c_int::MAX. */
    NET_ERR_INT_OVERFLOW = -9,
    /* Stream handle does not belong to the supplied node. */
    NET_ERR_MISMATCHED_HANDLES = -10,
    /* CString::new interior NUL — see net.h for full rationale. */
    NET_ERR_INTERIOR_NUL = -11,
    NET_ERR_UNKNOWN = -99,
    /* CortEX / RedEX surface (compiled when the Rust cdylib has
     * `netdb` + `redex-disk` features on). Codes below -99 so they
     * don't collide with the event-bus surface above. */
    NET_ERR_CORTEX_CLOSED = -100,
    NET_ERR_CORTEX_FOLD = -101,
    NET_ERR_NETDB = -102,
    NET_ERR_REDEX = -103,
    /* Mesh / channel surface (compiled when the Rust cdylib has the
     * `net` feature on). */
    NET_ERR_MESH_INIT = -110,
    NET_ERR_MESH_HANDSHAKE = -111,
    NET_ERR_MESH_BACKPRESSURE = -112,
    NET_ERR_MESH_NOT_CONNECTED = -113,
    NET_ERR_MESH_TRANSPORT = -114,
    NET_ERR_CHANNEL = -115,
    NET_ERR_CHANNEL_AUTH = -116,
    /* Identity + permission-token surface (compiled when the Rust
     * cdylib has the `net` feature on). Codes below -119 — one per
     * `TokenError` kind so Go callers can `errors.Is` without
     * parsing strings. Mirrors the PyO3 / NAPI prefix conventions
     * (`"identity: ..."` / `"token: <kind>"`). */
    NET_ERR_IDENTITY = -120,
    NET_ERR_TOKEN_INVALID_FORMAT = -121,
    NET_ERR_TOKEN_INVALID_SIGNATURE = -122,
    NET_ERR_TOKEN_EXPIRED = -123,
    NET_ERR_TOKEN_NOT_YET_VALID = -124,
    NET_ERR_TOKEN_DELEGATION_EXHAUSTED = -125,
    NET_ERR_TOKEN_DELEGATION_NOT_ALLOWED = -126,
    NET_ERR_TOKEN_NOT_AUTHORIZED = -127,
    /* Capability announce / find_nodes errors. Wraps core
     * `AdapterError` when announcement dispatch fails. */
    NET_ERR_CAPABILITY = -128,
    /* NAT-traversal surface (compiled when the Rust cdylib has
     * the `nat-traversal` feature on). Codes mirror core
     * `TraversalError` variants — one integer per `kind` so Go
     * callers can `errors.Is(err, net.ErrTraversalPunchFailed)`.
     * Framing (plan §5): every traversal error is a missed
     * optimization, not a connectivity failure — the routed-
     * handshake path is always available. */
    NET_ERR_TRAVERSAL_REFLEX_TIMEOUT = -130,
    NET_ERR_TRAVERSAL_PEER_NOT_REACHABLE = -131,
    NET_ERR_TRAVERSAL_TRANSPORT = -132,
    NET_ERR_TRAVERSAL_RENDEZVOUS_NO_RELAY = -133,
    NET_ERR_TRAVERSAL_RENDEZVOUS_REJECTED = -134,
    NET_ERR_TRAVERSAL_PUNCH_FAILED = -135,
    NET_ERR_TRAVERSAL_PORT_MAP_UNAVAILABLE = -136,
    NET_ERR_TRAVERSAL_UNSUPPORTED = -137
} net_error_t;

/* Watch / tail cursor status codes. Returned from net_*_next functions
 * instead of the negative error scheme; positive to distinguish
 * "no event available" from an actual failure. */
#define NET_STREAM_TIMEOUT 1
#define NET_STREAM_ENDED   2

/*
 * Initialize a new event bus with optional JSON configuration.
 *
 * @param config_json JSON configuration string (UTF-8, null-terminated), or NULL for defaults.
 * @return Handle to the event bus, or NULL on failure.
 *
 * Example config:
 * {
 *   "num_shards": 8,
 *   "ring_buffer_capacity": 1048576,
 *   "backpressure_mode": "DropOldest"
 * }
 */
net_handle_t net_init(const char* config_json);

/*
 * Ingest a single event (parses JSON).
 *
 * @param handle Event bus handle.
 * @param event_json JSON event string.
 * @param len Length of the event string.
 * @return 0 on success, negative error code on failure.
 */
int net_ingest(net_handle_t handle, const char* event_json, size_t len);

/*
 * Ingest a raw JSON string (fastest path, no parsing).
 *
 * @param handle Event bus handle.
 * @param json JSON string.
 * @param len Length of the JSON string.
 * @return 0 on success, negative error code on failure.
 */
int net_ingest_raw(net_handle_t handle, const char* json, size_t len);

/*
 * Ingest multiple raw JSON strings in a batch.
 *
 * @param handle Event bus handle.
 * @param jsons Array of pointers to JSON strings.
 * @param lens Array of lengths for each JSON string.
 * @param count Number of events.
 * @return Number of successfully ingested events.
 */
int net_ingest_raw_batch(
    net_handle_t handle,
    const char** jsons,
    const size_t* lens,
    size_t count
);

/*
 * Ingest multiple events from a JSON array.
 *
 * @param handle Event bus handle.
 * @param events_json JSON array of events.
 * @return Number of ingested events, or negative error code.
 */
int net_ingest_batch(net_handle_t handle, const char* events_json);

/*
 * Poll events from the bus.
 *
 * @param handle Event bus handle.
 * @param request_json JSON request (e.g., {"limit": 100}).
 * @param out_buffer Output buffer for JSON response.
 * @param buffer_len Size of output buffer.
 * @return Bytes written on success, negative error code on failure.
 */
int net_poll(
    net_handle_t handle,
    const char* request_json,
    char* out_buffer,
    size_t buffer_len
);

/*
 * Get event bus statistics.
 *
 * @param handle Event bus handle.
 * @param out_buffer Output buffer for JSON statistics.
 * @param buffer_len Size of output buffer.
 * @return Bytes written on success, negative error code on failure.
 */
int net_stats(net_handle_t handle, char* out_buffer, size_t buffer_len);

/*
 * Flush pending batches to the adapter.
 *
 * @param handle Event bus handle.
 * @return 0 on success, negative error code on failure.
 */
int net_flush(net_handle_t handle);

/*
 * Get the number of shards.
 *
 * @param handle Event bus handle.
 * @return Number of shards, or 0 if handle is null.
 */
uint16_t net_num_shards(net_handle_t handle);

/*
 * Shut down the event bus and free resources.
 *
 * @param handle Event bus handle (invalid after this call).
 * @return 0 on success, negative error code on failure.
 */
int net_shutdown(net_handle_t handle);

/*
 * Get the library version.
 *
 * @return Version string (static, do not free).
 */
const char* net_version(void);

/*
 * Generate a new Net keypair (requires Net feature).
 *
 * @return JSON string with hex-encoded public_key and secret_key,
 *         or NULL if Net is not enabled. Caller must free with net_free_string.
 */
char* net_generate_keypair(void);

/*
 * Free a string returned by Net functions.
 *
 * @param s String to free (may be NULL).
 */
void net_free_string(char* s);

/* =========================================================================
 * CortEX + RedEX surface.
 *
 * Compiled when the Rust cdylib is built with `--features "netdb redex-disk"`.
 * Symbols remain unresolved when the cdylib lacks those features — Go code
 * must gate usage accordingly (the Go wrapper exposes a compile-time
 * check via a build tag).
 *
 * Watch / tail cursors:
 *   * `next(cursor, timeout_ms, &out_json, &out_len)` returns:
 *       `0`                 — event delivered; *out_json owned by caller
 *       `NET_STREAM_TIMEOUT`— no event within timeout_ms
 *       `NET_STREAM_ENDED`  — cursor reached end-of-stream
 *       negative            — net_error_t
 *     Caller frees *out_json via `net_free_string` when `0` is returned.
 * ========================================================================= */

/* Opaque handle types */
typedef struct net_redex_s           net_redex_t;
typedef struct net_redex_file_s      net_redex_file_t;
typedef struct net_redex_tail_s      net_redex_tail_t;
typedef struct net_tasks_adapter_s   net_tasks_adapter_t;
typedef struct net_tasks_watch_s     net_tasks_watch_t;
typedef struct net_memories_adapter_s net_memories_adapter_t;
typedef struct net_memories_watch_s  net_memories_watch_t;

/* ---- Redex manager ---- */
net_redex_t* net_redex_new(const char* persistent_dir);
void         net_redex_free(net_redex_t* handle);

/* ---- RedexFile ---- */
int  net_redex_open_file(net_redex_t* redex, const char* name,
                         const char* config_json,
                         net_redex_file_t** out_handle);
void net_redex_file_free(net_redex_file_t* handle);
int  net_redex_file_append(net_redex_file_t* handle, const uint8_t* payload,
                           size_t len, uint64_t* out_seq);
uint64_t net_redex_file_len(net_redex_file_t* handle);
int  net_redex_file_read_range(net_redex_file_t* handle,
                               uint64_t start, uint64_t end,
                               char** out_json, size_t* out_len);
int  net_redex_file_sync(net_redex_file_t* handle);
int  net_redex_file_close(net_redex_file_t* handle);

int  net_redex_file_tail(net_redex_file_t* handle, uint64_t from_seq,
                         net_redex_tail_t** out_cursor);
int  net_redex_tail_next(net_redex_tail_t* cursor, uint32_t timeout_ms,
                         char** out_json, size_t* out_len);
void net_redex_tail_free(net_redex_tail_t* cursor);

/* ---- Tasks adapter ---- */
int  net_tasks_adapter_open(net_redex_t* redex, uint32_t origin_hash,
                            int persistent, net_tasks_adapter_t** out_handle);
int  net_tasks_adapter_close(net_tasks_adapter_t* handle);
void net_tasks_adapter_free(net_tasks_adapter_t* handle);

int  net_tasks_create(net_tasks_adapter_t* handle, uint64_t id,
                      const char* title, uint64_t now_ns, uint64_t* out_seq);
int  net_tasks_rename(net_tasks_adapter_t* handle, uint64_t id,
                      const char* new_title, uint64_t now_ns, uint64_t* out_seq);
int  net_tasks_complete(net_tasks_adapter_t* handle, uint64_t id,
                        uint64_t now_ns, uint64_t* out_seq);
int  net_tasks_delete(net_tasks_adapter_t* handle, uint64_t id,
                      uint64_t* out_seq);
int  net_tasks_wait_for_seq(net_tasks_adapter_t* handle, uint64_t seq,
                            uint32_t timeout_ms);
int  net_tasks_list(net_tasks_adapter_t* handle, const char* filter_json,
                    char** out_json, size_t* out_len);
int  net_tasks_snapshot_and_watch(net_tasks_adapter_t* handle,
                                  const char* filter_json,
                                  char** out_snapshot, size_t* out_snapshot_len,
                                  net_tasks_watch_t** out_cursor);
int  net_tasks_watch_next(net_tasks_watch_t* cursor, uint32_t timeout_ms,
                          char** out_json, size_t* out_len);
void net_tasks_watch_free(net_tasks_watch_t* cursor);

/* ---- Memories adapter ---- */
int  net_memories_adapter_open(net_redex_t* redex, uint32_t origin_hash,
                               int persistent, net_memories_adapter_t** out_handle);
int  net_memories_adapter_close(net_memories_adapter_t* handle);
void net_memories_adapter_free(net_memories_adapter_t* handle);

/* `input_json` carries all store/retag parameters because Go strings
 * and tag lists are awkward to marshal one-field-at-a-time across cgo.
 * Shape: {"id": <u64>, "content": <str>, "tags": [<str>...],
 *         "source": <str>, "now_ns": <u64>}.
 * Retag shape drops `content` / `source`. */
int  net_memories_store(net_memories_adapter_t* handle,
                        const char* input_json, uint64_t* out_seq);
int  net_memories_retag(net_memories_adapter_t* handle,
                        const char* input_json, uint64_t* out_seq);
int  net_memories_pin(net_memories_adapter_t* handle, uint64_t id,
                      uint64_t now_ns, uint64_t* out_seq);
int  net_memories_unpin(net_memories_adapter_t* handle, uint64_t id,
                        uint64_t now_ns, uint64_t* out_seq);
int  net_memories_delete(net_memories_adapter_t* handle, uint64_t id,
                         uint64_t* out_seq);
int  net_memories_wait_for_seq(net_memories_adapter_t* handle, uint64_t seq,
                               uint32_t timeout_ms);
int  net_memories_list(net_memories_adapter_t* handle, const char* filter_json,
                       char** out_json, size_t* out_len);
int  net_memories_snapshot_and_watch(net_memories_adapter_t* handle,
                                     const char* filter_json,
                                     char** out_snapshot, size_t* out_snapshot_len,
                                     net_memories_watch_t** out_cursor);
int  net_memories_watch_next(net_memories_watch_t* cursor, uint32_t timeout_ms,
                             char** out_json, size_t* out_len);
void net_memories_watch_free(net_memories_watch_t* cursor);

/* =========================================================================
 * Redis Streams consumer-side dedup helper (`redis` feature).
 *
 * Mirrors `net::adapter::RedisStreamDedup` and the cross-language
 * wrappers in the Node / Python SDKs. The Redis adapter writes a
 * stable `dedup_id` field on every XADD entry
 * (`"{producer_nonce:hex}:{shard_id}:{sequence_start}:{i}"`); this
 * helper filters duplicates whose `dedup_id`s repeat — handling the
 * producer-side `MULTI/EXEC`-timeout race that lands two stream
 * entries for one logical event.
 *
 * Each handle wraps an LRU-bounded set of seen ids; the LRU is
 * mutex-protected so concurrent calls from multiple goroutines /
 * threads are safe but serialize. Production callers typically
 * instantiate one helper per consumer goroutine and key on the
 * `dedup_id` field they extract from each `XRANGE` / `XREAD`
 * entry. See `bindings/go/README.md` and the Python / Node SDK
 * READMEs for runnable examples.
 * ========================================================================= */

typedef struct net_redis_dedup_s net_redis_dedup_t;

/* Create a new dedup helper. `capacity == 0` selects the default
 * (4096); production callers should size to their dedup window
 * (consumer at ~10k events/sec with a 1 min window: ~600,000).
 * Never returns NULL. Free with `net_redis_dedup_free`.
 */
net_redis_dedup_t* net_redis_dedup_new(size_t capacity);

/* Free a helper handle. NULL is a no-op. */
void net_redis_dedup_free(net_redis_dedup_t* handle);

/* Test-and-insert. Returns:
 *    1 — duplicate (caller should skip the entry)
 *    0 — new       (caller should process AND we've now marked it seen)
 *   -1 — NULL handle or NULL dedup_id
 *   -2 — invalid UTF-8 in dedup_id
 */
int net_redis_dedup_is_duplicate(net_redis_dedup_t* handle, const char* dedup_id);

/* Number of distinct ids currently tracked. Returns 0 on NULL
 * handle (mirrors the "no ids" semantic).
 */
size_t net_redis_dedup_len(net_redis_dedup_t* handle);

/* Configured maximum capacity. Returns 0 on NULL handle. */
size_t net_redis_dedup_capacity(net_redis_dedup_t* handle);

/* Returns 1 if no ids are tracked, 0 if the helper has at least
 * one id, -1 on NULL handle.
 */
int net_redis_dedup_is_empty(net_redis_dedup_t* handle);

/* Clear all tracked ids. Use after a consumer-group rebalance.
 * NULL is a no-op.
 */
void net_redis_dedup_clear(net_redis_dedup_t* handle);

/* =========================================================================
 * Mesh transport (`net` feature).
 *
 * Encrypted UDP mesh: handshake, per-peer streams, channels (named
 * pub/sub), shard receive. Mirrors the Rust SDK's `Mesh` type; not
 * full parity with the core `MeshNode`.
 *
 * Strings returned via `char**` are heap-allocated and must be freed
 * with `net_free_string`.
 * ========================================================================= */

typedef struct net_meshnode_s    net_meshnode_t;
typedef struct net_mesh_stream_s net_mesh_stream_t;

/* ---- Lifecycle ---- */

/* Open a mesh node. `config_json`:
 *   { "bind_addr": "127.0.0.1:9000",
 *     "psk_hex":   "<64 hex chars>",
 *     "heartbeat_ms":        5000,      // optional
 *     "session_timeout_ms":  30000,     // optional
 *     "num_shards":          4 }        // optional
 */
int      net_mesh_new(const char* config_json, net_meshnode_t** out);
void     net_mesh_free(net_meshnode_t* handle);
int      net_mesh_shutdown(net_meshnode_t* handle);

/* ---- Identity + handshake ---- */

int      net_mesh_public_key_hex(net_meshnode_t* handle,
                                 char** out_hex, size_t* out_len);
uint64_t net_mesh_node_id(net_meshnode_t* handle);

int      net_mesh_connect(net_meshnode_t* handle,
                          const char* peer_addr,
                          const char* peer_pubkey_hex,
                          uint64_t peer_node_id);
int      net_mesh_accept(net_meshnode_t* handle,
                         uint64_t peer_node_id,
                         char** out_addr, size_t* out_len);
int      net_mesh_start(net_meshnode_t* handle);

/* ---- Per-peer streams ---- */

/* `config_json`:
 *   { "reliability": "reliable" | "fire_and_forget",
 *     "window_bytes":    65536,
 *     "fairness_weight": 1 }
 * May be NULL for defaults.
 */
int      net_mesh_open_stream(net_meshnode_t* handle,
                              uint64_t peer_node_id,
                              uint64_t stream_id,
                              const char* config_json,
                              net_mesh_stream_t** out_stream);
void     net_mesh_stream_free(net_mesh_stream_t* handle);

/* Send a batch of payloads on an open stream.
 *
 * `payloads` is a pointer to an array of `count` byte-pointers;
 * `lens` is the parallel array of lengths. Borrowed for the call
 * duration only — caller owns the memory. Pass `node_handle` so the
 * FFI can reach the owning runtime without creating a global index.
 *
 * Returns `NET_ERR_MESH_BACKPRESSURE` when the window is full,
 * `NET_ERR_MESH_NOT_CONNECTED` when the peer is gone,
 * `NET_ERR_MESH_TRANSPORT` for other I/O errors.
 */
int      net_mesh_send(net_mesh_stream_t* stream,
                       const uint8_t* const* payloads,
                       const size_t* lens,
                       size_t count,
                       net_meshnode_t* node_handle);
int      net_mesh_send_with_retry(net_mesh_stream_t* stream,
                                  const uint8_t* const* payloads,
                                  const size_t* lens,
                                  size_t count,
                                  uint32_t max_retries,
                                  net_meshnode_t* node_handle);
int      net_mesh_send_blocking(net_mesh_stream_t* stream,
                                const uint8_t* const* payloads,
                                const size_t* lens,
                                size_t count,
                                net_meshnode_t* node_handle);

/* Stream stats — JSON shape mirrors `StreamStats`. Writes `null` to
 * *out_json when the stream isn't open. */
int      net_mesh_stream_stats(net_meshnode_t* handle,
                               uint64_t peer_node_id,
                               uint64_t stream_id,
                               char** out_json, size_t* out_len);

/* ---- Shard receive ----
 *
 * Drain up to `limit` events from shard `shard_id`. Output is a JSON
 * array of {id, payload_b64, insertion_ts, shard_id}.
 */
int      net_mesh_recv_shard(net_meshnode_t* handle,
                             uint16_t shard_id, uint32_t limit,
                             char** out_json, size_t* out_len);

/* ---- Channels ----
 *
 * `config_json`:
 *   { "name": "sensors/temp",
 *     "visibility": "global" | "subnet-local" | "parent-visible" | "exported",
 *     "reliable":      false,
 *     "require_token": false,
 *     "priority":      0,
 *     "max_rate_pps":  1000,
 *     "publish_caps":   { ... CapabilityFilter ... },   // Stage G-4
 *     "subscribe_caps": { ... CapabilityFilter ... } }  // Stage G-4
 */
int      net_mesh_register_channel(net_meshnode_t* handle, const char* config_json);
int      net_mesh_subscribe_channel(net_meshnode_t* handle,
                                    uint64_t publisher_node_id,
                                    const char* channel);

/* Subscribe with a serialized `PermissionToken` (159 bytes) attached.
 * Required when the publisher set `require_token=true`, or when the
 * subscriber's announced caps don't satisfy `subscribe_caps`. Parses
 * the token client-side — malformed bytes return
 * `NET_ERR_TOKEN_INVALID_FORMAT` with no network I/O. Signature
 * tampering is caught server-side and surfaces as
 * `NET_ERR_CHANNEL_AUTH`. */
int      net_mesh_subscribe_channel_with_token(net_meshnode_t* handle,
                                               uint64_t publisher_node_id,
                                               const char* channel,
                                               const uint8_t* token,
                                               size_t token_len);

int      net_mesh_unsubscribe_channel(net_meshnode_t* handle,
                                      uint64_t publisher_node_id,
                                      const char* channel);
/* Publish one payload to every subscriber. `config_json`:
 *   { "reliability": "reliable" | "fire_and_forget",
 *     "on_failure":  "best_effort" | "fail_fast" | "collect",
 *     "max_inflight": 32 }
 * May be NULL. Writes a JSON `PublishReport` to `*out_json`. */
int      net_mesh_publish(net_meshnode_t* handle,
                          const char* channel,
                          const uint8_t* payload, size_t len,
                          const char* config_json,
                          char** out_json, size_t* out_len);

/* =========================================================================
 * Identity + permission tokens (Stage G — security surface).
 *
 * `net_identity_t` is an opaque handle bundling an ed25519 keypair
 * with a local token cache. Free with `net_identity_free`. Binary
 * buffers returned as `uint8_t**` + `size_t*` are heap-allocated by
 * Rust and must be released with `net_free_bytes(ptr, len)` — do NOT
 * use `net_free_string` on them (CString-only).
 * ========================================================================= */

typedef struct net_identity_s net_identity_t;

/* Free a byte buffer returned by any `net_*` function that returns
 * raw token / entity-id bytes through `uint8_t**`. Uses `len` to
 * reconstruct the Vec — pass the exact length returned via the
 * matching `size_t*` out-param. No-op on NULL / len=0. */
void     net_free_bytes(uint8_t* ptr, size_t len);

/* Lifecycle + seed round-trip */
int      net_identity_generate(net_identity_t** out_handle);
int      net_identity_from_seed(const uint8_t* seed, size_t seed_len,
                                net_identity_t** out_handle);
void     net_identity_free(net_identity_t* handle);

/* Writes the 32-byte ed25519 seed into `out[32]`. Caller owns the
 * buffer — treat the bytes as secret material. */
int      net_identity_to_seed(net_identity_t* handle, uint8_t* out);

/* Writes the 32-byte entity id into `out[32]`. */
int      net_identity_entity_id(net_identity_t* handle, uint8_t* out);

uint64_t net_identity_node_id(net_identity_t* handle);
uint32_t net_identity_origin_hash(net_identity_t* handle);

/* Signs `msg[len]`; writes a 64-byte ed25519 signature into `out_sig[64]`. */
int      net_identity_sign(net_identity_t* handle,
                           const uint8_t* msg, size_t len,
                           uint8_t* out_sig);

/* Issue a token to `subject` (32 bytes). `scope_json` is a JSON
 * array of strings: `["publish"]`, `["subscribe","delegate"]`, etc.
 * `channel` is the canonical name (not the u16 hash). Writes a
 * newly-allocated blob — free via `net_free_bytes`. */
int      net_identity_issue_token(net_identity_t* signer,
                                  const uint8_t* subject, size_t subject_len,
                                  const char* scope_json,
                                  const char* channel,
                                  uint32_t ttl_seconds,
                                  uint8_t delegation_depth,
                                  uint8_t** out_token,
                                  size_t* out_token_len);

/* Install a token received from another issuer. Signature check
 * runs on insert — malformed / tampered tokens return the relevant
 * `NET_ERR_TOKEN_*` code. */
int      net_identity_install_token(net_identity_t* handle,
                                    const uint8_t* token, size_t len);

/* Look up a cached token by `(subject, channel)`. On hit, writes a
 * newly-allocated blob (free via `net_free_bytes`). On miss, writes
 * NULL/0 and returns `NET_SUCCESS`. */
int      net_identity_lookup_token(net_identity_t* handle,
                                   const uint8_t* subject, size_t subject_len,
                                   const char* channel,
                                   uint8_t** out_token,
                                   size_t* out_token_len);

uint32_t net_identity_token_cache_len(net_identity_t* handle);

/* Parse a serialized `PermissionToken` into a JSON dict with keys
 * `issuer_hex`, `subject_hex`, `scope`, `channel_hash`, `not_before`,
 * `not_after`, `delegation_depth`, `nonce`, `signature_hex`. Returns
 * `NET_ERR_TOKEN_INVALID_FORMAT` on bad length / layout. */
int      net_parse_token(const uint8_t* token, size_t len,
                         char** out_json, size_t* out_len);

/* Verify the token's ed25519 signature. Writes 1 (valid) or 0
 * (tampered/wrong-subject) to `*out_ok`. Does NOT check time-bound
 * validity — see `net_token_is_expired`. */
int      net_verify_token(const uint8_t* token, size_t len, int* out_ok);

/* Writes 1 to `*out_expired` if `not_after` has passed. */
int      net_token_is_expired(const uint8_t* token, size_t len, int* out_expired);

/* Delegate a token to a new subject. Parent must have the
 * `delegate` scope and `delegation_depth > 0`; `signer` must be the
 * subject of the parent. */
int      net_delegate_token(net_identity_t* signer,
                            const uint8_t* parent, size_t parent_len,
                            const uint8_t* new_subject, size_t new_subject_len,
                            const char* restricted_scope_json,
                            uint8_t** out_token, size_t* out_token_len);

/* Writes the mesh's 32-byte ed25519 entity id into `out[32]`. */
int      net_mesh_entity_id(net_meshnode_t* handle, uint8_t* out);

/* =========================================================================
 * NAT traversal surface (compiled when the Rust cdylib has the
 * `nat-traversal` feature on). Framing (plan §5): these APIs let
 * the mesh upgrade to a *direct* path when the underlying NATs
 * allow it — they are NOT required for NATed peers to
 * communicate. The routed-handshake path reaches every peer
 * regardless. Strings use the stable vocabulary
 * `"open" | "cone" | "symmetric" | "unknown"`.
 * ========================================================================= */

/* Write this mesh's NAT classification into `out_str`. Caller
 * frees via `net_free_string`. */
int      net_mesh_nat_type(net_meshnode_t* handle,
                           char** out_str, size_t* out_len);

/* Write this mesh's last-observed reflex `ip:port`. Empty string
 * when classification has not yet produced an observation. */
int      net_mesh_reflex_addr(net_meshnode_t* handle,
                              char** out_str, size_t* out_len);

/* Write `peer_node_id`'s advertised NAT classification (read
 * from its `nat:*` capability tag). Returns `"unknown"` when we
 * have no announcement from that peer. */
int      net_mesh_peer_nat_type(net_meshnode_t* handle,
                                uint64_t peer_node_id,
                                char** out_str, size_t* out_len);

/* Send one reflex probe to `peer_node_id` and write the public
 * `ip:port` the peer observed. Blocks on the shared runtime. */
int      net_mesh_probe_reflex(net_meshnode_t* handle,
                               uint64_t peer_node_id,
                               char** out_str, size_t* out_len);

/* Explicitly re-run the classification sweep. No-op when fewer
 * than 2 peers are connected; never returns an error. */
int      net_mesh_reclassify_nat(net_meshnode_t* handle);

/* Fill cumulative traversal counters. Any of the out-pointers may
 * be NULL to skip that field. Monotonic — counters never reset. */
int      net_mesh_traversal_stats(net_meshnode_t* handle,
                                  uint64_t* out_punches_attempted,
                                  uint64_t* out_punches_succeeded,
                                  uint64_t* out_relay_fallbacks);

/* Establish a session via rendezvous through `coordinator`. The
 * pair-type matrix picks between a direct handshake and a
 * coordinated punch. Always resolves (on punch-failed, falls
 * back to routed). Inspect `net_mesh_traversal_stats` afterward
 * to distinguish outcomes. */
int      net_mesh_connect_direct(net_meshnode_t* handle,
                                 uint64_t peer_node_id,
                                 const char* peer_pubkey_hex,
                                 uint64_t coordinator);

/* Install a runtime reflex override. `external` is a
 * null-terminated "ip:port" string. Forces `nat_type` to "open"
 * and `reflex_addr` to `external`; short-circuits any further
 * classifier sweeps. */
int      net_mesh_set_reflex_override(net_meshnode_t* handle,
                                      const char* external);

/* Drop a previously-installed reflex override. The classifier
 * resumes on its normal cadence. No-op when no override is
 * active. */
int      net_mesh_clear_reflex_override(net_meshnode_t* handle);

/* Hash a channel name to its 16-bit wire representation. */
int      net_channel_hash(const char* channel, uint16_t* out_hash);

/* =========================================================================
 * Capabilities (announce / find_nodes).
 *
 * `caps_json` is the same POJO shape as PyO3 / NAPI:
 *   { "hardware": {...}, "software": {...},
 *     "models": [{...}], "tools": [{...}],
 *     "tags": ["gpu", "prod"], "limits": {...} }
 *
 * `filter_json`:
 *   { "require_tags": [...], "require_models": [...],
 *     "require_tools": [...], "min_memory_mb": N,
 *     "require_gpu": bool, "gpu_vendor": "nvidia",
 *     "min_vram_mb": N, "min_context_length": N,
 *     "require_modalities": [...] }
 * ========================================================================= */
int      net_mesh_announce_capabilities(net_meshnode_t* handle,
                                        const char* caps_json);

/* Writes a JSON array `[node_id, ...]` of matching nodes (including
 * own node id when self-match). Free `*out_json` via `net_free_string`. */
int      net_mesh_find_nodes(net_meshnode_t* handle,
                             const char* filter_json,
                             char** out_json, size_t* out_len);

/* Scoped variant of `net_mesh_find_nodes`. `scope_json` is a tagged
 * union by `kind`:
 *   {"kind": "any"}                              — all non-SubnetLocal nodes
 *   {"kind": "global_only"}                      — only untagged nodes
 *   {"kind": "same_subnet"}                      — caller's subnet only
 *   {"kind": "tenant", "tenant": "<id>"}         — that tenant + Global
 *   {"kind": "tenants", "tenants": ["<id>", …]}  — any of those + Global
 *   {"kind": "region", "region": "<name>"}       — that region + Global
 *   {"kind": "regions", "regions": ["<name>", …]}— any of those + Global
 *
 * Untagged nodes resolve to Global and stay visible under most
 * filters; nodes tagged `scope:subnet-local` only show up under
 * `same_subnet`. */
int      net_mesh_find_nodes_scoped(net_meshnode_t* handle,
                                    const char* filter_json,
                                    const char* scope_json,
                                    char** out_json, size_t* out_len);

/* Pick the best-scoring node for a placement requirement. Writes
 * the winning node id to `*out_node_id` and `1` to `*out_has_match`
 * on hit; writes `0` to `*out_has_match` on no match. Returns 0 in
 * either case; non-zero only on input / parse error.
 *
 * `requirement_json` is `{"filter": <CapabilityFilter>,
 * "prefer_more_memory": f, "prefer_more_vram": f,
 * "prefer_faster_inference": f, "prefer_loaded_models": f}` —
 * weights are optional and clamped to `[0.0, 1.0]`. */
int      net_mesh_find_best_node(net_meshnode_t* handle,
                                 const char* requirement_json,
                                 uint64_t* out_node_id,
                                 int* out_has_match);

/* Scoped variant of `net_mesh_find_best_node`. `scope_json` accepts
 * the same shapes as `net_mesh_find_nodes_scoped`. Picks the highest-
 * scoring node within the scope-filtered set. Same out-param
 * contract as `net_mesh_find_best_node`. */
int      net_mesh_find_best_node_scoped(net_meshnode_t* handle,
                                        const char* requirement_json,
                                        const char* scope_json,
                                        uint64_t* out_node_id,
                                        int* out_has_match);

/* Normalize a GPU vendor string to canonical lowercase
 * (`"nvidia"` | `"amd"` | `"intel"` | `"apple"` | `"qualcomm"` |
 * `"unknown"`). Writes the result via `*out` / `*out_len`; free via
 * `net_free_string`. Matches the NAPI / PyO3 helper so every SDK
 * produces the same wire-normalized value. */
int      net_normalize_gpu_vendor(const char* raw,
                                  char** out, size_t* out_len);

/* =========================================================================
 * Compute — MeshDaemon + migration. Stage 6 of
 * SDK_COMPUTE_SURFACE_PLAN.md. Symbols live in `libnet_compute`
 * (sibling shared library built from `bindings/go/compute-ffi`).
 * Link with `-lnet -lnet_compute`.
 * ========================================================================= */

/* Opaque handles. Go wrappers own the pointer lifetime via
 * runtime.SetFinalizer — consistent with `net_meshnode_t`. */
typedef struct net_compute_runtime_s      net_compute_runtime_t;
typedef struct net_compute_mesh_arc_s     net_compute_mesh_arc_t;
typedef struct net_compute_cc_arc_s       net_compute_cc_arc_t;

/* Compute error codes (negative). */
#define NET_COMPUTE_OK                   0
#define NET_COMPUTE_ERR_NULL            -1
#define NET_COMPUTE_ERR_CALL_FAILED     -2
#define NET_COMPUTE_ERR_DUPLICATE_KIND  -3

/* --- Arc<MeshNode> / Arc<ChannelConfigRegistry> accessors ---
 *
 * Produced by `net_mesh_*_arc_clone` (in libnet); compute-ffi's
 * `net_compute_runtime_new` CONSUMES them. Pair each clone with
 * either a `net_compute_runtime_new` consume-call or a matching
 * `_free`.
 */
net_compute_mesh_arc_t* net_mesh_arc_clone(net_meshnode_t* handle);
void                    net_mesh_arc_free(net_compute_mesh_arc_t* p);

net_compute_cc_arc_t*   net_mesh_channel_configs_arc_clone(net_meshnode_t* handle);
void                    net_mesh_channel_configs_arc_free(net_compute_cc_arc_t* p);

/* --- Runtime lifecycle --- */

/* Build a DaemonRuntime sharing the given mesh's node + channel
 * configs. Both Arc pointers are consumed on success (do not free
 * afterwards). Returns NULL if any input is NULL. */
net_compute_runtime_t*  net_compute_runtime_new(
    net_compute_mesh_arc_t* node_arc,
    net_compute_cc_arc_t* channel_configs_arc);

/* Free a runtime handle. The underlying MeshNode is untouched. */
void                    net_compute_runtime_free(net_compute_runtime_t* handle);

/* Return the monotonic, process-unique identifier assigned to this
 * runtime by `net_compute_runtime_new`. Go uses this id to scope
 * its factory map so two runtimes in the same process can register
 * the same `kind` without colliding. Returns 0 on NULL input. */
uint64_t                net_compute_runtime_id(const net_compute_runtime_t* handle);

/* Transition to Ready. On failure, writes a heap-allocated char*
 * detail to `*err_out` (free via `net_compute_free_cstring`). */
int                     net_compute_runtime_start(
    net_compute_runtime_t* handle,
    char** err_out);

/* Tear down the runtime. `*err_out` carries detail on failure. */
int                     net_compute_runtime_shutdown(
    net_compute_runtime_t* handle,
    char** err_out);

/* 1 = ready, 0 = not-ready, NET_COMPUTE_ERR_NULL on NULL handle. */
int                     net_compute_runtime_is_ready(net_compute_runtime_t* handle);

/* Number of daemons registered. Returns -1 on NULL handle. */
int64_t                 net_compute_runtime_daemon_count(net_compute_runtime_t* handle);

/* Register a placeholder kind. Enables `spawn`; migration-target
 * reconstruction falls back to NoopBridge (migrated-in daemons on
 * this node run as no-op). Use `net_compute_register_factory_with_func`
 * when you need migrated-in daemons to run user code. */
int                     net_compute_register_factory(
    net_compute_runtime_t* handle,
    const char* kind_ptr,
    size_t kind_len);

/* Register a kind with a Go-side factory func (the caller already
 * stored the func in the Go-side factoryFuncs map; we install an
 * SDK factory closure that reaches back via the dispatcher's
 * factory trampoline on every migration-target reconstruction). */
int                     net_compute_register_factory_with_func(
    net_compute_runtime_t* handle,
    const char* kind_ptr,
    size_t kind_len);

/* Free a CString returned by compute-ffi (e.g., an err_out detail). */
void                    net_compute_free_cstring(char* s);

/* --- Callback dispatcher (sub-step 2) ---
 *
 * Go registers four trampolines with C linkage via
 * `net_compute_set_dispatcher` in its `init()`. Rust invokes them
 * whenever a bridged daemon's `process` / `snapshot` / `restore`
 * method needs to run, plus a `free` callback on daemon drop so
 * Go can release its registry entry.
 */

typedef struct net_compute_outputs_s      net_compute_outputs_t;
typedef struct net_compute_daemon_handle_s net_compute_daemon_handle_t;

typedef int (*net_compute_process_fn)(
    uint64_t daemon_id,
    uint32_t origin_hash,
    uint64_t sequence,
    const uint8_t* payload_ptr,
    size_t payload_len,
    net_compute_outputs_t* outputs);

typedef int (*net_compute_snapshot_fn)(
    uint64_t daemon_id,
    uint8_t** out_ptr,
    size_t* out_len);

typedef int (*net_compute_restore_fn)(
    uint64_t daemon_id,
    const uint8_t* state_ptr,
    size_t state_len);

typedef void (*net_compute_free_fn)(uint64_t daemon_id);

typedef int (*net_compute_factory_fn)(
    uint64_t runtime_id,
    const char* kind_ptr,
    size_t kind_len,
    uint64_t* out_daemon_id);

/* Install the Go dispatcher. Call once from Go's init; second call
 * is ignored (first registration wins). All five pointers must be
 * non-NULL. */
int                     net_compute_set_dispatcher(
    net_compute_process_fn process,
    net_compute_snapshot_fn snapshot,
    net_compute_restore_fn restore,
    net_compute_free_fn free,
    net_compute_factory_fn factory);

/* Push one output payload into the outputs vec. Called by Go's
 * process trampoline. Copies `len` bytes. */
int                     net_compute_outputs_push(
    net_compute_outputs_t* vec,
    const uint8_t* data,
    size_t len);

/* Free a snapshot buffer the Go side malloc'd and handed to Rust
 * via `net_compute_snapshot_fn`. (The Rust bridge calls this after
 * copying the bytes into its own Bytes.) Normal callers should not
 * invoke this directly. */
void                    net_compute_snapshot_bytes_free(uint8_t* ptr, size_t len);

/* --- Spawn / stop / deliver --- */

/* Spawn a daemon. `daemon_id` is the Go-side registry key
 * trampolines will receive on every callback. `identity_seed`
 * points at 32 bytes of ed25519 seed. Writes the opaque handle to
 * `*out_handle` on success. `auto_snapshot_interval` and
 * `max_log_entries` take 0 for defaults. */
int                     net_compute_spawn(
    net_compute_runtime_t* runtime,
    const char* kind_ptr,
    size_t kind_len,
    const uint8_t* identity_seed,
    uint64_t daemon_id,
    uint64_t auto_snapshot_interval,
    uint32_t max_log_entries,
    net_compute_daemon_handle_t** out_handle,
    char** err_out);

/* Read the origin_hash from a daemon handle. Returns 0 on NULL. */
uint32_t                net_compute_daemon_handle_origin_hash(
    const net_compute_daemon_handle_t* handle);

/* Copy the 32-byte entity_id from a daemon handle into `out`. */
int                     net_compute_daemon_handle_entity_id(
    const net_compute_daemon_handle_t* handle,
    uint8_t* out);

/* Free a daemon handle (does NOT stop the daemon — call
 * `net_compute_runtime_stop(origin_hash)` separately). */
void                    net_compute_daemon_handle_free(
    net_compute_daemon_handle_t* handle);

/* Stop a daemon by origin_hash. */
int                     net_compute_runtime_stop(
    net_compute_runtime_t* runtime,
    uint32_t origin_hash,
    char** err_out);

/* Deliver one event. Writes a heap-allocated outputs vec to
 * `*out_outputs`; caller reads via `net_compute_outputs_len`/`_at`
 * and frees via `net_compute_outputs_free`. */
int                     net_compute_runtime_deliver(
    net_compute_runtime_t* runtime,
    uint32_t origin_hash,
    uint32_t event_origin_hash,
    uint64_t event_sequence,
    const uint8_t* event_payload,
    size_t event_payload_len,
    net_compute_outputs_t** out_outputs,
    char** err_out);

size_t                  net_compute_outputs_len(const net_compute_outputs_t* vec);
int                     net_compute_outputs_at(
    const net_compute_outputs_t* vec,
    size_t idx,
    const uint8_t** out_ptr,
    size_t* out_len);
void                    net_compute_outputs_free(net_compute_outputs_t* vec);

/* --- Snapshot + restore (sub-step 3) --- */

/* Take a snapshot of a running daemon. On success, `*out_outputs`
 * carries the serialized StateSnapshot bytes as a single-entry
 * outputs vec, or an empty vec for stateless daemons. */
int                     net_compute_runtime_snapshot(
    net_compute_runtime_t* runtime,
    uint32_t origin_hash,
    net_compute_outputs_t** out_outputs,
    char** err_out);

/* Spawn from a previously-taken snapshot. `snapshot_ptr` /
 * `snapshot_len` must be the exact bytes returned by a prior
 * `net_compute_runtime_snapshot`. Corrupted bytes fail fast with
 * `daemon: snapshot decode failed`; identity mismatch surfaces via
 * the SDK's existing `snapshot identity mismatch` error. */
int                     net_compute_spawn_from_snapshot(
    net_compute_runtime_t* runtime,
    const char* kind_ptr,
    size_t kind_len,
    const uint8_t* identity_seed,
    const uint8_t* snapshot_ptr,
    size_t snapshot_len,
    uint64_t daemon_id,
    uint64_t auto_snapshot_interval,
    uint32_t max_log_entries,
    net_compute_daemon_handle_t** out_handle,
    char** err_out);

/* --- Migration (sub-step 4) ---
 *
 * Error messages from the migration surface use the prefix
 * `migration: <kind>[: <detail>]` (written into the `err_out`
 * CString) so the Go side can dispatch on the stable kind:
 * `not-ready` | `factory-not-found` | `compute-not-supported` |
 * `state-failed` | `already-migrating` | `identity-transport-failed` |
 * `not-ready-timeout` | `daemon-not-found` | `target-unavailable` |
 * `wrong-phase` | `snapshot-too-large`.
 */

typedef struct net_compute_migration_handle_s net_compute_migration_handle_t;

/* Start a migration. `transport_identity`: 0 = skip envelope,
 * non-zero = seal the daemon's keypair into the snapshot.
 * `retry_not_ready_ms`: 0 disables retry; otherwise the source
 * backs off + re-initiates on `NotReady` up to this budget. */
int                     net_compute_start_migration(
    net_compute_runtime_t* runtime,
    uint32_t origin_hash,
    uint64_t source_node,
    uint64_t target_node,
    uint8_t transport_identity,
    uint64_t retry_not_ready_ms,
    net_compute_migration_handle_t** out_handle,
    char** err_out);

/* Declare that a migration will land on this node for
 * `origin_hash`. Uses the snapshot's envelope to supply the
 * keypair. */
int                     net_compute_expect_migration(
    net_compute_runtime_t* runtime,
    const char* kind_ptr,
    size_t kind_len,
    uint32_t origin_hash,
    uint64_t auto_snapshot_interval,
    uint32_t max_log_entries,
    char** err_out);

/* Register an identity for target-side restore when the source
 * migrates without an envelope (transport_identity=0). */
int                     net_compute_register_migration_target_identity(
    net_compute_runtime_t* runtime,
    const char* kind_ptr,
    size_t kind_len,
    const uint8_t* identity_seed,
    uint64_t auto_snapshot_interval,
    uint32_t max_log_entries,
    char** err_out);

/* Query the orchestrator phase for `origin_hash`. Returns NULL
 * if no migration is in flight, else a heap-allocated CString
 * the caller frees with `net_compute_free_cstring`. */
char*                   net_compute_migration_phase(
    net_compute_runtime_t* runtime,
    uint32_t origin_hash);

/* Free a migration handle. Does NOT cancel the migration. */
void                    net_compute_migration_handle_free(
    net_compute_migration_handle_t* handle);

uint32_t                net_compute_migration_handle_origin_hash(
    const net_compute_migration_handle_t* handle);
uint64_t                net_compute_migration_handle_source_node(
    const net_compute_migration_handle_t* handle);
uint64_t                net_compute_migration_handle_target_node(
    const net_compute_migration_handle_t* handle);

/* Current phase or NULL (same semantics as
 * `net_compute_migration_phase`). Caller frees non-NULL result. */
char*                   net_compute_migration_handle_phase(
    const net_compute_migration_handle_t* handle);

/* Block until terminal state. 0 on `complete`; err_out carries
 * `migration: <kind>` body on abort/failure. */
int                     net_compute_migration_handle_wait(
    net_compute_migration_handle_t* handle,
    char** err_out);

int                     net_compute_migration_handle_wait_with_timeout(
    net_compute_migration_handle_t* handle,
    uint64_t timeout_ms,
    char** err_out);

int                     net_compute_migration_handle_cancel(
    net_compute_migration_handle_t* handle,
    char** err_out);

/* Test-only helper — `net_compute_test_inject_synthetic_peer` —
 * lives in the test-only Go file `groups_testhelpers_test.go`,
 * gated at the Rust layer behind the `test-helpers` cargo
 * feature on compute-ffi. Intentionally NOT declared here
 * because this header ships with production consumers; the test
 * binary supplies its own extern declaration. */

/* =========================================================================
 * Groups — Stage 4 of SDK_GROUPS_SURFACE_PLAN.md.
 *
 * `ReplicaGroup` / `ForkGroup` / `StandbyGroup` overlays on top of
 * `DaemonRuntime`. Errors use the stable prefix
 * `group: <kind>[: <detail>]`, where `<kind>` is one of:
 *   `not-ready` | `factory-not-found` | `no-healthy-member` |
 *   `placement-failed` | `registry-failed` | `invalid-config` |
 *   `daemon`
 * ========================================================================= */

typedef struct net_compute_replica_group_s  net_compute_replica_group_t;
typedef struct net_compute_fork_group_s     net_compute_fork_group_t;
typedef struct net_compute_standby_group_s  net_compute_standby_group_t;

/* --- ReplicaGroup --- */

int  net_compute_replica_group_spawn(
    net_compute_runtime_t* runtime,
    const char* kind_ptr, size_t kind_len,
    uint32_t replica_count,
    const uint8_t* group_seed,
    const char* lb_strategy_ptr, size_t lb_strategy_len,
    uint64_t auto_snapshot_interval,
    uint32_t max_log_entries,
    net_compute_replica_group_t** out_handle,
    char** err_out);

void net_compute_replica_group_free(net_compute_replica_group_t* h);
int  net_compute_replica_group_replica_count(const net_compute_replica_group_t* h);
int  net_compute_replica_group_healthy_count(const net_compute_replica_group_t* h);
uint32_t net_compute_replica_group_group_id(const net_compute_replica_group_t* h);

/* status: 0=healthy 1=degraded 2=dead */
int  net_compute_replica_group_health(
    const net_compute_replica_group_t* h,
    int* out_status, uint32_t* out_healthy, uint32_t* out_total);

int  net_compute_replica_group_route_event(
    const net_compute_replica_group_t* h,
    const char* routing_key_ptr, size_t routing_key_len,
    uint32_t* out_origin, char** err_out);

int  net_compute_replica_group_scale_to(
    const net_compute_replica_group_t* h,
    uint32_t n, char** err_out);

void net_compute_replica_group_on_node_recovery(
    const net_compute_replica_group_t* h, uint64_t node_id);

/* Returns JSON string; free with `net_compute_free_cstring`. */
char* net_compute_replica_group_members_json(
    const net_compute_replica_group_t* h);

/* --- ForkGroup --- */

int  net_compute_fork_group_spawn(
    net_compute_runtime_t* runtime,
    const char* kind_ptr, size_t kind_len,
    uint32_t parent_origin,
    uint64_t fork_seq,
    uint32_t fork_count,
    const char* lb_strategy_ptr, size_t lb_strategy_len,
    uint64_t auto_snapshot_interval,
    uint32_t max_log_entries,
    net_compute_fork_group_t** out_handle,
    char** err_out);

void net_compute_fork_group_free(net_compute_fork_group_t* h);
int  net_compute_fork_group_fork_count(const net_compute_fork_group_t* h);
int  net_compute_fork_group_healthy_count(const net_compute_fork_group_t* h);
uint32_t net_compute_fork_group_parent_origin(const net_compute_fork_group_t* h);
uint64_t net_compute_fork_group_fork_seq(const net_compute_fork_group_t* h);
/* Returns 1 if every fork's lineage verifies, 0 otherwise. */
int  net_compute_fork_group_verify_lineage(const net_compute_fork_group_t* h);
int  net_compute_fork_group_scale_to(
    const net_compute_fork_group_t* h, uint32_t n, char** err_out);
void net_compute_fork_group_on_node_recovery(
    const net_compute_fork_group_t* h, uint64_t node_id);

char* net_compute_fork_group_members_json(const net_compute_fork_group_t* h);
char* net_compute_fork_group_fork_records_json(const net_compute_fork_group_t* h);

/* --- StandbyGroup --- */

int  net_compute_standby_group_spawn(
    net_compute_runtime_t* runtime,
    const char* kind_ptr, size_t kind_len,
    uint32_t member_count,
    const uint8_t* group_seed,
    uint64_t auto_snapshot_interval,
    uint32_t max_log_entries,
    net_compute_standby_group_t** out_handle,
    char** err_out);

void net_compute_standby_group_free(net_compute_standby_group_t* h);
int  net_compute_standby_group_member_count(const net_compute_standby_group_t* h);
int  net_compute_standby_group_standby_count(const net_compute_standby_group_t* h);
int  net_compute_standby_group_active_index(const net_compute_standby_group_t* h);
uint32_t net_compute_standby_group_active_origin(const net_compute_standby_group_t* h);
int  net_compute_standby_group_active_healthy(const net_compute_standby_group_t* h);
uint32_t net_compute_standby_group_group_id(const net_compute_standby_group_t* h);
int  net_compute_standby_group_buffered_event_count(const net_compute_standby_group_t* h);

int  net_compute_standby_group_sync_standbys(
    const net_compute_standby_group_t* h,
    uint64_t* out_through, char** err_out);
int  net_compute_standby_group_promote(
    const net_compute_standby_group_t* h,
    uint32_t* out_origin, char** err_out);
void net_compute_standby_group_on_node_recovery(
    const net_compute_standby_group_t* h, uint64_t node_id);

char* net_compute_standby_group_members_json(const net_compute_standby_group_t* h);
/* Returns "active" | "standby" (caller frees) or NULL for OOB. */
char* net_compute_standby_group_member_role(
    const net_compute_standby_group_t* h, uint32_t index);

#ifdef __cplusplus
}
#endif

#endif /* NET_SDK_H */
