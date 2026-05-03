/*
 * Net C SDK
 *
 * Network Event Transport — a latency-first encrypted mesh protocol.
 *
 * One header, one shared library. This is the entire C SDK.
 * Links against libnet.so (Linux), libnet.dylib (macOS), or net.dll (Windows).
 *
 * Thread Safety: All functions are thread-safe. Handles can be shared across threads.
 *
 * Memory: Handles from net_init() must be freed with net_shutdown().
 *         Poll results from net_poll_ex() must be freed with net_free_poll_result().
 *         Strings from net_generate_keypair() must be freed with net_free_string().
 */

#ifndef NET_SDK_H
#define NET_SDK_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ========================================================================= */
/* Types                                                                     */
/* ========================================================================= */

/* Opaque handle to the event bus. */
typedef void* net_handle_t;

/*
 * Error codes.
 *
 * Kept in sync with the Rust-side `NetError` enum and with the Go
 * binding's copy at `bindings/go/net/net.h`. The library has a
 * regression test that scans both headers to detect drift.
 */
typedef enum {
    NET_SUCCESS              =  0,
    NET_ERR_NULL_POINTER     = -1,
    NET_ERR_INVALID_UTF8     = -2,
    NET_ERR_INVALID_JSON     = -3,
    NET_ERR_INIT_FAILED      = -4,
    NET_ERR_INGESTION_FAILED = -5,
    NET_ERR_POLL_FAILED      = -6,
    NET_ERR_BUFFER_TOO_SMALL = -7,
    NET_ERR_SHUTTING_DOWN    = -8,
    /*
     * Response byte count exceeds `c_int::MAX`. The data was already
     * copied into the caller's buffer (so resizing won't help — that
     * is BUFFER_TOO_SMALL); the caller's int counter just can't
     * represent the count. Surfaced by net_poll / net_stats when
     * their JSON output is multi-gigabyte.
     */
    NET_ERR_INT_OVERFLOW     = -9,
    /*
     * A stream handle was passed to a send-family FFI for a
     * net_handle_t that did not create it. The FFI layer rejects
     * such cross-handle traffic to prevent silent leaks between
     * sessions when a caller bug crosses handles.
     */
    NET_ERR_MISMATCHED_HANDLES = -10,
    /*
     * `CString::new` failure: the input bytes are valid UTF-8 (by
     * Rust's String invariant) but contain an interior NUL byte
     * that the C ABI's NUL-terminated string can't carry. Pre-fix
     * this was reported as NET_ERR_INVALID_UTF8, which was wrong:
     * the input is UTF-8-valid; it just has a NUL where C expects
     * none. Bindings that branch on the typed error get the right
     * cause now.
     */
    NET_ERR_INTERIOR_NUL     = -11,
    NET_ERR_UNKNOWN          = -99
} net_error_t;

/* Ingestion receipt. */
typedef struct {
    uint16_t shard_id;
    uint64_t timestamp;
} net_receipt_t;

/* A single stored event. */
typedef struct {
    const char* id;
    size_t      id_len;
    const char* raw;
    size_t      raw_len;
    uint64_t    insertion_ts;
    uint16_t    shard_id;
} net_event_t;

/* Poll result containing events and cursor. */
typedef struct {
    net_event_t* events;
    size_t       count;
    char*        next_id;
    int          has_more;
} net_poll_result_t;

/* Ingestion statistics. */
typedef struct {
    uint64_t events_ingested;
    uint64_t events_dropped;
    uint64_t batches_dispatched;
} net_stats_t;

/* ========================================================================= */
/* Lifecycle                                                                 */
/* ========================================================================= */

/*
 * Initialize a new node.
 *
 * @param config_json  JSON config string (null-terminated), or NULL for defaults.
 * @return  Handle to the node, or NULL on failure.
 *
 * Example: net_init("{\"num_shards\": 4}")
 * Example: net_init(NULL)  // defaults
 */
net_handle_t net_init(const char* config_json);

/*
 * Shut down the node and free all resources.
 * The handle is invalid after this call.
 *
 * @return  0 on success, negative error code on failure.
 */
int net_shutdown(net_handle_t handle);

/*
 * Get the library version string (static, do not free).
 */
const char* net_version(void);

/*
 * Get the number of shards.
 *
 * @return  Number of shards, or 0 if handle is NULL.
 */
uint16_t net_num_shards(net_handle_t handle);

/* ========================================================================= */
/* Ingestion                                                                 */
/* ========================================================================= */

/*
 * Ingest a raw JSON string (fastest path, no parsing).
 *
 * @param json  JSON string (not null-terminated — length is explicit).
 * @param len   Length of the JSON string in bytes.
 * @return  0 on success, negative error code on failure.
 */
int net_ingest_raw(net_handle_t handle, const char* json, size_t len);

/*
 * Ingest a raw JSON string and get a receipt.
 *
 * @param json  JSON string.
 * @param len   Length of the JSON string in bytes.
 * @param out   Receipt output (shard_id, timestamp). May be NULL.
 * @return  0 on success, negative error code on failure.
 */
int net_ingest_raw_ex(net_handle_t handle, const char* json, size_t len, net_receipt_t* out);

/*
 * Ingest a single event (parses JSON for validation).
 *
 * @param event_json  JSON event string.
 * @param len         Length of the event string in bytes.
 * @return  0 on success, negative error code on failure.
 */
int net_ingest(net_handle_t handle, const char* event_json, size_t len);

/*
 * Ingest multiple raw JSON strings in a batch.
 *
 * @param jsons  Array of pointers to JSON strings.
 * @param lens   Array of lengths for each string.
 * @param count  Number of events.
 * @return  Number of successfully ingested events, or negative error code.
 */
int net_ingest_raw_batch(
    net_handle_t handle,
    const char** jsons,
    const size_t* lens,
    size_t count
);

/*
 * Ingest events from a JSON array string.
 *
 * @param events_json  JSON array (null-terminated).
 * @return  Number of ingested events, or negative error code.
 */
int net_ingest_batch(net_handle_t handle, const char* events_json);

/* ========================================================================= */
/* Consumption                                                               */
/* ========================================================================= */

/*
 * Poll events (JSON interface).
 *
 * @param request_json  JSON request, e.g. {"limit": 100}. NULL for defaults.
 * @param out_buffer    Output buffer for JSON response.
 * @param buffer_len    Size of output buffer.
 * @return  Bytes written on success, negative error code on failure.
 */
int net_poll(
    net_handle_t handle,
    const char* request_json,
    char* out_buffer,
    size_t buffer_len
);

/*
 * Poll events (structured interface, no JSON overhead).
 *
 * @param limit   Maximum number of events.
 * @param cursor  Resume cursor (null-terminated), or NULL for start.
 * @param out     Poll result output. Must be freed with net_free_poll_result().
 * @return  0 on success, negative error code on failure.
 */
int net_poll_ex(
    net_handle_t handle,
    size_t limit,
    const char* cursor,
    net_poll_result_t* out
);

/*
 * Free a poll result returned by net_poll_ex().
 */
void net_free_poll_result(net_poll_result_t* result);

/* ========================================================================= */
/* Statistics                                                                */
/* ========================================================================= */

/*
 * Get statistics (JSON interface).
 *
 * @param out_buffer  Output buffer for JSON.
 * @param buffer_len  Size of output buffer.
 * @return  Bytes written on success, negative error code on failure.
 */
int net_stats(net_handle_t handle, char* out_buffer, size_t buffer_len);

/*
 * Get statistics (structured, no JSON overhead).
 *
 * @param out  Stats output.
 * @return  0 on success, negative error code on failure.
 */
int net_stats_ex(net_handle_t handle, net_stats_t* out);

/* ========================================================================= */
/* Utilities                                                                 */
/* ========================================================================= */

/*
 * Flush pending batches to the adapter.
 *
 * @return  0 on success, negative error code on failure.
 */
int net_flush(net_handle_t handle);

/*
 * Generate a new keypair for encrypted mesh transport.
 *
 * @return  JSON string with hex-encoded public_key and secret_key.
 *          Caller must free with net_free_string(). NULL if not available.
 */
char* net_generate_keypair(void);

/*
 * Free a string returned by net_generate_keypair() or similar.
 */
void net_free_string(char* s);

/* =========================================================================
 * Redis Streams consumer-side dedup helper
 * (compiled when the Rust cdylib has the `redis` feature on).
 *
 * The Net Redis adapter writes a stable `dedup_id` field on every
 * XADD entry of the form
 *
 *     {producer_nonce:hex}:{shard_id}:{sequence_start}:{i}
 *
 * When the producer's MULTI/EXEC times out client-side but runs
 * server-side anyway, the retry produces a duplicate stream entry
 * with a distinct server-generated `*` id but the same `dedup_id`.
 * This helper maintains an LRU-bounded set of seen ids and answers
 * a test-and-insert query so consumers can filter at consume time.
 *
 * Each handle wraps an LRU-bounded set protected by an internal
 * mutex; concurrent calls from multiple threads on the same handle
 * are safe but serialize. Production callers typically instantiate
 * one helper per consumer thread and key on the `dedup_id` field
 * extracted from each XRANGE / XREAD entry. See `include/README.md`
 * and the language-specific binding READMEs for runnable examples.
 * ========================================================================= */

typedef struct net_redis_dedup_s net_redis_dedup_t;

/*
 * Create a new dedup helper.
 *
 * @param capacity  LRU capacity. `0` selects the default (4096).
 *                  Production callers should size to their dedup
 *                  window — a consumer at ~10k events/sec with a
 *                  1 min window wants ~600,000.
 * @return  Heap-allocated handle. Never returns NULL. Free with
 *          `net_redis_dedup_free`.
 */
net_redis_dedup_t* net_redis_dedup_new(size_t capacity);

/*
 * Free a helper handle. NULL is a no-op.
 */
void net_redis_dedup_free(net_redis_dedup_t* handle);

/*
 * Test-and-insert.
 *
 * @return   1 — duplicate (caller should skip the entry)
 *           0 — new       (caller should process AND we've now
 *                          marked it seen)
 *          -1 — NULL handle or NULL dedup_id
 *          -2 — invalid UTF-8 in dedup_id
 */
int net_redis_dedup_is_duplicate(net_redis_dedup_t* handle, const char* dedup_id);

/*
 * Number of distinct ids currently tracked. Returns 0 on NULL
 * handle (mirrors the "no ids" semantic).
 */
size_t net_redis_dedup_len(net_redis_dedup_t* handle);

/*
 * Configured maximum capacity. Returns 0 on NULL handle.
 */
size_t net_redis_dedup_capacity(net_redis_dedup_t* handle);

/*
 * Returns 1 if no ids are tracked, 0 if the helper has at least
 * one id, -1 on NULL handle.
 */
int net_redis_dedup_is_empty(net_redis_dedup_t* handle);

/*
 * Clear all tracked ids. Use after a consumer-group rebalance.
 * NULL is a no-op.
 */
void net_redis_dedup_clear(net_redis_dedup_t* handle);

#ifdef __cplusplus
}
#endif

#endif /* NET_SDK_H */
