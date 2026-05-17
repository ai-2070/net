// Package net — Deck operator-side SDK consumer wrapper for the
// C ABI exported by `net::ffi::deck` (compiled as `libnet_deck`).
//
// # Scope (slice 1)
//
// Client lifecycle, all 9 AdminCommands methods, one-shot status /
// status_summary, snapshot + status-summary streams. Audit / logs /
// failures land in slice 2; ICE in slice 3.
//
// # Operator-only mode
//
// `NewDeckClient` constructs a private supervisor runtime inside
// the cdylib. The caller supplies only the operator seed +
// supervisor config; the cdylib wraps the substrate's runtime
// end-to-end. Composing against an externally-managed
// `NetMeshOsSdk` lands in slice 2.
//
// # Memory model
//
// Every Rust object that crosses the FFI is wrapped in a
// `runtime.SetFinalizer`-protected Go handle. Manual `.Free()`
// methods are exposed for callers that want deterministic
// teardown.
//
// # Error model
//
// FFI functions return `c_int` status codes:
//
//   - 0 (`NET_DECK_OK`)                  — success.
//   - -1 (`NET_DECK_ERR_NULL`)          — null handle.
//   - -2 (`NET_DECK_ERR_CALL_FAILED`)   — substrate-side failure.
//   - -3 (`NET_DECK_ERR_INVALID_ARG`)   — null pointer / bad input.
//   - -4 (`NET_DECK_ERR_ALREADY_SHUTDOWN`) — already freed.
//   - -5 (`NET_DECK_ERR_END_OF_STREAM`) — stream drained or closed.
//
// Detail flows through a thread-local last-error pair populated
// on every non-OK status. `DeckSdkError` carries both the `kind`
// discriminator (`"register_failed"`, `"queue_full"`,
// `"already_shutdown"`, `"snapshot_serialize_failed"`,
// `"invalid_argument"`, `"runtime_panic"`, …) and the message.
package net

/*
#include <stdint.h>
#include <stdlib.h>

// Forward-declared opaque handles from `libnet_deck`.
typedef struct NetDeckClient NetDeckClient;
typedef struct NetDeckSnapshotStream NetDeckSnapshotStream;
typedef struct NetDeckStatusSummaryStream NetDeckStatusSummaryStream;

// Status codes.
#define NET_DECK_OK 0
#define NET_DECK_ERR_NULL -1
#define NET_DECK_ERR_CALL_FAILED -2
#define NET_DECK_ERR_INVALID_ARG -3
#define NET_DECK_ERR_ALREADY_SHUTDOWN -4
#define NET_DECK_ERR_END_OF_STREAM -5

// Event-kind discriminator for ChainCommit.event_kind.
#define NET_DECK_EVENT_KIND_UNKNOWN 0
#define NET_DECK_EVENT_KIND_DRAIN 1
#define NET_DECK_EVENT_KIND_ENTER_MAINTENANCE 2
#define NET_DECK_EVENT_KIND_EXIT_MAINTENANCE 3
#define NET_DECK_EVENT_KIND_CORDON 4
#define NET_DECK_EVENT_KIND_UNCORDON 5
#define NET_DECK_EVENT_KIND_DROP_REPLICAS 6
#define NET_DECK_EVENT_KIND_INVALIDATE_PLACEMENT 7
#define NET_DECK_EVENT_KIND_RESTART_ALL_DAEMONS 8
#define NET_DECK_EVENT_KIND_CLEAR_AVOID_LIST 9

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

typedef struct {
    uint64_t commit_id;
    uint64_t operator_id;
    int event_kind;
    uint64_t committed_at_ms;
} NetDeckChainCommit;

// Client lifecycle.
extern int net_deck_client_new(
    uint64_t this_node,
    uint64_t tick_interval_ms,
    size_t event_queue_capacity,
    size_t action_queue_capacity,
    uint64_t snapshot_poll_interval_ms,
    size_t ice_signature_threshold,
    const uint8_t* operator_seed_ptr,
    NetDeckClient** out
);
extern void net_deck_client_free(NetDeckClient* client);
extern uint64_t net_deck_client_operator_id(const NetDeckClient* client);

// One-shot reads.
extern char* net_deck_status(const NetDeckClient* client);
extern int net_deck_status_summary(const NetDeckClient* client, NetDeckStatusSummary* out);

// String free helper for `net_deck_status` returns.
extern void net_deck_free_string(char* s);

// AdminCommands.
extern int net_deck_admin_drain(const NetDeckClient* client, uint64_t node, uint64_t drain_for_ms, NetDeckChainCommit* out);
extern int net_deck_admin_enter_maintenance(const NetDeckClient* client, uint64_t node, uint64_t drain_for_ms, int has_drain_for, NetDeckChainCommit* out);
extern int net_deck_admin_exit_maintenance(const NetDeckClient* client, uint64_t node, NetDeckChainCommit* out);
extern int net_deck_admin_cordon(const NetDeckClient* client, uint64_t node, NetDeckChainCommit* out);
extern int net_deck_admin_uncordon(const NetDeckClient* client, uint64_t node, NetDeckChainCommit* out);
extern int net_deck_admin_drop_replicas(const NetDeckClient* client, uint64_t node, const uint64_t* chains_ptr, size_t chains_len, NetDeckChainCommit* out);
extern int net_deck_admin_invalidate_placement(const NetDeckClient* client, uint64_t node, NetDeckChainCommit* out);
extern int net_deck_admin_restart_all_daemons(const NetDeckClient* client, uint64_t node, NetDeckChainCommit* out);
extern int net_deck_admin_clear_avoid_list(const NetDeckClient* client, uint64_t node, NetDeckChainCommit* out);

// Snapshot stream.
extern int net_deck_subscribe_snapshots(const NetDeckClient* client, NetDeckSnapshotStream** out);
extern int net_deck_snapshot_stream_next(NetDeckSnapshotStream* stream, uint64_t timeout_ms, char** out);
extern void net_deck_snapshot_stream_free(NetDeckSnapshotStream* stream);

// Status summary stream.
extern int net_deck_subscribe_status_summaries(const NetDeckClient* client, NetDeckStatusSummaryStream** out);
extern int net_deck_status_summary_stream_next(NetDeckStatusSummaryStream* stream, uint64_t timeout_ms, NetDeckStatusSummary* out, int* has_item_out);
extern void net_deck_status_summary_stream_free(NetDeckStatusSummaryStream* stream);

// Last-error trio.
extern const char* net_deck_last_error_message(void);
extern const char* net_deck_last_error_kind(void);
extern void net_deck_clear_last_error(void);

// Slice 2 — log levels + LogFilter.
#define NET_DECK_LOG_TRACE 0
#define NET_DECK_LOG_DEBUG 1
#define NET_DECK_LOG_INFO  2
#define NET_DECK_LOG_WARN  3
#define NET_DECK_LOG_ERROR 4

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

// Slice 2 — Log + Failure record wire forms.
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

extern void net_deck_log_record_drop(NetDeckLogRecord* record);

typedef struct {
    uint64_t seq;
    char* source;
    char* reason;
    uint64_t recorded_at_ms;
} NetDeckFailureRecord;

extern void net_deck_failure_record_drop(NetDeckFailureRecord* record);

// Slice 2 — Log + Failure streams.
typedef struct NetDeckLogStream NetDeckLogStream;
typedef struct NetDeckFailureStream NetDeckFailureStream;

extern int net_deck_subscribe_logs(
    const NetDeckClient* client,
    const NetDeckLogFilter* filter,
    NetDeckLogStream** out
);
extern int net_deck_log_stream_next(
    NetDeckLogStream* stream,
    uint64_t timeout_ms,
    NetDeckLogRecord* out,
    int* has_item_out
);
extern void net_deck_log_stream_free(NetDeckLogStream* stream);

extern int net_deck_subscribe_failures(
    const NetDeckClient* client,
    uint64_t since_seq,
    NetDeckFailureStream** out
);
extern int net_deck_failure_stream_next(
    NetDeckFailureStream* stream,
    uint64_t timeout_ms,
    NetDeckFailureRecord* out,
    int* has_item_out
);
extern void net_deck_failure_stream_free(NetDeckFailureStream* stream);

// Slice 2 — AuditQuery + AuditStream.
typedef struct NetDeckAuditQuery NetDeckAuditQuery;
typedef struct NetDeckAuditStream NetDeckAuditStream;

extern int net_deck_audit_query_new(NetDeckAuditQuery** out);
extern void net_deck_audit_query_free(NetDeckAuditQuery* query);

extern int net_deck_audit_query_recent(NetDeckAuditQuery* query, size_t limit);
extern int net_deck_audit_query_by_operator(NetDeckAuditQuery* query, uint64_t operator_id);
extern int net_deck_audit_query_between(NetDeckAuditQuery* query, uint64_t start_ms, uint64_t end_ms);
extern int net_deck_audit_query_force_only(NetDeckAuditQuery* query);
extern int net_deck_audit_query_since(NetDeckAuditQuery* query, uint64_t seq);

extern int net_deck_audit_query_collect(
    const NetDeckAuditQuery* query,
    const NetDeckClient* client,
    char*** records_out,
    size_t* count_out
);
extern void net_deck_audit_records_free(char** records, size_t count);

extern int net_deck_audit_query_stream(
    const NetDeckAuditQuery* query,
    const NetDeckClient* client,
    NetDeckAuditStream** out
);
extern int net_deck_audit_stream_next(
    NetDeckAuditStream* stream,
    uint64_t timeout_ms,
    char** out
);
extern void net_deck_audit_stream_free(NetDeckAuditStream* stream);

// Slice 3 — ICE break-glass surface.
#define NET_DECK_AVOID_SCOPE_GLOBAL  0
#define NET_DECK_AVOID_SCOPE_LOCAL   1
#define NET_DECK_AVOID_SCOPE_ON_PEER 2

typedef struct {
    uint64_t operator_id;
    const uint8_t* signature_ptr;
    size_t signature_len;
} NetDeckOperatorSignature;

typedef struct NetDeckIceProposal NetDeckIceProposal;
typedef struct NetDeckSimulatedIceProposal NetDeckSimulatedIceProposal;

extern int net_deck_ice_freeze_cluster(const NetDeckClient* client, uint64_t ttl_ms, NetDeckIceProposal** out);
extern int net_deck_ice_flush_avoid_lists(const NetDeckClient* client, int scope_kind, uint64_t scope_node, uint64_t scope_peer, NetDeckIceProposal** out);
extern int net_deck_ice_force_evict_replica(const NetDeckClient* client, uint64_t chain, uint64_t victim, NetDeckIceProposal** out);
extern int net_deck_ice_force_restart_daemon(const NetDeckClient* client, uint64_t id, const char* name_ptr, size_t name_len, NetDeckIceProposal** out);
extern int net_deck_ice_force_cutover(const NetDeckClient* client, uint64_t chain, uint64_t target, NetDeckIceProposal** out);
extern int net_deck_ice_kill_migration(const NetDeckClient* client, uint64_t migration, NetDeckIceProposal** out);
extern int net_deck_ice_thaw_cluster(const NetDeckClient* client, NetDeckIceProposal** out);

extern uint64_t net_deck_ice_proposal_issued_at_ms(const NetDeckIceProposal* proposal);
extern void net_deck_ice_proposal_free(NetDeckIceProposal* proposal);
extern int net_deck_ice_proposal_simulate(NetDeckIceProposal* proposal, const NetDeckClient* client, NetDeckSimulatedIceProposal** out);

extern uint64_t net_deck_simulated_issued_at_ms(const NetDeckSimulatedIceProposal* simulated);
extern char* net_deck_simulated_blast_radius(const NetDeckSimulatedIceProposal* simulated);
extern int net_deck_simulated_blast_hash(const NetDeckSimulatedIceProposal* simulated, uint8_t* out_buf);
extern int net_deck_simulated_commit(NetDeckSimulatedIceProposal* simulated, const NetDeckClient* client, const NetDeckOperatorSignature* sigs_ptr, size_t sigs_count, NetDeckChainCommit* out);
extern void net_deck_simulated_free(NetDeckSimulatedIceProposal* simulated);
extern int net_deck_simulated_signing_payload(const NetDeckSimulatedIceProposal* simulated, uint8_t** out_ptr, size_t* out_len);
extern void net_deck_signing_payload_free(uint8_t* ptr, size_t len);

typedef struct NetDeckOperatorIdentity NetDeckOperatorIdentity;
typedef struct NetDeckOperatorRegistry NetDeckOperatorRegistry;
typedef struct NetDeckAdminVerifier   NetDeckAdminVerifier;

extern NetDeckOperatorIdentity* net_deck_operator_identity_generate(void);
extern int net_deck_operator_identity_from_seed(const uint8_t* seed_ptr, NetDeckOperatorIdentity** out);
extern uint64_t net_deck_operator_identity_operator_id(const NetDeckOperatorIdentity* identity);
extern int net_deck_operator_identity_public_key(const NetDeckOperatorIdentity* identity, uint8_t* out_buf);
extern int net_deck_operator_identity_sign_proposal(const NetDeckOperatorIdentity* identity, const NetDeckSimulatedIceProposal* simulated, uint64_t* out_operator_id, uint8_t* out_signature);
extern int net_deck_operator_identity_sign_payload(const NetDeckOperatorIdentity* identity, const uint8_t* payload_ptr, size_t payload_len, uint64_t* out_operator_id, uint8_t* out_signature);
extern void net_deck_operator_identity_free(NetDeckOperatorIdentity* identity);

extern NetDeckOperatorRegistry* net_deck_operator_registry_new(void);
extern int net_deck_operator_registry_insert(NetDeckOperatorRegistry* registry, uint64_t operator_id, const uint8_t* public_key);
extern int net_deck_operator_registry_register(NetDeckOperatorRegistry* registry, const NetDeckOperatorIdentity* identity);
extern int net_deck_operator_registry_contains(const NetDeckOperatorRegistry* registry, uint64_t operator_id);
extern size_t net_deck_operator_registry_len(const NetDeckOperatorRegistry* registry);
extern int net_deck_operator_registry_verify(const NetDeckOperatorRegistry* registry, const NetDeckOperatorSignature* signature, const uint8_t* payload_ptr, size_t payload_len);
extern int net_deck_operator_registry_verify_bundle(const NetDeckOperatorRegistry* registry, const NetDeckOperatorSignature* sigs_ptr, size_t sigs_count, const uint8_t* payload_ptr, size_t payload_len, size_t threshold);
extern void net_deck_operator_registry_free(NetDeckOperatorRegistry* registry);

extern NetDeckAdminVerifier* net_deck_admin_verifier_new(const NetDeckOperatorRegistry* registry, size_t threshold);
extern NetDeckAdminVerifier* net_deck_admin_verifier_with_freshness(const NetDeckOperatorRegistry* registry, size_t threshold, uint64_t freshness_window_ms, uint64_t future_skew_ms);
extern NetDeckAdminVerifier* net_deck_admin_verifier_with_full_policy(const NetDeckOperatorRegistry* registry, size_t threshold, uint64_t freshness_window_ms, uint64_t future_skew_ms, uint64_t ice_cooldown_ms);
extern size_t net_deck_admin_verifier_threshold(const NetDeckAdminVerifier* verifier);
extern uint64_t net_deck_admin_verifier_freshness_window_ms(const NetDeckAdminVerifier* verifier);
extern uint64_t net_deck_admin_verifier_future_skew_ms(const NetDeckAdminVerifier* verifier);
extern uint64_t net_deck_admin_verifier_ice_cooldown_ms(const NetDeckAdminVerifier* verifier);
extern void net_deck_admin_verifier_free(NetDeckAdminVerifier* verifier);
*/
import "C"

import (
	"encoding/json"
	"errors"
	"fmt"
	"runtime"
	"unsafe"
)

// =====================================================================
// Errors
// =====================================================================

// ErrDeck is the root discriminator for Deck-side errors.
var ErrDeck = errors.New("deck")
var ErrDeckInvalidArg = errors.New("deck: invalid argument")
var ErrDeckAlreadyShutdown = errors.New("deck: already shutdown")
var ErrDeckCallFailed = errors.New("deck: call failed")
var ErrDeckEndOfStream = errors.New("deck: end of stream")

// DeckSdkError carries the substrate's structured envelope.
type DeckSdkError struct {
	Sentinel error
	Kind     string
	Message  string
}

func (e *DeckSdkError) Error() string {
	if e == nil {
		return "<nil deck error>"
	}
	switch {
	case e.Kind != "" && e.Message != "":
		return fmt.Sprintf("%s (kind=%s): %s", e.Sentinel.Error(), e.Kind, e.Message)
	case e.Kind != "":
		return fmt.Sprintf("%s (kind=%s)", e.Sentinel.Error(), e.Kind)
	case e.Message != "":
		return fmt.Sprintf("%s: %s", e.Sentinel.Error(), e.Message)
	default:
		return e.Sentinel.Error()
	}
}

func (e *DeckSdkError) Unwrap() error {
	if e == nil {
		return nil
	}
	return e.Sentinel
}

func wrapDeckError(sentinel error) *DeckSdkError {
	err := &DeckSdkError{Sentinel: sentinel}
	if msgPtr := C.net_deck_last_error_message(); msgPtr != nil {
		err.Message = C.GoString(msgPtr)
	}
	if kindPtr := C.net_deck_last_error_kind(); kindPtr != nil {
		err.Kind = C.GoString(kindPtr)
	}
	C.net_deck_clear_last_error()
	return err
}

func deckStatusToError(status C.int) error {
	switch status {
	case C.NET_DECK_OK:
		return nil
	case C.NET_DECK_ERR_NULL, C.NET_DECK_ERR_INVALID_ARG:
		return wrapDeckError(ErrDeckInvalidArg)
	case C.NET_DECK_ERR_ALREADY_SHUTDOWN:
		return wrapDeckError(ErrDeckAlreadyShutdown)
	case C.NET_DECK_ERR_END_OF_STREAM:
		return wrapDeckError(ErrDeckEndOfStream)
	default:
		return wrapDeckError(ErrDeckCallFailed)
	}
}

// =====================================================================
// Typed wire form
// =====================================================================

// EventKind discriminates the AdminEvent variant carried by a
// ChainCommit. Constants match the FFI's `NET_DECK_EVENT_KIND_*`.
type EventKind int

const (
	EventKindUnknown             EventKind = 0
	EventKindDrain               EventKind = 1
	EventKindEnterMaintenance    EventKind = 2
	EventKindExitMaintenance     EventKind = 3
	EventKindCordon              EventKind = 4
	EventKindUncordon            EventKind = 5
	EventKindDropReplicas        EventKind = 6
	EventKindInvalidatePlacement EventKind = 7
	EventKindRestartAllDaemons   EventKind = 8
	EventKindClearAvoidList      EventKind = 9
)

func (k EventKind) String() string {
	switch k {
	case EventKindDrain:
		return "drain"
	case EventKindEnterMaintenance:
		return "enter_maintenance"
	case EventKindExitMaintenance:
		return "exit_maintenance"
	case EventKindCordon:
		return "cordon"
	case EventKindUncordon:
		return "uncordon"
	case EventKindDropReplicas:
		return "drop_replicas"
	case EventKindInvalidatePlacement:
		return "invalidate_placement"
	case EventKindRestartAllDaemons:
		return "restart_all_daemons"
	case EventKindClearAvoidList:
		return "clear_avoid_list"
	default:
		return "unknown"
	}
}

// ChainCommit returned by every admin commit.
type ChainCommit struct {
	CommitID      uint64
	OperatorID    uint64
	EventKind     EventKind
	CommittedAtMs uint64
}

func chainCommitFromC(c C.NetDeckChainCommit) ChainCommit {
	return ChainCommit{
		CommitID:      uint64(c.commit_id),
		OperatorID:    uint64(c.operator_id),
		EventKind:     EventKind(c.event_kind),
		CommittedAtMs: uint64(c.committed_at_ms),
	}
}

type PeerCounts struct {
	Healthy     uint32
	Degraded    uint32
	Unreachable uint32
	Unknown     uint32
}

type DaemonCounts struct {
	Running      uint32
	Starting     uint32
	Stopping     uint32
	Stopped      uint32
	BackingOff   uint32
	CrashLooping uint32
}

// DeckStatusSummary mirrors the substrate's StatusSummary.
// `FreezeRemainingMs` is `nil` when no cluster freeze is active.
type DeckStatusSummary struct {
	Peers                  PeerCounts
	Daemons                DaemonCounts
	ReplicaChains          uint32
	AvoidListEntries       uint32
	RecentlyEmittedCount   uint32
	RecentFailureCount     uint32
	AdminAuditRingDepth    uint32
	FreezeRemainingMs      *uint64
	LocalMaintenanceActive bool
}

func summaryFromC(s C.NetDeckStatusSummary) DeckStatusSummary {
	out := DeckStatusSummary{
		Peers: PeerCounts{
			Healthy:     uint32(s.peers.healthy),
			Degraded:    uint32(s.peers.degraded),
			Unreachable: uint32(s.peers.unreachable),
			Unknown:     uint32(s.peers.unknown),
		},
		Daemons: DaemonCounts{
			Running:      uint32(s.daemons.running),
			Starting:     uint32(s.daemons.starting),
			Stopping:     uint32(s.daemons.stopping),
			Stopped:      uint32(s.daemons.stopped),
			BackingOff:   uint32(s.daemons.backing_off),
			CrashLooping: uint32(s.daemons.crash_looping),
		},
		ReplicaChains:          uint32(s.replica_chains),
		AvoidListEntries:       uint32(s.avoid_list_entries),
		RecentlyEmittedCount:   uint32(s.recently_emitted_count),
		RecentFailureCount:     uint32(s.recent_failure_count),
		AdminAuditRingDepth:    uint32(s.admin_audit_ring_depth),
		LocalMaintenanceActive: s.local_maintenance_active != 0,
	}
	if s.freeze_remaining_present != 0 {
		ms := uint64(s.freeze_remaining_ms)
		out.FreezeRemainingMs = &ms
	}
	return out
}

// DeckClientConfig holds the optional supervisor + deck tunables.
// Zero-value fields take the substrate default.
type DeckClientConfig struct {
	ThisNode               uint64
	TickIntervalMs         uint64
	EventQueueCapacity     int
	ActionQueueCapacity    int
	SnapshotPollIntervalMs uint64
	IceSignatureThreshold  int
}

// =====================================================================
// DeckClient
// =====================================================================

// DeckClient is the Go-side handle for the operator SDK.
type DeckClient struct {
	ptr *C.NetDeckClient
}

// NewDeckClient constructs a deck client owning a private MeshOS
// supervisor runtime. `operatorSeed` MUST be exactly 32 bytes.
func NewDeckClient(operatorSeed []byte, cfg DeckClientConfig) (*DeckClient, error) {
	if len(operatorSeed) != 32 {
		return nil, fmt.Errorf("%w: operator seed must be 32 bytes, got %d", ErrDeckInvalidArg, len(operatorSeed))
	}
	var raw *C.NetDeckClient
	status := C.net_deck_client_new(
		C.uint64_t(cfg.ThisNode),
		C.uint64_t(cfg.TickIntervalMs),
		C.size_t(cfg.EventQueueCapacity),
		C.size_t(cfg.ActionQueueCapacity),
		C.uint64_t(cfg.SnapshotPollIntervalMs),
		C.size_t(cfg.IceSignatureThreshold),
		(*C.uint8_t)(unsafe.Pointer(&operatorSeed[0])),
		&raw,
	)
	if err := deckStatusToError(status); err != nil {
		return nil, err
	}
	c := &DeckClient{ptr: raw}
	runtime.SetFinalizer(c, func(c *DeckClient) { c.Free() })
	return c, nil
}

// OperatorID returns the 64-bit operator id derived from the seed.
func (c *DeckClient) OperatorID() uint64 {
	if c == nil || c.ptr == nil {
		return 0
	}
	return uint64(C.net_deck_client_operator_id(c.ptr))
}

// Status reads the latest MeshOsSnapshot, returning it as the
// parsed JSON shape. The substrate's `MeshOsSnapshot` has many
// fields; the slice-1 contract keeps this loose-typed —
// downstream wrappers can decode into substrate-pinned structs.
func (c *DeckClient) Status() (map[string]any, error) {
	if c == nil || c.ptr == nil {
		return nil, ErrDeckInvalidArg
	}
	jsonPtr := C.net_deck_status(c.ptr)
	if jsonPtr == nil {
		return nil, wrapDeckError(ErrDeckCallFailed)
	}
	defer C.net_deck_free_string(jsonPtr)
	raw := C.GoString(jsonPtr)
	var parsed map[string]any
	if err := json.Unmarshal([]byte(raw), &parsed); err != nil {
		return nil, fmt.Errorf("%w: snapshot JSON parse: %v", ErrDeckCallFailed, err)
	}
	return parsed, nil
}

// StatusSummary reads the rolled-up StatusSummary.
func (c *DeckClient) StatusSummary() (DeckStatusSummary, error) {
	if c == nil || c.ptr == nil {
		return DeckStatusSummary{}, ErrDeckInvalidArg
	}
	var out C.NetDeckStatusSummary
	if err := deckStatusToError(C.net_deck_status_summary(c.ptr, &out)); err != nil {
		return DeckStatusSummary{}, err
	}
	return summaryFromC(out), nil
}

// Free releases the deck client (tears down the private
// supervisor runtime). Idempotent.
func (c *DeckClient) Free() {
	if c == nil || c.ptr == nil {
		return
	}
	C.net_deck_client_free(c.ptr)
	c.ptr = nil
	runtime.SetFinalizer(c, nil)
}

// =====================================================================
// AdminCommands — one method per AdminEvent variant.
// =====================================================================

func (c *DeckClient) Drain(node, drainForMs uint64) (ChainCommit, error) {
	if c == nil || c.ptr == nil {
		return ChainCommit{}, ErrDeckInvalidArg
	}
	var out C.NetDeckChainCommit
	if err := deckStatusToError(C.net_deck_admin_drain(c.ptr, C.uint64_t(node), C.uint64_t(drainForMs), &out)); err != nil {
		return ChainCommit{}, err
	}
	return chainCommitFromC(out), nil
}

// EnterMaintenance with optional deadline. Pass `nil` for the
// substrate default.
func (c *DeckClient) EnterMaintenance(node uint64, drainForMs *uint64) (ChainCommit, error) {
	if c == nil || c.ptr == nil {
		return ChainCommit{}, ErrDeckInvalidArg
	}
	var out C.NetDeckChainCommit
	var ms C.uint64_t
	var has C.int
	if drainForMs != nil {
		ms = C.uint64_t(*drainForMs)
		has = 1
	}
	if err := deckStatusToError(C.net_deck_admin_enter_maintenance(c.ptr, C.uint64_t(node), ms, has, &out)); err != nil {
		return ChainCommit{}, err
	}
	return chainCommitFromC(out), nil
}

func (c *DeckClient) ExitMaintenance(node uint64) (ChainCommit, error) {
	if c == nil || c.ptr == nil {
		return ChainCommit{}, ErrDeckInvalidArg
	}
	var out C.NetDeckChainCommit
	if err := deckStatusToError(C.net_deck_admin_exit_maintenance(c.ptr, C.uint64_t(node), &out)); err != nil {
		return ChainCommit{}, err
	}
	return chainCommitFromC(out), nil
}

func (c *DeckClient) Cordon(node uint64) (ChainCommit, error) {
	if c == nil || c.ptr == nil {
		return ChainCommit{}, ErrDeckInvalidArg
	}
	var out C.NetDeckChainCommit
	if err := deckStatusToError(C.net_deck_admin_cordon(c.ptr, C.uint64_t(node), &out)); err != nil {
		return ChainCommit{}, err
	}
	return chainCommitFromC(out), nil
}

func (c *DeckClient) Uncordon(node uint64) (ChainCommit, error) {
	if c == nil || c.ptr == nil {
		return ChainCommit{}, ErrDeckInvalidArg
	}
	var out C.NetDeckChainCommit
	if err := deckStatusToError(C.net_deck_admin_uncordon(c.ptr, C.uint64_t(node), &out)); err != nil {
		return ChainCommit{}, err
	}
	return chainCommitFromC(out), nil
}

func (c *DeckClient) DropReplicas(node uint64, chains []uint64) (ChainCommit, error) {
	if c == nil || c.ptr == nil {
		return ChainCommit{}, ErrDeckInvalidArg
	}
	var out C.NetDeckChainCommit
	var ptr *C.uint64_t
	if len(chains) > 0 {
		ptr = (*C.uint64_t)(unsafe.Pointer(&chains[0]))
	}
	if err := deckStatusToError(C.net_deck_admin_drop_replicas(c.ptr, C.uint64_t(node), ptr, C.size_t(len(chains)), &out)); err != nil {
		return ChainCommit{}, err
	}
	return chainCommitFromC(out), nil
}

func (c *DeckClient) InvalidatePlacement(node uint64) (ChainCommit, error) {
	if c == nil || c.ptr == nil {
		return ChainCommit{}, ErrDeckInvalidArg
	}
	var out C.NetDeckChainCommit
	if err := deckStatusToError(C.net_deck_admin_invalidate_placement(c.ptr, C.uint64_t(node), &out)); err != nil {
		return ChainCommit{}, err
	}
	return chainCommitFromC(out), nil
}

func (c *DeckClient) RestartAllDaemons(node uint64) (ChainCommit, error) {
	if c == nil || c.ptr == nil {
		return ChainCommit{}, ErrDeckInvalidArg
	}
	var out C.NetDeckChainCommit
	if err := deckStatusToError(C.net_deck_admin_restart_all_daemons(c.ptr, C.uint64_t(node), &out)); err != nil {
		return ChainCommit{}, err
	}
	return chainCommitFromC(out), nil
}

func (c *DeckClient) ClearAvoidList(node uint64) (ChainCommit, error) {
	if c == nil || c.ptr == nil {
		return ChainCommit{}, ErrDeckInvalidArg
	}
	var out C.NetDeckChainCommit
	if err := deckStatusToError(C.net_deck_admin_clear_avoid_list(c.ptr, C.uint64_t(node), &out)); err != nil {
		return ChainCommit{}, err
	}
	return chainCommitFromC(out), nil
}

// =====================================================================
// Streams
// =====================================================================

// DeckSnapshotStream is the Go-side handle for the snapshot stream.
type DeckSnapshotStream struct {
	ptr *C.NetDeckSnapshotStream
}

// SubscribeSnapshots opens a live snapshot stream. Caller `.Free()`s
// (or `.Close()` as a synonym).
func (c *DeckClient) SubscribeSnapshots() (*DeckSnapshotStream, error) {
	if c == nil || c.ptr == nil {
		return nil, ErrDeckInvalidArg
	}
	var raw *C.NetDeckSnapshotStream
	if err := deckStatusToError(C.net_deck_subscribe_snapshots(c.ptr, &raw)); err != nil {
		return nil, err
	}
	s := &DeckSnapshotStream{ptr: raw}
	runtime.SetFinalizer(s, func(s *DeckSnapshotStream) { s.Free() })
	return s, nil
}

// Next blocks until the next snapshot arrives or `timeoutMs`
// elapses. Returns `(nil, nil)` on timeout. Returns
// `(nil, ErrDeckEndOfStream)` when the underlying stream closes.
// `timeoutMs == 0` waits indefinitely.
func (s *DeckSnapshotStream) Next(timeoutMs uint64) (map[string]any, error) {
	if s == nil || s.ptr == nil {
		return nil, ErrDeckInvalidArg
	}
	var jsonPtr *C.char
	status := C.net_deck_snapshot_stream_next(s.ptr, C.uint64_t(timeoutMs), &jsonPtr)
	if err := deckStatusToError(status); err != nil {
		return nil, err
	}
	if jsonPtr == nil {
		return nil, nil // timeout
	}
	defer C.net_deck_free_string(jsonPtr)
	raw := C.GoString(jsonPtr)
	var parsed map[string]any
	if err := json.Unmarshal([]byte(raw), &parsed); err != nil {
		return nil, fmt.Errorf("%w: snapshot JSON parse: %v", ErrDeckCallFailed, err)
	}
	return parsed, nil
}

// Close + free the stream. Idempotent.
func (s *DeckSnapshotStream) Close() { s.Free() }

func (s *DeckSnapshotStream) Free() {
	if s == nil || s.ptr == nil {
		return
	}
	C.net_deck_snapshot_stream_free(s.ptr)
	s.ptr = nil
	runtime.SetFinalizer(s, nil)
}

// DeckStatusSummaryStream is the Go-side handle for the status-
// summary stream.
type DeckStatusSummaryStream struct {
	ptr *C.NetDeckStatusSummaryStream
}

func (c *DeckClient) SubscribeStatusSummaries() (*DeckStatusSummaryStream, error) {
	if c == nil || c.ptr == nil {
		return nil, ErrDeckInvalidArg
	}
	var raw *C.NetDeckStatusSummaryStream
	if err := deckStatusToError(C.net_deck_subscribe_status_summaries(c.ptr, &raw)); err != nil {
		return nil, err
	}
	s := &DeckStatusSummaryStream{ptr: raw}
	runtime.SetFinalizer(s, func(s *DeckStatusSummaryStream) { s.Free() })
	return s, nil
}

// Next blocks until the next status summary arrives or `timeoutMs`
// elapses. Returns `(nil, nil)` on timeout. Returns
// `(nil, ErrDeckEndOfStream)` when the stream closes.
func (s *DeckStatusSummaryStream) Next(timeoutMs uint64) (*DeckStatusSummary, error) {
	if s == nil || s.ptr == nil {
		return nil, ErrDeckInvalidArg
	}
	var out C.NetDeckStatusSummary
	var hasItem C.int
	status := C.net_deck_status_summary_stream_next(s.ptr, C.uint64_t(timeoutMs), &out, &hasItem)
	if err := deckStatusToError(status); err != nil {
		return nil, err
	}
	if hasItem == 0 {
		return nil, nil // timeout
	}
	summary := summaryFromC(out)
	return &summary, nil
}

func (s *DeckStatusSummaryStream) Close() { s.Free() }

func (s *DeckStatusSummaryStream) Free() {
	if s == nil || s.ptr == nil {
		return
	}
	C.net_deck_status_summary_stream_free(s.ptr)
	s.ptr = nil
	runtime.SetFinalizer(s, nil)
}

// =====================================================================
// Slice 2 — Log + Failure streams + Audit query
// =====================================================================

// DeckLogLevel mirrors the FFI `NET_DECK_LOG_*` constants.
type DeckLogLevel int

const (
	DeckLogTrace DeckLogLevel = DeckLogLevel(C.NET_DECK_LOG_TRACE)
	DeckLogDebug DeckLogLevel = DeckLogLevel(C.NET_DECK_LOG_DEBUG)
	DeckLogInfo  DeckLogLevel = DeckLogLevel(C.NET_DECK_LOG_INFO)
	DeckLogWarn  DeckLogLevel = DeckLogLevel(C.NET_DECK_LOG_WARN)
	DeckLogError DeckLogLevel = DeckLogLevel(C.NET_DECK_LOG_ERROR)
)

func (l DeckLogLevel) String() string {
	switch l {
	case DeckLogTrace:
		return "trace"
	case DeckLogDebug:
		return "debug"
	case DeckLogInfo:
		return "info"
	case DeckLogWarn:
		return "warn"
	case DeckLogError:
		return "error"
	default:
		return "unknown"
	}
}

// DeckLogFilter restricts the log stream. Every field is
// optional — `nil`-valued pointers match every record.
type DeckLogFilter struct {
	MinLevel *DeckLogLevel
	DaemonID *uint64
	NodeID   *uint64
	SinceSeq *uint64
}

func (f *DeckLogFilter) toC() C.NetDeckLogFilter {
	var out C.NetDeckLogFilter
	if f == nil {
		return out
	}
	if f.MinLevel != nil {
		out.min_level_present = 1
		out.min_level = C.int(*f.MinLevel)
	}
	if f.DaemonID != nil {
		out.daemon_id_present = 1
		out.daemon_id = C.uint64_t(*f.DaemonID)
	}
	if f.NodeID != nil {
		out.node_id_present = 1
		out.node_id = C.uint64_t(*f.NodeID)
	}
	if f.SinceSeq != nil {
		out.since_seq_present = 1
		out.since_seq = C.uint64_t(*f.SinceSeq)
	}
	return out
}

// DeckLogRecord is one log line. `DaemonID` / `NodeID` are nil
// for substrate-level messages.
type DeckLogRecord struct {
	Seq      uint64
	TsMs     uint64
	Level    DeckLogLevel
	DaemonID *uint64
	NodeID   *uint64
	Message  string
}

func logRecordFromC(rec *C.NetDeckLogRecord) DeckLogRecord {
	out := DeckLogRecord{
		Seq:   uint64(rec.seq),
		TsMs:  uint64(rec.ts_ms),
		Level: DeckLogLevel(rec.level),
	}
	if rec.daemon_id_present != 0 {
		d := uint64(rec.daemon_id)
		out.DaemonID = &d
	}
	if rec.node_id_present != 0 {
		n := uint64(rec.node_id)
		out.NodeID = &n
	}
	if rec.message != nil {
		out.Message = C.GoString(rec.message)
	}
	return out
}

// DeckFailureRecord is one executor-failure record.
type DeckFailureRecord struct {
	Seq          uint64
	Source       string
	Reason       string
	RecordedAtMs uint64
}

func failureRecordFromC(rec *C.NetDeckFailureRecord) DeckFailureRecord {
	out := DeckFailureRecord{
		Seq:          uint64(rec.seq),
		RecordedAtMs: uint64(rec.recorded_at_ms),
	}
	if rec.source != nil {
		out.Source = C.GoString(rec.source)
	}
	if rec.reason != nil {
		out.Reason = C.GoString(rec.reason)
	}
	return out
}

// DeckLogStream — handle for the live log stream.
type DeckLogStream struct {
	ptr *C.NetDeckLogStream
}

// SubscribeLogs opens a log stream. `filter == nil` matches
// every record.
func (c *DeckClient) SubscribeLogs(filter *DeckLogFilter) (*DeckLogStream, error) {
	if c == nil || c.ptr == nil {
		return nil, ErrDeckInvalidArg
	}
	var raw *C.NetDeckLogStream
	var filterPtr *C.NetDeckLogFilter
	var cFilter C.NetDeckLogFilter
	if filter != nil {
		cFilter = filter.toC()
		filterPtr = &cFilter
	}
	if err := deckStatusToError(C.net_deck_subscribe_logs(c.ptr, filterPtr, &raw)); err != nil {
		return nil, err
	}
	s := &DeckLogStream{ptr: raw}
	runtime.SetFinalizer(s, func(s *DeckLogStream) { s.Free() })
	return s, nil
}

// Next blocks up to `timeoutMs` for the next log record. Returns
// `(nil, nil)` on timeout, `(nil, ErrDeckEndOfStream)` on stream
// end. Pass `0` for an unbounded wait.
func (s *DeckLogStream) Next(timeoutMs uint64) (*DeckLogRecord, error) {
	if s == nil || s.ptr == nil {
		return nil, ErrDeckInvalidArg
	}
	var rec C.NetDeckLogRecord
	var hasItem C.int
	status := C.net_deck_log_stream_next(s.ptr, C.uint64_t(timeoutMs), &rec, &hasItem)
	if err := deckStatusToError(status); err != nil {
		return nil, err
	}
	if hasItem == 0 {
		return nil, nil
	}
	out := logRecordFromC(&rec)
	// Free the FFI-allocated message string.
	C.net_deck_log_record_drop(&rec)
	return &out, nil
}

func (s *DeckLogStream) Close() { s.Free() }

func (s *DeckLogStream) Free() {
	if s == nil || s.ptr == nil {
		return
	}
	C.net_deck_log_stream_free(s.ptr)
	s.ptr = nil
	runtime.SetFinalizer(s, nil)
}

// DeckFailureStream — handle for the live failure stream.
type DeckFailureStream struct {
	ptr *C.NetDeckFailureStream
}

func (c *DeckClient) SubscribeFailures(sinceSeq uint64) (*DeckFailureStream, error) {
	if c == nil || c.ptr == nil {
		return nil, ErrDeckInvalidArg
	}
	var raw *C.NetDeckFailureStream
	if err := deckStatusToError(C.net_deck_subscribe_failures(c.ptr, C.uint64_t(sinceSeq), &raw)); err != nil {
		return nil, err
	}
	s := &DeckFailureStream{ptr: raw}
	runtime.SetFinalizer(s, func(s *DeckFailureStream) { s.Free() })
	return s, nil
}

func (s *DeckFailureStream) Next(timeoutMs uint64) (*DeckFailureRecord, error) {
	if s == nil || s.ptr == nil {
		return nil, ErrDeckInvalidArg
	}
	var rec C.NetDeckFailureRecord
	var hasItem C.int
	status := C.net_deck_failure_stream_next(s.ptr, C.uint64_t(timeoutMs), &rec, &hasItem)
	if err := deckStatusToError(status); err != nil {
		return nil, err
	}
	if hasItem == 0 {
		return nil, nil
	}
	out := failureRecordFromC(&rec)
	C.net_deck_failure_record_drop(&rec)
	return &out, nil
}

func (s *DeckFailureStream) Close() { s.Free() }

func (s *DeckFailureStream) Free() {
	if s == nil || s.ptr == nil {
		return
	}
	C.net_deck_failure_stream_free(s.ptr)
	s.ptr = nil
	runtime.SetFinalizer(s, nil)
}

// =====================================================================
// AuditQuery — fluent builder + AuditStream
// =====================================================================

// DeckAuditQuery is the Go-side handle for the audit query
// builder. Holds only filter state — pass the parent
// `DeckClient` on Collect / Stream.
type DeckAuditQuery struct {
	ptr *C.NetDeckAuditQuery
}

func (c *DeckClient) Audit() (*DeckAuditQuery, error) {
	var raw *C.NetDeckAuditQuery
	if err := deckStatusToError(C.net_deck_audit_query_new(&raw)); err != nil {
		return nil, err
	}
	q := &DeckAuditQuery{ptr: raw}
	runtime.SetFinalizer(q, func(q *DeckAuditQuery) { q.Free() })
	return q, nil
}

func (q *DeckAuditQuery) Recent(limit uint) *DeckAuditQuery {
	if q != nil && q.ptr != nil {
		C.net_deck_audit_query_recent(q.ptr, C.size_t(limit))
	}
	return q
}

func (q *DeckAuditQuery) ByOperator(operatorID uint64) *DeckAuditQuery {
	if q != nil && q.ptr != nil {
		C.net_deck_audit_query_by_operator(q.ptr, C.uint64_t(operatorID))
	}
	return q
}

func (q *DeckAuditQuery) Between(startMs, endMs uint64) *DeckAuditQuery {
	if q != nil && q.ptr != nil {
		C.net_deck_audit_query_between(q.ptr, C.uint64_t(startMs), C.uint64_t(endMs))
	}
	return q
}

func (q *DeckAuditQuery) ForceOnly() *DeckAuditQuery {
	if q != nil && q.ptr != nil {
		C.net_deck_audit_query_force_only(q.ptr)
	}
	return q
}

func (q *DeckAuditQuery) Since(seq uint64) *DeckAuditQuery {
	if q != nil && q.ptr != nil {
		C.net_deck_audit_query_since(q.ptr, C.uint64_t(seq))
	}
	return q
}

// Collect returns the audit records as parsed `map[string]any`
// objects. JSON parsing happens in Go; the FFI returns an array
// of CString JSON payloads which we free immediately after copy.
func (q *DeckAuditQuery) Collect(client *DeckClient) ([]map[string]any, error) {
	if q == nil || q.ptr == nil || client == nil || client.ptr == nil {
		return nil, ErrDeckInvalidArg
	}
	var records **C.char
	var count C.size_t
	status := C.net_deck_audit_query_collect(q.ptr, client.ptr, &records, &count)
	if err := deckStatusToError(status); err != nil {
		return nil, err
	}
	defer C.net_deck_audit_records_free(records, count)
	out := make([]map[string]any, 0, int(count))
	for i := 0; i < int(count); i++ {
		ptr := *(**C.char)(unsafe.Pointer(uintptr(unsafe.Pointer(records)) + uintptr(i)*unsafe.Sizeof(uintptr(0))))
		if ptr == nil {
			continue
		}
		raw := C.GoString(ptr)
		var parsed map[string]any
		if err := json.Unmarshal([]byte(raw), &parsed); err != nil {
			return nil, fmt.Errorf("%w: audit JSON parse: %v", ErrDeckCallFailed, err)
		}
		out = append(out, parsed)
	}
	return out, nil
}

func (q *DeckAuditQuery) Stream(client *DeckClient) (*DeckAuditStream, error) {
	if q == nil || q.ptr == nil || client == nil || client.ptr == nil {
		return nil, ErrDeckInvalidArg
	}
	var raw *C.NetDeckAuditStream
	if err := deckStatusToError(C.net_deck_audit_query_stream(q.ptr, client.ptr, &raw)); err != nil {
		return nil, err
	}
	s := &DeckAuditStream{ptr: raw}
	runtime.SetFinalizer(s, func(s *DeckAuditStream) { s.Free() })
	return s, nil
}

func (q *DeckAuditQuery) Free() {
	if q == nil || q.ptr == nil {
		return
	}
	C.net_deck_audit_query_free(q.ptr)
	q.ptr = nil
	runtime.SetFinalizer(q, nil)
}

// DeckAuditStream — sync iterator over audit records (returned
// as parsed `map[string]any`).
type DeckAuditStream struct {
	ptr *C.NetDeckAuditStream
}

func (s *DeckAuditStream) Next(timeoutMs uint64) (map[string]any, error) {
	if s == nil || s.ptr == nil {
		return nil, ErrDeckInvalidArg
	}
	var jsonPtr *C.char
	status := C.net_deck_audit_stream_next(s.ptr, C.uint64_t(timeoutMs), &jsonPtr)
	if err := deckStatusToError(status); err != nil {
		return nil, err
	}
	if jsonPtr == nil {
		return nil, nil
	}
	defer C.net_deck_free_string(jsonPtr)
	raw := C.GoString(jsonPtr)
	var parsed map[string]any
	if err := json.Unmarshal([]byte(raw), &parsed); err != nil {
		return nil, fmt.Errorf("%w: audit JSON parse: %v", ErrDeckCallFailed, err)
	}
	return parsed, nil
}

func (s *DeckAuditStream) Close() { s.Free() }

func (s *DeckAuditStream) Free() {
	if s == nil || s.ptr == nil {
		return
	}
	C.net_deck_audit_stream_free(s.ptr)
	s.ptr = nil
	runtime.SetFinalizer(s, nil)
}

// =====================================================================
// Slice 3 — ICE break-glass surface
// =====================================================================

// AvoidScope is the discriminator for `Ice.FlushAvoidLists`.
// Use the constructors `AvoidScopeGlobal`,
// `AvoidScopeLocal(nodeID)`, `AvoidScopeOnPeer(peerID)`.
type AvoidScope struct {
	kind C.int
	node uint64
	peer uint64
}

func AvoidScopeGlobal() AvoidScope {
	return AvoidScope{kind: C.NET_DECK_AVOID_SCOPE_GLOBAL}
}

func AvoidScopeLocal(node uint64) AvoidScope {
	return AvoidScope{kind: C.NET_DECK_AVOID_SCOPE_LOCAL, node: node}
}

func AvoidScopeOnPeer(peer uint64) AvoidScope {
	return AvoidScope{kind: C.NET_DECK_AVOID_SCOPE_ON_PEER, peer: peer}
}

// DeckOperatorSignature is one entry in the bundle passed to
// `SimulatedIceProposal.Commit`. `Signature` must be exactly 64
// ed25519 bytes.
type DeckOperatorSignature struct {
	OperatorID uint64
	Signature  []byte
}

// DeckIceCommands — operator-side break-glass surface. Every
// factory returns a `*DeckIceProposal` that must be `.Simulate()`-d
// before commit.
type DeckIceCommands struct {
	client *DeckClient
}

// Ice returns the break-glass surface for the deck client.
func (c *DeckClient) Ice() *DeckIceCommands {
	if c == nil || c.ptr == nil {
		return nil
	}
	return &DeckIceCommands{client: c}
}

func (ic *DeckIceCommands) FreezeCluster(ttlMs uint64) (*DeckIceProposal, error) {
	if ic == nil || ic.client == nil {
		return nil, ErrDeckInvalidArg
	}
	var raw *C.NetDeckIceProposal
	if err := deckStatusToError(C.net_deck_ice_freeze_cluster(ic.client.ptr, C.uint64_t(ttlMs), &raw)); err != nil {
		return nil, err
	}
	p := newIceProposal(raw)
	return p, nil
}

func (ic *DeckIceCommands) FlushAvoidLists(scope AvoidScope) (*DeckIceProposal, error) {
	if ic == nil || ic.client == nil {
		return nil, ErrDeckInvalidArg
	}
	var raw *C.NetDeckIceProposal
	if err := deckStatusToError(C.net_deck_ice_flush_avoid_lists(ic.client.ptr, scope.kind, C.uint64_t(scope.node), C.uint64_t(scope.peer), &raw)); err != nil {
		return nil, err
	}
	return newIceProposal(raw), nil
}

func (ic *DeckIceCommands) ForceEvictReplica(chain, victim uint64) (*DeckIceProposal, error) {
	if ic == nil || ic.client == nil {
		return nil, ErrDeckInvalidArg
	}
	var raw *C.NetDeckIceProposal
	if err := deckStatusToError(C.net_deck_ice_force_evict_replica(ic.client.ptr, C.uint64_t(chain), C.uint64_t(victim), &raw)); err != nil {
		return nil, err
	}
	return newIceProposal(raw), nil
}

// ForceRestartDaemon — `name` is the daemon's `MeshDaemon::name()`.
func (ic *DeckIceCommands) ForceRestartDaemon(id uint64, name string) (*DeckIceProposal, error) {
	if ic == nil || ic.client == nil {
		return nil, ErrDeckInvalidArg
	}
	var raw *C.NetDeckIceProposal
	var namePtr *C.char
	var nameLen C.size_t
	if len(name) > 0 {
		nameBytes := []byte(name)
		namePtr = (*C.char)(unsafe.Pointer(&nameBytes[0]))
		nameLen = C.size_t(len(nameBytes))
	}
	if err := deckStatusToError(C.net_deck_ice_force_restart_daemon(ic.client.ptr, C.uint64_t(id), namePtr, nameLen, &raw)); err != nil {
		return nil, err
	}
	return newIceProposal(raw), nil
}

func (ic *DeckIceCommands) ForceCutover(chain, target uint64) (*DeckIceProposal, error) {
	if ic == nil || ic.client == nil {
		return nil, ErrDeckInvalidArg
	}
	var raw *C.NetDeckIceProposal
	if err := deckStatusToError(C.net_deck_ice_force_cutover(ic.client.ptr, C.uint64_t(chain), C.uint64_t(target), &raw)); err != nil {
		return nil, err
	}
	return newIceProposal(raw), nil
}

func (ic *DeckIceCommands) KillMigration(migration uint64) (*DeckIceProposal, error) {
	if ic == nil || ic.client == nil {
		return nil, ErrDeckInvalidArg
	}
	var raw *C.NetDeckIceProposal
	if err := deckStatusToError(C.net_deck_ice_kill_migration(ic.client.ptr, C.uint64_t(migration), &raw)); err != nil {
		return nil, err
	}
	return newIceProposal(raw), nil
}

func (ic *DeckIceCommands) ThawCluster() (*DeckIceProposal, error) {
	if ic == nil || ic.client == nil {
		return nil, ErrDeckInvalidArg
	}
	var raw *C.NetDeckIceProposal
	if err := deckStatusToError(C.net_deck_ice_thaw_cluster(ic.client.ptr, &raw)); err != nil {
		return nil, err
	}
	return newIceProposal(raw), nil
}

// DeckIceProposal — pre-simulation. No `Commit` method —
// typestate enforces `Simulate()` first.
type DeckIceProposal struct {
	ptr *C.NetDeckIceProposal
}

func newIceProposal(raw *C.NetDeckIceProposal) *DeckIceProposal {
	p := &DeckIceProposal{ptr: raw}
	runtime.SetFinalizer(p, func(p *DeckIceProposal) { p.Free() })
	return p
}

func (p *DeckIceProposal) IssuedAtMs() uint64 {
	if p == nil || p.ptr == nil {
		return 0
	}
	return uint64(C.net_deck_ice_proposal_issued_at_ms(p.ptr))
}

// Simulate consumes the proposal and runs the substrate
// simulator. Subsequent calls return `DeckSdkError(kind:
// "already_simulated")`. The caller still must `Free()` the
// proposal husk after Simulate.
func (p *DeckIceProposal) Simulate(client *DeckClient) (*DeckSimulatedIceProposal, error) {
	if p == nil || p.ptr == nil || client == nil || client.ptr == nil {
		return nil, ErrDeckInvalidArg
	}
	var raw *C.NetDeckSimulatedIceProposal
	if err := deckStatusToError(C.net_deck_ice_proposal_simulate(p.ptr, client.ptr, &raw)); err != nil {
		return nil, err
	}
	s := &DeckSimulatedIceProposal{ptr: raw}
	runtime.SetFinalizer(s, func(s *DeckSimulatedIceProposal) { s.Free() })
	return s, nil
}

func (p *DeckIceProposal) Free() {
	if p == nil || p.ptr == nil {
		return
	}
	C.net_deck_ice_proposal_free(p.ptr)
	p.ptr = nil
	runtime.SetFinalizer(p, nil)
}

// DeckSimulatedIceProposal — the only handle exposing Commit.
type DeckSimulatedIceProposal struct {
	ptr *C.NetDeckSimulatedIceProposal
}

func (s *DeckSimulatedIceProposal) IssuedAtMs() uint64 {
	if s == nil || s.ptr == nil {
		return 0
	}
	return uint64(C.net_deck_simulated_issued_at_ms(s.ptr))
}

// BlastRadius returns the simulator's pre-execution preview as
// a parsed map. JSON parsing happens Go-side; the FFI emits a
// heap CString which we free immediately.
func (s *DeckSimulatedIceProposal) BlastRadius() (map[string]any, error) {
	if s == nil || s.ptr == nil {
		return nil, ErrDeckInvalidArg
	}
	jsonPtr := C.net_deck_simulated_blast_radius(s.ptr)
	if jsonPtr == nil {
		return nil, wrapDeckError(ErrDeckCallFailed)
	}
	defer C.net_deck_free_string(jsonPtr)
	raw := C.GoString(jsonPtr)
	var parsed map[string]any
	if err := json.Unmarshal([]byte(raw), &parsed); err != nil {
		return nil, fmt.Errorf("%w: blast JSON parse: %v", ErrDeckCallFailed, err)
	}
	return parsed, nil
}

// BlastHash returns the 32-byte Blake3 digest signers must cover.
func (s *DeckSimulatedIceProposal) BlastHash() ([32]byte, error) {
	var out [32]byte
	if s == nil || s.ptr == nil {
		return out, ErrDeckInvalidArg
	}
	status := C.net_deck_simulated_blast_hash(s.ptr, (*C.uint8_t)(unsafe.Pointer(&out[0])))
	if err := deckStatusToError(status); err != nil {
		return out, err
	}
	return out, nil
}

// Commit publishes the simulated proposal with the supplied
// signatures. Consumes the proposal — subsequent calls return
// `DeckSdkError(kind: "already_committed")`.
func (s *DeckSimulatedIceProposal) Commit(client *DeckClient, signatures []DeckOperatorSignature) (ChainCommit, error) {
	if s == nil || s.ptr == nil || client == nil || client.ptr == nil {
		return ChainCommit{}, ErrDeckInvalidArg
	}
	// Build the C-side signature array. We keep the Go-side
	// byte slices alive for the duration of the call by holding
	// references to them in a local slice.
	cSigs := make([]C.NetDeckOperatorSignature, len(signatures))
	for i, sig := range signatures {
		if len(sig.Signature) == 0 {
			return ChainCommit{}, fmt.Errorf("%w: signature %d has empty bytes", ErrDeckInvalidArg, i)
		}
		cSigs[i] = C.NetDeckOperatorSignature{
			operator_id:   C.uint64_t(sig.OperatorID),
			signature_ptr: (*C.uint8_t)(unsafe.Pointer(&sig.Signature[0])),
			signature_len: C.size_t(len(sig.Signature)),
		}
	}
	var sigsPtr *C.NetDeckOperatorSignature
	if len(cSigs) > 0 {
		sigsPtr = &cSigs[0]
	}
	var commit C.NetDeckChainCommit
	status := C.net_deck_simulated_commit(s.ptr, client.ptr, sigsPtr, C.size_t(len(signatures)), &commit)
	if err := deckStatusToError(status); err != nil {
		return ChainCommit{}, err
	}
	return chainCommitFromC(commit), nil
}

func (s *DeckSimulatedIceProposal) Free() {
	if s == nil || s.ptr == nil {
		return
	}
	C.net_deck_simulated_free(s.ptr)
	s.ptr = nil
	runtime.SetFinalizer(s, nil)
}

// SigningPayload returns the deterministic ICE signing payload
// bytes (`ICE_SIGNING_DOMAIN || issued_at_ms (LE u64) ||
// blast_hash (32) || postcard(action)`). Useful for the offline
// / cross-deck signing workflow — pair with
// `DeckOperatorIdentity.SignPayload(payload)` on a remote deck
// to produce a signature the local deck can hand into
// `Commit([]DeckOperatorSignature{...})`.
//
// Returns `DeckSdkError(kind: "already_committed")` once the
// proposal has been consumed by `Commit`.
func (s *DeckSimulatedIceProposal) SigningPayload() ([]byte, error) {
	if s == nil || s.ptr == nil {
		return nil, ErrDeckInvalidArg
	}
	var ptr *C.uint8_t
	var length C.size_t
	status := C.net_deck_simulated_signing_payload(s.ptr, &ptr, &length)
	if err := deckStatusToError(status); err != nil {
		return nil, err
	}
	if ptr == nil || length == 0 {
		return nil, nil
	}
	out := C.GoBytes(unsafe.Pointer(ptr), C.int(length))
	C.net_deck_signing_payload_free(ptr, length)
	return out, nil
}

// =====================================================================
// DeckOperatorIdentity — opaque handle for the offline signing seam
// =====================================================================

// DeckOperatorIdentity is an operator's ed25519 keypair wrapped
// as an opaque handle. Distinct from the identity passed to
// `NewDeckClient` (which currently takes a raw seed); this
// handle is for the offline signing flow + operator-policy
// authoring. Call `Free` when done.
type DeckOperatorIdentity struct {
	ptr *C.NetDeckOperatorIdentity
}

// GenerateDeckOperatorIdentity creates a fresh keypair.
func GenerateDeckOperatorIdentity() *DeckOperatorIdentity {
	raw := C.net_deck_operator_identity_generate()
	if raw == nil {
		return nil
	}
	h := &DeckOperatorIdentity{ptr: raw}
	runtime.SetFinalizer(h, func(h *DeckOperatorIdentity) { h.Free() })
	return h
}

// NewDeckOperatorIdentityFromSeed loads an identity from a
// 32-byte ed25519 seed.
func NewDeckOperatorIdentityFromSeed(seed []byte) (*DeckOperatorIdentity, error) {
	if len(seed) != 32 {
		return nil, fmt.Errorf("%w: seed must be 32 bytes, got %d", ErrDeckInvalidArg, len(seed))
	}
	var raw *C.NetDeckOperatorIdentity
	status := C.net_deck_operator_identity_from_seed(
		(*C.uint8_t)(unsafe.Pointer(&seed[0])),
		&raw,
	)
	if err := deckStatusToError(status); err != nil {
		return nil, err
	}
	h := &DeckOperatorIdentity{ptr: raw}
	runtime.SetFinalizer(h, func(h *DeckOperatorIdentity) { h.Free() })
	return h, nil
}

// OperatorID returns the keypair's origin hash.
func (i *DeckOperatorIdentity) OperatorID() uint64 {
	if i == nil || i.ptr == nil {
		return 0
	}
	return uint64(C.net_deck_operator_identity_operator_id(i.ptr))
}

// PublicKey returns the 32-byte ed25519 verifying key. Used to
// author an `OperatorRegistry` from a set of known identities.
func (i *DeckOperatorIdentity) PublicKey() ([]byte, error) {
	if i == nil || i.ptr == nil {
		return nil, ErrDeckInvalidArg
	}
	buf := make([]byte, 32)
	status := C.net_deck_operator_identity_public_key(
		i.ptr,
		(*C.uint8_t)(unsafe.Pointer(&buf[0])),
	)
	if err := deckStatusToError(status); err != nil {
		return nil, err
	}
	return buf, nil
}

// SignProposal signs a simulated ICE proposal. Returns the
// operator id + 64-byte ed25519 signature shaped as a
// `DeckOperatorSignature` that `Commit` accepts directly.
//
// Returns `DeckSdkError(kind: "already_committed")` if the
// simulated proposal has been consumed by `Commit`.
func (i *DeckOperatorIdentity) SignProposal(simulated *DeckSimulatedIceProposal) (DeckOperatorSignature, error) {
	if i == nil || i.ptr == nil || simulated == nil || simulated.ptr == nil {
		return DeckOperatorSignature{}, ErrDeckInvalidArg
	}
	var opID C.uint64_t
	var sigBytes [64]byte
	status := C.net_deck_operator_identity_sign_proposal(
		i.ptr,
		simulated.ptr,
		&opID,
		(*C.uint8_t)(unsafe.Pointer(&sigBytes[0])),
	)
	if err := deckStatusToError(status); err != nil {
		return DeckOperatorSignature{}, err
	}
	out := make([]byte, 64)
	copy(out, sigBytes[:])
	return DeckOperatorSignature{
		OperatorID: uint64(opID),
		Signature:  out,
	}, nil
}

// SignPayload signs raw payload bytes with this identity's
// ed25519 key. Useful for offline / cross-deck signing flows
// where the deterministic ICE signing payload is exchanged
// out-of-band (see `DeckSimulatedIceProposal.SigningPayload`).
func (i *DeckOperatorIdentity) SignPayload(payload []byte) (DeckOperatorSignature, error) {
	if i == nil || i.ptr == nil {
		return DeckOperatorSignature{}, ErrDeckInvalidArg
	}
	var payloadPtr *C.uint8_t
	if len(payload) > 0 {
		payloadPtr = (*C.uint8_t)(unsafe.Pointer(&payload[0]))
	}
	var opID C.uint64_t
	var sigBytes [64]byte
	status := C.net_deck_operator_identity_sign_payload(
		i.ptr,
		payloadPtr,
		C.size_t(len(payload)),
		&opID,
		(*C.uint8_t)(unsafe.Pointer(&sigBytes[0])),
	)
	if err := deckStatusToError(status); err != nil {
		return DeckOperatorSignature{}, err
	}
	out := make([]byte, 64)
	copy(out, sigBytes[:])
	return DeckOperatorSignature{
		OperatorID: uint64(opID),
		Signature:  out,
	}, nil
}

// Free releases the identity handle. Idempotent.
func (i *DeckOperatorIdentity) Free() {
	if i == nil || i.ptr == nil {
		return
	}
	C.net_deck_operator_identity_free(i.ptr)
	i.ptr = nil
	runtime.SetFinalizer(i, nil)
}

// =====================================================================
// DeckOperatorRegistry — operator-policy authoring + offline verify
// =====================================================================

// DeckOperatorRegistry holds known operator public keys keyed
// by 64-bit operator id. Use to author the cluster's
// operator-policy snapshot or to pre-verify bundles before
// invoking `DeckSimulatedIceProposal.Commit`. Mutations are
// thread-safe at the cdylib layer.
type DeckOperatorRegistry struct {
	ptr *C.NetDeckOperatorRegistry
}

// NewDeckOperatorRegistry creates an empty registry.
func NewDeckOperatorRegistry() *DeckOperatorRegistry {
	raw := C.net_deck_operator_registry_new()
	if raw == nil {
		return nil
	}
	r := &DeckOperatorRegistry{ptr: raw}
	runtime.SetFinalizer(r, func(r *DeckOperatorRegistry) { r.Free() })
	return r
}

// Insert an operator's 32-byte ed25519 public key under
// `operatorID`.
func (r *DeckOperatorRegistry) Insert(operatorID uint64, publicKey []byte) error {
	if r == nil || r.ptr == nil {
		return ErrDeckInvalidArg
	}
	if len(publicKey) != 32 {
		return fmt.Errorf("%w: publicKey must be 32 bytes, got %d", ErrDeckInvalidArg, len(publicKey))
	}
	status := C.net_deck_operator_registry_insert(
		r.ptr,
		C.uint64_t(operatorID),
		(*C.uint8_t)(unsafe.Pointer(&publicKey[0])),
	)
	return deckStatusToError(status)
}

// Register an identity under its derived operator id (the
// keypair's origin hash).
func (r *DeckOperatorRegistry) Register(identity *DeckOperatorIdentity) error {
	if r == nil || r.ptr == nil || identity == nil || identity.ptr == nil {
		return ErrDeckInvalidArg
	}
	status := C.net_deck_operator_registry_register(r.ptr, identity.ptr)
	return deckStatusToError(status)
}

// Contains reports whether `operatorID` is registered.
func (r *DeckOperatorRegistry) Contains(operatorID uint64) bool {
	if r == nil || r.ptr == nil {
		return false
	}
	return C.net_deck_operator_registry_contains(r.ptr, C.uint64_t(operatorID)) == 1
}

// Len returns the number of registered operators.
func (r *DeckOperatorRegistry) Len() int {
	if r == nil || r.ptr == nil {
		return 0
	}
	return int(C.net_deck_operator_registry_len(r.ptr))
}

// Verify a single signature over `payload`. Returns a
// `DeckSdkError` carrying the substrate's stable kind
// discriminator (`not_authorized`, `signature_invalid`) on
// failure.
func (r *DeckOperatorRegistry) Verify(signature DeckOperatorSignature, payload []byte) error {
	if r == nil || r.ptr == nil {
		return ErrDeckInvalidArg
	}
	if len(signature.Signature) == 0 {
		return fmt.Errorf("%w: signature is empty", ErrDeckInvalidArg)
	}
	cSig := C.NetDeckOperatorSignature{
		operator_id:   C.uint64_t(signature.OperatorID),
		signature_ptr: (*C.uint8_t)(unsafe.Pointer(&signature.Signature[0])),
		signature_len: C.size_t(len(signature.Signature)),
	}
	var payloadPtr *C.uint8_t
	if len(payload) > 0 {
		payloadPtr = (*C.uint8_t)(unsafe.Pointer(&payload[0]))
	}
	status := C.net_deck_operator_registry_verify(r.ptr, &cSig, payloadPtr, C.size_t(len(payload)))
	return deckStatusToError(status)
}

// VerifyBundle confirms every signature over `payload` and that
// at least `threshold` *distinct* operator ids signed it. The
// distinct-operator dedup gate is the M-of-N guarantee.
func (r *DeckOperatorRegistry) VerifyBundle(signatures []DeckOperatorSignature, payload []byte, threshold int) error {
	if r == nil || r.ptr == nil {
		return ErrDeckInvalidArg
	}
	cSigs := make([]C.NetDeckOperatorSignature, len(signatures))
	for i, sig := range signatures {
		if len(sig.Signature) == 0 {
			return fmt.Errorf("%w: signature %d is empty", ErrDeckInvalidArg, i)
		}
		cSigs[i] = C.NetDeckOperatorSignature{
			operator_id:   C.uint64_t(sig.OperatorID),
			signature_ptr: (*C.uint8_t)(unsafe.Pointer(&sig.Signature[0])),
			signature_len: C.size_t(len(sig.Signature)),
		}
	}
	var sigsPtr *C.NetDeckOperatorSignature
	if len(cSigs) > 0 {
		sigsPtr = &cSigs[0]
	}
	var payloadPtr *C.uint8_t
	if len(payload) > 0 {
		payloadPtr = (*C.uint8_t)(unsafe.Pointer(&payload[0]))
	}
	status := C.net_deck_operator_registry_verify_bundle(
		r.ptr,
		sigsPtr,
		C.size_t(len(signatures)),
		payloadPtr,
		C.size_t(len(payload)),
		C.size_t(threshold),
	)
	return deckStatusToError(status)
}

// Free releases the registry. Idempotent.
func (r *DeckOperatorRegistry) Free() {
	if r == nil || r.ptr == nil {
		return
	}
	C.net_deck_operator_registry_free(r.ptr)
	r.ptr = nil
	runtime.SetFinalizer(r, nil)
}

// =====================================================================
// DeckAdminVerifier — substrate verifier wrapper
// =====================================================================

// DeckAdminVerifier bundles a snapshotted OperatorRegistry with
// the cluster's policy knobs (signature threshold, freshness
// window, future-skew tolerance, ICE cooldown). Useful for
// offline unit testing of operator-policy decisions.
//
// Constructors snapshot the registry at build time — rebuild
// after every policy change.
type DeckAdminVerifier struct {
	ptr *C.NetDeckAdminVerifier
}

// NewDeckAdminVerifier builds a verifier with the substrate's
// default freshness (300s), future-skew (30s), and ICE cooldown
// (300s) windows. `threshold = 0` is clamped to `1`.
func NewDeckAdminVerifier(registry *DeckOperatorRegistry, threshold int) *DeckAdminVerifier {
	if registry == nil || registry.ptr == nil {
		return nil
	}
	raw := C.net_deck_admin_verifier_new(registry.ptr, C.size_t(threshold))
	if raw == nil {
		return nil
	}
	v := &DeckAdminVerifier{ptr: raw}
	runtime.SetFinalizer(v, func(v *DeckAdminVerifier) { v.Free() })
	return v
}

// NewDeckAdminVerifierWithFreshness uses explicit freshness +
// future-skew windows and the default ICE cooldown.
func NewDeckAdminVerifierWithFreshness(registry *DeckOperatorRegistry, threshold int, freshnessWindowMs, futureSkewMs uint64) *DeckAdminVerifier {
	if registry == nil || registry.ptr == nil {
		return nil
	}
	raw := C.net_deck_admin_verifier_with_freshness(
		registry.ptr,
		C.size_t(threshold),
		C.uint64_t(freshnessWindowMs),
		C.uint64_t(futureSkewMs),
	)
	if raw == nil {
		return nil
	}
	v := &DeckAdminVerifier{ptr: raw}
	runtime.SetFinalizer(v, func(v *DeckAdminVerifier) { v.Free() })
	return v
}

// NewDeckAdminVerifierWithFullPolicy sets every policy knob.
// Primarily for tests that need a short cooldown window.
func NewDeckAdminVerifierWithFullPolicy(registry *DeckOperatorRegistry, threshold int, freshnessWindowMs, futureSkewMs, iceCooldownMs uint64) *DeckAdminVerifier {
	if registry == nil || registry.ptr == nil {
		return nil
	}
	raw := C.net_deck_admin_verifier_with_full_policy(
		registry.ptr,
		C.size_t(threshold),
		C.uint64_t(freshnessWindowMs),
		C.uint64_t(futureSkewMs),
		C.uint64_t(iceCooldownMs),
	)
	if raw == nil {
		return nil
	}
	v := &DeckAdminVerifier{ptr: raw}
	runtime.SetFinalizer(v, func(v *DeckAdminVerifier) { v.Free() })
	return v
}

func (v *DeckAdminVerifier) Threshold() int {
	if v == nil || v.ptr == nil {
		return 0
	}
	return int(C.net_deck_admin_verifier_threshold(v.ptr))
}

func (v *DeckAdminVerifier) FreshnessWindowMs() uint64 {
	if v == nil || v.ptr == nil {
		return 0
	}
	return uint64(C.net_deck_admin_verifier_freshness_window_ms(v.ptr))
}

func (v *DeckAdminVerifier) FutureSkewMs() uint64 {
	if v == nil || v.ptr == nil {
		return 0
	}
	return uint64(C.net_deck_admin_verifier_future_skew_ms(v.ptr))
}

func (v *DeckAdminVerifier) IceCooldownMs() uint64 {
	if v == nil || v.ptr == nil {
		return 0
	}
	return uint64(C.net_deck_admin_verifier_ice_cooldown_ms(v.ptr))
}

func (v *DeckAdminVerifier) Free() {
	if v == nil || v.ptr == nil {
		return
	}
	C.net_deck_admin_verifier_free(v.ptr)
	v.ptr = nil
	runtime.SetFinalizer(v, nil)
}
