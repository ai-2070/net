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
	ThisNode                uint64
	TickIntervalMs          uint64
	EventQueueCapacity      int
	ActionQueueCapacity     int
	SnapshotPollIntervalMs  uint64
	IceSignatureThreshold   int
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
