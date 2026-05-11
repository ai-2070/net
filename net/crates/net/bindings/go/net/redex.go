// Package net — Redex (storage + replication) consumer wrapper for
// the C ABI exported by the `net::ffi::cortex` module in the main
// `net` crate.
//
// This file is a **reference implementation** documenting the
// expected Go-side surface for consumers of `libnet` (the same
// cdylib the mesh / nRPC / capability surfaces consume). The
// upstream `net` repo owns the C ABI side and ships this file as
// the canonical contract for what the cgo wrapper should look
// like; downstream binding trees follow the same shape.
//
// # Build prerequisites
//
//   - Build the main `net` crate as a cdylib with the cortex feature:
//
//     cd net/crates/net
//     cargo build --release --features "net netdb redex-disk"
//
//   - Add to your CGO flags:
//
//     #cgo LDFLAGS: -L/path/to/target/release -lnet
//     #cgo darwin LDFLAGS: -framework Security -framework CoreFoundation
//
// # Surface scope
//
// This wrapper covers the operator-facing replication surface that
// landed alongside the Phase I Go binding work in
// `docs/plans/REDEX_DISTRIBUTED_PLAN.md`:
//
//   - `Redex.New() / Redex.NewWithPersistentDir(dir) / Redex.Close()`
//   - `Redex.EnableReplication(mesh)`
//   - `Redex.OpenFile(name, RedexFileConfig)` with full replication
//     config in JSON
//   - `Redex.ReplicationRuntimeCount()`
//   - `Redex.ReplicationPrometheusText()`
//   - `RedexFile.Append(bytes) -> seq`
//   - `RedexFile.NextSeq()`
//   - `RedexFile.Close()`
//
// Out of scope for v1 (per `REDEX_GO_FFI_DESIGN.md`): streaming
// tail, structured status snapshot, per-channel coordinator
// handle. The `read_range` path is the existing
// `net_redex_file_read_range` symbol; this reference impl skips
// the byte-buffer parsing wrapper (the existing FFI already
// supports it; a Go consumer adds the parser).
//
// # Lifecycle pattern (mirrors compute.go / mesh_rpc.go)
//
//   - `redex := net.NewRedex()` (or `NewRedexWithPersistentDir(dir)`)
//     takes a heap-allocated `*RedexHandle` and installs a
//     runtime finalizer. Call `redex.Close()` for deterministic
//     cleanup.
//   - `redex.EnableReplication(mesh)` consumes an `*Arc<MeshNode>`
//     handle from the upstream mesh binding's `net_mesh_arc_clone`.
//     Idempotent on repeated calls.
//   - `redex.OpenFile(name, cfg)` returns a `*RedexFile` whose
//     finalizer + explicit `Close()` mirrors the mesh handle pattern.
//
// # Replication config
//
// `RedexFileConfig` carries an optional `Replication` field. When
// set, `OpenFile` requires `EnableReplication` to have been called
// first — otherwise the call returns `ErrReplicationRequiresEnable`.
//
//   cfg := &RedexFileConfig{
//       Replication: &ReplicationConfig{
//           Factor:                    3,
//           HeartbeatMs:               500,
//           Placement:                 PlacementStandard,
//           OnUnderCapacity:           UnderCapacityWithdraw,
//           ReplicationBudgetFraction: 0.5,
//       },
//   }
//   file, err := redex.OpenFile("my/channel", cfg)
//
// # Error model
//
// Operations return Go errors built from the FFI's negative `c_int`
// return codes. The same `NetError` constants from `ffi/mod.rs`
// surface as `ErrNull`, `ErrShuttingDown`, etc. — the `redex:`
// prefix on the substrate side is preserved as a Go error
// `errors.Is(err, ErrRedex)` discriminator.
package net

/*
#include <stdint.h>
#include <stdlib.h>

// Forward-declared opaque handle types from `libnet`.
// R-43: `ArcMeshNode` is locally declared as the opaque shape of
// `*mut Arc<MeshNode>` in Rust. The upstream header generated
// from `net::ffi::mesh` calls the same type `net_compute_mesh_arc_t`
// — both name the same underlying pointer to a Rust `Arc<MeshNode>`,
// and any C consumer reading both headers should treat them as
// interchangeable opaque types.
typedef struct RedexHandle RedexHandle;
typedef struct RedexFileHandle RedexFileHandle;
typedef struct ArcMeshNode ArcMeshNode;

// Imported FFI surface from `net::ffi::cortex`.
extern RedexHandle* net_redex_new(const char* persistent_dir);
extern void net_redex_free(RedexHandle* handle);
extern int net_redex_enable_replication(RedexHandle* redex, ArcMeshNode* mesh_arc);
extern uint32_t net_redex_replication_runtime_count(const RedexHandle* redex);
extern char* net_redex_replication_prometheus_text(const RedexHandle* redex);

// Greedy-LRU dataforts operator surface — DATAFORTS_PLAN § Phase 1.
extern int net_redex_enable_greedy_dataforts(
    RedexHandle* redex,
    ArcMeshNode* mesh_arc,
    const char* config_json
);
extern int net_redex_disable_greedy_dataforts(RedexHandle* redex);
extern uint32_t net_redex_greedy_cached_channel_count(const RedexHandle* redex);
extern char* net_redex_greedy_prometheus_text(const RedexHandle* redex);

// Data-gravity heat-counter layer (DATAFORTS_PLAN § Phase 4).
extern int net_redex_enable_gravity_for_greedy(
    RedexHandle* redex,
    ArcMeshNode* mesh_arc,
    const char* config_json
);
extern int net_redex_disable_gravity_for_greedy(RedexHandle* redex);

extern int net_redex_open_file(
    RedexHandle* redex,
    const char* name,
    const char* config_json,
    RedexFileHandle** out_handle
);
extern void net_redex_file_free(RedexFileHandle* handle);
extern int net_redex_file_append(
    RedexFileHandle* handle,
    const uint8_t* payload,
    size_t payload_len,
    uint64_t* out_seq
);
extern uint64_t net_redex_file_len(RedexFileHandle* handle);
extern int net_redex_file_close(RedexFileHandle* handle);

// `net_free_string` is the shared CString free path used across
// the cortex + mesh FFI surfaces.
extern void net_free_string(char* s);

// `net_mesh_arc_clone` returns the `*ArcMeshNode` handle this Go
// file consumes for `EnableReplication`. Declared here so a Go
// consumer doesn't have to import the mesh binding's cgo block
// transitively.
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

// =====================================================================
// Errors
// =====================================================================

// ErrRedex is the umbrella error for any failure surfaced by the
// `net_redex_*` FFI. Use `errors.Is(err, ErrRedex)` to detect any
// Redex-specific failure regardless of the underlying typed kind.
var ErrRedex = errors.New("redex")

// ErrReplicationRequiresEnable is returned by `OpenFile` when the
// `RedexFileConfig.Replication` field is set but
// `EnableReplication` was never called on the owning `Redex`.
var ErrReplicationRequiresEnable = fmt.Errorf("%w: replication requires Redex.EnableReplication(mesh)", ErrRedex)

// ErrInvalidReplicationConfig is returned when the
// `ReplicationConfig` fails validation (factor below
// REPLICATION_FACTOR_MIN, unknown placement strategy, pinned
// without nodes, etc.).
var ErrInvalidReplicationConfig = fmt.Errorf("%w: invalid replication config", ErrRedex)

// =====================================================================
// Handle types
// =====================================================================

// Redex wraps the C `*RedexHandle`. Cheap to share via the Go
// runtime; calls take an internal lock around `Close()` to serialize
// the FFI `_free` against any concurrent in-flight op (the
// underlying `HandleGuard` quiesces in-flight ops too, but the
// Go-side mutex makes the close path deterministic from the Go
// caller's POV).
type Redex struct {
	mu     sync.Mutex
	handle *C.RedexHandle
}

// RedexFile wraps the C `*RedexFileHandle` for a single channel.
//
// R-31: `mu` is an RWMutex — Append/NextSeq/Read take RLock so
// concurrent ops don't serialize on this Go-side mutex (the
// Rust substrate's HandleGuard is a reader-counter that already
// permits concurrent ops). Close takes the write Lock so it
// quiesces against in-flight ops deterministically from the Go
// caller's POV.
type RedexFile struct {
	mu     sync.RWMutex
	handle *C.RedexFileHandle
}

// =====================================================================
// Replication config — JSON wire shape
// =====================================================================

// PlacementStrategy is the replica-placement enumeration. The
// wire form is the lowercase string sent through the JSON
// `replication.placement` field.
type PlacementStrategy string

const (
	// PlacementStandard lets PlacementFilter choose replicas.
	// Default.
	PlacementStandard PlacementStrategy = "standard"
	// PlacementPinned pins replicas to an explicit NodeId list.
	// Requires PinnedNodes to be set.
	PlacementPinned PlacementStrategy = "pinned"
	// PlacementColocationStrict requires every replica to live on
	// a node already holding the colocate-with-strict chain.
	PlacementColocationStrict PlacementStrategy = "colocation_strict"
)

// UnderCapacityPolicy controls the replica's reaction to local
// disk pressure.
type UnderCapacityPolicy string

const (
	// UnderCapacityWithdraw drops the replica role on disk
	// pressure. Default.
	UnderCapacityWithdraw UnderCapacityPolicy = "withdraw"
	// UnderCapacityEvictOldest calls retention sweep + retries.
	// Requires retention caps to be set on the channel.
	UnderCapacityEvictOldest UnderCapacityPolicy = "evict_oldest"
)

// ReplicationConfig mirrors the substrate's ReplicationConfig.
// Marshaled to JSON via the standard `encoding/json` rules; the
// FFI side deserializes via `RedexReplicationConfigJson`. Field
// defaults match the core defaults (factor=3, heartbeat=500ms,
// placement=Standard, on_under_capacity=Withdraw, budget=0.5).
type ReplicationConfig struct {
	// Factor is the replica count including the leader.
	// Range [1, 16]. 0 = use core default (3).
	Factor uint8 `json:"factor,omitempty"`
	// HeartbeatMs is the heartbeat cadence in milliseconds.
	// 0 = use core default (500).
	HeartbeatMs uint64 `json:"heartbeat_ms,omitempty"`
	// Placement is the replica-placement strategy. Empty string
	// defaults to Standard.
	Placement PlacementStrategy `json:"placement,omitempty"`
	// PinnedNodes is required when Placement == PlacementPinned.
	PinnedNodes []uint64 `json:"pinned_nodes,omitempty"`
	// LeaderPinned, when non-nil, pins the leader to a specific
	// NodeId. The deterministic election picks this node whenever
	// it's healthy.
	LeaderPinned *uint64 `json:"leader_pinned,omitempty"`
	// OnUnderCapacity is the disk-pressure policy. Empty string
	// defaults to Withdraw.
	OnUnderCapacity UnderCapacityPolicy `json:"on_under_capacity,omitempty"`
	// ReplicationBudgetFraction is the bandwidth budget for
	// replication-sync I/O as a fraction of measured NIC peak.
	// Range (0.0, 1.0]. 0.0 = use core default (0.5).
	ReplicationBudgetFraction float32 `json:"replication_budget_fraction,omitempty"`
}

// RedexFileConfig is the per-channel configuration. Marshaled to
// the JSON shape `net_redex_open_file` consumes.
type RedexFileConfig struct {
	// Persistent enables disk-backed storage. Requires the owning
	// `Redex` to have been constructed with `NewRedexWithPersistentDir`.
	Persistent bool `json:"persistent,omitempty"`
	// FsyncEveryN fsyncs after every N appends. Mutually exclusive
	// with FsyncIntervalMs.
	FsyncEveryN uint64 `json:"fsync_every_n,omitempty"`
	// FsyncIntervalMs fsyncs on a timer. Mutually exclusive with
	// FsyncEveryN.
	FsyncIntervalMs uint64 `json:"fsync_interval_ms,omitempty"`
	// RetentionMaxEvents caps the channel at N retained events.
	RetentionMaxEvents uint64 `json:"retention_max_events,omitempty"`
	// RetentionMaxBytes caps the channel at N bytes of payload.
	RetentionMaxBytes uint64 `json:"retention_max_bytes,omitempty"`
	// RetentionMaxAgeMs drops entries older than N milliseconds at
	// the next sweep.
	RetentionMaxAgeMs uint64 `json:"retention_max_age_ms,omitempty"`
	// Replication, when non-nil, opts the channel into cross-node
	// replication. Requires `EnableReplication` to have been
	// called on the owning Redex.
	Replication *ReplicationConfig `json:"replication,omitempty"`
}

// =====================================================================
// Redex lifecycle
// =====================================================================
//
// Both NewRedex constructors install a finalizer that calls Close
// on GC. The finalizer is a safety net for callers who forget the
// explicit Close. **Prefer explicit Close**: the substrate's
// net_redex_free wait_until_quiesced step can block waiting for
// in-flight operations, and Go runs finalizers on a GC-owned
// goroutine that should not stall. The same pattern is used by
// every Go binding handle in this crate (Net, NetStream, Mesh,
// etc.); it's a known shared footgun, not a dataforts regression.
// Callers in long-running processes should pair every constructor
// with a `defer r.Close()` and not rely on the finalizer.

// NewRedex constructs an empty heap-only Redex manager. Never
// fails; the returned handle is non-nil.
func NewRedex() *Redex {
	h := C.net_redex_new(nil)
	r := &Redex{handle: h}
	runtime.SetFinalizer(r, func(r *Redex) { _ = r.Close() })
	return r
}

// NewRedexWithPersistentDir constructs a Redex with `dir` set as
// the persistent base directory for `Persistent: true` channels.
func NewRedexWithPersistentDir(dir string) *Redex {
	cDir := C.CString(dir)
	defer C.free(unsafe.Pointer(cDir))
	h := C.net_redex_new(cDir)
	r := &Redex{handle: h}
	runtime.SetFinalizer(r, func(r *Redex) { _ = r.Close() })
	return r
}

// Close releases the underlying `Redex` handle. Idempotent.
func (r *Redex) Close() error {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.handle == nil {
		return nil
	}
	C.net_redex_free(r.handle)
	r.handle = nil
	runtime.SetFinalizer(r, nil)
	return nil
}

// EnableReplication installs cross-node replication on this Redex
// using `mesh` as the underlying transport. Consumes an
// `*Arc<MeshNode>` handle obtained from `mesh.ArcClone()` (the
// upstream mesh binding's `net_mesh_arc_clone` wrapper); the Go
// caller MUST NOT free `meshArcPtr` after a successful call.
//
// Idempotent — repeated calls return nil without disturbing the
// existing wiring.
func (r *Redex) EnableReplication(meshArcPtr unsafe.Pointer) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.handle == nil {
		return fmt.Errorf("%w: redex handle already closed", ErrRedex)
	}
	rc := C.net_redex_enable_replication(r.handle, (*C.ArcMeshNode)(meshArcPtr))
	if rc != 0 {
		return fmt.Errorf("%w: enable_replication failed (rc=%d)", ErrRedex, int(rc))
	}
	return nil
}

// ReplicationRuntimeCount returns the number of per-channel
// replication runtimes registered on this Redex. 0 when
// replication isn't enabled.
func (r *Redex) ReplicationRuntimeCount() uint32 {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.handle == nil {
		return 0
	}
	return uint32(C.net_redex_replication_runtime_count(r.handle))
}

// ReplicationPrometheusText renders the per-channel replication
// metrics as a Prometheus-text document. Returns the empty string
// when replication isn't enabled — pipe straight into an HTTP
// scrape body without branching.
//
// Covers the seven shapes from `CONFIG_REPLICATION.md`:
// `*_lag_seconds{role}`, `*_sync_bytes_total`,
// `*_leader_changes_total`, `*_under_capacity_total`,
// `*_skip_ahead_total`, `*_election_thrash_total`,
// `*_witness_withdrawals_total`.
func (r *Redex) ReplicationPrometheusText() string {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.handle == nil {
		return ""
	}
	c := C.net_redex_replication_prometheus_text(r.handle)
	if c == nil {
		return ""
	}
	defer C.net_free_string(c)
	return C.GoString(c)
}

// GreedyConfig — operator-facing config for greedy-LRU dataforts
// (Rebel Yell Phase 1). All fields optional; the substrate fills
// in the locked Phase-1 defaults for any field left at the zero
// value, with the exception of zero-meaning-no-filter semantics
// noted per-field.
//
// Marshalled to the same JSON shape the Rust core's
// `RedexGreedyConfigJson` deserializes — field tags pin the
// wire-form names.
type GreedyConfig struct {
	// Scope filter. Empty / nil admits regardless of `scope:` tags.
	Scopes []string `json:"scopes,omitempty"`
	// Proximity bound (ms). 0 = use substrate default (200 ms).
	ProximityMaxRttMs uint64 `json:"proximity_max_rtt_ms,omitempty"`
	// Per-channel byte cap. 0 = use substrate default (100 MiB).
	PerChannelCapBytes uint64 `json:"per_channel_cap_bytes,omitempty"`
	// Total byte cap. 0 = use substrate default (10 GiB).
	TotalCapBytes uint64 `json:"total_cap_bytes,omitempty"`
	// I/O budget as a fraction of NIC peak. 0 = use substrate
	// default (0.25). Range `(0.0, 1.0]`.
	BandwidthBudgetFraction float32 `json:"bandwidth_budget_fraction,omitempty"`
	// Override for the NIC peak (bytes/sec) the bandwidth budget
	// computes against. 0 (or unset) = substrate default (1 Gbps).
	// Deployments on faster NICs should set this explicitly to
	// avoid the substrate's bandwidth-axis reject counter
	// saturating under normal load.
	NicPeakBytesPerS uint64 `json:"nic_peak_bytes_per_s,omitempty"`
	// Maximum in-flight observe_event tasks before the observer
	// drops events under load. 0 = use substrate default (1024).
	// Floor 1.
	ObserverInflightCap uint64 `json:"observer_inflight_cap,omitempty"`
	// `"disabled"` / `"any_of_local_capabilities"` (default) /
	// `"strict"`. Empty = substrate default.
	IntentMatch string `json:"intent_match,omitempty"`
	// `"ignore"` / `"soft_preference"` (default) /
	// `"strict_required"`. Empty = substrate default.
	ColocationPolicy string `json:"colocation_policy,omitempty"`
}

// ErrInvalidGreedyConfig is returned when the supplied greedy
// config fails binding-side or substrate-side validation.
var ErrInvalidGreedyConfig = fmt.Errorf("%w: greedy config invalid", ErrRedex)

// EnableGreedyDataforts installs greedy-LRU dataforts wiring on
// this Redex. Same Arc-consumption contract as EnableReplication:
// `meshArcPtr` is consumed regardless of return code — do NOT
// free it again.
//
// Pass nil for `config` to accept the locked Phase-1 defaults.
//
// Idempotent — repeated calls return nil without disturbing the
// existing wiring.
func (r *Redex) EnableGreedyDataforts(meshArcPtr unsafe.Pointer, config *GreedyConfig) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.handle == nil {
		return fmt.Errorf("%w: redex handle already closed", ErrRedex)
	}
	var cfgPtr *C.char
	if config != nil {
		buf, err := json.Marshal(config)
		if err != nil {
			return fmt.Errorf("%w: marshal config: %v", ErrInvalidGreedyConfig, err)
		}
		c := C.CString(string(buf))
		defer C.free(unsafe.Pointer(c))
		cfgPtr = c
	}
	rc := C.net_redex_enable_greedy_dataforts(
		r.handle,
		(*C.ArcMeshNode)(meshArcPtr),
		cfgPtr,
	)
	if rc != 0 {
		return fmt.Errorf("%w: enable_greedy_dataforts failed (rc=%d)", ErrRedex, int(rc))
	}
	return nil
}

// DisableGreedyDataforts un-installs the greedy wiring. Idempotent.
func (r *Redex) DisableGreedyDataforts() error {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.handle == nil {
		return fmt.Errorf("%w: redex handle already closed", ErrRedex)
	}
	rc := C.net_redex_disable_greedy_dataforts(r.handle)
	if rc != 0 {
		return fmt.Errorf("%w: disable_greedy_dataforts failed (rc=%d)", ErrRedex, int(rc))
	}
	return nil
}

// GreedyCachedChannelCount returns the number of channels
// currently in the greedy cache. 0 when greedy isn't enabled.
func (r *Redex) GreedyCachedChannelCount() uint32 {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.handle == nil {
		return 0
	}
	return uint32(C.net_redex_greedy_cached_channel_count(r.handle))
}

// GreedyPrometheusText renders the greedy-LRU metrics as a
// Prometheus-text document. Returns the empty string when greedy
// isn't enabled.
//
// Covers per-channel `dataforts_greedy_cache_hits_total`,
// `_serve_count_total`, `_evictions_total`, `_bytes_resident`,
// plus the cluster-wide `_admit_rejected_total{reason=...}` and
// `_io_budget_used_bytes`.
func (r *Redex) GreedyPrometheusText() string {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.handle == nil {
		return ""
	}
	c := C.net_redex_greedy_prometheus_text(r.handle)
	if c == nil {
		return ""
	}
	defer C.net_free_string(c)
	return C.GoString(c)
}

// DataGravityConfig — operator-facing config for the data-gravity
// heat-counter emission cycle (Rebel Yell Phase 4). All fields
// optional; zero values keep the substrate defaults.
type DataGravityConfig struct {
	// Whether the counter + emission cycle is active. Default
	// true; set to false to keep the config carried without
	// emitting.
	Enabled *bool `json:"enabled,omitempty"`
	// Re-emission threshold ratio. Range [1.01, 10.0]. Default 2.0.
	EmitThresholdRatio float32 `json:"emit_threshold_ratio,omitempty"`
	// Decay half-life in seconds. Default 1800 (30 min).
	DecayHalfLifeSecs uint64 `json:"decay_half_life_secs,omitempty"`
	// Tick interval in milliseconds. Default 500.
	TickIntervalMs uint64 `json:"tick_interval_ms,omitempty"`
	// Wire normalization reference rate. Higher value = wider
	// dynamic range on the [0.0, 1.0] wire encoding for heat
	// tags. Default 1000.0. 0 (or unset) = substrate default.
	NormalizationReferenceRate float32 `json:"normalization_reference_rate,omitempty"`
}

// EnableGravityForGreedy installs the data-gravity heat-counter
// layer on top of an already-installed greedy runtime. Pass nil
// for `config` to accept the locked Phase-4 defaults.
//
// Same Arc-consumption contract as EnableReplication: meshArcPtr
// is consumed regardless of return code — do NOT free it again.
//
// Requires EnableGreedyDataforts to have been called first.
// Returns a wrapped ErrRedex describing the failure on validation
// errors or when greedy isn t installed.
//
// Idempotent — a second call replaces the prior policy and
// restarts the tick task.
func (r *Redex) EnableGravityForGreedy(
	meshArcPtr unsafe.Pointer,
	config *DataGravityConfig,
) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.handle == nil {
		return fmt.Errorf("%w: redex handle already closed", ErrRedex)
	}
	var cfgPtr *C.char
	if config != nil {
		buf, err := json.Marshal(config)
		if err != nil {
			return fmt.Errorf("%w: marshal gravity config: %v", ErrRedex, err)
		}
		c := C.CString(string(buf))
		defer C.free(unsafe.Pointer(c))
		cfgPtr = c
	}
	rc := C.net_redex_enable_gravity_for_greedy(
		r.handle,
		(*C.ArcMeshNode)(meshArcPtr),
		cfgPtr,
	)
	if rc != 0 {
		return fmt.Errorf("%w: enable_gravity_for_greedy failed (rc=%d)", ErrRedex, int(rc))
	}
	return nil
}

// DisableGravityForGreedy un-installs the gravity layer. Greedy
// stays running. Idempotent — no-op when not enabled.
func (r *Redex) DisableGravityForGreedy() error {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.handle == nil {
		return fmt.Errorf("%w: redex handle already closed", ErrRedex)
	}
	rc := C.net_redex_disable_gravity_for_greedy(r.handle)
	if rc != 0 {
		return fmt.Errorf("%w: disable_gravity_for_greedy failed (rc=%d)", ErrRedex, int(rc))
	}
	return nil
}

// validateReplicationConfig runs the binding-side checks so
// `OpenFile` can return a clearly-typed error without round-
// tripping through the FFI. The substrate revalidates regardless.
//
// R-30: this routes replication-config shape errors to
// `ErrInvalidReplicationConfig` instead of conflating them with
// `ErrReplicationRequiresEnable`. Operators inspecting an invalid
// factor now see the right sentinel.
// Numeric bounds mirror the Rust core (`REPLICATION_FACTOR_MIN/MAX`
// and `HEARTBEAT_MS_MIN/MAX` in `replication_config.rs`). Pinning
// them on the Go side lets `OpenFile` surface
// `ErrInvalidReplicationConfig` for out-of-range values rather than
// the catch-all `ErrReplicationRequiresEnable` the FFI's
// `NET_ERR_REDEX` code maps to.
const (
	replicationFactorMin = uint8(1)
	replicationFactorMax = uint8(16)
	heartbeatMsMin       = uint64(100)
	heartbeatMsMax       = uint64(300_000)
)

func validateReplicationConfig(cfg *ReplicationConfig) error {
	if cfg == nil {
		return nil
	}
	if cfg.Factor != 0 && (cfg.Factor < replicationFactorMin || cfg.Factor > replicationFactorMax) {
		return fmt.Errorf(
			"%w: Factor %d out of range [%d, %d]",
			ErrInvalidReplicationConfig, cfg.Factor, replicationFactorMin, replicationFactorMax,
		)
	}
	if cfg.HeartbeatMs != 0 && (cfg.HeartbeatMs < heartbeatMsMin || cfg.HeartbeatMs > heartbeatMsMax) {
		return fmt.Errorf(
			"%w: HeartbeatMs %d out of range [%d, %d]",
			ErrInvalidReplicationConfig, cfg.HeartbeatMs, heartbeatMsMin, heartbeatMsMax,
		)
	}
	switch cfg.Placement {
	case "", PlacementStandard, PlacementColocationStrict:
		// OK.
	case PlacementPinned:
		if len(cfg.PinnedNodes) == 0 {
			return fmt.Errorf(
				"%w: PinnedNodes must be non-empty when Placement is %q",
				ErrInvalidReplicationConfig, PlacementPinned,
			)
		}
		if cfg.LeaderPinned != nil {
			found := false
			for _, n := range cfg.PinnedNodes {
				if n == *cfg.LeaderPinned {
					found = true
					break
				}
			}
			if !found {
				return fmt.Errorf(
					"%w: LeaderPinned %d is not in PinnedNodes",
					ErrInvalidReplicationConfig, *cfg.LeaderPinned,
				)
			}
		}
	default:
		return fmt.Errorf(
			"%w: unknown Placement %q (expected 'standard', 'pinned', or 'colocation_strict')",
			ErrInvalidReplicationConfig, cfg.Placement,
		)
	}
	switch cfg.OnUnderCapacity {
	case "", UnderCapacityWithdraw, UnderCapacityEvictOldest:
		// OK.
	default:
		return fmt.Errorf(
			"%w: unknown OnUnderCapacity %q (expected 'withdraw' or 'evict_oldest')",
			ErrInvalidReplicationConfig, cfg.OnUnderCapacity,
		)
	}
	if cfg.ReplicationBudgetFraction < 0 || cfg.ReplicationBudgetFraction > 1 {
		return fmt.Errorf(
			"%w: ReplicationBudgetFraction %g out of range (0.0, 1.0]",
			ErrInvalidReplicationConfig, cfg.ReplicationBudgetFraction,
		)
	}
	return nil
}

// OpenFile opens (or returns the existing) RedexFile for the
// channel `name`. The `config` is honored only on first open;
// subsequent opens return the live handle.
//
// With `config.Replication != nil`, the owning Redex must have
// called `EnableReplication` first; otherwise the call returns
// `ErrReplicationRequiresEnable` (wrapping `ErrRedex`).
//
// R-30: replication-config shape errors (empty PinnedNodes,
// LeaderPinned not in PinnedNodes, unknown placement / under-
// capacity strings, budget out of range) surface as
// `ErrInvalidReplicationConfig`. Other FFI errors after that
// validation fall through to `ErrReplicationRequiresEnable` (when
// replication was requested) or a generic wrapped `ErrRedex`.
func (r *Redex) OpenFile(name string, config *RedexFileConfig) (*RedexFile, error) {
	// R-30: validate before locking so we don't hold the mutex
	// across a binding-side rejection.
	if config != nil {
		if err := validateReplicationConfig(config.Replication); err != nil {
			return nil, err
		}
	}

	r.mu.Lock()
	defer r.mu.Unlock()
	if r.handle == nil {
		return nil, fmt.Errorf("%w: redex handle already closed", ErrRedex)
	}

	cName := C.CString(name)
	defer C.free(unsafe.Pointer(cName))

	var cCfg *C.char
	if config != nil {
		b, err := json.Marshal(config)
		if err != nil {
			return nil, fmt.Errorf("%w: marshal config: %v", ErrRedex, err)
		}
		cCfg = C.CString(string(b))
		defer C.free(unsafe.Pointer(cCfg))
	}

	var out *C.RedexFileHandle
	rc := C.net_redex_open_file(r.handle, cName, cCfg, &out)
	if rc != 0 {
		// `NET_ERR_REDEX = -103` is the umbrella code for any
		// Redex-side failure. The binding pre-check above
		// caught the shape errors; what's left is most often
		// "enable_replication wasn't called" or a numeric-range
		// rejection (factor < min, heartbeat below threshold).
		if config != nil && config.Replication != nil {
			return nil, ErrReplicationRequiresEnable
		}
		return nil, fmt.Errorf("%w: open_file failed (rc=%d)", ErrRedex, int(rc))
	}

	f := &RedexFile{handle: out}
	runtime.SetFinalizer(f, func(f *RedexFile) { _ = f.Close() })
	return f, nil
}

// =====================================================================
// RedexFile operations
// =====================================================================

// Close releases the file handle. Idempotent.
func (f *RedexFile) Close() error {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.handle == nil {
		return nil
	}
	C.net_redex_file_free(f.handle)
	f.handle = nil
	runtime.SetFinalizer(f, nil)
	return nil
}

// Append writes `payload` to the file and returns the assigned
// monotonic sequence number.
//
// R-31: takes RLock so concurrent Append calls don't serialize
// at the Go-side mutex. The Rust HandleGuard handles concurrent
// in-flight ops; Close takes the write Lock above to quiesce
// them.
func (f *RedexFile) Append(payload []byte) (uint64, error) {
	f.mu.RLock()
	defer f.mu.RUnlock()
	if f.handle == nil {
		return 0, fmt.Errorf("%w: file handle already closed", ErrRedex)
	}
	var seq C.uint64_t
	var ptr *C.uint8_t
	var length C.size_t
	if len(payload) > 0 {
		ptr = (*C.uint8_t)(unsafe.Pointer(&payload[0]))
		length = C.size_t(len(payload))
	}
	rc := C.net_redex_file_append(f.handle, ptr, length, &seq)
	if rc != 0 {
		return 0, fmt.Errorf("%w: append failed (rc=%d)", ErrRedex, int(rc))
	}
	// R-31: KeepAlive the payload across the cgo call so the
	// GC can't move it. The local `ptr` reference would
	// normally satisfy escape analysis, but the explicit
	// KeepAlive matches the documented cgo pattern.
	runtime.KeepAlive(payload)
	return uint64(seq), nil
}

// NextSeq returns the next sequence number the file will assign
// (== total append count since open).
func (f *RedexFile) NextSeq() uint64 {
	f.mu.RLock()
	defer f.mu.RUnlock()
	if f.handle == nil {
		return 0
	}
	return uint64(C.net_redex_file_len(f.handle))
}
