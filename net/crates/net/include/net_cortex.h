/*
 * net_cortex.h — C SDK header for libnet_cortex (the CortEX /
 * NetDB tasks + memories adapter C ABI).
 *
 * One header, one shared library. Mirrors the layout of
 * `net_meshdb.h` / `net_deck.h` next to it. Symbols live in
 * `libnet_cortex.{so,dylib,dll}` built from
 * `bindings/go/cortex-ffi`. The header is the canonical
 * drop-in for C / C++ / Zig / Swift / Java JNI / etc; the Go
 * binding's `bindings/go/net/cortex_tasks.go` +
 * `cortex_memories.go` are written against the same symbols.
 *
 * # Build
 *
 *   cargo build --release -p net-cortex-ffi
 *
 *   Linux:   target/release/libnet_cortex.so
 *   macOS:   target/release/libnet_cortex.dylib
 *   Windows: target/release/net_cortex.dll
 *
 * # Link
 *
 *   gcc -o app app.c -L target/release -lnet_cortex -lpthread -ldl -lm
 *
 * # Handle model
 *
 * Five opaque heap-allocated handles cross the FFI:
 *
 *   NetCortexRedex            — Redex manager (substrate of an adapter).
 *   NetCortexTasksAdapter     — typed tasks adapter; CRUD + watch.
 *   NetCortexMemoriesAdapter  — typed memories adapter; CRUD + watch.
 *   NetCortexTasksStream      — live tasks-filter result stream.
 *   NetCortexMemoriesStream   — live memories-filter result stream.
 *
 * Caller owns every returned pointer and MUST call the
 * matching `_free` exactly once. Each `_free` is idempotent on
 * NULL. Adapters built from a Redex hold their own
 * `Arc<Redex>` clone, so freeing the Redex before the adapter
 * is sound. Stream handles built from an adapter hold their
 * own `Arc<Adapter>` clone — freeing the adapter does not
 * tear down active streams.
 *
 * # Error model
 *
 * Status-code functions return `int`:
 *
 *   NET_CORTEX_OK                 ( 0)  — success.
 *   NET_CORTEX_ERR_NULL           (-1)  — caller passed a NULL handle.
 *   NET_CORTEX_ERR_INVALID_ARG    (-2)  — bad argument (out-of-range,
 *                                          malformed JSON, NUL byte).
 *   NET_CORTEX_ERR_CALL_FAILED    (-3)  — substrate / JSON failure
 *                                          (CortexAdapterError, etc.).
 *   NET_CORTEX_ERR_ALREADY_SHUTDOWN (-4) — handle's lifecycle is done.
 *   NET_CORTEX_ERR_END_OF_STREAM  (-5)  — stream has ended cleanly.
 *
 * Detail for the most recent failure is available on a per-
 * thread basis via `net_cortex_last_error_message` (human-
 * readable detail) and `net_cortex_last_error_kind` (a stable
 * discriminator: `"redex_error"`, `"closed"`, `"fold_stopped"`,
 * `"invalid_start_position"`, `"invalid_argument"`,
 * `"runtime_panic"`, `"json_serialize_failed"`). Both return
 * NULL when no error has been recorded on the calling thread.
 * Returned pointers are valid until the next FFI call on the
 * same thread touches the thread-local; callers must NOT free
 * them. Use `net_cortex_clear_last_error` to reset state.
 *
 * Panics from any FFI body are trapped by `catch_unwind` and
 * mapped to `NET_CORTEX_ERR_CALL_FAILED` with last-error kind
 * `"runtime_panic"` — unwinding across the C ABI is UB.
 *
 * # Threading
 *
 * The crate owns one tokio multi-thread runtime, shared by
 * every adapter + stream. CRUD calls are non-blocking;
 * `_open` + `_wait_for_seq` + `_stream_next` block the calling
 * thread on the runtime.
 *
 * Handles are safe to MOVE across threads (Send-equivalent).
 * Concurrent calls from multiple threads on the SAME stream
 * handle are NOT supported — a single `TokioMutex` guards the
 * inner stream so the second caller will block until the
 * first returns; the Go binding's wrapper goroutines avoid
 * this contention by polling from a single owner.
 *
 * # Wire formats
 *
 * - `list` filter input + result rows: one JSON value per call.
 *   Filter shapes are documented at each export site.
 * - `_snapshot` / `_open_from_snapshot`: opaque byte blob,
 *   stable across this minor version. Persist the
 *   `(state_bytes, state_len, last_seq)` triple together.
 * - Watcher `_stream_next` (T54): JSON array of row objects
 *   per emission. NULL `*out` distinguishes "timeout, no
 *   item" from end-of-stream.
 */

#ifndef NET_CORTEX_H
#define NET_CORTEX_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* === Status codes ====================================================== */

#define NET_CORTEX_OK                    0
#define NET_CORTEX_ERR_NULL             -1
#define NET_CORTEX_ERR_INVALID_ARG      -2
#define NET_CORTEX_ERR_CALL_FAILED      -3
#define NET_CORTEX_ERR_ALREADY_SHUTDOWN -4
#define NET_CORTEX_ERR_END_OF_STREAM    -5

/* === Opaque handles ==================================================== */

typedef struct NetCortexRedex            NetCortexRedex;
typedef struct NetCortexTasksAdapter     NetCortexTasksAdapter;
typedef struct NetCortexMemoriesAdapter  NetCortexMemoriesAdapter;
typedef struct NetCortexTasksStream      NetCortexTasksStream;
typedef struct NetCortexMemoriesStream   NetCortexMemoriesStream;

/* === Last-error envelope =============================================== */

/*
 * Returns the most recent error message recorded on the
 * calling thread, or NULL if none. Pointer is valid until the
 * next FFI call on the same thread.
 */
const char* net_cortex_last_error_message(void);

/*
 * Returns the most recent error kind discriminator (see the
 * # Error model section above). NULL if no error has been
 * recorded on the calling thread. Same lifetime rules as
 * `net_cortex_last_error_message`.
 */
const char* net_cortex_last_error_kind(void);

/* Clear the per-thread last-error pair. */
void net_cortex_clear_last_error(void);

/*
 * Free a heap-allocated `char*` returned from a `list` or
 * stream-`_next` export. No-op on NULL.
 */
void net_cortex_free_string(char* ptr);

/*
 * Free a byte buffer returned from `_snapshot`. `len` MUST be
 * the exact length the snapshot call wrote into `*out_len`.
 * No-op on NULL.
 */
void net_cortex_free_bytes(uint8_t* ptr, size_t len);

/* === Redex manager ===================================================== */

/*
 * Allocate an in-memory Redex manager (no auth, no persistent
 * directory). Returns NULL on allocation failure (very rare —
 * an OOM at startup, essentially).
 */
NetCortexRedex* net_cortex_redex_new(void);

/* Free a Redex handle. Adapters built from it stay usable. */
void net_cortex_redex_free(NetCortexRedex* handle);

/* === Tasks adapter ===================================================== */

/*
 * Open a tasks adapter. Writes the new handle to `*out`.
 * `origin_hash` must be the 64-bit origin id this caller will
 * write under — see the CortEX layer's append-budget contract.
 */
int net_cortex_tasks_open(const NetCortexRedex* redex,
                         uint64_t origin_hash,
                         NetCortexTasksAdapter** out);

/*
 * Rehydrate from a snapshot. `last_seq == UINT64_MAX` encodes
 * `Option::None` (i.e. no prior events observed).
 */
int net_cortex_tasks_open_from_snapshot(const NetCortexRedex* redex,
                                       uint64_t origin_hash,
                                       const uint8_t* state_bytes,
                                       size_t state_len,
                                       uint64_t last_seq,
                                       NetCortexTasksAdapter** out);

void net_cortex_tasks_free(NetCortexTasksAdapter* adapter);

/* Mutations. `*out_seq` receives the RedEX seq of the append. */
int net_cortex_tasks_create(const NetCortexTasksAdapter* adapter,
                            uint64_t id,
                            const char* title,
                            uint64_t now_ns,
                            uint64_t* out_seq);
int net_cortex_tasks_rename(const NetCortexTasksAdapter* adapter,
                            uint64_t id,
                            const char* new_title,
                            uint64_t now_ns,
                            uint64_t* out_seq);
int net_cortex_tasks_complete(const NetCortexTasksAdapter* adapter,
                              uint64_t id,
                              uint64_t now_ns,
                              uint64_t* out_seq);
int net_cortex_tasks_delete(const NetCortexTasksAdapter* adapter,
                            uint64_t id,
                            uint64_t* out_seq);

/* Reads. */
int net_cortex_tasks_count(const NetCortexTasksAdapter* adapter, size_t* out_count);
int net_cortex_tasks_wait_for_seq(const NetCortexTasksAdapter* adapter, uint64_t seq);

/*
 * Capture an opaque state snapshot. `*out_state` is heap-
 * allocated (free with `net_cortex_free_bytes`); `*out_last_seq`
 * receives the applied seq (or `UINT64_MAX` if the adapter has
 * never observed an event).
 */
int net_cortex_tasks_snapshot(const NetCortexTasksAdapter* adapter,
                              uint8_t** out_state,
                              size_t* out_state_len,
                              uint64_t* out_last_seq);

/*
 * Run a filter against current state. Writes a JSON array of
 * Task objects to `*out_json` (caller frees via
 * `net_cortex_free_string`). Pass NULL `filter_json` for an
 * unfiltered listing; otherwise the filter is a JSON object
 * with all-optional fields:
 *
 *   status:           "pending" | "completed"
 *   title_contains:   string
 *   created_after_ns, created_before_ns,
 *   updated_after_ns, updated_before_ns: uint64
 *   order_by:         "id_asc" | "id_desc" |
 *                     "created_asc" | "created_desc" |
 *                     "updated_asc" | "updated_desc"
 *   limit:            uint32
 *
 * Task JSON shape:
 *   { id: u64, title: string, status: "pending"|"completed",
 *     created_ns: u64, updated_ns: u64 }
 */
int net_cortex_tasks_list(const NetCortexTasksAdapter* adapter,
                          const char* filter_json,
                          char** out_json);

/* === Memories adapter ================================================== */

int net_cortex_memories_open(const NetCortexRedex* redex,
                             uint64_t origin_hash,
                             NetCortexMemoriesAdapter** out);

int net_cortex_memories_open_from_snapshot(const NetCortexRedex* redex,
                                           uint64_t origin_hash,
                                           const uint8_t* state_bytes,
                                           size_t state_len,
                                           uint64_t last_seq,
                                           NetCortexMemoriesAdapter** out);

void net_cortex_memories_free(NetCortexMemoriesAdapter* adapter);

/*
 * Store a new memory. `tags_json` is a JSON array of strings —
 * pass NULL or "[]" for none.
 */
int net_cortex_memories_store(const NetCortexMemoriesAdapter* adapter,
                              uint64_t id,
                              const char* content,
                              const char* tags_json,
                              const char* source,
                              uint64_t now_ns,
                              uint64_t* out_seq);
int net_cortex_memories_retag(const NetCortexMemoriesAdapter* adapter,
                              uint64_t id,
                              const char* tags_json,
                              uint64_t now_ns,
                              uint64_t* out_seq);
int net_cortex_memories_pin(const NetCortexMemoriesAdapter* adapter,
                            uint64_t id,
                            uint64_t now_ns,
                            uint64_t* out_seq);
int net_cortex_memories_unpin(const NetCortexMemoriesAdapter* adapter,
                              uint64_t id,
                              uint64_t now_ns,
                              uint64_t* out_seq);
int net_cortex_memories_delete(const NetCortexMemoriesAdapter* adapter,
                               uint64_t id,
                               uint64_t* out_seq);

int net_cortex_memories_count(const NetCortexMemoriesAdapter* adapter, size_t* out_count);
int net_cortex_memories_wait_for_seq(const NetCortexMemoriesAdapter* adapter, uint64_t seq);
int net_cortex_memories_snapshot(const NetCortexMemoriesAdapter* adapter,
                                 uint8_t** out_state,
                                 size_t* out_state_len,
                                 uint64_t* out_last_seq);

/*
 * Run a filter against current state. JSON object filter shape
 * (all fields optional):
 *
 *   source:           string
 *   content_contains: string
 *   tag:              string (single-tag match)
 *   any_tag:          [string]  (any-of)
 *   all_tags:         [string]  (every-of)
 *   created_after_ns, created_before_ns,
 *   updated_after_ns, updated_before_ns: uint64
 *   pinned:           bool
 *   order_by:         "id_asc" | "id_desc" |
 *                     "created_asc" | "created_desc" |
 *                     "updated_asc" | "updated_desc"
 *   limit:            uint32
 *
 * Memory JSON shape:
 *   { id: u64, content: string, tags: [string], source: string,
 *     created_ns: u64, updated_ns: u64, pinned: bool }
 */
int net_cortex_memories_list(const NetCortexMemoriesAdapter* adapter,
                             const char* filter_json,
                             char** out_json);

/* === Watcher streams ================================================== */
/*
 * Watcher / `snapshot_and_watch` exports land alongside their
 * Go wrappers in T54 (see NET_CORTEX_PLAN.md). The handle
 * typedefs `NetCortexTasksStream` / `NetCortexMemoriesStream`
 * are forward-declared above so downstream code that only
 * needs CRUD stays binary-compatible across the rollout.
 */

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* NET_CORTEX_H */
