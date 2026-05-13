/*
 * net_meshdb.h — C SDK header for libnet_meshdb (the MeshDB
 * query layer C ABI).
 *
 * One header, one shared library. Mirrors the layout of `net.h` /
 * `net.go.h` / `net_rpc.h` next to it. Symbols live in the
 * `libnet_meshdb.{so,dylib,dll}` cdylib built from
 * `bindings/go/meshdb-ffi`. The Go binding's
 * `bindings/go/net/meshdb.go` cgo include block has been the
 * de-facto contract for non-Go consumers; this file is the
 * canonical drop-in for C / C++ / Zig / Swift / Java JNI / etc.
 *
 * # Build
 *
 *   cargo build --release -p net-meshdb-ffi
 *
 *   Linux:   target/release/libnet_meshdb.so
 *   macOS:   target/release/libnet_meshdb.dylib
 *   Windows: target/release/net_meshdb.dll
 *
 * # Link
 *
 *   gcc -o app app.c -L target/release -lnet_meshdb -lpthread -ldl -lm
 *
 * # Handle model
 *
 * Four opaque heap-allocated handles cross the FFI:
 *
 *   MeshDbReader   — in-memory ChainReader (substrate of a runner).
 *   MeshDbRunner   — owns the Tokio runtime + LocalMeshQueryExecutor.
 *   MeshDbQuery    — a planned query AST; reusable across runners.
 *   MeshDbIter     — a drained result-row stream (eager today).
 *
 * Caller owns every returned pointer and MUST call the matching
 * `_free` exactly once. Each `_free` is idempotent on NULL. A
 * runner clones the reader's underlying `Arc<InMemoryStore>` on
 * construction, so freeing the reader before the runner is sound.
 *
 * # Error model
 *
 * Status-code functions (iterator + reader append) return `int`:
 *
 *   NET_MESHDB_OK            (0)  — success.
 *   NET_MESHDB_END           (1)  — iterator drained (no more rows).
 *   NET_MESHDB_INVALID_ARG   (2)  — NULL handle / out-of-range input.
 *   NET_MESHDB_RUNTIME_ERR   (3)  — planner / executor failure.
 *
 * Factory functions (query / runner / iter constructors) return a
 * pointer; NULL signals failure. The factory's failure mode is
 * "invalid input or planner rejection".
 *
 * Detail for the most recent failure is available on a per-thread
 * basis via `net_meshdb_last_error_message` (human-readable
 * detail) and `net_meshdb_last_error_kind` (one of the
 * `MeshError` variant tags such as `"planner_error"`,
 * `"executor_error"`, `"query_cancelled"`, `"runtime_panic"`,
 * `"invalid_arg"`, etc.). Both return NULL when no error has
 * been recorded on the calling thread. Returned pointers are
 * valid until the next FFI call on the same thread touches the
 * thread-local; callers must NOT free them. Use
 * `net_meshdb_clear_last_error` to reset state.
 *
 * Panics from user-controlled operators (aggregate division by
 * zero, OOM in hash-join, etc.) are trapped by `catch_unwind`
 * around the async closure; instead of unwinding across the C
 * ABI (which is UB), the runner returns NULL and populates the
 * last-error pair with kind `"runtime_panic"`.
 *
 * # Threading
 *
 * The crate owns a Tokio multi-thread runtime per `MeshDbRunner`.
 * `execute` / `execute_with` block the caller's thread until the
 * full result stream is drained into the returned iterator.
 * Multiple runners can coexist; each holds its own runtime.
 *
 * Handles are safe to MOVE across threads (Send-equivalent) —
 * the Go binding wraps `execute` in a goroutine to surface
 * results through a Go channel. Concurrent calls from
 * multiple threads on the SAME handle (Sync-equivalent
 * behaviour) are NOT supported in this slice: a single
 * `MeshDbRunner` or `MeshDbIter` must be used from one thread
 * at a time (or guarded by external synchronisation). The
 * thread-local last-error pair behaves like POSIX errno —
 * each calling thread sees its own most-recent error.
 *
 * Non-Go consumers can pick whatever threading discipline
 * matches their language as long as they respect the
 * single-thread-at-a-time-per-handle constraint.
 *
 * # Wire-format payloads
 *
 * Atomic-operator rows (At / Between / Latest / LineageEmit)
 * carry the raw event bytes in their payload. Composite-operator
 * rows (Count / Sum / Avg / Min / Max / DistinctCount /
 * Percentile / Join / Window) carry a postcard-encoded sentinel
 * envelope. Use `net_meshdb_decode_payload_json` to turn a
 * sentinel envelope into a tagged JSON string; the function
 * returns NULL for plain (non-sentinel) payloads, letting callers
 * branch on "did the decoder recognise this?" without a separate
 * type query.
 */

#ifndef NET_MESHDB_H
#define NET_MESHDB_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* =========================================================================
 * Status codes
 * ========================================================================= */

#define NET_MESHDB_OK            0
#define NET_MESHDB_END           1
#define NET_MESHDB_INVALID_ARG   2
#define NET_MESHDB_RUNTIME_ERR   3

/* Cache-policy discriminator for `net_meshdb_runner_execute_with`.
 *
 *   NET_MESHDB_CACHE_PERMANENT  — cache until LRU eviction; safe
 *                                 only when the query result is
 *                                 immutable under substrate
 *                                 semantics.
 *   NET_MESHDB_CACHE_TIME_BOUND — TTL expiry; `cache_ttl_secs`
 *                                 is consulted. Pass 5.0 to
 *                                 match the canonical default
 *                                 (mirrors the Phase F join
 *                                 watermark). Non-finite /
 *                                 negative values fall back to
 *                                 5.0 internally. */
#define NET_MESHDB_CACHE_PERMANENT   0
#define NET_MESHDB_CACHE_TIME_BOUND  1

/* =========================================================================
 * Opaque handle types
 * ========================================================================= */

typedef struct MeshDbReader  MeshDbReader;
typedef struct MeshDbRunner  MeshDbRunner;
typedef struct MeshDbQuery   MeshDbQuery;
typedef struct MeshDbIter    MeshDbIter;

/* =========================================================================
 * Reader (in-memory ChainReader)
 * ========================================================================= */

/* Allocate a new in-memory `ChainReader`. Never returns NULL on
 * the current allocator. Free with `net_meshdb_reader_free`. */
MeshDbReader* net_meshdb_reader_new(void);

/* Free a reader handle. No-op on NULL.
 *
 * Freeing the reader does NOT tear down a MeshDbRunner built
 * from it — the runner holds its own Arc<InMemoryStore> clone
 * and stays usable. Once the reader is freed, however, calling
 * net_meshdb_reader_append on that pointer is undefined
 * behaviour (use-after-free). Two valid patterns:
 *
 *   (a) snapshot-then-free: append everything you need, build
 *       the runner, then free the reader. Do not append further.
 *   (b) keep-alive: do not free the reader while you still want
 *       to append. New appends are visible to the runner. Free
 *       the reader after the last append. */
void net_meshdb_reader_free(MeshDbReader* reader);

/* Append `(origin, seq, payload)` to the reader. `payload` may
 * be NULL when `payload_len == 0`. Returns NET_MESHDB_OK on
 * success or NET_MESHDB_INVALID_ARG on a NULL reader / NULL
 * payload with non-zero length.
 *
 * New rows are visible to every MeshDbRunner that was
 * constructed from this reader before or after the append —
 * they share the same Arc<InMemoryStore>. Calling this after
 * net_meshdb_reader_free(reader) is undefined behaviour. */
int net_meshdb_reader_append(
    MeshDbReader* reader,
    uint64_t origin,
    uint64_t seq,
    const uint8_t* payload,
    size_t payload_len
);

/* =========================================================================
 * Query factories — atomic operators
 * ========================================================================= */

/* `At(origin, seq)` — read a single event by chain + seq. */
MeshDbQuery* net_meshdb_query_at(uint64_t origin, uint64_t seq);

/* `Between(origin, start, end)` — half-open seq range. Returns
 * NULL when `start >= end`. */
MeshDbQuery* net_meshdb_query_between(
    uint64_t origin,
    uint64_t start,
    uint64_t end
);

/* `Latest(origin)` — tip event for the chain. */
MeshDbQuery* net_meshdb_query_latest(uint64_t origin);

/* `LineageEmit(origin, entries, direction)` — emit one row per
 * pre-walked lineage entry. The SDK does NOT walk the fork-of:
 * graph itself; callers supply the walk result.
 *
 * `entries_json` is a JSON array of objects with the shape
 *   `{"origin": <u64>, "depth": <u32>, "tip_seq": <u64 | null>}`
 * in walk order. Each entry produces a `ResultRow` with
 * `origin = entry.origin`, `seq = entry.tip_seq ?? 0`, payload
 * empty. Compose with `at` / `between` to fetch event bodies for
 * each ancestor / descendant.
 *
 * `direction` is `"back"` or `"forward"`. Returns NULL on parse
 * error, unknown direction, or a NULL pointer arg. */
MeshDbQuery* net_meshdb_query_lineage_emit(
    uint64_t origin,
    const char* entries_json,
    const char* direction
);

/* Free a query handle. No-op on NULL. */
void net_meshdb_query_free(MeshDbQuery* query);

/* =========================================================================
 * Query factories — composite operators
 *
 * `group_by` is a comma-separated UTF-8 C-string of row-intrinsic
 * field names: NULL or `""` for no grouping; `"origin"`, `"seq"`,
 * `"origin,seq"` (order-insensitive) for the typed variants.
 * Anything else returns NULL.
 * ========================================================================= */

/* `Window(inner, size)` — tumbling-on-seq window. Returns NULL
 * when `size == 0`. `inner` is NOT consumed (caller still owns).
 * The emitted rows carry postcard-encoded `WindowBoundary`
 * envelopes — decode via `net_meshdb_decode_payload_json`. */
MeshDbQuery* net_meshdb_query_window(
    const MeshDbQuery* inner,
    uint64_t size
);

/* `Count(inner, group_by)` — row count. Emits one sentinel row
 * per group (or a single row when `group_by` is NULL / empty). */
MeshDbQuery* net_meshdb_query_count(
    const MeshDbQuery* inner,
    const char* group_by
);

/* `Sum / Avg / Min / Max / DistinctCount` — numeric aggregates
 * keyed by `kind` (one of `"sum"`, `"avg"`, `"min"`, `"max"`,
 * `"distinct_count"`). `field` is a row-intrinsic name (`"origin"`
 * / `"seq"`) or a dotted JSON payload path. Returns NULL on
 * unknown `kind`, invalid args, or a NULL inner. */
MeshDbQuery* net_meshdb_query_numeric_agg(
    const MeshDbQuery* inner,
    const char* kind,
    const char* field,
    const char* group_by
);

/* `Percentile(inner, field, p, group_by)` — nearest-rank exact
 * percentile. `p` must be finite in `[0.0, 1.0]`. Returns NULL
 * otherwise. Field-extraction semantics match the numeric
 * aggregates. */
MeshDbQuery* net_meshdb_query_percentile(
    const MeshDbQuery* inner,
    const char* field,
    double p,
    const char* group_by
);

/* `Join(left, right, kind, key, strategy, watermark_secs)` —
 * hash- or sort-merge join.
 *
 *   `kind`           — one of `"inner"`, `"left_outer"`,
 *                      `"right_outer"`, `"full_outer"`.
 *   `key`            — `"origin"`, `"seq"`, `"origin,seq"`, or
 *                      any other string (treated as a JSON
 *                      payload path).
 *   `strategy`       — `"hash_broadcast"` (default) or
 *                      `"sort_merge"`. NULL or `""` selects the
 *                      default.
 *   `watermark_secs` — informational under snapshot semantics;
 *                      pass 5.0 to match the locked join
 *                      watermark. Non-finite / negative values
 *                      fall back to 5.0 internally.
 *
 * Returns NULL on unknown kind / strategy or any NULL pointer
 * arg. */
MeshDbQuery* net_meshdb_query_join(
    const MeshDbQuery* left,
    const MeshDbQuery* right,
    const char* kind,
    const char* key,
    const char* strategy,
    double watermark_secs
);

/* `Filter(inner, predicate)` — JSON-encoded predicate. The JSON
 * shape mirrors the Python / Node `Predicate` factories:
 *
 *   {"kind":"exists","field":"<name>"}
 *   {"kind":"equals","field":"<name>","value":"<str>"}
 *   {"kind":"numeric_at_least","field":"<name>","threshold":N}
 *   {"kind":"numeric_at_most","field":"<name>","threshold":N}
 *   {"kind":"numeric_in_range","field":"<name>","min":N,"max":N}
 *   {"kind":"string_prefix","field":"<name>","prefix":"<str>"}
 *   {"kind":"string_matches","field":"<name>","pattern":"<str>"}
 *   {"kind":"semver_at_least","field":"<name>","version":"<str>"}
 *   {"kind":"and","children":[<pred>,...]}
 *   {"kind":"or","children":[<pred>,...]}
 *   {"kind":"not","child":<pred>}
 *
 * Field names are row-intrinsic (`"origin"` / `"seq"`) or JSON
 * payload paths; matching is done against the synthetic per-row
 * tag view. Returns NULL on JSON parse error or invalid args. */
MeshDbQuery* net_meshdb_query_filter_json(
    const MeshDbQuery* inner,
    const char* predicate_json
);

/* =========================================================================
 * Runner — owns the Tokio runtime + LocalMeshQueryExecutor
 * ========================================================================= */

/* Build a cache-less runner over `reader`. The runner clones the
 * reader's `Arc<InMemoryStore>`; freeing the reader after the
 * runner is built is sound — but see the lifetime note on
 * `net_meshdb_reader_free`: once freed, calling
 * `net_meshdb_reader_append` against the same pointer is UB.
 * Returns NULL on a NULL reader. */
MeshDbRunner* net_meshdb_runner_new(const MeshDbReader* reader);

/* Build a runner with the Phase F single-node LRU result cache
 * wired in. The capability-version closure is fixed at `0`
 * because no `CapabilityIndex` is plumbed through the C ABI yet;
 * pull-invalidation across version changes lands when this
 * surface grows a federated-executor path. Returns NULL on a
 * NULL reader. */
MeshDbRunner* net_meshdb_runner_new_cached(const MeshDbReader* reader);

/* Free a runner handle. No-op on NULL. Outstanding iterators
 * remain valid (they own their drained rows independently of
 * the runner). */
void net_meshdb_runner_free(MeshDbRunner* runner);

/* Execute `query` on `runner`. Blocks the caller until the
 * result stream is drained into the returned iterator. Returns
 * NULL on planner / executor failure. The iterator is owned by
 * the caller — free via `net_meshdb_iter_free`. Both pointer
 * args may be safely null (yields NULL). Neither the runner
 * nor the query is mutated across the call; C++ consumers
 * holding a `const MeshDbQuery*` may pass it without a cast. */
MeshDbIter* net_meshdb_runner_execute(
    MeshDbRunner* runner,
    const MeshDbQuery* query
);

/* Execute `query` with explicit Phase F options. Matches the
 * Python / Node `execute_with` surface.
 *
 *   `bypass_cache`     — non-zero: skip both cache lookup and
 *                        writeback.
 *   `cache_policy_kind` — NET_MESHDB_CACHE_PERMANENT or
 *                        NET_MESHDB_CACHE_TIME_BOUND. Unrecognized
 *                        values fall back to TimeBound.
 *   `cache_ttl_secs`   — TTL for TimeBound. Ignored for Permanent.
 *                        Non-finite / negative falls back to 5.0.
 *
 * Returns NULL on the same failures as `_execute`. Same
 * const-correctness as `_execute`. */
MeshDbIter* net_meshdb_runner_execute_with(
    MeshDbRunner* runner,
    const MeshDbQuery* query,
    int bypass_cache,
    int cache_policy_kind,
    double cache_ttl_secs
);

/* =========================================================================
 * Result iterator
 * ========================================================================= */

/* Pull the next row from `iter`.
 *
 * On NET_MESHDB_OK, populates `*origin_out`, `*seq_out`, and
 * `*payload_out_ptr` / `*payload_out_len` with a freshly heap-
 * allocated payload copy. Caller MUST free the payload via
 * `net_meshdb_payload_free` (short-lived test code may leak, but
 * production callers must free).
 *
 * On NET_MESHDB_END, no out params are written.
 *
 * Any NULL `iter` or out-pointer arg returns
 * NET_MESHDB_INVALID_ARG without writing any output. */
int net_meshdb_iter_next(
    MeshDbIter* iter,
    uint64_t* origin_out,
    uint64_t* seq_out,
    uint8_t** payload_out_ptr,
    size_t* payload_out_len
);

/* Free a payload buffer returned via `_iter_next`. No-op on
 * NULL or zero-length. `len` MUST match what `_iter_next`
 * wrote — the buffer was boxed with `(ptr, len)` layout and the
 * deallocator reads `len`. */
void net_meshdb_payload_free(uint8_t* ptr, size_t len);

/* Free an iterator handle. No-op on NULL. Safe to call before
 * the iterator is fully drained — pending rows are dropped. */
void net_meshdb_iter_free(MeshDbIter* iter);

/* =========================================================================
 * Sentinel-envelope decoder
 * ========================================================================= */

/* Decode a result-row payload into a JSON description of the
 * sentinel envelope. Returns NULL when the payload doesn't
 * deserialize as any known envelope (atomic-operator rows return
 * NULL — their payload is the raw event body, not a sentinel).
 *
 * JSON shape per variant:
 *
 *   Aggregate:
 *     {"kind":"aggregate","group":{...|null},
 *      "value":{"kind":"count","value":N,"count":N}}
 *
 *   Joined:
 *     {"kind":"joined","left":{...|null},"right":{...|null}}
 *
 *   Window:
 *     {"kind":"window","start":N,"end":N,"rows":[{...},...]}
 *
 * Each nested row is `{"origin":N,"seq":N,"payload":[<byte>,...]}`
 * (a JSON array of byte integers — no base64 to avoid a dep on
 * the consumer side).
 *
 * Aggregate `value.kind` is one of: `"count"`, `"sum"`, `"avg"`,
 * `"min"`, `"max"`, `"distinct_count"`, `"percentile"`. `value`
 * is the numeric result (or `null` for empty avg / min / max /
 * percentile groups); `count` mirrors `value` for the count /
 * distinct_count kinds.
 *
 * Free the returned string with `net_meshdb_free_string`. NULL
 * payload or `payload_len == 0` returns NULL. */
char* net_meshdb_decode_payload_json(
    const uint8_t* payload,
    size_t payload_len
);

/* Free a C-string returned by `net_meshdb_decode_payload_json`.
 * No-op on NULL. */
void net_meshdb_free_string(char* s);

/* =========================================================================
 * Last-error reporting (thread-local)
 * ========================================================================= */

/* Most recent error message recorded on the calling thread, or
 * NULL if no error has been recorded. The pointer is valid until
 * the next FFI call on the same thread that touches the
 * thread-local. Callers must NOT free the returned pointer. */
const char* net_meshdb_last_error_message(void);

/* Most recent error kind discriminator recorded on the calling
 * thread. One of the `MeshError` variant tags
 * (`"planner_error"`, `"executor_error"`, `"query_cancelled"`,
 * `"join_memory_exceeded"`, `"query_budget_exceeded"`,
 * `"runtime_panic"`, `"invalid_arg"`, etc.), or NULL if no error
 * has been recorded. Same lifetime rules as the message. */
const char* net_meshdb_last_error_kind(void);

/* Reset the thread-local last-error pair to NULL. Useful as a
 * defensive prelude in test harnesses that may have inherited
 * state from a prior iteration. */
void net_meshdb_clear_last_error(void);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* NET_MESHDB_H */
