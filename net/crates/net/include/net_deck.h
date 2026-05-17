/*
 * net_deck.h — C SDK header for libnet_deck (the Deck operator-
 * side SDK C ABI).
 *
 * One header, one shared library. Mirrors the layout of
 * `net_meshos.h` / `net_meshdb.h` next to it. Symbols live in the
 * `libnet_deck.{so,dylib,dll}` cdylib built from
 * `bindings/go/deck-ffi`. The Go binding's
 * `bindings/go/net/deck.go` cgo include block is the sister-
 * consumer contract; this file is the canonical drop-in for C /
 * C++ / Zig / Swift / Java JNI / etc.
 *
 * Companion to `DECK_SDK_PLAN.md` Phase 7; consumes the cdylib
 * shipped for Phase 6 (Go) without modification.
 *
 * # Scope (slice 1)
 *
 * Client lifecycle, all 9 `AdminCommands` methods, one-shot
 * `status` / `status_summary`, and snapshot + status-summary
 * streams. Audit / log / failure streams land in slice 2; ICE
 * (force_*, simulate/commit typestate) in slice 3.
 *
 * # Operator-only mode
 *
 * `net_deck_client_new` constructs a private MeshOS supervisor
 * runtime inside the cdylib. The caller supplies only the 32-byte
 * operator seed + supervisor config; the cdylib wraps the
 * substrate's runtime end-to-end. Composing against an
 * externally-managed `NetMeshOsSdk` handle (from
 * `libnet_meshos`) requires cross-cdylib symbol sharing and lands
 * in slice 2 when an operator workflow demands it.
 *
 * # Build
 *
 *   cargo build --release -p net-deck-ffi
 *
 *   Linux:   target/release/libnet_deck.so
 *   macOS:   target/release/libnet_deck.dylib
 *   Windows: target/release/net_deck.dll
 *
 * # Link
 *
 *   gcc -o app app.c -L target/release -lnet_deck -lpthread -ldl -lm
 *
 * # Handle model
 *
 * Three opaque heap-allocated handles cross the FFI:
 *
 *   NetDeckClient              — the deck client (owns the
 *                                supervisor runtime + admin /
 *                                status surfaces).
 *   NetDeckSnapshotStream      — live MeshOsSnapshot stream.
 *   NetDeckStatusSummaryStream — live StatusSummary stream.
 *
 * Caller owns every returned pointer and MUST call the matching
 * `_free` exactly once. `_free` is idempotent on NULL.
 *
 * `net_deck_status` returns a heap-allocated JSON string; caller
 * frees with `net_deck_free_string`. Streams' `_next` calls also
 * allocate a fresh JSON string per call (snapshot stream only);
 * caller frees per call.
 *
 * # Error model
 *
 * Status-code functions return `int`:
 *
 *   NET_DECK_OK                  (0)   — success.
 *   NET_DECK_ERR_NULL           (-1)   — NULL handle.
 *   NET_DECK_ERR_CALL_FAILED    (-2)   — substrate-side failure;
 *                                         see last-error pair.
 *   NET_DECK_ERR_INVALID_ARG    (-3)   — NULL pointer / bad input.
 *   NET_DECK_ERR_ALREADY_SHUTDOWN (-4) — handle already freed.
 *   NET_DECK_ERR_END_OF_STREAM  (-5)   — stream drained / closed.
 *
 * Detail flows through a per-thread last-error pair populated
 * on every non-OK status. After any non-OK status, call
 * `net_deck_last_error_message` for the human-readable text and
 * `net_deck_last_error_kind` for the substrate discriminator
 * (`"register_failed"`, `"queue_full"`, `"already_shutdown"`,
 * `"snapshot_serialize_failed"`, `"invalid_argument"`,
 * `"runtime_panic"`). Both return NULL when no error has been
 * recorded on the calling thread. Returned pointers are valid
 * until the next FFI call on the same thread touches the
 * thread-local; callers must NOT free.
 *
 * The substrate-side envelope is `<<deck-sdk-kind:KIND>>MSG` —
 * cross-language consumers (Python / Node / Go) parse the same
 * envelope. C consumers reach the discriminator directly through
 * `net_deck_last_error_kind`.
 *
 * Panics from substrate calls are trapped by `catch_unwind` at
 * every FFI entry point; instead of unwinding across the C ABI
 * (UB), the call returns the appropriate error status and
 * populates the last-error pair with kind `"runtime_panic"`.
 *
 * # Threading
 *
 * The cdylib owns one process-global Tokio multi-thread runtime.
 * `net_deck_admin_*`, `net_deck_snapshot_stream_next`, and
 * `net_deck_status_summary_stream_next` block the caller's thread.
 * Handles are safe to MOVE across threads. Concurrent calls
 * from multiple threads on the SAME handle are NOT supported
 * in slice 1 — guard with external synchronisation if you need
 * it. The thread-local last-error pair behaves like POSIX errno.
 */

#ifndef NET_DECK_H
#define NET_DECK_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* =========================================================================
 * Status codes
 * ========================================================================= */

#define NET_DECK_OK                      0
#define NET_DECK_ERR_NULL               -1
#define NET_DECK_ERR_CALL_FAILED        -2
#define NET_DECK_ERR_INVALID_ARG        -3
#define NET_DECK_ERR_ALREADY_SHUTDOWN   -4
#define NET_DECK_ERR_END_OF_STREAM      -5

/* =========================================================================
 * ChainCommit event-kind discriminator
 *
 * Every successful `net_deck_admin_*` call writes a
 * `NetDeckChainCommit`. The `event_kind` field is one of these
 * constants — keep the numbering stable across the FFI boundary.
 * ========================================================================= */

#define NET_DECK_EVENT_KIND_UNKNOWN              0
#define NET_DECK_EVENT_KIND_DRAIN                1
#define NET_DECK_EVENT_KIND_ENTER_MAINTENANCE    2
#define NET_DECK_EVENT_KIND_EXIT_MAINTENANCE     3
#define NET_DECK_EVENT_KIND_CORDON               4
#define NET_DECK_EVENT_KIND_UNCORDON             5
#define NET_DECK_EVENT_KIND_DROP_REPLICAS        6
#define NET_DECK_EVENT_KIND_INVALIDATE_PLACEMENT 7
#define NET_DECK_EVENT_KIND_RESTART_ALL_DAEMONS  8
#define NET_DECK_EVENT_KIND_CLEAR_AVOID_LIST     9

/* =========================================================================
 * Wire-form structs
 * ========================================================================= */

typedef struct {
    unsigned int healthy;
    unsigned int degraded;
    unsigned int unreachable;
    unsigned int unknown;
} NetDeckPeerCounts;

typedef struct {
    unsigned int running;
    unsigned int starting;
    unsigned int stopping;
    unsigned int stopped;
    unsigned int backing_off;
    unsigned int crash_looping;
} NetDeckDaemonCounts;

/* Rolled-up cluster summary. `freeze_remaining_present == 0`
 * means no cluster-wide freeze is active; otherwise
 * `freeze_remaining_ms` is the remaining TTL in milliseconds.
 * `local_maintenance_active` is 1 iff this node's local
 * maintenance state is not `Active`. */
typedef struct {
    NetDeckPeerCounts peers;
    NetDeckDaemonCounts daemons;
    unsigned int replica_chains;
    unsigned int avoid_list_entries;
    unsigned int recently_emitted_count;
    unsigned int recent_failure_count;
    unsigned int admin_audit_ring_depth;
    int freeze_remaining_present;
    uint64_t freeze_remaining_ms;
    int local_maintenance_active;
} NetDeckStatusSummary;

/* ChainCommit returned by every admin commit. */
typedef struct {
    uint64_t commit_id;
    uint64_t operator_id;
    int event_kind;
    uint64_t committed_at_ms;
} NetDeckChainCommit;

/* =========================================================================
 * Opaque handle types
 * ========================================================================= */

typedef struct NetDeckClient               NetDeckClient;
typedef struct NetDeckSnapshotStream       NetDeckSnapshotStream;
typedef struct NetDeckStatusSummaryStream  NetDeckStatusSummaryStream;

/* =========================================================================
 * Client lifecycle
 * ========================================================================= */

/* Construct a deck client owning a private supervisor runtime.
 * Every config field accepts 0 to pick the substrate default:
 *
 *   this_node                — supervisor's local node id.
 *   tick_interval_ms         — reconcile cadence.
 *   event_queue_capacity     — supervisor event-source mpsc cap.
 *   action_queue_capacity    — executor mpsc cap.
 *   snapshot_poll_interval_ms — DeckClientConfig poll cadence.
 *   ice_signature_threshold  — DeckClientConfig ICE M-of-N
 *                              (default 1, raised per-cluster).
 *
 * `operator_seed_ptr` must point to exactly 32 bytes of ed25519
 * seed material. The substrate derives the operator id as the
 * keypair's origin hash.
 *
 * On success writes a heap-allocated handle to `*out` and returns
 * NET_DECK_OK. Caller MUST free via `net_deck_client_free`. */
int net_deck_client_new(
    uint64_t this_node,
    uint64_t tick_interval_ms,
    size_t event_queue_capacity,
    size_t action_queue_capacity,
    uint64_t snapshot_poll_interval_ms,
    size_t ice_signature_threshold,
    const uint8_t* operator_seed_ptr,
    NetDeckClient** out
);

/* Free a deck client. Tears down the private supervisor runtime.
 * Idempotent on NULL. */
void net_deck_client_free(NetDeckClient* client);

/* Operator identifier bound to this client. Returns 0 on NULL. */
uint64_t net_deck_client_operator_id(const NetDeckClient* client);

/* =========================================================================
 * One-shot reads
 * ========================================================================= */

/* One-shot read of the latest `MeshOsSnapshot` as a heap-
 * allocated JSON string. Caller frees via
 * `net_deck_free_string`. Returns NULL on error (last-error pair
 * populated). */
char* net_deck_status(const NetDeckClient* client);

/* One-shot read of the rolled-up status summary. Writes the
 * typed struct to `*out`. */
int net_deck_status_summary(
    const NetDeckClient* client,
    NetDeckStatusSummary* out
);

/* Free a heap-allocated C string returned by this crate (e.g.
 * `net_deck_status`, `net_deck_snapshot_stream_next`). Idempotent
 * on NULL. */
void net_deck_free_string(char* s);

/* =========================================================================
 * AdminCommands × 9
 *
 * Every `net_deck_admin_*` commits to the substrate's admin chain
 * and writes a `NetDeckChainCommit` to `*out` on success.
 * ========================================================================= */

int net_deck_admin_drain(
    const NetDeckClient* client,
    uint64_t node,
    uint64_t drain_for_ms,
    NetDeckChainCommit* out
);

/* `has_drain_for == 0` uses the substrate default deadline;
 * non-zero honors `drain_for_ms`. */
int net_deck_admin_enter_maintenance(
    const NetDeckClient* client,
    uint64_t node,
    uint64_t drain_for_ms,
    int has_drain_for,
    NetDeckChainCommit* out
);

int net_deck_admin_exit_maintenance(
    const NetDeckClient* client,
    uint64_t node,
    NetDeckChainCommit* out
);

int net_deck_admin_cordon(
    const NetDeckClient* client,
    uint64_t node,
    NetDeckChainCommit* out
);

int net_deck_admin_uncordon(
    const NetDeckClient* client,
    uint64_t node,
    NetDeckChainCommit* out
);

/* `chains_ptr` may be NULL when `chains_len == 0`. */
int net_deck_admin_drop_replicas(
    const NetDeckClient* client,
    uint64_t node,
    const uint64_t* chains_ptr,
    size_t chains_len,
    NetDeckChainCommit* out
);

int net_deck_admin_invalidate_placement(
    const NetDeckClient* client,
    uint64_t node,
    NetDeckChainCommit* out
);

int net_deck_admin_restart_all_daemons(
    const NetDeckClient* client,
    uint64_t node,
    NetDeckChainCommit* out
);

int net_deck_admin_clear_avoid_list(
    const NetDeckClient* client,
    uint64_t node,
    NetDeckChainCommit* out
);

/* =========================================================================
 * Streams
 * ========================================================================= */

/* Subscribe to the live snapshot stream. */
int net_deck_subscribe_snapshots(
    const NetDeckClient* client,
    NetDeckSnapshotStream** out
);

/* Wait up to `timeout_ms` for the next snapshot. On success writes
 * a heap-allocated JSON string to `*out` (caller frees via
 * `net_deck_free_string`) and returns NET_DECK_OK. On timeout
 * returns NET_DECK_OK with `*out = NULL`. On stream end returns
 * NET_DECK_ERR_END_OF_STREAM. Pass `0` for an unbounded wait. */
int net_deck_snapshot_stream_next(
    NetDeckSnapshotStream* stream,
    uint64_t timeout_ms,
    char** out
);

/* Close + free a snapshot stream. Idempotent on NULL. */
void net_deck_snapshot_stream_free(NetDeckSnapshotStream* stream);

/* Subscribe to the live status-summary stream. */
int net_deck_subscribe_status_summaries(
    const NetDeckClient* client,
    NetDeckStatusSummaryStream** out
);

/* Wait up to `timeout_ms` for the next status summary. On success
 * writes the typed struct to `*out` and `*has_item_out = 1`. On
 * timeout writes `*has_item_out = 0`. On stream end returns
 * NET_DECK_ERR_END_OF_STREAM. */
int net_deck_status_summary_stream_next(
    NetDeckStatusSummaryStream* stream,
    uint64_t timeout_ms,
    NetDeckStatusSummary* out,
    int* has_item_out
);

void net_deck_status_summary_stream_free(NetDeckStatusSummaryStream* stream);

/* =========================================================================
 * Last-error trio (thread-local)
 * ========================================================================= */

const char* net_deck_last_error_message(void);
const char* net_deck_last_error_kind(void);
void net_deck_clear_last_error(void);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* NET_DECK_H */
