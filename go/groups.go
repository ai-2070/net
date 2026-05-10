// Groups surface — Stage 4 of SDK_GROUPS_SURFACE_PLAN.md.
//
// ReplicaGroup, ForkGroup, StandbyGroup overlays on top of
// DaemonRuntime. Each group delegates to the same SDK wrappers
// the Node + Python bindings use; the Go side is a thin C-ABI
// shim with runtime.SetFinalizer discipline matching the existing
// `DaemonRuntime` / `MigrationHandle` handle pattern.
//
// Error surface: all methods that can fail return a typed
// *GroupError wrapping a DaemonError. Use `errors.As(err, &ge)`
// to recover the discriminator, or just read `ge.Kind`.
package net

/*
#include "net.h"
#include <stdlib.h>
*/
import "C"

import (
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"runtime"
	"strings"
	"sync"
	"unsafe"
)

// ------------------------------------------------------------------------
// GroupError — typed subclass of DaemonError
// ------------------------------------------------------------------------

// GroupErrorKind is the stable discriminator parsed from the
// Rust side's `daemon: group: <kind>[: detail]` prefix.
type GroupErrorKind string

const (
	GroupErrNotReady         GroupErrorKind = "not-ready"
	GroupErrFactoryNotFound  GroupErrorKind = "factory-not-found"
	GroupErrNoHealthyMember  GroupErrorKind = "no-healthy-member"
	GroupErrPlacementFailed  GroupErrorKind = "placement-failed"
	GroupErrRegistryFailed   GroupErrorKind = "registry-failed"
	GroupErrInvalidConfig    GroupErrorKind = "invalid-config"
	GroupErrDaemon           GroupErrorKind = "daemon"
	GroupErrUnknown          GroupErrorKind = "unknown"
)

// GroupError is the typed failure returned by group methods.
// Embeds *DaemonError so callers catching the broader type still
// match.
type GroupError struct {
	*DaemonError
	Kind   GroupErrorKind
	Detail string
}

// Error implements the error interface.
func (e *GroupError) Error() string {
	if e.DaemonError != nil {
		return e.DaemonError.Error()
	}
	return "group error"
}

// Unwrap returns the embedded DaemonError so errors.As reaches it.
func (e *GroupError) Unwrap() error { return e.DaemonError }

// parseGroupError tries to lift a DaemonError into a typed
// GroupError by parsing its "daemon: group: <kind>[: detail]"
// prefix. Returns nil if the message doesn't match that shape.
func parseGroupError(d *DaemonError) *GroupError {
	if d == nil {
		return nil
	}
	// DaemonError.Message is already stripped of "daemon: " —
	// starts with "group: ..." for typed errors.
	const prefix = "group:"
	msg := d.Message
	if !strings.HasPrefix(msg, prefix) {
		return nil
	}
	body := strings.TrimSpace(msg[len(prefix):])
	kind := body
	var detail string
	if i := strings.Index(body, ":"); i != -1 {
		kind = strings.TrimSpace(body[:i])
		detail = strings.TrimSpace(body[i+1:])
	}
	k := GroupErrorKind(kind)
	switch k {
	case GroupErrNotReady, GroupErrFactoryNotFound, GroupErrNoHealthyMember,
		GroupErrPlacementFailed, GroupErrRegistryFailed, GroupErrInvalidConfig,
		GroupErrDaemon:
		// Known tag.
	default:
		k = GroupErrUnknown
	}
	return &GroupError{DaemonError: d, Kind: k, Detail: detail}
}

// groupErr lifts a compute-ffi return code into a typed
// *GroupError. Non-group errors fall through to *DaemonError.
func groupErr(code C.int, errOut *C.char) error {
	err := computeErr(code, errOut)
	if err == nil {
		return nil
	}
	var de *DaemonError
	if errors.As(err, &de) {
		if ge := parseGroupError(de); ge != nil {
			return ge
		}
	}
	return err
}

// ------------------------------------------------------------------------
// Shared types
// ------------------------------------------------------------------------

// GroupStrategy is the load-balancing strategy for inbound group
// events.
type GroupStrategy string

const (
	StrategyRoundRobin     GroupStrategy = "round-robin"
	StrategyConsistentHash GroupStrategy = "consistent-hash"
	StrategyLeastLoad      GroupStrategy = "least-load"
	StrategyLeastConns     GroupStrategy = "least-connections"
	StrategyRandom         GroupStrategy = "random"
)

// GroupHealth is the aggregate health of a group.
type GroupHealth struct {
	// Status: "healthy" | "degraded" | "dead".
	Status string
	// Populated on "degraded".
	Healthy uint32
	Total   uint32
}

// GroupMemberInfo is one member's metadata within a group.
type GroupMemberInfo struct {
	Index      uint8
	OriginHash uint64
	NodeID     uint64
	EntityID   []byte
	Healthy    bool
}

// GroupForkRecord is one fork's lineage record. See the core
// `ForkRecord` for semantics.
type GroupForkRecord struct {
	OriginalOrigin  uint64
	ForkedOrigin    uint64
	ForkSeq         uint64
	FromSnapshotSeq *uint64
}

// GroupHostConfig is the per-member host configuration applied
// to every group member (zero fields = runtime defaults).
type GroupHostConfig struct {
	AutoSnapshotInterval uint64
	MaxLogEntries        uint32
}

// ReplicaGroupConfig is the spawn-time config for a ReplicaGroup.
type ReplicaGroupConfig struct {
	ReplicaCount uint8
	// GroupSeed must be exactly 32 bytes.
	GroupSeed  []byte
	LBStrategy GroupStrategy
	HostConfig *GroupHostConfig
}

// ForkGroupConfig is the spawn-time config for a ForkGroup.
type ForkGroupConfig struct {
	ForkCount  uint8
	LBStrategy GroupStrategy
	HostConfig *GroupHostConfig
}

// StandbyGroupConfig is the spawn-time config for a StandbyGroup.
type StandbyGroupConfig struct {
	MemberCount uint8
	// GroupSeed must be exactly 32 bytes.
	GroupSeed  []byte
	HostConfig *GroupHostConfig
}

// ------------------------------------------------------------------------
// Helpers
// ------------------------------------------------------------------------

func hostConfigFields(c *GroupHostConfig) (C.uint64_t, C.uint32_t) {
	if c == nil {
		return 0, 0
	}
	return C.uint64_t(c.AutoSnapshotInterval), C.uint32_t(c.MaxLogEntries)
}

func strategyOrDefault(s GroupStrategy) GroupStrategy {
	if s == "" {
		return StrategyRoundRobin
	}
	return s
}

func statusString(code C.int) string {
	switch code {
	case 0:
		return "healthy"
	case 1:
		return "degraded"
	default:
		return "dead"
	}
}

// parseMembersJSON turns the Rust-side JSON member array into
// typed Go structs.
func parseMembersJSON(jsonStr string) []GroupMemberInfo {
	if jsonStr == "" || jsonStr == "[]" {
		return nil
	}
	// Parse as an array of maps; the JSON uses snake_case keys
	// and stores entity_id as a hex string.
	var raw []struct {
		Index      uint8  `json:"index"`
		OriginHash uint64 `json:"origin_hash"`
		NodeID     uint64 `json:"node_id"`
		EntityID   string `json:"entity_id"`
		Healthy    bool   `json:"healthy"`
	}
	if err := json.Unmarshal([]byte(jsonStr), &raw); err != nil {
		return nil
	}
	out := make([]GroupMemberInfo, len(raw))
	for i, r := range raw {
		eid, _ := hex.DecodeString(r.EntityID)
		out[i] = GroupMemberInfo{
			Index:      r.Index,
			OriginHash: r.OriginHash,
			NodeID:     r.NodeID,
			EntityID:   eid,
			Healthy:    r.Healthy,
		}
	}
	return out
}

func parseForkRecordsJSON(jsonStr string) []GroupForkRecord {
	if jsonStr == "" || jsonStr == "[]" {
		return nil
	}
	var raw []struct {
		OriginalOrigin  uint64  `json:"original_origin"`
		ForkedOrigin    uint64  `json:"forked_origin"`
		ForkSeq         uint64  `json:"fork_seq"`
		FromSnapshotSeq *uint64 `json:"from_snapshot_seq"`
	}
	if err := json.Unmarshal([]byte(jsonStr), &raw); err != nil {
		return nil
	}
	out := make([]GroupForkRecord, len(raw))
	for i, r := range raw {
		out[i] = GroupForkRecord{
			OriginalOrigin:  r.OriginalOrigin,
			ForkedOrigin:    r.ForkedOrigin,
			ForkSeq:         r.ForkSeq,
			FromSnapshotSeq: r.FromSnapshotSeq,
		}
	}
	return out
}

// cKindBytes converts a Go string to a `(*C.char, C.size_t)` pair
// suitable for the len-prefixed strings the FFI expects. The
// pointer is valid for the lifetime of the caller-owned byte
// slice; callers use `runtime.KeepAlive` to pin it.
func cKindBytes(s string) ([]byte, *C.char, C.size_t) {
	b := []byte(s)
	if len(b) == 0 {
		return b, nil, 0
	}
	return b, (*C.char)(unsafe.Pointer(&b[0])), C.size_t(len(b))
}

// ------------------------------------------------------------------------
// ReplicaGroup
// ------------------------------------------------------------------------

// ReplicaGroup is a Go handle to a native replica group. Call
// Close to free the handle (idempotent; also called from a
// finalizer for defensive cleanup).
type ReplicaGroup struct {
	handle *C.net_compute_replica_group_t
	mu     sync.Mutex
}

// NewReplicaGroup spawns a replica group bound to `rt`. `kind`
// must have been registered via `rt.RegisterFactoryFunc(kind, fn)`
// (or equivalent) — the group invokes the factory once per member
// at spawn, scale-up, and failure-replacement.
//
// On failure returns a *GroupError with the structured kind.
func NewReplicaGroup(rt *DaemonRuntime, kind string, cfg ReplicaGroupConfig) (*ReplicaGroup, error) {
	if rt == nil {
		return nil, &DaemonError{Message: "runtime is nil"}
	}
	if len(cfg.GroupSeed) != 32 {
		return nil, &GroupError{
			DaemonError: &DaemonError{Message: "group: invalid-config: group_seed must be 32 bytes"},
			Kind:        GroupErrInvalidConfig,
		}
	}
	kindBytes, kindPtr, kindLen := cKindBytes(kind)
	lbBytes, lbPtr, lbLen := cKindBytes(string(strategyOrDefault(cfg.LBStrategy)))
	autoSnap, maxLog := hostConfigFields(cfg.HostConfig)

	var nativeHandle *C.net_compute_replica_group_t
	var errOut *C.char
	code := C.net_compute_replica_group_spawn(
		rt.handle,
		kindPtr, kindLen,
		C.uint32_t(cfg.ReplicaCount),
		(*C.uint8_t)(unsafe.Pointer(&cfg.GroupSeed[0])),
		lbPtr, lbLen,
		autoSnap,
		maxLog,
		&nativeHandle,
		&errOut,
	)
	runtime.KeepAlive(kindBytes)
	runtime.KeepAlive(lbBytes)
	runtime.KeepAlive(cfg.GroupSeed)

	if code != C.NET_COMPUTE_OK {
		return nil, groupErr(code, errOut)
	}
	g := &ReplicaGroup{handle: nativeHandle}
	runtime.SetFinalizer(g, (*ReplicaGroup).Close)
	return g, nil
}

// Close releases the native handle. Idempotent.
func (g *ReplicaGroup) Close() {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return
	}
	C.net_compute_replica_group_free(g.handle)
	g.handle = nil
	runtime.SetFinalizer(g, nil)
}

// ReplicaCount returns the current number of replicas.
func (g *ReplicaGroup) ReplicaCount() int {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return 0
	}
	return int(C.net_compute_replica_group_replica_count(g.handle))
}

// HealthyCount returns the number of currently-healthy replicas.
func (g *ReplicaGroup) HealthyCount() int {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return 0
	}
	return int(C.net_compute_replica_group_healthy_count(g.handle))
}

// GroupID returns the deterministic 32-bit group identifier.
func (g *ReplicaGroup) GroupID() uint32 {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return 0
	}
	return uint32(C.net_compute_replica_group_group_id(g.handle))
}

// Health returns the aggregate health.
func (g *ReplicaGroup) Health() GroupHealth {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return GroupHealth{Status: "dead"}
	}
	var status C.int
	var healthy, total C.uint32_t
	C.net_compute_replica_group_health(g.handle, &status, &healthy, &total)
	return GroupHealth{Status: statusString(status), Healthy: uint32(healthy), Total: uint32(total)}
}

// RouteEvent routes to the best healthy replica and returns its
// `origin_hash`. `routingKey` is consistent-hashed for stickiness
// (pass "" to let the LB strategy pick without a key).
func (g *ReplicaGroup) RouteEvent(routingKey string) (uint64, error) {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return 0, ErrRuntimeShutDown
	}
	kb, kp, kl := cKindBytes(routingKey)
	var origin C.uint64_t
	var errOut *C.char
	code := C.net_compute_replica_group_route_event(g.handle, kp, kl, &origin, &errOut)
	runtime.KeepAlive(kb)
	if code != C.NET_COMPUTE_OK {
		return 0, groupErr(code, errOut)
	}
	return uint64(origin), nil
}

// ScaleTo resizes the group to `n` replicas. Growing calls the
// factory once per new replica; shrinking unregisters members in
// reverse index order.
func (g *ReplicaGroup) ScaleTo(n uint8) error {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return ErrRuntimeShutDown
	}
	var errOut *C.char
	code := C.net_compute_replica_group_scale_to(g.handle, C.uint32_t(n), &errOut)
	return groupErr(code, errOut)
}

// OnNodeRecovery re-marks members still alive on `nodeID` as
// healthy. Idempotent.
func (g *ReplicaGroup) OnNodeRecovery(nodeID uint64) {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return
	}
	C.net_compute_replica_group_on_node_recovery(g.handle, C.uint64_t(nodeID))
}

// Replicas returns the current member roster (owned copy).
func (g *ReplicaGroup) Replicas() []GroupMemberInfo {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return nil
	}
	c := C.net_compute_replica_group_members_json(g.handle)
	if c == nil {
		return nil
	}
	defer C.net_compute_free_cstring(c)
	return parseMembersJSON(C.GoString(c))
}

// ------------------------------------------------------------------------
// ForkGroup
// ------------------------------------------------------------------------

// ForkGroup is a Go handle to a native fork group.
type ForkGroup struct {
	handle *C.net_compute_fork_group_t
	mu     sync.Mutex
}

// NewForkGroup forks `cfg.ForkCount` new daemons from `parentOrigin`
// at `forkSeq`. Each fork gets a unique keypair + a ForkRecord
// linking it to the parent.
func NewForkGroup(
	rt *DaemonRuntime,
	kind string,
	parentOrigin uint64,
	forkSeq uint64,
	cfg ForkGroupConfig,
) (*ForkGroup, error) {
	if rt == nil {
		return nil, &DaemonError{Message: "runtime is nil"}
	}
	kindBytes, kindPtr, kindLen := cKindBytes(kind)
	lbBytes, lbPtr, lbLen := cKindBytes(string(strategyOrDefault(cfg.LBStrategy)))
	autoSnap, maxLog := hostConfigFields(cfg.HostConfig)

	var nativeHandle *C.net_compute_fork_group_t
	var errOut *C.char
	code := C.net_compute_fork_group_spawn(
		rt.handle,
		kindPtr, kindLen,
		C.uint64_t(parentOrigin),
		C.uint64_t(forkSeq),
		C.uint32_t(cfg.ForkCount),
		lbPtr, lbLen,
		autoSnap, maxLog,
		&nativeHandle,
		&errOut,
	)
	runtime.KeepAlive(kindBytes)
	runtime.KeepAlive(lbBytes)
	if code != C.NET_COMPUTE_OK {
		return nil, groupErr(code, errOut)
	}
	g := &ForkGroup{handle: nativeHandle}
	runtime.SetFinalizer(g, (*ForkGroup).Close)
	return g, nil
}

func (g *ForkGroup) Close() {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return
	}
	C.net_compute_fork_group_free(g.handle)
	g.handle = nil
	runtime.SetFinalizer(g, nil)
}

func (g *ForkGroup) ForkCount() int {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return 0
	}
	return int(C.net_compute_fork_group_fork_count(g.handle))
}

func (g *ForkGroup) HealthyCount() int {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return 0
	}
	return int(C.net_compute_fork_group_healthy_count(g.handle))
}

func (g *ForkGroup) ParentOrigin() uint64 {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return 0
	}
	return uint64(C.net_compute_fork_group_parent_origin(g.handle))
}

func (g *ForkGroup) ForkSeq() uint64 {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return 0
	}
	return uint64(C.net_compute_fork_group_fork_seq(g.handle))
}

// VerifyLineage returns true iff every fork's ForkRecord
// verifies against its parent.
func (g *ForkGroup) VerifyLineage() bool {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return false
	}
	return C.net_compute_fork_group_verify_lineage(g.handle) == 1
}

// ScaleTo resizes the fork group to `n` members.
func (g *ForkGroup) ScaleTo(n uint8) error {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return ErrRuntimeShutDown
	}
	var errOut *C.char
	code := C.net_compute_fork_group_scale_to(g.handle, C.uint32_t(n), &errOut)
	return groupErr(code, errOut)
}

// OnNodeRecovery re-marks forks on the recovered node.
func (g *ForkGroup) OnNodeRecovery(nodeID uint64) {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return
	}
	C.net_compute_fork_group_on_node_recovery(g.handle, C.uint64_t(nodeID))
}

func (g *ForkGroup) Members() []GroupMemberInfo {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return nil
	}
	c := C.net_compute_fork_group_members_json(g.handle)
	if c == nil {
		return nil
	}
	defer C.net_compute_free_cstring(c)
	return parseMembersJSON(C.GoString(c))
}

func (g *ForkGroup) ForkRecords() []GroupForkRecord {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return nil
	}
	c := C.net_compute_fork_group_fork_records_json(g.handle)
	if c == nil {
		return nil
	}
	defer C.net_compute_free_cstring(c)
	return parseForkRecordsJSON(C.GoString(c))
}

// ------------------------------------------------------------------------
// StandbyGroup
// ------------------------------------------------------------------------

// StandbyGroup is a Go handle to a native standby group.
type StandbyGroup struct {
	handle *C.net_compute_standby_group_t
	mu     sync.Mutex
}

// NewStandbyGroup spawns a standby group. Member 0 starts as
// active; members 1..N-1 are standbys.
func NewStandbyGroup(rt *DaemonRuntime, kind string, cfg StandbyGroupConfig) (*StandbyGroup, error) {
	if rt == nil {
		return nil, &DaemonError{Message: "runtime is nil"}
	}
	if len(cfg.GroupSeed) != 32 {
		return nil, &GroupError{
			DaemonError: &DaemonError{Message: "group: invalid-config: group_seed must be 32 bytes"},
			Kind:        GroupErrInvalidConfig,
		}
	}
	kindBytes, kindPtr, kindLen := cKindBytes(kind)
	autoSnap, maxLog := hostConfigFields(cfg.HostConfig)

	var nativeHandle *C.net_compute_standby_group_t
	var errOut *C.char
	code := C.net_compute_standby_group_spawn(
		rt.handle,
		kindPtr, kindLen,
		C.uint32_t(cfg.MemberCount),
		(*C.uint8_t)(unsafe.Pointer(&cfg.GroupSeed[0])),
		autoSnap, maxLog,
		&nativeHandle,
		&errOut,
	)
	runtime.KeepAlive(kindBytes)
	runtime.KeepAlive(cfg.GroupSeed)
	if code != C.NET_COMPUTE_OK {
		return nil, groupErr(code, errOut)
	}
	g := &StandbyGroup{handle: nativeHandle}
	runtime.SetFinalizer(g, (*StandbyGroup).Close)
	return g, nil
}

func (g *StandbyGroup) Close() {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return
	}
	C.net_compute_standby_group_free(g.handle)
	g.handle = nil
	runtime.SetFinalizer(g, nil)
}

func (g *StandbyGroup) MemberCount() int {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return 0
	}
	return int(C.net_compute_standby_group_member_count(g.handle))
}

func (g *StandbyGroup) StandbyCount() int {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return 0
	}
	return int(C.net_compute_standby_group_standby_count(g.handle))
}

func (g *StandbyGroup) ActiveIndex() int {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return 0
	}
	return int(C.net_compute_standby_group_active_index(g.handle))
}

func (g *StandbyGroup) ActiveOrigin() uint64 {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return 0
	}
	return uint64(C.net_compute_standby_group_active_origin(g.handle))
}

func (g *StandbyGroup) ActiveHealthy() bool {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return false
	}
	return C.net_compute_standby_group_active_healthy(g.handle) == 1
}

func (g *StandbyGroup) GroupID() uint32 {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return 0
	}
	return uint32(C.net_compute_standby_group_group_id(g.handle))
}

func (g *StandbyGroup) BufferedEventCount() int {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return 0
	}
	return int(C.net_compute_standby_group_buffered_event_count(g.handle))
}

// SyncStandbys snapshots the active and pushes to every standby.
// Returns the sequence the sync caught up through.
func (g *StandbyGroup) SyncStandbys() (uint64, error) {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return 0, ErrRuntimeShutDown
	}
	var out C.uint64_t
	var errOut *C.char
	code := C.net_compute_standby_group_sync_standbys(g.handle, &out, &errOut)
	if code != C.NET_COMPUTE_OK {
		return 0, groupErr(code, errOut)
	}
	return uint64(out), nil
}

// Promote promotes the most-synced standby to active.
func (g *StandbyGroup) Promote() (uint64, error) {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return 0, ErrRuntimeShutDown
	}
	var out C.uint64_t
	var errOut *C.char
	code := C.net_compute_standby_group_promote(g.handle, &out, &errOut)
	if code != C.NET_COMPUTE_OK {
		return 0, groupErr(code, errOut)
	}
	return uint64(out), nil
}

func (g *StandbyGroup) OnNodeRecovery(nodeID uint64) {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return
	}
	C.net_compute_standby_group_on_node_recovery(g.handle, C.uint64_t(nodeID))
}

func (g *StandbyGroup) Members() []GroupMemberInfo {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return nil
	}
	c := C.net_compute_standby_group_members_json(g.handle)
	if c == nil {
		return nil
	}
	defer C.net_compute_free_cstring(c)
	return parseMembersJSON(C.GoString(c))
}

// MemberRole returns "active" | "standby" | "" (out-of-range).
func (g *StandbyGroup) MemberRole(index uint8) string {
	g.mu.Lock()
	defer g.mu.Unlock()
	if g.handle == nil {
		return ""
	}
	c := C.net_compute_standby_group_member_role(g.handle, C.uint32_t(index))
	if c == nil {
		return ""
	}
	defer C.net_compute_free_cstring(c)
	return C.GoString(c)
}

var _ = fmt.Sprintf
