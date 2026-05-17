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
 * # Scope (slice 3)
 *
 * Client lifecycle, all 9 `AdminCommands` methods, one-shot
 * `status` / `status_summary`, snapshot + status-summary
 * streams, the audit-query fluent builder + audit stream, log
 * stream with filter, failure stream, and the **ICE break-glass
 * surface** (7 factories + `simulate()` → `commit(signatures)`
 * typestate enforced by two distinct opaque pointer types).
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
 * every FFI entry point that calls into the substrate; instead
 * of unwinding across the C ABI (UB), the call returns the
 * appropriate error status and populates the last-error pair
 * with kind `"runtime_panic"`. Trivial setters / getters that
 * only tag a pointer and assign a field skip the trap — they
 * have no panic surface, so wrapping would only add `catch_unwind`
 * overhead.
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
 * Seed material hygiene. The cdylib zeroizes the transient
 * stack copy it makes of `operator_seed_ptr` before returning,
 * so the FFI shim does not itself become a long-lived window
 * onto the seed. However: (a) the caller's buffer is NOT
 * touched — callers that treat seeds as sensitive should
 * `explicit_bzero` (or equivalent) their own copy after this
 * call returns; and (b) the substrate's `EntityKeypair` holds
 * its own internal copy for the life of the client handle.
 * Free the client when done to release that copy.
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
 * Slice 2 — log levels + LogFilter
 *
 * Constants match the substrate's `LogLevel` enum.
 * ========================================================================= */

#define NET_DECK_LOG_TRACE 0
#define NET_DECK_LOG_DEBUG 1
#define NET_DECK_LOG_INFO  2
#define NET_DECK_LOG_WARN  3
#define NET_DECK_LOG_ERROR 4

/* Optional fields for filtering the log stream. Each `_present`
 * flag guards the matching scalar. Pass NULL to
 * `net_deck_subscribe_logs` to match every record. */
typedef struct {
    int min_level_present;
    int min_level;
    int daemon_id_present;
    uint64_t daemon_id;
    int node_id_present;
    uint64_t node_id;
    int since_seq_present;
    uint64_t since_seq;
} NetDeckLogFilter;

/* =========================================================================
 * Slice 2 — Log + Failure record wire forms
 *
 * String fields are heap-allocated by the cdylib. Caller MUST
 * call the matching `_drop` function to release them when done
 * with the record. The `_drop` calls are idempotent on records
 * whose string fields are NULL (e.g., a zero-initialized struct
 * returned from `_next` on timeout).
 * ========================================================================= */

typedef struct {
    uint64_t seq;
    uint64_t ts_ms;
    int level;
    int daemon_id_present;
    uint64_t daemon_id;
    int node_id_present;
    uint64_t node_id;
    char* message;
} NetDeckLogRecord;

void net_deck_log_record_drop(NetDeckLogRecord* record);

typedef struct {
    uint64_t seq;
    char* source;
    char* reason;
    uint64_t recorded_at_ms;
} NetDeckFailureRecord;

void net_deck_failure_record_drop(NetDeckFailureRecord* record);

/* =========================================================================
 * Slice 2 — Log + Failure streams
 * ========================================================================= */

typedef struct NetDeckLogStream      NetDeckLogStream;
typedef struct NetDeckFailureStream  NetDeckFailureStream;

/* Subscribe to the log ring. `filter` may be NULL — matches
 * every record. */
int net_deck_subscribe_logs(
    const NetDeckClient* client,
    const NetDeckLogFilter* filter,
    NetDeckLogStream** out
);

/* Wait up to `timeout_ms` for the next log record. On success
 * writes the record (caller frees the message via
 * `net_deck_log_record_drop`) and sets `*has_item_out = 1`. On
 * timeout sets `*has_item_out = 0` and returns OK. On stream end
 * returns NET_DECK_ERR_END_OF_STREAM. Pass `0` for an unbounded
 * wait. */
int net_deck_log_stream_next(
    NetDeckLogStream* stream,
    uint64_t timeout_ms,
    NetDeckLogRecord* out,
    int* has_item_out
);

void net_deck_log_stream_free(NetDeckLogStream* stream);

/* Subscribe to the failure ring starting at `since_seq + 1`. */
int net_deck_subscribe_failures(
    const NetDeckClient* client,
    uint64_t since_seq,
    NetDeckFailureStream** out
);

int net_deck_failure_stream_next(
    NetDeckFailureStream* stream,
    uint64_t timeout_ms,
    NetDeckFailureRecord* out,
    int* has_item_out
);

void net_deck_failure_stream_free(NetDeckFailureStream* stream);

/* =========================================================================
 * Slice 2 — AuditQuery fluent builder + AuditStream
 *
 * Build a query via the freestanding builder, then call
 * `_collect` (eager list of JSON CStrings) or `_stream` (sync
 * iterator). Pass the parent `NetDeckClient` on each terminal
 * call — the builder itself doesn't reference the client.
 * ========================================================================= */

typedef struct NetDeckAuditQuery  NetDeckAuditQuery;
typedef struct NetDeckAuditStream NetDeckAuditStream;

/* Construct a freestanding audit query builder. */
int net_deck_audit_query_new(NetDeckAuditQuery** out);

/* Free the builder. Idempotent on NULL. */
void net_deck_audit_query_free(NetDeckAuditQuery* query);

/* Filter setters. Each returns NET_DECK_OK or
 * NET_DECK_ERR_NULL on a NULL builder. */
int net_deck_audit_query_recent(NetDeckAuditQuery* query, size_t limit);
int net_deck_audit_query_by_operator(NetDeckAuditQuery* query, uint64_t operator_id);
int net_deck_audit_query_between(
    NetDeckAuditQuery* query, uint64_t start_ms, uint64_t end_ms
);
int net_deck_audit_query_force_only(NetDeckAuditQuery* query);
int net_deck_audit_query_since(NetDeckAuditQuery* query, uint64_t seq);

/* Collect audit records as a heap-allocated array of JSON
 * CStrings. Writes the count to `*count_out` and the array
 * pointer to `*records_out`. Caller frees via
 * `net_deck_audit_records_free(records, count)`. */
int net_deck_audit_query_collect(
    const NetDeckAuditQuery* query,
    const NetDeckClient* client,
    char*** records_out,
    size_t* count_out
);

/* Free the records array returned by `_collect`. Frees each
 * JSON CString + the outer array. Idempotent on NULL. */
void net_deck_audit_records_free(char** records, size_t count);

/* Open a sync audit stream. */
int net_deck_audit_query_stream(
    const NetDeckAuditQuery* query,
    const NetDeckClient* client,
    NetDeckAuditStream** out
);

/* Wait up to `timeout_ms` for the next audit record. On success
 * writes a heap-allocated JSON CString to `*out` (caller frees
 * via `net_deck_free_string`). On timeout writes NULL + returns
 * NET_DECK_OK. On stream end returns NET_DECK_ERR_END_OF_STREAM.
 *
 * `timeout_ms == 0` waits indefinitely (consistent with the
 * snapshot / log / failure streams). With `0`, the only OK +
 * NULL return path is unreachable — every wakeup either yields
 * a record (Ok) or signals stream-end (END_OF_STREAM). */
int net_deck_audit_stream_next(
    NetDeckAuditStream* stream,
    uint64_t timeout_ms,
    char** out
);

void net_deck_audit_stream_free(NetDeckAuditStream* stream);

/* =========================================================================
 * Slice 3 — ICE break-glass surface
 *
 * Typestate enforced at the C boundary via two distinct opaque
 * handle types:
 *
 *   NetDeckIceProposal           — pre-simulation. NO commit fn.
 *   NetDeckSimulatedIceProposal  — returned from `_simulate`;
 *                                  the only handle commit accepts.
 *
 * Lifecycle:
 *
 *   1. Call any of the 7 factories (`net_deck_ice_freeze_cluster`
 *      / `_flush_avoid_lists` / `_force_evict_replica` /
 *      `_force_restart_daemon` / `_force_cutover` /
 *      `_kill_migration` / `_thaw_cluster`) → `NetDeckIceProposal*`.
 *   2. Call `net_deck_ice_proposal_simulate(proposal, client, &simulated)`
 *      to consume the proposal + run the substrate simulator.
 *   3. Optionally read `net_deck_simulated_blast_radius`
 *      (heap-allocated JSON CString, free via
 *      `net_deck_free_string`) or write `_blast_hash` into a
 *      32-byte caller buffer.
 *   4. Call `net_deck_simulated_commit(simulated, client,
 *      sigs_ptr, sigs_count, &commit)` with at least
 *      `ice_signature_threshold` signatures.
 *   5. Free both handles via `net_deck_ice_proposal_free` and
 *      `net_deck_simulated_free`. Both are idempotent on NULL.
 *
 * `force_drain` is substrate-deferred — not present in this
 * slice.
 * ========================================================================= */

/* AvoidScope kind discriminator. */
#define NET_DECK_AVOID_SCOPE_GLOBAL   0
#define NET_DECK_AVOID_SCOPE_LOCAL    1
#define NET_DECK_AVOID_SCOPE_ON_PEER  2

/* `OperatorSignature` wire form. `signature_ptr` MUST point to
 * exactly 64 ed25519 signature bytes; `signature_len` MUST be 64.
 * Substrate verifier rejects malformed sigs with kind
 * `signature_invalid`. */
typedef struct {
    uint64_t operator_id;
    const uint8_t* signature_ptr;
    size_t signature_len;
} NetDeckOperatorSignature;

typedef struct NetDeckIceProposal           NetDeckIceProposal;
typedef struct NetDeckSimulatedIceProposal  NetDeckSimulatedIceProposal;

/* Factories — all 7 return NET_DECK_OK + write the handle to
 * `*out` on success. Caller MUST free via
 * `net_deck_ice_proposal_free`. */
int net_deck_ice_freeze_cluster(
    const NetDeckClient* client,
    uint64_t ttl_ms,
    NetDeckIceProposal** out
);

/* `scope_kind` is one of NET_DECK_AVOID_SCOPE_*. `scope_node`
 * is consulted when kind=LOCAL; `scope_peer` when kind=ON_PEER.
 * Other fields are ignored for those variants. */
int net_deck_ice_flush_avoid_lists(
    const NetDeckClient* client,
    int scope_kind,
    uint64_t scope_node,
    uint64_t scope_peer,
    NetDeckIceProposal** out
);

int net_deck_ice_force_evict_replica(
    const NetDeckClient* client,
    uint64_t chain,
    uint64_t victim,
    NetDeckIceProposal** out
);

/* `name_ptr` / `name_len` is the daemon's `MeshDaemon::name()`
 * (UTF-8, NOT NUL-terminated). */
int net_deck_ice_force_restart_daemon(
    const NetDeckClient* client,
    uint64_t id,
    const char* name_ptr,
    size_t name_len,
    NetDeckIceProposal** out
);

int net_deck_ice_force_cutover(
    const NetDeckClient* client,
    uint64_t chain,
    uint64_t target,
    NetDeckIceProposal** out
);

int net_deck_ice_kill_migration(
    const NetDeckClient* client,
    uint64_t migration,
    NetDeckIceProposal** out
);

int net_deck_ice_thaw_cluster(
    const NetDeckClient* client,
    NetDeckIceProposal** out
);

/* Read the proposal's pinned `issued_at_ms` stamp. Returns 0
 * on NULL. */
uint64_t net_deck_ice_proposal_issued_at_ms(
    const NetDeckIceProposal* proposal
);

/* Free a freestanding IceProposal. Idempotent on NULL. Calling
 * after a successful `_simulate` is fine — `_simulate`
 * consumes the inner state; this only frees the empty husk. */
void net_deck_ice_proposal_free(NetDeckIceProposal* proposal);

/* Consume the proposal and run the substrate simulator. On
 * success writes a `*NetDeckSimulatedIceProposal` to `*out`
 * (caller frees via `net_deck_simulated_free`). Already-
 * simulated proposals return NET_DECK_ERR_CALL_FAILED with kind
 * `already_simulated`. */
int net_deck_ice_proposal_simulate(
    NetDeckIceProposal* proposal,
    const NetDeckClient* client,
    NetDeckSimulatedIceProposal** out
);

/* Read the pinned `issued_at_ms` stamp from the simulated form. */
uint64_t net_deck_simulated_issued_at_ms(
    const NetDeckSimulatedIceProposal* simulated
);

/* Return the blast radius as a heap-allocated JSON CString.
 * Caller frees via `net_deck_free_string`. Returns NULL on a
 * NULL handle (last-error populated). */
char* net_deck_simulated_blast_radius(
    const NetDeckSimulatedIceProposal* simulated
);

/* Write the 32-byte Blake3 digest of the blast radius into
 * `out_buf`. `out_buf` MUST point to at least 32 writable
 * bytes. Signers must cover this exact hash. */
int net_deck_simulated_blast_hash(
    const NetDeckSimulatedIceProposal* simulated,
    uint8_t* out_buf
);

/* Commit the simulated proposal with the supplied operator
 * signatures. Consumes the inner state — subsequent calls
 * return NET_DECK_ERR_CALL_FAILED with kind `already_committed`.
 * `sigs_ptr` may be NULL when `sigs_count == 0`. Substrate-side
 * the SDK gate rejects sub-threshold bundles with kind
 * `insufficient_signatures`. */
int net_deck_simulated_commit(
    NetDeckSimulatedIceProposal* simulated,
    const NetDeckClient* client,
    const NetDeckOperatorSignature* sigs_ptr,
    size_t sigs_count,
    NetDeckChainCommit* out
);

/* Free a simulated proposal handle. Idempotent on NULL. */
void net_deck_simulated_free(NetDeckSimulatedIceProposal* simulated);

/* Heap-allocate + return the deterministic ICE signing payload
 * bytes (`ICE_SIGNING_DOMAIN || issued_at_ms (LE u64) ||
 * blast_hash (32) || postcard(action)`). On success writes the
 * buffer pointer to `*out_ptr` and the byte count to `*out_len`;
 * caller MUST release the buffer via
 * `net_deck_signing_payload_free(*out_ptr, *out_len)`. Returns
 * `NET_DECK_ERR_CALL_FAILED` with kind `already_committed` if
 * the proposal has been consumed by `net_deck_simulated_commit`. */
int net_deck_simulated_signing_payload(
    const NetDeckSimulatedIceProposal* simulated,
    uint8_t** out_ptr,
    size_t* out_len
);

/* Free a buffer returned by `net_deck_simulated_signing_payload`.
 * Idempotent on NULL / zero length. */
void net_deck_signing_payload_free(uint8_t* ptr, size_t len);

/* =========================================================================
 * Operator identity opaque handle
 * ========================================================================= */

typedef struct NetDeckOperatorIdentity NetDeckOperatorIdentity;

/* Generate a fresh ed25519 keypair + operator identity. Caller
 * frees via `net_deck_operator_identity_free`. */
NetDeckOperatorIdentity* net_deck_operator_identity_generate(void);

/* Load from a 32-byte ed25519 seed. Writes the handle to `*out`.
 *
 * Seed material hygiene: see the `net_deck_client_new` note —
 * the cdylib zeroizes the transient stack copy it makes of
 * `seed_ptr`; the caller's source buffer is NOT touched, and
 * the substrate's keypair holds an internal copy for the
 * identity handle's lifetime. */
int net_deck_operator_identity_from_seed(
    const uint8_t* seed_ptr,
    NetDeckOperatorIdentity** out
);

/* Operator id (the keypair's origin hash). Returns 0 on NULL. */
uint64_t net_deck_operator_identity_operator_id(
    const NetDeckOperatorIdentity* identity
);

/* Write the 32-byte ed25519 public key into `out_buf`. */
int net_deck_operator_identity_public_key(
    const NetDeckOperatorIdentity* identity,
    uint8_t* out_buf
);

/* Sign a simulated ICE proposal. On success writes the operator
 * id to `*out_operator_id` and the 64-byte signature into
 * `out_signature`. Returns kind `already_committed` if the
 * proposal has been consumed by `net_deck_simulated_commit`. */
int net_deck_operator_identity_sign_proposal(
    const NetDeckOperatorIdentity* identity,
    const NetDeckSimulatedIceProposal* simulated,
    uint64_t* out_operator_id,
    uint8_t* out_signature
);

/* Sign raw payload bytes. Useful for offline / cross-deck
 * signing flows where the signing payload is exchanged
 * out-of-band (see `net_deck_simulated_signing_payload`). */
int net_deck_operator_identity_sign_payload(
    const NetDeckOperatorIdentity* identity,
    const uint8_t* payload_ptr,
    size_t payload_len,
    uint64_t* out_operator_id,
    uint8_t* out_signature
);

/* Free an operator identity. Idempotent on NULL. */
void net_deck_operator_identity_free(NetDeckOperatorIdentity* identity);

/* =========================================================================
 * Operator registry opaque handle
 *
 * Authoring tool + offline-friendly verifier for operator
 * signature bundles. Mutations are thread-safe at the cdylib
 * layer (internal mutex).
 * ========================================================================= */

typedef struct NetDeckOperatorRegistry NetDeckOperatorRegistry;

/* Create an empty operator registry. */
NetDeckOperatorRegistry* net_deck_operator_registry_new(void);

/* Insert a 32-byte ed25519 public key under `operator_id`. */
int net_deck_operator_registry_insert(
    NetDeckOperatorRegistry* registry,
    uint64_t operator_id,
    const uint8_t* public_key
);

/* Register an `OperatorIdentity`'s public key under its derived
 * operator id (the keypair's origin hash). */
int net_deck_operator_registry_register(
    NetDeckOperatorRegistry* registry,
    const NetDeckOperatorIdentity* identity
);

/* Returns 1 iff `operator_id` is registered, 0 otherwise, -1 on
 * NULL pointer. */
int net_deck_operator_registry_contains(
    const NetDeckOperatorRegistry* registry,
    uint64_t operator_id
);

/* Number of registered operators. Returns 0 on NULL. */
size_t net_deck_operator_registry_len(
    const NetDeckOperatorRegistry* registry
);

/* Verify a single signature over `payload`. On failure sets the
 * thread-local last-error kind to the substrate's stable
 * discriminator (`not_authorized`, `signature_invalid`). */
int net_deck_operator_registry_verify(
    const NetDeckOperatorRegistry* registry,
    const NetDeckOperatorSignature* signature,
    const uint8_t* payload_ptr,
    size_t payload_len
);

/* Verify every signature in the bundle and confirm at least
 * `threshold` *distinct* operator ids signed `payload`. The
 * distinct-operator dedup gate is the load-bearing M-of-N
 * guarantee. */
int net_deck_operator_registry_verify_bundle(
    const NetDeckOperatorRegistry* registry,
    const NetDeckOperatorSignature* sigs_ptr,
    size_t sigs_count,
    const uint8_t* payload_ptr,
    size_t payload_len,
    size_t threshold
);

/* Free an operator registry. Idempotent on NULL. */
void net_deck_operator_registry_free(NetDeckOperatorRegistry* registry);

/* =========================================================================
 * Admin verifier opaque handle
 *
 * Wraps a snapshotted `OperatorRegistry` with the cluster's
 * policy knobs (threshold, freshness window, future-skew
 * tolerance, ICE cooldown). Useful for offline unit testing of
 * operator-policy decisions. Constructors snapshot the
 * registry at build time — rebuild after every policy change.
 * ========================================================================= */

typedef struct NetDeckAdminVerifier NetDeckAdminVerifier;

/* Substrate defaults: 300s freshness, 30s future-skew, 300s ICE
 * cooldown. `threshold = 0` is clamped to `1`. Returns NULL on
 * NULL registry. */
NetDeckAdminVerifier* net_deck_admin_verifier_new(
    const NetDeckOperatorRegistry* registry,
    size_t threshold
);

/* Explicit freshness + future-skew windows, default ICE cooldown. */
NetDeckAdminVerifier* net_deck_admin_verifier_with_freshness(
    const NetDeckOperatorRegistry* registry,
    size_t threshold,
    uint64_t freshness_window_ms,
    uint64_t future_skew_ms
);

/* Every policy knob explicit. Primarily for tests that need a
 * short cooldown window. */
NetDeckAdminVerifier* net_deck_admin_verifier_with_full_policy(
    const NetDeckOperatorRegistry* registry,
    size_t threshold,
    uint64_t freshness_window_ms,
    uint64_t future_skew_ms,
    uint64_t ice_cooldown_ms
);

size_t net_deck_admin_verifier_threshold(const NetDeckAdminVerifier* verifier);
uint64_t net_deck_admin_verifier_freshness_window_ms(const NetDeckAdminVerifier* verifier);
uint64_t net_deck_admin_verifier_future_skew_ms(const NetDeckAdminVerifier* verifier);
uint64_t net_deck_admin_verifier_ice_cooldown_ms(const NetDeckAdminVerifier* verifier);

/* Free an admin verifier. Idempotent on NULL. */
void net_deck_admin_verifier_free(NetDeckAdminVerifier* verifier);

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
