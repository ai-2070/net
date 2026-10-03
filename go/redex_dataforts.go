// Package net — RedEX replication, greedy Dataforts and data gravity.
//
// Operator controls on a Redex that put it on the mesh: cross-node
// replication of channels opened with RedexFileConfig.Replication, the
// greedy-LRU cache of peers' channels, and the data-gravity heat layer on
// top of that cache. Declared in net_cortex.h.
//
// # Mesh ownership
//
// Each Enable* call hands the Redex its own reference to the mesh node.
// The binding clones that reference immediately before the C call, and the
// C side consumes it on every return code, success or error. Nothing here
// frees it afterwards, so a caller never manages it. Disable* releases it.
//
// # Gravity heat
//
// Gravity turns reads served from the greedy cache into `heat:` capability
// tags. No binding exposes the greedy read path (Redex::greedy_cache_for)
// yet, so from Go, EnableGravityForGreedy configures and starts the tick
// but cannot itself produce heat.

package net

/*
#include "net.h"
#include <stdlib.h>
*/
import "C"

import (
	"encoding/json"
	"fmt"
	"unsafe"
)

// ErrInvalidRedexConfig - a greedy or gravity config that could not be
// encoded, or that the native parser refused as malformed. A config that
// parses but fails validation (or an install that fails, such as gravity
// without greedy) is the native NET_ERR_REDEX: plain ErrRedex.
var ErrInvalidRedexConfig = fmt.Errorf("%w: invalid config", ErrRedex)

// redexOpError maps the return code of the replication / greedy / gravity
// entry points. Every failure matches ErrRedex.
func redexOpError(op string, rc C.int) error {
	switch rc {
	case 0:
		return nil
	case -2, -3:
		return fmt.Errorf("%w: %s (rc=%d)", ErrInvalidRedexConfig, op, int(rc))
	case -8:
		return fmt.Errorf("%w: %s: %w", ErrRedex, op, ErrShuttingDown)
	case -107:
		return fmt.Errorf("%w: %s: %w", ErrRedex, op, ErrFeatureNotBuilt)
	case -103:
		return fmt.Errorf("%w: %s refused (rc=%d)", ErrRedex, op, int(rc))
	default:
		return fmt.Errorf("%w: %s: %v", ErrRedex, op, cortexErrorFromCode(rc))
	}
}

// PlacementStrategy is the replica-placement choice, sent as a lowercase
// string.
type PlacementStrategy string

const (
	// PlacementStandard lets the placement filter choose replicas.
	PlacementStandard PlacementStrategy = "standard"
	// PlacementPinned pins replicas to PinnedNodes.
	PlacementPinned PlacementStrategy = "pinned"
	// PlacementColocationStrict requires every replica to hold the chain
	// named by PlacementMetadata["colocate-with-strict"].
	PlacementColocationStrict PlacementStrategy = "colocation_strict"
)

// UnderCapacityPolicy is a replica's reaction to local disk pressure.
type UnderCapacityPolicy string

const (
	// UnderCapacityWithdraw drops the replica role.
	UnderCapacityWithdraw UnderCapacityPolicy = "withdraw"
	// UnderCapacityEvictOldest sweeps retention and retries; the channel
	// needs retention caps.
	UnderCapacityEvictOldest UnderCapacityPolicy = "evict_oldest"
)

// RedexReplicationConfig opts a channel into cross-node replication when set
// on RedexFileConfig.Replication. Every field is optional: a zero value is
// left out of the JSON and the core default applies (factor 3, heartbeat
// 500 ms, standard placement, withdraw, budget 0.5). The native side
// validates the result.
type RedexReplicationConfig struct {
	// Factor is the replica count including the leader.
	Factor uint8 `json:"factor,omitempty"`
	// HeartbeatMs is the heartbeat cadence.
	HeartbeatMs uint64 `json:"heartbeat_ms,omitempty"`
	// Placement chooses how replicas are placed.
	Placement PlacementStrategy `json:"placement,omitempty"`
	// PinnedNodes is required with PlacementPinned.
	PinnedNodes []uint64 `json:"pinned_nodes,omitempty"`
	// LeaderPinned, when set, pins the leader to this node.
	LeaderPinned *uint64 `json:"leader_pinned,omitempty"`
	// OnUnderCapacity is the disk-pressure policy.
	OnUnderCapacity UnderCapacityPolicy `json:"on_under_capacity,omitempty"`
	// ReplicationBudgetFraction is the sync I/O budget as a fraction of
	// NIC peak, in (0, 1].
	ReplicationBudgetFraction float32 `json:"replication_budget_fraction,omitempty"`
	// PlacementMetadata carries placement hints: "colocate-with",
	// "colocate-with-strict" (a chain's 16-hex origin hash), "intent".
	PlacementMetadata map[string]string `json:"placement_metadata,omitempty"`
}

// GreedyConfig configures greedy Dataforts. Zero fields keep the substrate
// defaults.
type GreedyConfig struct {
	// Scopes admits only chains advertising a matching `scope:` tag.
	// Empty admits any chain.
	Scopes []string `json:"scopes,omitempty"`
	// ProximityMaxRttMs bounds the RTT to the chain's home node
	// (default 200).
	ProximityMaxRttMs uint64 `json:"proximity_max_rtt_ms,omitempty"`
	// PerChannelCapBytes caps one cached channel (default 100 MiB, floor
	// 1 MiB).
	PerChannelCapBytes uint64 `json:"per_channel_cap_bytes,omitempty"`
	// TotalCapBytes caps the whole cache (default 10 GiB).
	TotalCapBytes uint64 `json:"total_cap_bytes,omitempty"`
	// BandwidthBudgetFraction is the I/O budget as a fraction of NIC
	// peak, in (0, 1] (default 0.25).
	BandwidthBudgetFraction float32 `json:"bandwidth_budget_fraction,omitempty"`
	// NicPeakBytesPerS overrides the NIC peak the budget computes against
	// (default 1 Gbps).
	NicPeakBytesPerS uint64 `json:"nic_peak_bytes_per_s,omitempty"`
	// ObserverInflightCap bounds in-flight observed events (default 1024).
	ObserverInflightCap uint64 `json:"observer_inflight_cap,omitempty"`
	// IntentMatch: "disabled", "any_of_local_capabilities" (default) or
	// "strict".
	IntentMatch string `json:"intent_match,omitempty"`
	// ColocationPolicy: "ignore", "soft_preference" (default) or
	// "strict_required".
	ColocationPolicy string `json:"colocation_policy,omitempty"`
}

// DataGravityConfig configures the gravity heat layer. Zero fields keep
// the substrate defaults.
type DataGravityConfig struct {
	// Enabled turns emission on (default true). A pointer so false is
	// sendable.
	Enabled *bool `json:"enabled,omitempty"`
	// EmitThresholdRatio is the re-emission ratio, in [1.01, 10.0]
	// (default 2.0).
	EmitThresholdRatio float32 `json:"emit_threshold_ratio,omitempty"`
	// DecayHalfLifeSecs is the heat decay half-life (default 1800).
	DecayHalfLifeSecs uint64 `json:"decay_half_life_secs,omitempty"`
	// TickIntervalMs is the emission tick (default 500).
	TickIntervalMs uint64 `json:"tick_interval_ms,omitempty"`
	// NormalizationReferenceRate scales rates onto the wire's [0, 1]
	// (default 1000).
	NormalizationReferenceRate float32 `json:"normalization_reference_rate,omitempty"`
}

// configJSON marshals an optional config to a C string, or nil for "use the
// defaults". The caller frees a non-nil result.
func configJSON(op string, v any, isNil bool) (*C.char, error) {
	if isNil {
		return nil, nil
	}
	body, err := json.Marshal(v)
	if err != nil {
		return nil, fmt.Errorf("%w: %s: %v", ErrInvalidRedexConfig, op, err)
	}
	return C.CString(string(body)), nil
}

// withMeshArc runs fn with a fresh boxed reference to mesh, for a C call
// that consumes it. The clone happens under the mesh's read lock and right
// before fn, so a concurrent Shutdown cannot free the node in between; the
// reference is never freed here, because the callee owns it on every
// return code.
func withMeshArc(mesh *MeshNode, fn func(arc *C.struct_net_compute_mesh_arc_s) C.int) (C.int, error) {
	if mesh == nil {
		return 0, fmt.Errorf("%w: mesh is nil", ErrRedex)
	}
	mesh.mu.RLock()
	defer mesh.mu.RUnlock()
	if mesh.handle == nil {
		return 0, fmt.Errorf("%w: mesh: %w", ErrRedex, ErrShuttingDown)
	}
	arc := C.net_mesh_arc_clone(mesh.handle)
	if arc == nil {
		return 0, fmt.Errorf("%w: mesh: %w", ErrRedex, ErrShuttingDown)
	}
	return fn((*C.struct_net_compute_mesh_arc_s)(unsafe.Pointer(arc))), nil
}

// enableOnMesh is the shared body of the three Enable* calls.
func (r *Redex) enableOnMesh(
	op string,
	mesh *MeshNode,
	cfg *C.char,
	call func(h *C.net_redex_t, arc *C.struct_net_compute_mesh_arc_s, cfg *C.char) C.int,
) error {
	if cfg != nil {
		defer C.free(unsafe.Pointer(cfg))
	}
	r.mu.RLock()
	defer r.mu.RUnlock()
	if r.handle == nil {
		return fmt.Errorf("%w: %s: %w", ErrRedex, op, ErrShuttingDown)
	}
	rc, err := withMeshArc(mesh, func(arc *C.struct_net_compute_mesh_arc_s) C.int {
		return call(r.handle, arc, cfg)
	})
	if err != nil {
		return err
	}
	return redexOpError(op, rc)
}

// redexCall runs a no-argument control call under the read lock.
func (r *Redex) redexCall(op string, call func(h *C.net_redex_t) C.int) error {
	r.mu.RLock()
	defer r.mu.RUnlock()
	if r.handle == nil {
		return fmt.Errorf("%w: %s: %w", ErrRedex, op, ErrShuttingDown)
	}
	return redexOpError(op, call(r.handle))
}

// redexText reads one of the Prometheus bodies. "" when the surface isn't
// enabled.
func (r *Redex) redexText(op string, call func(h *C.net_redex_t) *C.char) (string, error) {
	r.mu.RLock()
	defer r.mu.RUnlock()
	if r.handle == nil {
		return "", fmt.Errorf("%w: %s: %w", ErrRedex, op, ErrShuttingDown)
	}
	body := call(r.handle)
	if body == nil {
		return "", fmt.Errorf("%w: %s returned null", ErrRedex, op)
	}
	defer C.net_free_string(body)
	return C.GoString(body), nil
}

// redexCount reads one of the counters; 0 on a closed Redex, as natively.
func (r *Redex) redexCount(call func(h *C.net_redex_t) C.uint32_t) uint32 {
	r.mu.RLock()
	defer r.mu.RUnlock()
	if r.handle == nil {
		return 0
	}
	return uint32(call(r.handle))
}

// EnableReplication installs cross-node replication over mesh. It starts no
// channel by itself: a channel replicates once it is opened with
// RedexFileConfig.Replication set. Idempotent.
func (r *Redex) EnableReplication(mesh *MeshNode) error {
	return r.enableOnMesh("enable_replication", mesh, nil,
		func(h *C.net_redex_t, arc *C.struct_net_compute_mesh_arc_s, _ *C.char) C.int {
			return C.net_redex_enable_replication(h, arc)
		})
}

// DisableReplication stops every channel's replication, waits for it to
// shut down, and releases the mesh. Open files stay open as local logs.
// Idempotent.
func (r *Redex) DisableReplication() error {
	return r.redexCall("disable_replication", func(h *C.net_redex_t) C.int {
		return C.net_redex_disable_replication(h)
	})
}

// ReplicationRuntimeCount is the number of channels currently replicating:
// 0 after EnableReplication until a channel is opened with a replication
// config.
func (r *Redex) ReplicationRuntimeCount() uint32 {
	return r.redexCount(func(h *C.net_redex_t) C.uint32_t {
		return C.net_redex_replication_runtime_count(h)
	})
}

// ReplicationPrometheusText renders the per-channel replication metrics.
func (r *Redex) ReplicationPrometheusText() (string, error) {
	return r.redexText("replication_prometheus_text", func(h *C.net_redex_t) *C.char {
		return C.net_redex_replication_prometheus_text(h)
	})
}

// EnableGreedyDataforts installs the greedy-LRU cache of peers' channels.
// Pass nil for the defaults. Idempotent.
func (r *Redex) EnableGreedyDataforts(mesh *MeshNode, cfg *GreedyConfig) error {
	cCfg, err := configJSON("enable_greedy_dataforts", cfg, cfg == nil)
	if err != nil {
		return err
	}
	return r.enableOnMesh("enable_greedy_dataforts", mesh, cCfg,
		func(h *C.net_redex_t, arc *C.struct_net_compute_mesh_arc_s, c *C.char) C.int {
			return C.net_redex_enable_greedy_dataforts(h, arc, c)
		})
}

// DisableGreedyDataforts removes the greedy cache. Idempotent.
func (r *Redex) DisableGreedyDataforts() error {
	return r.redexCall("disable_greedy_dataforts", func(h *C.net_redex_t) C.int {
		return C.net_redex_disable_greedy_dataforts(h)
	})
}

// GreedyCachedChannelCount is the number of peers' channels in the greedy
// cache.
func (r *Redex) GreedyCachedChannelCount() uint32 {
	return r.redexCount(func(h *C.net_redex_t) C.uint32_t {
		return C.net_redex_greedy_cached_channel_count(h)
	})
}

// GreedyPrometheusText renders the greedy cache metrics.
func (r *Redex) GreedyPrometheusText() (string, error) {
	return r.redexText("greedy_prometheus_text", func(h *C.net_redex_t) *C.char {
		return C.net_redex_greedy_prometheus_text(h)
	})
}

// EnableGravityForGreedy installs the data-gravity heat layer on the greedy
// cache, which must already be enabled. Pass nil for the defaults. A second
// call replaces the policy and restarts the tick.
func (r *Redex) EnableGravityForGreedy(mesh *MeshNode, cfg *DataGravityConfig) error {
	cCfg, err := configJSON("enable_gravity_for_greedy", cfg, cfg == nil)
	if err != nil {
		return err
	}
	return r.enableOnMesh("enable_gravity_for_greedy", mesh, cCfg,
		func(h *C.net_redex_t, arc *C.struct_net_compute_mesh_arc_s, c *C.char) C.int {
			return C.net_redex_enable_gravity_for_greedy(h, arc, c)
		})
}

// DisableGravityForGreedy removes the gravity layer; the greedy cache keeps
// running. Idempotent.
func (r *Redex) DisableGravityForGreedy() error {
	return r.redexCall("disable_gravity_for_greedy", func(h *C.net_redex_t) C.int {
		return C.net_redex_disable_gravity_for_greedy(h)
	})
}
