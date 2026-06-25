/*
 * net_cortex.h — RedEX, CortEX (Tasks + Memories adapters), and NetDb
 *                C ABI shipped from `libnet` when the cdylib is built
 *                with `--features "netdb redex-disk"`.
 *
 * Self-contained: only depends on `<stdint.h>` and `<stddef.h>`. Returns
 * raw `int` codes (zero = success, non-zero = NetError variant). For the
 * shared error enum see `net.h`. Mirror of the contents the Go binding
 * previously read inline from `net.go.h`.
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
typedef struct net_redex_file_s      net_redex_file_t;
typedef struct net_redex_tail_s      net_redex_tail_t;
typedef struct net_tasks_adapter_s   net_tasks_adapter_t;
typedef struct net_tasks_watch_s     net_tasks_watch_t;
typedef struct net_workflow_adapter_s net_workflow_adapter_t;
typedef struct net_shard_group_s     net_shard_group_t;
typedef struct net_trigger_engine_s  net_trigger_engine_t;
typedef struct net_memories_adapter_s net_memories_adapter_t;
typedef struct net_memories_watch_s  net_memories_watch_t;
typedef struct net_netdb_s           net_netdb_t;

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

/* ---- Task lifecycle (WorkflowAdapter) ----
 *
 * A single-writer RedEX chain folded into per-task { step, status,
 * attempts }. Status is a small integer code on the wire:
 *   0 submitted | 1 running | 2 waiting | 3 blocked | 4 done | 5 failed
 * Each transition entry returns the RedEX append sequence in *out_seq.
 */
typedef struct {
    uint64_t submitted;
    uint64_t running;
    uint64_t waiting;
    uint64_t blocked;
    uint64_t done;
    uint64_t failed;
} net_workflow_status_counts_t;

int  net_workflow_adapter_open(net_redex_t* redex, uint64_t origin_hash,
                               int persistent,
                               net_workflow_adapter_t** out_handle);
void net_workflow_adapter_free(net_workflow_adapter_t* handle);

int  net_workflow_submit(net_workflow_adapter_t* handle, uint64_t id, uint64_t* out_seq);
int  net_workflow_start(net_workflow_adapter_t* handle, uint64_t id, uint64_t* out_seq);
int  net_workflow_wait(net_workflow_adapter_t* handle, uint64_t id, uint64_t* out_seq);
int  net_workflow_block(net_workflow_adapter_t* handle, uint64_t id, uint64_t* out_seq);
int  net_workflow_complete(net_workflow_adapter_t* handle, uint64_t id, uint64_t* out_seq);
int  net_workflow_fail(net_workflow_adapter_t* handle, uint64_t id, uint64_t* out_seq);
int  net_workflow_advance(net_workflow_adapter_t* handle, uint64_t id, uint64_t* out_seq);
int  net_workflow_retry(net_workflow_adapter_t* handle, uint64_t id, uint64_t* out_seq);
int  net_workflow_delete(net_workflow_adapter_t* handle, uint64_t id, uint64_t* out_seq);
int  net_workflow_link(net_workflow_adapter_t* handle, uint64_t parent,
                       uint64_t child, uint64_t* out_seq);
int  net_workflow_request_cancel(net_workflow_adapter_t* handle, uint64_t id,
                                 uint64_t* out_seq);

int  net_workflow_get(net_workflow_adapter_t* handle, uint64_t id,
                      int* out_found, uint32_t* out_step,
                      int* out_status, uint32_t* out_attempts);
int  net_workflow_is_cancel_requested(net_workflow_adapter_t* handle, uint64_t id,
                                      int* out_bool);
int  net_workflow_status_counts(net_workflow_adapter_t* handle,
                                net_workflow_status_counts_t* out);
int  net_workflow_wait_for_seq(net_workflow_adapter_t* handle, uint64_t seq,
                               uint32_t timeout_ms);

/* ---- Tier 2: shards (fan-out / fan-in) ----
 * try_join's *out_kind: 0 submitted (reduce at *out_seq) | 1 already |
 * 2 pending | 3 failed (up to `cap` failed ids in out_failed, total in
 * *out_failed_count). */
int  net_shard_group_new(const uint64_t* shards, size_t n, uint64_t reduce,
                         net_shard_group_t** out_handle);
void net_shard_group_free(net_shard_group_t* handle);
int  net_workflow_fan_out(net_workflow_adapter_t* handle,
                          net_shard_group_t* group, uint64_t* out_seq);
int  net_workflow_try_join(net_workflow_adapter_t* handle,
                           net_shard_group_t* group, int* out_kind,
                           uint64_t* out_seq, uint64_t* out_failed, size_t cap,
                           size_t* out_failed_count);

/* ---- Tier 2: triggers ----
 * Bound to a WorkflowAdapter (reads its state internally). Actions are
 * (kind, id) pairs: kind 0 = submit, 1 = start. on_task_change / on_tick
 * fill the parallel out_kinds[]/out_ids[] up to `cap`, total in *out_count.
 *
 * IMPORTANT: on_task_change and on_tick are CONSUMING and SINGLE-SHOT —
 * they fire AND disarm the matching triggers on every call, whether or not
 * the out-buffers are NULL. Do NOT size them with a "probe with NULL to
 * learn *out_count, then fill" two-pass (the safe idiom for the read-only
 * getters net_workflow_subtree / net_mesh_match_islands): the probe
 * pass would fire + discard the actions and the fill pass would find
 * nothing armed. Size the buffer up front (net_trigger_armed_count is an
 * upper bound — fired is a subset of armed) and call ONCE. If *out_count
 * exceeds `cap` the surplus actions are lost (they were already
 * disarmed), so never under-size the buffer. */
int  net_trigger_engine_new(net_workflow_adapter_t* handle,
                            net_trigger_engine_t** out_handle);
void net_trigger_engine_free(net_trigger_engine_t* handle);
int  net_trigger_arm_after_task(net_trigger_engine_t* handle, uint64_t task,
                                int action_kind, uint64_t action_id);
int  net_trigger_arm_after_terminal(net_trigger_engine_t* handle, uint64_t task,
                                    int action_kind, uint64_t action_id);
int  net_trigger_on_task_change(net_trigger_engine_t* handle, uint64_t task,
                                uint64_t tick, int* out_kinds, uint64_t* out_ids,
                                size_t cap, size_t* out_count);
int  net_trigger_armed_count(net_trigger_engine_t* handle, size_t* out);
int  net_trigger_arm_at_tick(net_trigger_engine_t* handle, uint64_t tick,
                             int action_kind, uint64_t action_id);
int  net_trigger_arm_if_result(net_trigger_engine_t* handle, uint64_t task,
                               const char* key, const char* value,
                               int action_kind, uint64_t action_id);
int  net_trigger_record_result(net_trigger_engine_t* handle, uint64_t task,
                               const char* key, const char* value);
int  net_trigger_on_tick(net_trigger_engine_t* handle, uint64_t now,
                         int* out_kinds, uint64_t* out_ids, size_t cap,
                         size_t* out_count);
int  net_workflow_subtree(net_workflow_adapter_t* handle, uint64_t id,
                          uint64_t* out_ids, size_t cap, size_t* out_count);
int  net_workflow_snapshot(net_workflow_adapter_t* handle, uint8_t* out_bytes,
                           size_t cap, size_t* out_len, uint64_t* out_last_seq,
                           int* out_has_last_seq);
int  net_workflow_open_from_snapshot(net_redex_t* redex, uint64_t origin_hash,
                                     int persistent, const uint8_t* state_bytes,
                                     size_t state_len, uint64_t last_seq,
                                     int has_last_seq,
                                     net_workflow_adapter_t** out_handle);

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
