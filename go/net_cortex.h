/*
 * net_cortex.h — RedEX, CortEX (Tasks + Memories adapters), and NetDb
 *                C ABI shipped from `libnet` when the cdylib is built
 *                with `--features "netdb redex-disk"`.
 *
 * Mirror of `net/crates/net/include/net_cortex.h`. Self-contained: depends
 * only on `<stdint.h>` and `<stddef.h>`. Returns raw `int` codes (zero =
 * success, non-zero = NetError variant). For the shared error enum see
 * `net.h`.
 *
 * Watch / tail cursors:
 *   * `next(cursor, timeout_ms, &out_json, &out_len)` returns:
 *       `0`                 — event delivered; *out_json owned by caller
 *       `NET_STREAM_TIMEOUT`— no event within timeout_ms
 *       `NET_STREAM_ENDED`  — cursor reached end-of-stream
 *       negative            — net_error_t
 *     Caller frees *out_json via `net_free_string` (declared in `net.h`)
 *     when `0` is returned.
 */

#ifndef NET_CORTEX_H
#define NET_CORTEX_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Opaque handle types */
typedef struct net_redex_s           net_redex_t;
typedef struct net_netdb_s           net_netdb_t;
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
int  net_tasks_adapter_open(net_redex_t* redex, uint64_t origin_hash,
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
int  net_memories_adapter_open(net_redex_t* redex, uint64_t origin_hash,
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

/* ---- NetDb (cross-adapter bundle) ----
 *
 * Composes Tasks + Memories over a single Redex behind one handle.
 *
 * Open shape — `config_json`:
 *   { "origin_hash": <u64>, "persistent": <bool>,
 *     "with_tasks": <bool>, "with_memories": <bool> }
 *
 * Snapshot wire format is the postcard encoding of `NetDbSnapshot`. Bundles
 * captured here round-trip with the Rust, napi, and PyO3 surfaces.
 *
 * `net_netdb_tasks` / `net_netdb_memories` hand out independent Arc-cloned
 * adapter handles — freeing them does NOT close the underlying adapter, and
 * the NetDb itself can be freed before the adapter clones. Errors collapse
 * to `NET_ERR_NETDB`. */
int  net_netdb_open(net_redex_t* redex, const char* config_json,
                    net_netdb_t** out_handle);
int  net_netdb_open_from_snapshot(net_redex_t* redex, const char* config_json,
                                  const uint8_t* bundle, size_t bundle_len,
                                  net_netdb_t** out_handle);
int  net_netdb_snapshot(net_netdb_t* handle,
                        uint8_t** out_bytes, size_t* out_len);
void net_netdb_free_bundle(uint8_t* bytes, size_t len);
int  net_netdb_tasks(net_netdb_t* handle, net_tasks_adapter_t** out_handle);
int  net_netdb_memories(net_netdb_t* handle, net_memories_adapter_t** out_handle);
int  net_netdb_close(net_netdb_t* handle);
void net_netdb_free(net_netdb_t* handle);

#ifdef __cplusplus
}
#endif

#endif /* NET_CORTEX_H */
