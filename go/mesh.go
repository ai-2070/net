// Package net — encrypted-UDP mesh transport + per-peer streams +
// channels (distributed pub/sub).
//
// Compiled into the Rust cdylib when the core is built with
// `--features net`. Mirrors the Rust SDK's `Mesh` type rather than
// the full core `MeshNode` surface — just the common path needed by
// apps: handshake, per-peer streams with backpressure, channels,
// shard receive.

package net

/*
#include "net.h"
#include <stdlib.h>
#include <string.h>
*/
import "C"

import (
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"runtime"
	"sync"
	"unsafe"
)

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

var (
	ErrMeshInit      = errors.New("mesh init failed")
	ErrMeshHandshake = errors.New("mesh handshake failed")
	ErrBackpressure  = errors.New("stream backpressure")
	ErrNotConnected  = errors.New("stream not connected")
	ErrMeshTransport = errors.New("mesh transport error")
	ErrChannel       = errors.New("channel error")
	ErrChannelAuth   = errors.New("channel: unauthorized")

	// NAT traversal errors. One sentinel per `TraversalError`
	// variant so callers can `errors.Is(err, net.ErrTraversalPunchFailed)`.
	// Framing (plan §5): every one of these represents a missed
	// *optimization*, not a connectivity failure — the routed-
	// handshake path is always available regardless of NAT shape.
	ErrTraversalReflexTimeout      = errors.New("traversal: reflex-timeout")
	ErrTraversalPeerNotReachable   = errors.New("traversal: peer-not-reachable")
	ErrTraversalTransport          = errors.New("traversal: transport")
	ErrTraversalRendezvousNoRelay  = errors.New("traversal: rendezvous-no-relay")
	ErrTraversalRendezvousRejected = errors.New("traversal: rendezvous-rejected")
	ErrTraversalPunchFailed        = errors.New("traversal: punch-failed")
	ErrTraversalPortMapUnavailable = errors.New("traversal: port-map-unavailable")
	ErrTraversalUnsupported        = errors.New("traversal: unsupported")
)

func meshErrorFromCode(code C.int) error {
	switch code {
	case 0:
		return nil
	case -1:
		return ErrNullPointer
	case -2:
		return ErrInvalidUTF8
	case -3:
		return ErrInvalidJSON
	case -110:
		return ErrMeshInit
	case -111:
		return ErrMeshHandshake
	case -112:
		return ErrBackpressure
	case -113:
		return ErrNotConnected
	case -114:
		return ErrMeshTransport
	case -115:
		return ErrChannel
	case -116:
		return ErrChannelAuth
	case -130:
		return ErrTraversalReflexTimeout
	case -131:
		return ErrTraversalPeerNotReachable
	case -132:
		return ErrTraversalTransport
	case -133:
		return ErrTraversalRendezvousNoRelay
	case -134:
		return ErrTraversalRendezvousRejected
	case -135:
		return ErrTraversalPunchFailed
	case -136:
		return ErrTraversalPortMapUnavailable
	case -137:
		return ErrTraversalUnsupported
	default:
		return fmt.Errorf("mesh unknown error (code %d)", code)
	}
}

// ---------------------------------------------------------------------------
// Config types
// ---------------------------------------------------------------------------

// MeshConfig configures a new mesh node.
type MeshConfig struct {
	BindAddr string `json:"bind_addr"`
	// Hex-encoded 32-byte pre-shared key.
	PskHex           string `json:"psk_hex"`
	HeartbeatMs      uint64 `json:"heartbeat_ms,omitempty"`
	SessionTimeoutMs uint64 `json:"session_timeout_ms,omitempty"`
	NumShards        uint16 `json:"num_shards,omitempty"`

	// CapabilityGCIntervalMs controls how often the local capability
	// index evicts stale announcements. Leave zero for the core
	// default.
	CapabilityGCIntervalMs uint64 `json:"capability_gc_interval_ms,omitempty"`
	// RequireSignedCapabilities rejects unsigned announcements when
	// true. Leave nil/false for the core default (accept unsigned in v1).
	RequireSignedCapabilities bool `json:"require_signed_capabilities,omitempty"`

	// Subnet constrains the node to a hierarchical subnet (1–4 bytes
	// each 0–255). Empty / nil means `SubnetId::GLOBAL`.
	Subnet []uint32 `json:"subnet,omitempty"`
	// SubnetPolicy derives a subnet from capability tags at runtime
	// (alternative / complement to `Subnet`). See
	// `docs/SDK_SECURITY_SURFACE_PLAN.md`.
	SubnetPolicy *SubnetPolicy `json:"subnet_policy,omitempty"`

	// IdentitySeedHex reproduces a mesh keypair from a 32-byte seed
	// (64 hex chars). Matches `IdentityFromSeed(sameSeed)` so tokens
	// issued to that identity's `EntityID` work for this mesh.
	IdentitySeedHex string `json:"identity_seed_hex,omitempty"`

	// ReflexOverride pins this mesh's publicly-advertised reflex
	// to the supplied external "ip:port". Classification is
	// skipped; the node starts in nat:open and advertises this
	// address on capability announcements.
	//
	// Use for port-forwarded servers (operator knows the external
	// address) or stage-4 UPnP / NAT-PMP integration. This is
	// optimization, not correctness — nodes without an override
	// still reach every peer via the routed-handshake path.
	//
	// Silently ignored when the Rust cdylib was built without
	// `--features nat-traversal`.
	ReflexOverride string `json:"reflex_override,omitempty"`

	// TryPortMapping opts into opportunistic UPnP / NAT-PMP / PCP
	// port mapping at startup. When true, the mesh spawns a
	// port-mapping task that probes the operator's router,
	// installs a mapping on success, pins the reflex to the
	// mapped external, and renews every 30 min.
	//
	// Optimization, not correctness — a router that doesn't
	// speak UPnP / NAT-PMP just falls through to the classifier
	// path. Silently ignored when the Rust cdylib was built
	// without `--features port-mapping`.
	TryPortMapping bool `json:"try_port_mapping,omitempty"`
}

// StreamConfig configures an opened mesh stream.
type StreamConfig struct {
	// Reliability: "reliable" | "fire_and_forget" (default).
	Reliability string `json:"reliability,omitempty"`
	// WindowBytes sets the initial send-credit window. 0 disables
	// backpressure entirely. Default: 64 KiB.
	WindowBytes    uint32 `json:"window_bytes,omitempty"`
	FairnessWeight uint8  `json:"fairness_weight,omitempty"`
}

// StreamStats is a snapshot of a live stream's stats.
type StreamStats struct {
	TxSeq                uint64 `json:"tx_seq"`
	RxSeq                uint64 `json:"rx_seq"`
	InboundPending       uint64 `json:"inbound_pending"`
	LastActivityNs       uint64 `json:"last_activity_ns"`
	Active               bool   `json:"active"`
	BackpressureEvents   uint64 `json:"backpressure_events"`
	TxCreditRemaining    uint32 `json:"tx_credit_remaining"`
	TxWindow             uint32 `json:"tx_window"`
	CreditGrantsReceived uint64 `json:"credit_grants_received"`
	CreditGrantsSent     uint64 `json:"credit_grants_sent"`
}

// ChannelConfig mirrors the core `ChannelConfig`.
type ChannelConfig struct {
	Name         string `json:"name"`
	Visibility   string `json:"visibility,omitempty"` // "subnet-local" | "parent-visible" | "exported" | "global"
	Reliable     bool   `json:"reliable,omitempty"`
	RequireToken bool   `json:"require_token,omitempty"`
	Priority     uint8  `json:"priority,omitempty"`
	MaxRatePps   uint32 `json:"max_rate_pps,omitempty"`

	// PublishCaps restricts who may publish on this channel. Set
	// when the publisher wants to limit publishing to its own
	// `CapabilitySet` satisfying the filter.
	PublishCaps *CapabilityFilter `json:"publish_caps,omitempty"`
	// SubscribeCaps restricts who may subscribe. Subscribers whose
	// announced caps miss this filter are rejected with
	// `ErrChannelAuth`.
	SubscribeCaps *CapabilityFilter `json:"subscribe_caps,omitempty"`
}

// PublishConfig mirrors the core `PublishConfig`.
type PublishConfig struct {
	Reliability string `json:"reliability,omitempty"` // "reliable" | "fire_and_forget"
	OnFailure   string `json:"on_failure,omitempty"`  // "best_effort" | "fail_fast" | "collect"
	MaxInflight uint32 `json:"max_inflight,omitempty"`
}

// PublishFailure carries one per-peer error from a Publish call.
type PublishFailure struct {
	NodeID  uint64 `json:"node_id"`
	Message string `json:"message"`
}

// PublishReport is returned by Publish.
type PublishReport struct {
	Attempted uint32           `json:"attempted"`
	Delivered uint32           `json:"delivered"`
	Errors    []PublishFailure `json:"errors"`
}

// RecvdEvent is one event drained from a shard inbox.
type RecvdEvent struct {
	ID          string
	Payload     []byte
	InsertionTs uint64
	ShardID     uint16
}

// ---------------------------------------------------------------------------
// MeshNode
// ---------------------------------------------------------------------------

// MeshNode is a multi-peer encrypted mesh handle.
type MeshNode struct {
	mu     sync.RWMutex
	handle *C.net_meshnode_t
}

// NewMeshNode opens a mesh node. Call Shutdown to cleanly tear down.
func NewMeshNode(cfg MeshConfig) (*MeshNode, error) {
	data, err := json.Marshal(cfg)
	if err != nil {
		return nil, fmt.Errorf("marshal config: %w", err)
	}
	cCfg := C.CString(string(data))
	defer C.free(unsafe.Pointer(cCfg))

	var handle *C.net_meshnode_t
	code := C.net_mesh_new(cCfg, &handle)
	if err := meshErrorFromCode(code); err != nil {
		return nil, err
	}
	m := &MeshNode{handle: handle}
	runtime.SetFinalizer(m, (*MeshNode).free)
	return m, nil
}

func (m *MeshNode) free() {
	m.mu.Lock()
	defer m.mu.Unlock()
	if m.handle != nil {
		C.net_mesh_free(m.handle)
		m.handle = nil
		runtime.SetFinalizer(m, nil)
	}
}

// Shutdown gracefully tears down the node. Idempotent.
func (m *MeshNode) Shutdown() error {
	m.mu.Lock()
	defer m.mu.Unlock()
	if m.handle == nil {
		return nil
	}
	code := C.net_mesh_shutdown(m.handle)
	C.net_mesh_free(m.handle)
	m.handle = nil
	runtime.SetFinalizer(m, nil)
	return meshErrorFromCode(code)
}

// PublicKey returns this node's Noise static public key, hex-encoded.
func (m *MeshNode) PublicKey() (string, error) {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return "", ErrShuttingDown
	}
	var out *C.char
	var outLen C.size_t
	code := C.net_mesh_public_key_hex(m.handle, &out, &outLen)
	if err := meshErrorFromCode(code); err != nil {
		return "", err
	}
	defer C.net_free_string(out)
	return C.GoStringN(out, C.int(outLen)), nil
}

// NodeID returns this node's u64 id.
func (m *MeshNode) NodeID() uint64 {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return 0
	}
	return uint64(C.net_mesh_node_id(m.handle))
}

// arcClonePtr clones the inner `Arc<MeshNode>` and returns the raw
// pointer as `unsafe.Pointer`. Consumed by `NewMeshRpc` (and any
// future package-internal binding that needs an arc-clone the
// substrate side will take ownership of). Returns nil if the node
// is shutting down or has already been freed.
//
// The returned pointer is heap-allocated on the Rust side (a
// `Box<Arc<MeshNode>>`). The consumer takes ownership; callers MUST
// NOT free it directly.
func (m *MeshNode) arcClonePtr() unsafe.Pointer {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return nil
	}
	return unsafe.Pointer(C.net_mesh_arc_clone(m.handle))
}

// EntityID returns this node's 32-byte ed25519 entity id. Matches
// `IdentityFromSeed(seed).EntityID()` when the mesh was constructed
// with `MeshConfig{IdentitySeedHex: hex.EncodeToString(seed), ...}`.
func (m *MeshNode) EntityID() ([]byte, error) {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return nil, ErrShuttingDown
	}
	out := make([]byte, 32)
	code := C.net_mesh_entity_id(m.handle, (*C.uint8_t)(unsafe.Pointer(&out[0])))
	if err := meshErrorFromCode(code); err != nil {
		return nil, err
	}
	return out, nil
}

// Connect (initiator). Blocks until the handshake completes.
func (m *MeshNode) Connect(peerAddr, peerPubkeyHex string, peerNodeID uint64) error {
	cAddr := C.CString(peerAddr)
	defer C.free(unsafe.Pointer(cAddr))
	cPk := C.CString(peerPubkeyHex)
	defer C.free(unsafe.Pointer(cPk))
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return ErrShuttingDown
	}
	code := C.net_mesh_connect(m.handle, cAddr, cPk, C.uint64_t(peerNodeID))
	return meshErrorFromCode(code)
}

// Accept an incoming connection (responder). Returns the peer's wire address.
func (m *MeshNode) Accept(peerNodeID uint64) (string, error) {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return "", ErrShuttingDown
	}
	var out *C.char
	var outLen C.size_t
	code := C.net_mesh_accept(m.handle, C.uint64_t(peerNodeID), &out, &outLen)
	if err := meshErrorFromCode(code); err != nil {
		return "", err
	}
	defer C.net_free_string(out)
	return C.GoStringN(out, C.int(outLen)), nil
}

// Start the receive loop, heartbeats, and router.
func (m *MeshNode) Start() error {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return ErrShuttingDown
	}
	return meshErrorFromCode(C.net_mesh_start(m.handle))
}

// ---------------------------------------------------------------------------
// NAT traversal
// ---------------------------------------------------------------------------
//
// Framing (plan §5, load-bearing): every one of these APIs is an
// *optimization*, not a connectivity requirement. Nodes behind
// NAT can always reach each other through the routed-handshake
// path. A NatType of "symmetric" or an ErrTraversal* is not a
// connectivity failure — traffic keeps riding the relay.
//
// Compiled when the Rust cdylib has `--features nat-traversal`.
// Bindings are always present at Go compile time so callers can
// link unconditionally; at runtime, an unsupported build surfaces
// as ErrTraversalUnsupported from the shared-library stubs.

// TraversalStats is the snapshot returned by
// MeshNode.TraversalStats(). All counters are monotonic u64 —
// they never reset, so callers that want deltas should subtract
// successive snapshots.
//
//   - PunchesAttempted: the pair-type matrix elected to attempt a
//     hole-punch. Increments per attempt, regardless of outcome.
//   - PunchesSucceeded: subset of attempts that produced a direct
//     session. Always <= PunchesAttempted.
//   - RelayFallbacks: MeshNode.ConnectDirect resolutions that
//     stayed on the routed-handshake path — matrix-skipped pairs
//     plus punch-failed attempts.
type TraversalStats struct {
	PunchesAttempted uint64
	PunchesSucceeded uint64
	RelayFallbacks   uint64
}

// NatType returns this mesh's NAT classification as a stable
// string: "open" | "cone" | "symmetric" | "unknown". "unknown"
// is the pre-classification state; classification runs in the
// background after Start once >=2 peers are connected.
func (m *MeshNode) NatType() (string, error) {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return "", ErrShuttingDown
	}
	var out *C.char
	var outLen C.size_t
	code := C.net_mesh_nat_type(m.handle, &out, &outLen)
	if err := meshErrorFromCode(code); err != nil {
		return "", err
	}
	defer C.net_free_string(out)
	return C.GoStringN(out, C.int(outLen)), nil
}

// ReflexAddr returns this mesh's public-facing "ip:port" as
// observed by a remote peer, or the empty string when no
// reflex has been observed yet. Piggybacks on outbound
// capability announcements so peers can attempt direct
// connects without a separate discovery round-trip.
func (m *MeshNode) ReflexAddr() (string, error) {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return "", ErrShuttingDown
	}
	var out *C.char
	var outLen C.size_t
	code := C.net_mesh_reflex_addr(m.handle, &out, &outLen)
	if err := meshErrorFromCode(code); err != nil {
		return "", err
	}
	defer C.net_free_string(out)
	return C.GoStringN(out, C.int(outLen)), nil
}

// PeerNatType returns peerNodeID's NAT classification as
// advertised on its latest capability announcement. Returns
// "unknown" when the peer hasn't announced. The pair-type
// matrix treats "unknown" as "attempt direct, fall back on
// failure" — never as "don't attempt."
func (m *MeshNode) PeerNatType(peerNodeID uint64) (string, error) {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return "", ErrShuttingDown
	}
	var out *C.char
	var outLen C.size_t
	code := C.net_mesh_peer_nat_type(
		m.handle, C.uint64_t(peerNodeID), &out, &outLen)
	if err := meshErrorFromCode(code); err != nil {
		return "", err
	}
	defer C.net_free_string(out)
	return C.GoStringN(out, C.int(outLen)), nil
}

// ProbeReflex sends one reflex probe to peerNodeID and returns
// the public "ip:port" the peer observed on the probe's UDP
// envelope. Useful for diagnosing misclassifications.
//
// Returns ErrTraversalReflexTimeout on probe timeout or
// ErrTraversalPeerNotReachable when we have no session with
// peerNodeID.
func (m *MeshNode) ProbeReflex(peerNodeID uint64) (string, error) {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return "", ErrShuttingDown
	}
	var out *C.char
	var outLen C.size_t
	code := C.net_mesh_probe_reflex(
		m.handle, C.uint64_t(peerNodeID), &out, &outLen)
	if err := meshErrorFromCode(code); err != nil {
		return "", err
	}
	defer C.net_free_string(out)
	return C.GoStringN(out, C.int(outLen)), nil
}

// ReclassifyNat explicitly re-runs the classification sweep.
// The background loop takes care of this on its own cadence;
// call this after a suspected NAT rebind (gateway reboot,
// address change) to accelerate re-classification. No-op when
// fewer than 2 peers are connected. Never returns an error.
func (m *MeshNode) ReclassifyNat() error {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return ErrShuttingDown
	}
	return meshErrorFromCode(C.net_mesh_reclassify_nat(m.handle))
}

// TraversalStats returns a cumulative snapshot of NAT-traversal
// counters. See TraversalStats for per-field semantics.
func (m *MeshNode) TraversalStats() (TraversalStats, error) {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return TraversalStats{}, ErrShuttingDown
	}
	var stats TraversalStats
	code := C.net_mesh_traversal_stats(
		m.handle,
		(*C.uint64_t)(unsafe.Pointer(&stats.PunchesAttempted)),
		(*C.uint64_t)(unsafe.Pointer(&stats.PunchesSucceeded)),
		(*C.uint64_t)(unsafe.Pointer(&stats.RelayFallbacks)),
	)
	if err := meshErrorFromCode(code); err != nil {
		return TraversalStats{}, err
	}
	return stats, nil
}

// ConnectDirect establishes a session to peerNodeID via the
// rendezvous path, with coordinator mediating the introduction.
// The pair-type matrix picks between a direct handshake and a
// coordinated punch; either way the returned session is
// equivalent in correctness to Connect.
//
// Optimization, not correctness: ConnectDirect always resolves
// (on punch-failed, the session is established via the routed-
// handshake fallback). Inspect TraversalStats afterward to
// distinguish a successful punch from a relay fallback.
//
// Returns ErrTraversalPeerNotReachable when we have no cached
// reflex for peerNodeID, or ErrMeshHandshake on a socket-level
// handshake error.
func (m *MeshNode) ConnectDirect(
	peerNodeID uint64, peerPubkeyHex string, coordinator uint64,
) error {
	cPk := C.CString(peerPubkeyHex)
	defer C.free(unsafe.Pointer(cPk))
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return ErrShuttingDown
	}
	code := C.net_mesh_connect_direct(
		m.handle,
		C.uint64_t(peerNodeID),
		cPk,
		C.uint64_t(coordinator),
	)
	return meshErrorFromCode(code)
}

// SetReflexOverride installs a runtime reflex override. Forces
// NatType() to "open" and ReflexAddr() to the supplied "ip:port"
// string, short-circuiting any further classifier sweeps.
//
// Runtime counterpart of the MeshConfig.ReflexOverride startup
// option — useful when a port-forward goes live mid-session or
// when a stage-4 port-mapping task has just installed a mapping.
// Optimization, not correctness: nodes without an override still
// reach every peer via the routed-handshake path.
//
// Returns ErrMeshInit on a malformed external address.
func (m *MeshNode) SetReflexOverride(external string) error {
	cExt := C.CString(external)
	defer C.free(unsafe.Pointer(cExt))
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return ErrShuttingDown
	}
	return meshErrorFromCode(C.net_mesh_set_reflex_override(m.handle, cExt))
}

// ClearReflexOverride drops a previously-installed reflex
// override. The classifier resumes on its normal cadence;
// ReflexAddr() clears to "" immediately so a between-sweep read
// doesn't return a stale override.
//
// No-op when no override is active — safe to call unconditionally
// on shutdown or revoke paths.
func (m *MeshNode) ClearReflexOverride() error {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return ErrShuttingDown
	}
	return meshErrorFromCode(C.net_mesh_clear_reflex_override(m.handle))
}

// ---------------------------------------------------------------------------
// Streams
// ---------------------------------------------------------------------------

// MeshStream is an opaque handle to an open per-peer stream.
type MeshStream struct {
	mu     sync.RWMutex
	handle *C.net_mesh_stream_t
	// Parent node — kept alongside the stream so Send calls can
	// reach the owning runtime. The stream's own lifetime is bounded
	// by the node's.
	node *MeshNode
}

// OpenStream opens (or looks up) a stream to a connected peer.
// Repeated calls for the same (peer, streamID) are idempotent;
// first-open wins and later differing configs are logged and ignored.
func (m *MeshNode) OpenStream(peerNodeID, streamID uint64, cfg StreamConfig) (*MeshStream, error) {
	data, err := json.Marshal(cfg)
	if err != nil {
		return nil, fmt.Errorf("marshal stream cfg: %w", err)
	}
	var cCfg *C.char
	if string(data) != "{}" {
		cCfg = C.CString(string(data))
		defer C.free(unsafe.Pointer(cCfg))
	}
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return nil, ErrShuttingDown
	}
	var handle *C.net_mesh_stream_t
	code := C.net_mesh_open_stream(m.handle, C.uint64_t(peerNodeID), C.uint64_t(streamID), cCfg, &handle)
	if err := meshErrorFromCode(code); err != nil {
		return nil, err
	}
	s := &MeshStream{handle: handle, node: m}
	runtime.SetFinalizer(s, (*MeshStream).free)
	return s, nil
}

func (s *MeshStream) free() {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.handle != nil {
		C.net_mesh_stream_free(s.handle)
		s.handle = nil
		runtime.SetFinalizer(s, nil)
	}
}

// Close releases the stream handle. Idempotent.
func (s *MeshStream) Close() {
	s.free()
}

// payloadPtrs builds the parallel (pointers, lengths) arrays the C
// ABI expects. cgo forbids passing Go pointers that transitively hold
// Go pointers; the outer arrays must therefore live in C memory. The
// payloads themselves (individual `[]byte`) are Go memory and get
// passed in as single-level pointers, which cgo allows — we pin them
// with `runtime.Pinner` so the GC can't move them during the C call.
//
// Returns a releaser that must be called after the C call returns to
// free the C allocations and unpin the payloads.
func payloadPtrs(payloads [][]byte) (
	pointers **C.uint8_t,
	lens *C.size_t,
	count C.size_t,
	release func(),
) {
	n := len(payloads)
	if n == 0 {
		return nil, nil, 0, func() {}
	}
	ptrBytes := C.size_t(n) * C.size_t(unsafe.Sizeof((*C.uint8_t)(nil)))
	lenBytes := C.size_t(n) * C.size_t(unsafe.Sizeof(C.size_t(0)))
	ptrArr := (*[1 << 28]*C.uint8_t)(C.malloc(ptrBytes))[:n:n]
	lenArr := (*[1 << 28]C.size_t)(C.malloc(lenBytes))[:n:n]
	var pinner runtime.Pinner
	for i, p := range payloads {
		if len(p) == 0 {
			// Any non-nil pointer is fine — C side gates on len.
			ptrArr[i] = (*C.uint8_t)(unsafe.Pointer(&ptrArr[0]))
			lenArr[i] = 0
		} else {
			pinner.Pin(&p[0])
			ptrArr[i] = (*C.uint8_t)(unsafe.Pointer(&p[0]))
			lenArr[i] = C.size_t(len(p))
		}
	}
	pointers = (**C.uint8_t)(unsafe.Pointer(&ptrArr[0]))
	lens = (*C.size_t)(unsafe.Pointer(&lenArr[0]))
	count = C.size_t(n)
	release = func() {
		pinner.Unpin()
		C.free(unsafe.Pointer(pointers))
		C.free(unsafe.Pointer(lens))
	}
	return
}

// Send a batch of payloads on the stream. Returns ErrBackpressure
// when the window is full (nothing sent — caller decides to drop /
// retry / buffer), ErrNotConnected when the peer is gone, or a
// transport error.
//
// Holds the stream AND node read-locks through the C call so a
// concurrent Close/Shutdown can't race the native handles into a
// use-after-free. Concurrent sends run in parallel; Close waits.
func (s *MeshStream) Send(payloads [][]byte) error {
	ptrs, lens, count, release := payloadPtrs(payloads)
	defer release()
	s.mu.RLock()
	defer s.mu.RUnlock()
	n := s.node
	if s.handle == nil || n == nil {
		return ErrShuttingDown
	}
	n.mu.RLock()
	defer n.mu.RUnlock()
	if n.handle == nil {
		return ErrShuttingDown
	}
	code := C.net_mesh_send(s.handle, ptrs, lens, count, n.handle)
	return meshErrorFromCode(code)
}

// SendWithRetry absorbs ErrBackpressure with exponential backoff up
// to `maxRetries`. Other errors propagate immediately.
func (s *MeshStream) SendWithRetry(payloads [][]byte, maxRetries uint32) error {
	ptrs, lens, count, release := payloadPtrs(payloads)
	defer release()
	s.mu.RLock()
	defer s.mu.RUnlock()
	n := s.node
	if s.handle == nil || n == nil {
		return ErrShuttingDown
	}
	n.mu.RLock()
	defer n.mu.RUnlock()
	if n.handle == nil {
		return ErrShuttingDown
	}
	code := C.net_mesh_send_with_retry(s.handle, ptrs, lens, count, C.uint32_t(maxRetries), n.handle)
	return meshErrorFromCode(code)
}

// SendBlocking retries ErrBackpressure up to ~13 min worst case.
func (s *MeshStream) SendBlocking(payloads [][]byte) error {
	ptrs, lens, count, release := payloadPtrs(payloads)
	defer release()
	s.mu.RLock()
	defer s.mu.RUnlock()
	n := s.node
	if s.handle == nil || n == nil {
		return ErrShuttingDown
	}
	n.mu.RLock()
	defer n.mu.RUnlock()
	if n.handle == nil {
		return ErrShuttingDown
	}
	code := C.net_mesh_send_blocking(s.handle, ptrs, lens, count, n.handle)
	return meshErrorFromCode(code)
}

// StreamStats returns a snapshot. `nil` if the stream isn't open.
func (m *MeshNode) StreamStats(peerNodeID, streamID uint64) (*StreamStats, error) {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return nil, ErrShuttingDown
	}
	var out *C.char
	var outLen C.size_t
	code := C.net_mesh_stream_stats(m.handle, C.uint64_t(peerNodeID), C.uint64_t(streamID), &out, &outLen)
	if err := meshErrorFromCode(code); err != nil {
		return nil, err
	}
	defer C.net_free_string(out)
	js := C.GoStringN(out, C.int(outLen))
	if js == "null" {
		return nil, nil
	}
	var s StreamStats
	if err := json.Unmarshal([]byte(js), &s); err != nil {
		return nil, fmt.Errorf("decode stream stats: %w", err)
	}
	return &s, nil
}

// ---------------------------------------------------------------------------
// Recv
// ---------------------------------------------------------------------------

type recvEventWire struct {
	ID          string `json:"id"`
	PayloadB64  string `json:"payload_b64"`
	InsertionTs uint64 `json:"insertion_ts"`
	ShardID     uint16 `json:"shard_id"`
}

// RecvShard drains up to `limit` events from a specific inbound shard.
func (m *MeshNode) RecvShard(shardID uint16, limit uint32) ([]RecvdEvent, error) {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return nil, ErrShuttingDown
	}
	var out *C.char
	var outLen C.size_t
	code := C.net_mesh_recv_shard(m.handle, C.uint16_t(shardID), C.uint32_t(limit), &out, &outLen)
	if err := meshErrorFromCode(code); err != nil {
		return nil, err
	}
	defer C.net_free_string(out)
	js := C.GoStringN(out, C.int(outLen))
	var wire []recvEventWire
	if err := json.Unmarshal([]byte(js), &wire); err != nil {
		return nil, fmt.Errorf("decode recv_shard: %w", err)
	}
	events := make([]RecvdEvent, 0, len(wire))
	for _, w := range wire {
		payload, err := base64.StdEncoding.DecodeString(w.PayloadB64)
		if err != nil {
			return nil, fmt.Errorf("decode payload: %w", err)
		}
		events = append(events, RecvdEvent{
			ID:          w.ID,
			Payload:     payload,
			InsertionTs: w.InsertionTs,
			ShardID:     w.ShardID,
		})
	}
	return events, nil
}

// ---------------------------------------------------------------------------
// Channels
// ---------------------------------------------------------------------------

// RegisterChannel installs a channel config on this node. Subscribers
// must pass the publisher-side ACL before being added to the roster.
func (m *MeshNode) RegisterChannel(cfg ChannelConfig) error {
	data, err := json.Marshal(cfg)
	if err != nil {
		return fmt.Errorf("marshal channel cfg: %w", err)
	}
	cCfg := C.CString(string(data))
	defer C.free(unsafe.Pointer(cCfg))
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return ErrShuttingDown
	}
	return meshErrorFromCode(C.net_mesh_register_channel(m.handle, cCfg))
}

// SubscribeChannel joins `channel` on `publisherNodeID`. Blocks until
// the Ack arrives. `ErrChannelAuth` when the publisher rejected as
// unauthorized, `ErrChannel` for other rejections.
func (m *MeshNode) SubscribeChannel(publisherNodeID uint64, channel string) error {
	cCh := C.CString(channel)
	defer C.free(unsafe.Pointer(cCh))
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return ErrShuttingDown
	}
	return meshErrorFromCode(C.net_mesh_subscribe_channel(m.handle, C.uint64_t(publisherNodeID), cCh))
}

// SubscribeChannelWithToken subscribes to `channel` on
// `publisherNodeID` while presenting a serialized `PermissionToken`
// (typically 159 bytes — whatever `Identity.IssueToken` returned).
// Required when the publisher set `RequireToken=true` or when the
// subscriber's announced caps don't satisfy the publisher's
// `SubscribeCaps` filter.
//
// Malformed / truncated token bytes return `ErrTokenInvalidFormat`
// before any network I/O. Signature-tampered tokens surface as
// `ErrChannelAuth` (the publisher rejects the request).
func (m *MeshNode) SubscribeChannelWithToken(
	publisherNodeID uint64,
	channel string,
	token []byte,
) error {
	if len(token) == 0 {
		return ErrTokenInvalidFormat
	}
	cCh := C.CString(channel)
	defer C.free(unsafe.Pointer(cCh))
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return ErrShuttingDown
	}
	code := C.net_mesh_subscribe_channel_with_token(
		m.handle,
		C.uint64_t(publisherNodeID),
		cCh,
		(*C.uint8_t)(unsafe.Pointer(&token[0])),
		C.size_t(len(token)),
	)
	// Map token errors first so callers can distinguish them from
	// the channel/auth code range.
	switch code {
	case -121, -122, -123, -124, -125, -126, -127:
		return identityErrorFromCode(code)
	}
	return meshErrorFromCode(code)
}

// UnsubscribeChannel is the idempotent counterpart of SubscribeChannel.
func (m *MeshNode) UnsubscribeChannel(publisherNodeID uint64, channel string) error {
	cCh := C.CString(channel)
	defer C.free(unsafe.Pointer(cCh))
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return ErrShuttingDown
	}
	return meshErrorFromCode(C.net_mesh_unsubscribe_channel(m.handle, C.uint64_t(publisherNodeID), cCh))
}

// Publish fans one payload to every subscriber of `channel`.
func (m *MeshNode) Publish(channel string, payload []byte, cfg PublishConfig) (*PublishReport, error) {
	cCh := C.CString(channel)
	defer C.free(unsafe.Pointer(cCh))
	data, err := json.Marshal(cfg)
	if err != nil {
		return nil, fmt.Errorf("marshal publish cfg: %w", err)
	}
	var cCfg *C.char
	if string(data) != "{}" {
		cCfg = C.CString(string(data))
		defer C.free(unsafe.Pointer(cCfg))
	}
	var ptr *C.uint8_t
	var ln C.size_t
	if len(payload) > 0 {
		ptr = (*C.uint8_t)(unsafe.Pointer(&payload[0]))
		ln = C.size_t(len(payload))
	}
	var out *C.char
	var outLen C.size_t
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return nil, ErrShuttingDown
	}
	code := C.net_mesh_publish(m.handle, cCh, ptr, ln, cCfg, &out, &outLen)
	runtime.KeepAlive(payload)
	if err := meshErrorFromCode(code); err != nil {
		return nil, err
	}
	defer C.net_free_string(out)
	js := C.GoStringN(out, C.int(outLen))
	var report PublishReport
	if err := json.Unmarshal([]byte(js), &report); err != nil {
		return nil, fmt.Errorf("decode publish report: %w", err)
	}
	return &report, nil
}

// ---------------------------------------------------------------------------
// Gang-claim resource-island scheduler
// ---------------------------------------------------------------------------

// GangCriteria is the flat match criteria (serialized to JSON for the C
// ABI, which builds the core MatchCriteria internally). The scheduler is
// resource-agnostic: GPU specifics ride plain capability tags (e.g.
// "gpu:h100", "model:<hex>"). The Tags* fields filter the host capability
// match (AND / OR / AND-of-ORs); the Require* fields filter the island's
// resident capabilities. `Selection` is one of "least_loaded" (default) /
// "pack" / "load_band" / "lowest_id".
type GangCriteria struct {
	TagsAll          []string   `json:"tags_all,omitempty"`
	TagsAny          []string   `json:"tags_any,omitempty"`
	TagGroupsAll     [][]string `json:"tag_groups_all,omitempty"`
	MinUnits         uint       `json:"min_units,omitempty"`
	MaxLoad          *float32   `json:"max_load,omitempty"`
	MaxP50LatencyUs  *uint32    `json:"max_p50_latency_us,omitempty"`
	RequireAll       []string   `json:"require_all,omitempty"`
	RequireAny       []string   `json:"require_any,omitempty"`
	Selection        string     `json:"selection,omitempty"`
	LoadBandTarget   *float32   `json:"load_band_target,omitempty"`
	PreferCapability *string    `json:"prefer_capability,omitempty"`
}

// IslandRecord is one island a node self-publishes. Its host is forced
// to this node. Capabilities are resident tags (e.g. "model:<hex>").
type IslandRecord struct {
	ID           uint64   `json:"id"`
	Units        []uint32 `json:"units"`
	Capabilities []string `json:"capabilities,omitempty"`
	Load         float32  `json:"load"`
	P50LatencyUs uint32   `json:"p50_latency_us"`
}

// PublishIslandTopology publishes this node's island record (host forced
// to self). Returns the peer fan-out count.
func (m *MeshNode) PublishIslandTopology(rec IslandRecord) (int, error) {
	data, err := json.Marshal(rec)
	if err != nil {
		return 0, fmt.Errorf("marshal island record: %w", err)
	}
	cJSON := C.CString(string(data))
	defer C.free(unsafe.Pointer(cJSON))
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return 0, ErrShuttingDown
	}
	var count C.size_t
	code := C.net_mesh_publish_island_topology(m.handle, cJSON, &count)
	if err := meshErrorFromCode(code); err != nil {
		return 0, err
	}
	return int(count), nil
}

// MatchIslands matches islands against the criteria (read-only). Best
// island first; empty when nothing matched.
func (m *MeshNode) MatchIslands(crit GangCriteria) ([]uint64, error) {
	data, err := json.Marshal(crit)
	if err != nil {
		return nil, fmt.Errorf("marshal criteria: %w", err)
	}
	cJSON := C.CString(string(data))
	defer C.free(unsafe.Pointer(cJSON))
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return nil, ErrShuttingDown
	}
	// First pass: learn the total count, then fill a right-sized buffer.
	var count C.size_t
	code := C.net_mesh_match_islands(m.handle, cJSON, nil, 0, &count)
	if err := meshErrorFromCode(code); err != nil {
		return nil, err
	}
	if count == 0 {
		return nil, nil
	}
	ids := make([]uint64, int(count))
	code = C.net_mesh_match_islands(
		m.handle, cJSON, (*C.uint64_t)(unsafe.Pointer(&ids[0])), count, &count)
	if err := meshErrorFromCode(code); err != nil {
		return nil, err
	}
	if int(count) < len(ids) {
		ids = ids[:int(count)]
	}
	return ids, nil
}

// ReserveIsland reserves island until untilUnixUs (wall-clock micros).
// Returns "won" if this node now holds it, "lost" otherwise.
func (m *MeshNode) ReserveIsland(island, untilUnixUs uint64) (string, error) {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return "", ErrShuttingDown
	}
	var outcome C.int
	code := C.net_mesh_reserve_island(
		m.handle, C.uint64_t(island), C.uint64_t(untilUnixUs), &outcome)
	if err := meshErrorFromCode(code); err != nil {
		return "", err
	}
	return claimOutcomeString(int(outcome)), nil
}

// ReleaseIsland releases island this node holds. Returns "lost" if this
// node wasn't the holder.
func (m *MeshNode) ReleaseIsland(island uint64) (string, error) {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return "", ErrShuttingDown
	}
	var outcome C.int
	code := C.net_mesh_release_island(m.handle, C.uint64_t(island), &outcome)
	if err := meshErrorFromCode(code); err != nil {
		return "", err
	}
	return claimOutcomeString(int(outcome)), nil
}

// ClaimIsland matches + reserves the first available island in one
// call. Returns (id, true, nil) on success, or (0, false, nil) when
// nothing matched / all contended.
func (m *MeshNode) ClaimIsland(crit GangCriteria, untilUnixUs uint64) (uint64, bool, error) {
	data, err := json.Marshal(crit)
	if err != nil {
		return 0, false, fmt.Errorf("marshal criteria: %w", err)
	}
	cJSON := C.CString(string(data))
	defer C.free(unsafe.Pointer(cJSON))
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return 0, false, ErrShuttingDown
	}
	var found C.int
	var island C.uint64_t
	code := C.net_mesh_claim_island(
		m.handle, cJSON, C.uint64_t(untilUnixUs), &found, &island)
	if err := meshErrorFromCode(code); err != nil {
		return 0, false, err
	}
	if found == 0 {
		return 0, false, nil
	}
	return uint64(island), true, nil
}

func claimOutcomeString(code int) string {
	if code == 0 {
		return "won"
	}
	return "lost"
}
