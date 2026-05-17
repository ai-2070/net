// Package net — Deck operator client.
//
// The Deck surface is compiled into the `libnet_deck` cdylib (separate
// from `libnet`). Build with `cargo build --release -p net-deck-ffi`.
//
// This file covers the operator-side admin + status surface (slice 1):
// client lifecycle, all 9 AdminCommands verbs, one-shot status reads,
// and live snapshot + status-summary streams. The richer slice 2 (log
// + failure + audit streams) and slice 3 (ICE break-glass) lives in
// the reference Go binding at
// `net/crates/net/bindings/go/net/deck.go`; extend this file as you
// need additional surfaces.
//
// # Example
//
//	client, err := NewDeckClient(seed, DeckClientConfig{ThisNode: 1})
//	if err != nil { log.Fatal(err) }
//	defer client.Free()
//
//	commit, err := client.Drain(2, 30_000)
//	if err != nil { log.Fatal(err) }
//	log.Printf("drain committed at %d", commit.CommittedAtMs)

package net

/*
#cgo LDFLAGS: -L${SRCDIR}/../net/crates/net/target/release -lnet_deck
#include <stdint.h>
#include <stdlib.h>

typedef struct NetDeckClient              NetDeckClient;
typedef struct NetDeckSnapshotStream      NetDeckSnapshotStream;
typedef struct NetDeckStatusSummaryStream NetDeckStatusSummaryStream;

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

extern char* net_deck_status(const NetDeckClient* client);
extern int net_deck_status_summary(const NetDeckClient* client, NetDeckStatusSummary* out);
extern void net_deck_free_string(char* s);
extern const char* net_deck_last_error_kind(void);
extern const char* net_deck_last_error_message(void);

extern int net_deck_admin_drain(const NetDeckClient*, uint64_t, uint64_t, NetDeckChainCommit*);
extern int net_deck_admin_enter_maintenance(const NetDeckClient*, uint64_t, uint64_t, int, NetDeckChainCommit*);
extern int net_deck_admin_exit_maintenance(const NetDeckClient*, uint64_t, NetDeckChainCommit*);
extern int net_deck_admin_cordon(const NetDeckClient*, uint64_t, NetDeckChainCommit*);
extern int net_deck_admin_uncordon(const NetDeckClient*, uint64_t, NetDeckChainCommit*);
extern int net_deck_admin_drop_replicas(const NetDeckClient*, uint64_t, const uint64_t*, size_t, NetDeckChainCommit*);
extern int net_deck_admin_invalidate_placement(const NetDeckClient*, uint64_t, NetDeckChainCommit*);
extern int net_deck_admin_restart_all_daemons(const NetDeckClient*, uint64_t, NetDeckChainCommit*);
extern int net_deck_admin_clear_avoid_list(const NetDeckClient*, uint64_t, NetDeckChainCommit*);

extern int net_deck_subscribe_snapshots(const NetDeckClient*, NetDeckSnapshotStream**);
extern int net_deck_snapshot_stream_next(NetDeckSnapshotStream*, uint64_t, char**);
extern void net_deck_snapshot_stream_free(NetDeckSnapshotStream*);

extern int net_deck_subscribe_status_summaries(const NetDeckClient*, NetDeckStatusSummaryStream**);
extern int net_deck_status_summary_stream_next(NetDeckStatusSummaryStream*, uint64_t, NetDeckStatusSummary*, int*);
extern void net_deck_status_summary_stream_free(NetDeckStatusSummaryStream*);
*/
import "C"

import (
	"encoding/json"
	"errors"
	"fmt"
	"runtime"
	"sync"
	"unsafe"
)

// ---------------------------------------------------------------------------
// Status codes (mirror net_deck.h)
// ---------------------------------------------------------------------------

const (
	netDeckOK                 = 0
	netDeckErrNull            = -1
	netDeckErrCallFailed      = -2
	netDeckErrInvalidArg      = -3
	netDeckErrAlreadyShutdown = -4
	netDeckErrEndOfStream     = -5
)

// ChainCommit event kinds.
const (
	DeckEventKindUnknown             = 0
	DeckEventKindDrain               = 1
	DeckEventKindEnterMaintenance    = 2
	DeckEventKindExitMaintenance     = 3
	DeckEventKindCordon              = 4
	DeckEventKindUncordon            = 5
	DeckEventKindDropReplicas        = 6
	DeckEventKindInvalidatePlacement = 7
	DeckEventKindRestartAllDaemons   = 8
	DeckEventKindClearAvoidList      = 9
)

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

// ErrDeck is the umbrella error for any Deck client failure.
// Use `errors.Is(err, ErrDeck)` to detect any Deck-side failure.
var ErrDeck = errors.New("deck")

// ErrDeckEndOfStream signals a stream has been drained.
// Not a true error — callers loop on it.
var ErrDeckEndOfStream = fmt.Errorf("%w: stream ended", ErrDeck)

// DeckError carries the substrate-side discriminator alongside the
// human-readable message. The `.Kind` field matches the
// `<<deck-sdk-kind:KIND>>MSG` envelope ("register_failed", "queue_full",
// "already_shutdown", "snapshot_serialize_failed", "invalid_argument",
// "runtime_panic").
type DeckError struct {
	Kind string
	Msg  string
}

func (e *DeckError) Error() string {
	if e.Kind == "" {
		return fmt.Sprintf("deck: %s", e.Msg)
	}
	return fmt.Sprintf("deck: %s: %s", e.Kind, e.Msg)
}

func (e *DeckError) Unwrap() error { return ErrDeck }

// lastError reads the per-thread last-error pair populated on every
// non-OK FFI return. Returns nil if no error has been recorded on the
// calling thread (which would be unusual after a non-OK return).
func lastError(rc C.int) error {
	switch rc {
	case netDeckOK:
		return nil
	case netDeckErrEndOfStream:
		return ErrDeckEndOfStream
	}
	var kind, msg string
	if k := C.net_deck_last_error_kind(); k != nil {
		kind = C.GoString(k)
	}
	if m := C.net_deck_last_error_message(); m != nil {
		msg = C.GoString(m)
	}
	if msg == "" {
		msg = fmt.Sprintf("rc=%d", int(rc))
	}
	return &DeckError{Kind: kind, Msg: msg}
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

// DeckClientConfig — operator-facing client config. Zero values pick
// the substrate defaults; document each field's default at the
// substrate. See `net_deck.h:211-247` for the canonical defaults.
type DeckClientConfig struct {
	// ThisNode — supervisor's local node id.
	ThisNode uint64
	// TickIntervalMs — reconcile cadence. 0 = substrate default.
	TickIntervalMs uint64
	// EventQueueCapacity — supervisor event-source mpsc cap. 0 = default.
	EventQueueCapacity uint64
	// ActionQueueCapacity — executor mpsc cap. 0 = default.
	ActionQueueCapacity uint64
	// SnapshotPollIntervalMs — poll cadence for the snapshot stream. 0 = default.
	SnapshotPollIntervalMs uint64
	// IceSignatureThreshold — ICE M-of-N threshold. 0 = default (1).
	IceSignatureThreshold uint64
}

// ChainCommit is the receipt every successful admin verb returns. The
// `EventKind` field is one of the `DeckEventKind*` constants.
type ChainCommit struct {
	CommitID      uint64
	OperatorID    uint64
	EventKind     int
	CommittedAtMs uint64
}

func chainCommitFromC(c C.NetDeckChainCommit) ChainCommit {
	return ChainCommit{
		CommitID:      uint64(c.commit_id),
		OperatorID:    uint64(c.operator_id),
		EventKind:     int(c.event_kind),
		CommittedAtMs: uint64(c.committed_at_ms),
	}
}

// PeerCounts — cluster peer-health roll-up.
type PeerCounts struct {
	Healthy     uint32
	Degraded    uint32
	Unreachable uint32
	Unknown     uint32
}

// DaemonCounts — daemon-state roll-up across the cluster.
type DaemonCounts struct {
	Running      uint32
	Starting     uint32
	Stopping     uint32
	Stopped      uint32
	BackingOff   uint32
	CrashLooping uint32
}

// StatusSummary — rolled-up cluster health snapshot. `FreezeRemainingMs`
// is meaningful only when `FreezeRemainingPresent` is true.
type StatusSummary struct {
	Peers                  PeerCounts
	Daemons                DaemonCounts
	ReplicaChains          uint32
	AvoidListEntries       uint32
	RecentlyEmittedCount   uint32
	RecentFailureCount     uint32
	AdminAuditRingDepth    uint32
	FreezeRemainingPresent bool
	FreezeRemainingMs      uint64
	LocalMaintenanceActive bool
}

func statusSummaryFromC(s C.NetDeckStatusSummary) StatusSummary {
	return StatusSummary{
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
		FreezeRemainingPresent: s.freeze_remaining_present != 0,
		FreezeRemainingMs:      uint64(s.freeze_remaining_ms),
		LocalMaintenanceActive: s.local_maintenance_active != 0,
	}
}

// ---------------------------------------------------------------------------
// DeckClient lifecycle
// ---------------------------------------------------------------------------

// DeckClient wraps the C `*NetDeckClient`. Construct via NewDeckClient;
// pair with `defer client.Free()` for deterministic cleanup.
type DeckClient struct {
	mu     sync.RWMutex
	handle *C.NetDeckClient
}

// NewDeckClient constructs a deck client owning a private supervisor
// runtime. `operatorSeed` must be exactly 32 bytes of ed25519 seed
// material. The substrate derives the operator id from its origin
// hash.
//
// The cdylib zeroes its transient stack copy of the seed before
// returning, but the caller's buffer is untouched — wipe it yourself
// (`for i := range operatorSeed { operatorSeed[i] = 0 }`) after
// constructing the client.
func NewDeckClient(operatorSeed []byte, cfg DeckClientConfig) (*DeckClient, error) {
	if len(operatorSeed) != 32 {
		return nil, fmt.Errorf("%w: operatorSeed must be 32 bytes (got %d)",
			ErrDeck, len(operatorSeed))
	}
	var out *C.NetDeckClient
	rc := C.net_deck_client_new(
		C.uint64_t(cfg.ThisNode),
		C.uint64_t(cfg.TickIntervalMs),
		C.size_t(cfg.EventQueueCapacity),
		C.size_t(cfg.ActionQueueCapacity),
		C.uint64_t(cfg.SnapshotPollIntervalMs),
		C.size_t(cfg.IceSignatureThreshold),
		(*C.uint8_t)(unsafe.Pointer(&operatorSeed[0])),
		&out,
	)
	runtime.KeepAlive(operatorSeed)
	if rc != netDeckOK {
		return nil, lastError(rc)
	}
	c := &DeckClient{handle: out}
	runtime.SetFinalizer(c, func(c *DeckClient) { c.Free() })
	return c, nil
}

// Free releases the underlying handle and tears down the private
// supervisor runtime. Idempotent.
func (c *DeckClient) Free() {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.handle != nil {
		C.net_deck_client_free(c.handle)
		c.handle = nil
		runtime.SetFinalizer(c, nil)
	}
}

// OperatorID returns the operator identifier bound to this client.
// Returns 0 if the client is closed.
func (c *DeckClient) OperatorID() uint64 {
	c.mu.RLock()
	defer c.mu.RUnlock()
	if c.handle == nil {
		return 0
	}
	return uint64(C.net_deck_client_operator_id(c.handle))
}

// ---------------------------------------------------------------------------
// One-shot reads
// ---------------------------------------------------------------------------

// Status returns the latest `MeshOsSnapshot` parsed from its JSON
// representation.
func (c *DeckClient) Status() (map[string]any, error) {
	c.mu.RLock()
	defer c.mu.RUnlock()
	if c.handle == nil {
		return nil, fmt.Errorf("%w: client closed", ErrDeck)
	}
	cStr := C.net_deck_status(c.handle)
	if cStr == nil {
		return nil, lastError(netDeckErrCallFailed)
	}
	defer C.net_deck_free_string(cStr)
	js := C.GoString(cStr)
	var out map[string]any
	if err := json.Unmarshal([]byte(js), &out); err != nil {
		return nil, fmt.Errorf("%w: decode status: %v", ErrDeck, err)
	}
	return out, nil
}

// StatusSummary reads the rolled-up cluster summary.
func (c *DeckClient) StatusSummary() (StatusSummary, error) {
	c.mu.RLock()
	defer c.mu.RUnlock()
	if c.handle == nil {
		return StatusSummary{}, fmt.Errorf("%w: client closed", ErrDeck)
	}
	var raw C.NetDeckStatusSummary
	rc := C.net_deck_status_summary(c.handle, &raw)
	if rc != netDeckOK {
		return StatusSummary{}, lastError(rc)
	}
	return statusSummaryFromC(raw), nil
}

// ---------------------------------------------------------------------------
// AdminCommands — 9 verbs
// ---------------------------------------------------------------------------

// adminCall is the shared shape for the simple admin verbs (those that
// take only the client + a node id and return a ChainCommit).
func (c *DeckClient) adminCall(
	verb string,
	call func(*C.NetDeckClient, *C.NetDeckChainCommit) C.int,
) (ChainCommit, error) {
	c.mu.RLock()
	defer c.mu.RUnlock()
	if c.handle == nil {
		return ChainCommit{}, fmt.Errorf("%w: client closed (%s)", ErrDeck, verb)
	}
	var commit C.NetDeckChainCommit
	rc := call(c.handle, &commit)
	if rc != netDeckOK {
		return ChainCommit{}, lastError(rc)
	}
	return chainCommitFromC(commit), nil
}

// Drain emits a `Drain { node, drain_for_ms }` admin event.
func (c *DeckClient) Drain(node, drainForMs uint64) (ChainCommit, error) {
	return c.adminCall("drain", func(h *C.NetDeckClient, out *C.NetDeckChainCommit) C.int {
		return C.net_deck_admin_drain(h, C.uint64_t(node), C.uint64_t(drainForMs), out)
	})
}

// EnterMaintenance places `node` into maintenance. Pass `nil` for
// `drainForMs` to use the substrate's default deadline.
func (c *DeckClient) EnterMaintenance(node uint64, drainForMs *uint64) (ChainCommit, error) {
	var ms C.uint64_t
	has := C.int(0)
	if drainForMs != nil {
		ms = C.uint64_t(*drainForMs)
		has = 1
	}
	return c.adminCall("enter_maintenance", func(h *C.NetDeckClient, out *C.NetDeckChainCommit) C.int {
		return C.net_deck_admin_enter_maintenance(h, C.uint64_t(node), ms, has, out)
	})
}

// ExitMaintenance brings `node` back online.
func (c *DeckClient) ExitMaintenance(node uint64) (ChainCommit, error) {
	return c.adminCall("exit_maintenance", func(h *C.NetDeckClient, out *C.NetDeckChainCommit) C.int {
		return C.net_deck_admin_exit_maintenance(h, C.uint64_t(node), out)
	})
}

// Cordon marks `node` as cordoned (no new placements).
func (c *DeckClient) Cordon(node uint64) (ChainCommit, error) {
	return c.adminCall("cordon", func(h *C.NetDeckClient, out *C.NetDeckChainCommit) C.int {
		return C.net_deck_admin_cordon(h, C.uint64_t(node), out)
	})
}

// Uncordon clears a cordon.
func (c *DeckClient) Uncordon(node uint64) (ChainCommit, error) {
	return c.adminCall("uncordon", func(h *C.NetDeckClient, out *C.NetDeckChainCommit) C.int {
		return C.net_deck_admin_uncordon(h, C.uint64_t(node), out)
	})
}

// DropReplicas drops the `node`'s replica roles for the listed chains.
// Pass `nil` or an empty slice to drop all replicas on the node.
func (c *DeckClient) DropReplicas(node uint64, chains []uint64) (ChainCommit, error) {
	c.mu.RLock()
	defer c.mu.RUnlock()
	if c.handle == nil {
		return ChainCommit{}, fmt.Errorf("%w: client closed (drop_replicas)", ErrDeck)
	}
	var ptr *C.uint64_t
	if len(chains) > 0 {
		ptr = (*C.uint64_t)(unsafe.Pointer(&chains[0]))
	}
	var commit C.NetDeckChainCommit
	rc := C.net_deck_admin_drop_replicas(
		c.handle, C.uint64_t(node), ptr, C.size_t(len(chains)), &commit,
	)
	runtime.KeepAlive(chains)
	if rc != netDeckOK {
		return ChainCommit{}, lastError(rc)
	}
	return chainCommitFromC(commit), nil
}

// InvalidatePlacement marks the `node`'s placement-cache entry stale.
func (c *DeckClient) InvalidatePlacement(node uint64) (ChainCommit, error) {
	return c.adminCall("invalidate_placement", func(h *C.NetDeckClient, out *C.NetDeckChainCommit) C.int {
		return C.net_deck_admin_invalidate_placement(h, C.uint64_t(node), out)
	})
}

// RestartAllDaemons emits a restart-all event for `node`.
func (c *DeckClient) RestartAllDaemons(node uint64) (ChainCommit, error) {
	return c.adminCall("restart_all_daemons", func(h *C.NetDeckClient, out *C.NetDeckChainCommit) C.int {
		return C.net_deck_admin_restart_all_daemons(h, C.uint64_t(node), out)
	})
}

// ClearAvoidList clears `node`'s avoid list.
func (c *DeckClient) ClearAvoidList(node uint64) (ChainCommit, error) {
	return c.adminCall("clear_avoid_list", func(h *C.NetDeckClient, out *C.NetDeckChainCommit) C.int {
		return C.net_deck_admin_clear_avoid_list(h, C.uint64_t(node), out)
	})
}

// ---------------------------------------------------------------------------
// Snapshot stream
// ---------------------------------------------------------------------------

// DeckSnapshotStream is a live stream of `MeshOsSnapshot` JSON
// documents.
type DeckSnapshotStream struct {
	mu     sync.RWMutex
	handle *C.NetDeckSnapshotStream
}

// SubscribeSnapshots opens a live snapshot stream.
func (c *DeckClient) SubscribeSnapshots() (*DeckSnapshotStream, error) {
	c.mu.RLock()
	defer c.mu.RUnlock()
	if c.handle == nil {
		return nil, fmt.Errorf("%w: client closed", ErrDeck)
	}
	var out *C.NetDeckSnapshotStream
	rc := C.net_deck_subscribe_snapshots(c.handle, &out)
	if rc != netDeckOK {
		return nil, lastError(rc)
	}
	s := &DeckSnapshotStream{handle: out}
	runtime.SetFinalizer(s, func(s *DeckSnapshotStream) { s.Free() })
	return s, nil
}

// Next blocks up to `timeoutMs` for the next snapshot. Returns
// `(nil, nil)` on timeout. Returns `ErrDeckEndOfStream` when the
// stream has been drained. Pass 0 for an unbounded wait.
func (s *DeckSnapshotStream) Next(timeoutMs uint64) (map[string]any, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	if s.handle == nil {
		return nil, fmt.Errorf("%w: stream closed", ErrDeck)
	}
	var c *C.char
	rc := C.net_deck_snapshot_stream_next(s.handle, C.uint64_t(timeoutMs), &c)
	if rc == netDeckErrEndOfStream {
		return nil, ErrDeckEndOfStream
	}
	if rc != netDeckOK {
		return nil, lastError(rc)
	}
	if c == nil {
		return nil, nil // timeout
	}
	defer C.net_deck_free_string(c)
	js := C.GoString(c)
	var out map[string]any
	if err := json.Unmarshal([]byte(js), &out); err != nil {
		return nil, fmt.Errorf("%w: decode snapshot: %v", ErrDeck, err)
	}
	return out, nil
}

// Free releases the stream handle. Idempotent.
func (s *DeckSnapshotStream) Free() {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.handle != nil {
		C.net_deck_snapshot_stream_free(s.handle)
		s.handle = nil
		runtime.SetFinalizer(s, nil)
	}
}

// ---------------------------------------------------------------------------
// Status-summary stream
// ---------------------------------------------------------------------------

// DeckStatusSummaryStream is a live stream of rolled-up cluster
// summaries.
type DeckStatusSummaryStream struct {
	mu     sync.RWMutex
	handle *C.NetDeckStatusSummaryStream
}

// SubscribeStatusSummaries opens a live status-summary stream.
func (c *DeckClient) SubscribeStatusSummaries() (*DeckStatusSummaryStream, error) {
	c.mu.RLock()
	defer c.mu.RUnlock()
	if c.handle == nil {
		return nil, fmt.Errorf("%w: client closed", ErrDeck)
	}
	var out *C.NetDeckStatusSummaryStream
	rc := C.net_deck_subscribe_status_summaries(c.handle, &out)
	if rc != netDeckOK {
		return nil, lastError(rc)
	}
	s := &DeckStatusSummaryStream{handle: out}
	runtime.SetFinalizer(s, func(s *DeckStatusSummaryStream) { s.Free() })
	return s, nil
}

// Next blocks up to `timeoutMs` for the next status summary. Returns
// `(nil, nil)` on timeout. Returns `ErrDeckEndOfStream` when drained.
func (s *DeckStatusSummaryStream) Next(timeoutMs uint64) (*StatusSummary, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	if s.handle == nil {
		return nil, fmt.Errorf("%w: stream closed", ErrDeck)
	}
	var raw C.NetDeckStatusSummary
	var has C.int
	rc := C.net_deck_status_summary_stream_next(s.handle, C.uint64_t(timeoutMs), &raw, &has)
	if rc == netDeckErrEndOfStream {
		return nil, ErrDeckEndOfStream
	}
	if rc != netDeckOK {
		return nil, lastError(rc)
	}
	if has == 0 {
		return nil, nil // timeout
	}
	sum := statusSummaryFromC(raw)
	return &sum, nil
}

// Free releases the stream handle. Idempotent.
func (s *DeckStatusSummaryStream) Free() {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.handle != nil {
		C.net_deck_status_summary_stream_free(s.handle)
		s.handle = nil
		runtime.SetFinalizer(s, nil)
	}
}
