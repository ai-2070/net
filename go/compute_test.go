// Tests for the compute surface — Stage 6 of
// SDK_COMPUTE_SURFACE_PLAN.md.
//
// Sub-step 1 covers lifecycle only: a Go caller can build a
// DaemonRuntime against a NetMesh, register a kind (stored but
// not yet invoked), start the runtime, and shut it down. Event
// dispatch, spawn, snapshot/restore, and migration land in
// sub-steps 2-4.
package net

import (
	"errors"
	"fmt"
	"strings"
	"testing"
)

// newLocalMesh builds a single-node mesh on an OS-assigned
// localhost port. Caller is responsible for `m.Close()`.
func newLocalMesh(t *testing.T) *MeshNode {
	t.Helper()
	addr := reserveLocalUDPPort(t)
	m, err := NewMeshNode(MeshConfig{
		BindAddr: addr,
		PskHex:   meshPsk,
	})
	if err != nil {
		t.Fatalf("NewMeshNode(%q) failed: %v", addr, err)
	}
	return m
}

// newTestIdentity wraps `GenerateIdentity()` with a `t.Fatalf` on
// failure, so tests can't accidentally proceed with a nil `*Identity`
// whose `Close` / `OriginHash` / `EntityID` calls would panic.
// Caller is still responsible for `defer id.Close()` — we want the
// close sequencing to be visible in each test, same convention as
// `newLocalMesh`.
func newTestIdentity(t *testing.T) *Identity {
	t.Helper()
	id, err := GenerateIdentity()
	if err != nil {
		t.Fatalf("GenerateIdentity failed: %v", err)
	}
	return id
}

func TestDaemonRuntime_BuildsAndReportsNotReadyBeforeStart(t *testing.T) {
	m := newLocalMesh(t)
	defer m.Shutdown()

	rt, err := NewDaemonRuntime(m)
	if err != nil {
		t.Fatalf("NewDaemonRuntime failed: %v", err)
	}
	defer rt.Close()

	if rt.IsReady() {
		t.Errorf("IsReady() = true before Start(), want false")
	}
	if n := rt.DaemonCount(); n != 0 {
		t.Errorf("DaemonCount() = %d, want 0", n)
	}
}

func TestDaemonRuntime_StartFlipsToReady(t *testing.T) {
	m := newLocalMesh(t)
	defer m.Shutdown()

	rt, err := NewDaemonRuntime(m)
	if err != nil {
		t.Fatalf("NewDaemonRuntime failed: %v", err)
	}
	defer rt.Close()

	if err := rt.Start(); err != nil {
		t.Fatalf("Start failed: %v", err)
	}
	if !rt.IsReady() {
		t.Errorf("IsReady() = false after Start, want true")
	}
}

func TestDaemonRuntime_ShutdownFlipsBack(t *testing.T) {
	m := newLocalMesh(t)
	defer m.Shutdown()

	rt, err := NewDaemonRuntime(m)
	if err != nil {
		t.Fatalf("NewDaemonRuntime failed: %v", err)
	}
	defer rt.Close()

	if err := rt.Start(); err != nil {
		t.Fatalf("Start failed: %v", err)
	}
	if err := rt.Shutdown(); err != nil {
		t.Fatalf("Shutdown failed: %v", err)
	}
	if rt.IsReady() {
		t.Errorf("IsReady() = true after Shutdown, want false")
	}
}

func TestDaemonRuntime_ShutdownIsIdempotent(t *testing.T) {
	m := newLocalMesh(t)
	defer m.Shutdown()

	rt, err := NewDaemonRuntime(m)
	if err != nil {
		t.Fatalf("NewDaemonRuntime failed: %v", err)
	}
	defer rt.Close()

	if err := rt.Start(); err != nil {
		t.Fatalf("Start failed: %v", err)
	}
	if err := rt.Shutdown(); err != nil {
		t.Fatalf("first Shutdown failed: %v", err)
	}
	// Second Shutdown on an already-shut-down SDK runtime is a
	// no-op at the Rust layer. Accept either nil or a clean error
	// as long as nothing panics.
	_ = rt.Shutdown()
}

func TestDaemonRuntime_RegisterFactory_AcceptsKind(t *testing.T) {
	m := newLocalMesh(t)
	defer m.Shutdown()
	rt, err := NewDaemonRuntime(m)
	if err != nil {
		t.Fatalf("NewDaemonRuntime failed: %v", err)
	}
	defer rt.Close()

	if err := rt.RegisterFactory("echo"); err != nil {
		t.Errorf("RegisterFactory('echo') failed: %v", err)
	}
}

func TestDaemonRuntime_RegisterFactory_DuplicateKindFails(t *testing.T) {
	m := newLocalMesh(t)
	defer m.Shutdown()
	rt, err := NewDaemonRuntime(m)
	if err != nil {
		t.Fatalf("NewDaemonRuntime failed: %v", err)
	}
	defer rt.Close()

	if err := rt.RegisterFactory("echo"); err != nil {
		t.Fatalf("first RegisterFactory failed: %v", err)
	}
	err = rt.RegisterFactory("echo")
	if err == nil {
		t.Fatalf("duplicate RegisterFactory('echo') succeeded, want error")
	}
	var dup *DuplicateKindError
	if !errors.As(err, &dup) {
		t.Errorf("err = %v (type %T), want *DuplicateKindError", err, err)
	} else if dup.Kind != "echo" {
		t.Errorf("DuplicateKindError.Kind = %q, want 'echo'", dup.Kind)
	}
}

func TestDaemonRuntime_RegisterFactory_DifferentKindsCoexist(t *testing.T) {
	m := newLocalMesh(t)
	defer m.Shutdown()
	rt, err := NewDaemonRuntime(m)
	if err != nil {
		t.Fatalf("NewDaemonRuntime failed: %v", err)
	}
	defer rt.Close()

	for _, k := range []string{"echo", "counter", "router"} {
		if err := rt.RegisterFactory(k); err != nil {
			t.Errorf("RegisterFactory(%q) failed: %v", k, err)
		}
	}
}

func TestDaemonRuntime_NewWithNilMeshErrors(t *testing.T) {
	_, err := NewDaemonRuntime(nil)
	if err == nil {
		t.Fatalf("NewDaemonRuntime(nil) succeeded, want error")
	}
	if !strings.HasPrefix(err.Error(), "daemon:") {
		t.Errorf("err = %q, want prefix 'daemon:'", err.Error())
	}
}

func TestDaemonRuntime_DoesNotShutDownUnderlyingMesh(t *testing.T) {
	// Shutting down the runtime tears down daemons + migration
	// handler but leaves the NetMesh alive.
	m := newLocalMesh(t)
	defer m.Shutdown()
	rt, err := NewDaemonRuntime(m)
	if err != nil {
		t.Fatalf("NewDaemonRuntime failed: %v", err)
	}
	if err := rt.Start(); err != nil {
		t.Fatalf("Start failed: %v", err)
	}
	if err := rt.Shutdown(); err != nil {
		t.Fatalf("Shutdown failed: %v", err)
	}
	rt.Close()
	// Mesh should still be usable.
	if id := m.NodeID(); id == 0 {
		t.Errorf("mesh.NodeID() = 0 after runtime shutdown, mesh should still be alive")
	}
}

// -------------------------------------------------------------------------
// Sub-step 2: spawn + stop + event dispatch via Go callbacks
// -------------------------------------------------------------------------

// EchoDaemon — trivial stateless daemon that echoes the payload.
type echoDaemon struct{}

func (echoDaemon) Process(event CausalEvent) ([][]byte, error) {
	out := make([]byte, len(event.Payload))
	copy(out, event.Payload)
	return [][]byte{out}, nil
}

// counterDaemon — stateful, increments per event, snapshot/restore
// round-trip the 4-byte LE count.
type counterDaemon struct {
	count uint32
}

func (c *counterDaemon) Process(event CausalEvent) ([][]byte, error) {
	c.count++
	buf := make([]byte, 4)
	buf[0] = byte(c.count)
	buf[1] = byte(c.count >> 8)
	buf[2] = byte(c.count >> 16)
	buf[3] = byte(c.count >> 24)
	return [][]byte{buf}, nil
}

func (c *counterDaemon) Snapshot() ([]byte, error) {
	buf := make([]byte, 4)
	buf[0] = byte(c.count)
	buf[1] = byte(c.count >> 8)
	buf[2] = byte(c.count >> 16)
	buf[3] = byte(c.count >> 24)
	return buf, nil
}

func (c *counterDaemon) Restore(state []byte) error {
	if len(state) != 4 {
		return fmt.Errorf("expected 4-byte state, got %d", len(state))
	}
	c.count = uint32(state[0]) | uint32(state[1])<<8 | uint32(state[2])<<16 | uint32(state[3])<<24
	return nil
}

func readU32LE(b []byte) uint32 {
	return uint32(b[0]) | uint32(b[1])<<8 | uint32(b[2])<<16 | uint32(b[3])<<24
}

// Regression for the `WIRE_ORIGIN_HASH_64BIT` cutover: the C ABI
// surface (`net_identity_origin_hash` / `net_compute_daemon_handle_origin_hash`)
// returns the full 64-bit hash, not a truncated u32 zero-extended
// to u64. If the Go side were seeing a truncated value, every
// generated identity would have `origin_hash & 0xFFFFFFFF_00000000 == 0`.
//
// Strategy: generate enough random identities that at least one
// would, statistically, have bits set above 2^32 if the FFI surface
// wasn't truncating. The bound is `1 - (1/2)^N`; N=32 brings the
// false-negative probability to ~2.3e-10 — effectively impossible
// to flake. If every identity reports a zero high half, that's
// near-certain proof that truncation is happening at the FFI
// boundary.
func TestIdentityOriginHash_PreservesHighBitsAcrossFFI(t *testing.T) {
	const samples = 32
	const highMask uint64 = 0xFFFF_FFFF_0000_0000

	var withHighBits *Identity
	var highHash uint64
	for i := 0; i < samples; i++ {
		id := newTestIdentity(t)
		h := id.OriginHash()
		if h&highMask != 0 {
			withHighBits = id
			highHash = h
			break
		}
		id.Close()
	}
	if withHighBits == nil {
		t.Fatalf("none of %d random identities had bits set above 2^32; "+
			"FFI is almost certainly truncating origin_hash to u32 "+
			"(this branch is statistically impossible without truncation)", samples)
	}
	defer withHighBits.Close()

	// Cross-FFI surface check: spawning a daemon against this
	// identity must return a handle whose OriginHash() matches the
	// identity's full u64 value. If the daemon-side FFI truncated,
	// the handle.OriginHash() and id.OriginHash() would differ in
	// the high bits.
	m := newLocalMesh(t)
	defer m.Shutdown()
	rt, err := NewDaemonRuntime(m)
	if err != nil {
		t.Fatalf("NewDaemonRuntime: %v", err)
	}
	defer rt.Close()
	if err := rt.Start(); err != nil {
		t.Fatalf("Start: %v", err)
	}

	h, err := rt.Spawn("echo", withHighBits, echoDaemon{}, nil)
	if err != nil {
		t.Fatalf("Spawn: %v", err)
	}
	defer h.Close()

	got := h.OriginHash()
	if got != highHash {
		t.Fatalf("handle origin_hash = %#x, identity origin_hash = %#x; "+
			"high bits diverged across the spawn FFI surface", got, highHash)
	}
}

func TestDaemonSpawn_ReturnsHandleWithOriginHash(t *testing.T) {
	m := newLocalMesh(t)
	defer m.Shutdown()
	rt, err := NewDaemonRuntime(m)
	if err != nil {
		t.Fatalf("NewDaemonRuntime failed: %v", err)
	}
	defer rt.Close()
	if err := rt.Start(); err != nil {
		t.Fatalf("Start failed: %v", err)
	}

	id, err := GenerateIdentity()
	if err != nil {
		t.Fatalf("IdentityGenerate failed: %v", err)
	}
	defer id.Close()

	h, err := rt.Spawn("echo", id, echoDaemon{}, nil)
	if err != nil {
		t.Fatalf("Spawn failed: %v", err)
	}
	defer h.Close()

	if h.OriginHash() != id.OriginHash() {
		t.Errorf("OriginHash mismatch: handle=%x identity=%x", h.OriginHash(), id.OriginHash())
	}
	expectedEID, err := id.EntityID()
	if err != nil {
		t.Fatalf("identity.EntityID failed: %v", err)
	}
	if !bytesEqual(h.EntityID(), expectedEID) {
		t.Errorf("EntityID mismatch: handle=%x identity=%x", h.EntityID(), expectedEID)
	}
}

func TestDaemonSpawn_StopReducesDaemonCount(t *testing.T) {
	m := newLocalMesh(t)
	defer m.Shutdown()
	rt, err := NewDaemonRuntime(m)
	if err != nil {
		t.Fatalf("NewDaemonRuntime: %v", err)
	}
	defer rt.Close()
	if err := rt.Start(); err != nil {
		t.Fatalf("Start: %v", err)
	}

	id := newTestIdentity(t)
	defer id.Close()

	h, err := rt.Spawn("echo", id, echoDaemon{}, nil)
	if err != nil {
		t.Fatalf("Spawn: %v", err)
	}
	defer h.Close()
	if rt.DaemonCount() != 1 {
		t.Errorf("DaemonCount = %d after Spawn, want 1", rt.DaemonCount())
	}

	if err := rt.Stop(h.OriginHash()); err != nil {
		t.Fatalf("Stop: %v", err)
	}
	if rt.DaemonCount() != 0 {
		t.Errorf("DaemonCount = %d after Stop, want 0", rt.DaemonCount())
	}
}

func TestDeliver_EchoDaemonRoundTrip(t *testing.T) {
	m := newLocalMesh(t)
	defer m.Shutdown()
	rt, err := NewDaemonRuntime(m)
	if err != nil {
		t.Fatalf("NewDaemonRuntime: %v", err)
	}
	defer rt.Close()
	if err := rt.Start(); err != nil {
		t.Fatalf("Start: %v", err)
	}

	id := newTestIdentity(t)
	defer id.Close()
	h, err := rt.Spawn("echo", id, echoDaemon{}, nil)
	if err != nil {
		t.Fatalf("Spawn: %v", err)
	}
	defer h.Close()

	payload := []byte("hello from go")
	outputs, err := rt.Deliver(h.OriginHash(), CausalEvent{
		OriginHash: id.OriginHash(),
		Sequence:   1,
		Payload:    payload,
	})
	if err != nil {
		t.Fatalf("Deliver: %v", err)
	}
	if len(outputs) != 1 {
		t.Fatalf("len(outputs) = %d, want 1", len(outputs))
	}
	if !bytesEqual(outputs[0], payload) {
		t.Errorf("output = %q, want %q", outputs[0], payload)
	}
}

func TestDeliver_CounterDaemonAccumulatesState(t *testing.T) {
	m := newLocalMesh(t)
	defer m.Shutdown()
	rt, err := NewDaemonRuntime(m)
	if err != nil {
		t.Fatalf("NewDaemonRuntime: %v", err)
	}
	defer rt.Close()
	if err := rt.Start(); err != nil {
		t.Fatalf("Start: %v", err)
	}

	id := newTestIdentity(t)
	defer id.Close()
	h, err := rt.Spawn("counter", id, &counterDaemon{}, nil)
	if err != nil {
		t.Fatalf("Spawn: %v", err)
	}
	defer h.Close()

	for i := uint64(1); i <= 5; i++ {
		out, err := rt.Deliver(h.OriginHash(), CausalEvent{
			OriginHash: id.OriginHash(),
			Sequence:   i,
			Payload:    nil,
		})
		if err != nil {
			t.Fatalf("Deliver(%d): %v", i, err)
		}
		if len(out) != 1 {
			t.Fatalf("Deliver(%d): len(outputs) = %d", i, len(out))
		}
		if got := readU32LE(out[0]); got != uint32(i) {
			t.Errorf("Deliver(%d): counter = %d, want %d", i, got, i)
		}
	}
}

func TestDeliver_FanoutReturnsMultipleOutputs(t *testing.T) {
	type fanout struct{}
	// Local daemon — can be declared inline because `MeshDaemon`
	// interface is satisfied by a single Process method.
	m := newLocalMesh(t)
	defer m.Shutdown()
	rt, err := NewDaemonRuntime(m)
	if err != nil {
		t.Fatalf("NewDaemonRuntime: %v", err)
	}
	defer rt.Close()
	if err := rt.Start(); err != nil {
		t.Fatalf("Start: %v", err)
	}

	id := newTestIdentity(t)
	defer id.Close()

	// Use a closure-based daemon to avoid another top-level type.
	h, err := rt.Spawn("fanout", id, &fanoutDaemon{
		outs: [][]byte{[]byte("a"), []byte("bb"), []byte("ccc")},
	}, nil)
	if err != nil {
		t.Fatalf("Spawn: %v", err)
	}
	defer h.Close()

	outputs, err := rt.Deliver(h.OriginHash(), CausalEvent{
		OriginHash: id.OriginHash(),
		Sequence:   1,
	})
	if err != nil {
		t.Fatalf("Deliver: %v", err)
	}
	if len(outputs) != 3 {
		t.Fatalf("len(outputs) = %d, want 3", len(outputs))
	}
	want := []string{"a", "bb", "ccc"}
	for i, o := range outputs {
		if string(o) != want[i] {
			t.Errorf("outputs[%d] = %q, want %q", i, o, want[i])
		}
	}
	_ = fanout{}
}

type fanoutDaemon struct {
	outs [][]byte
}

func (f *fanoutDaemon) Process(_ CausalEvent) ([][]byte, error) {
	return f.outs, nil
}

func TestDeliver_ProcessErrorSurfaces(t *testing.T) {
	m := newLocalMesh(t)
	defer m.Shutdown()
	rt, err := NewDaemonRuntime(m)
	if err != nil {
		t.Fatalf("NewDaemonRuntime: %v", err)
	}
	defer rt.Close()
	if err := rt.Start(); err != nil {
		t.Fatalf("Start: %v", err)
	}

	id := newTestIdentity(t)
	defer id.Close()

	h, err := rt.Spawn("buggy", id, &buggyDaemon{}, nil)
	if err != nil {
		t.Fatalf("Spawn: %v", err)
	}
	defer h.Close()

	_, err = rt.Deliver(h.OriginHash(), CausalEvent{OriginHash: id.OriginHash(), Sequence: 1})
	if err == nil {
		t.Fatalf("expected error from Deliver, got nil")
	}
	var de *DaemonError
	if !errors.As(err, &de) {
		t.Errorf("err = %v (type %T), want *DaemonError", err, err)
	}
}

type buggyDaemon struct{}

func (buggyDaemon) Process(_ CausalEvent) ([][]byte, error) {
	return nil, errors.New("deliberate failure")
}

func TestDeliver_UnknownOriginReturnsError(t *testing.T) {
	m := newLocalMesh(t)
	defer m.Shutdown()
	rt, err := NewDaemonRuntime(m)
	if err != nil {
		t.Fatalf("NewDaemonRuntime: %v", err)
	}
	defer rt.Close()
	if err := rt.Start(); err != nil {
		t.Fatalf("Start: %v", err)
	}
	_, err = rt.Deliver(0xDEADBEEF, CausalEvent{OriginHash: 0xDEADBEEF, Sequence: 1, Payload: []byte("x")})
	if err == nil {
		t.Fatalf("expected error from Deliver to unknown origin, got nil")
	}
}

func TestDeliver_TwoDaemonsIndependentState(t *testing.T) {
	m := newLocalMesh(t)
	defer m.Shutdown()
	rt, err := NewDaemonRuntime(m)
	if err != nil {
		t.Fatalf("NewDaemonRuntime: %v", err)
	}
	defer rt.Close()
	if err := rt.Start(); err != nil {
		t.Fatalf("Start: %v", err)
	}

	idA, _ := GenerateIdentity()
	defer idA.Close()
	idB, _ := GenerateIdentity()
	defer idB.Close()

	hA, err := rt.Spawn("counter", idA, &counterDaemon{}, nil)
	if err != nil {
		t.Fatalf("Spawn A: %v", err)
	}
	defer hA.Close()
	hB, err := rt.Spawn("counter", idB, &counterDaemon{}, nil)
	if err != nil {
		t.Fatalf("Spawn B: %v", err)
	}
	defer hB.Close()

	for i := uint64(1); i <= 3; i++ {
		out, err := rt.Deliver(hA.OriginHash(), CausalEvent{OriginHash: idA.OriginHash(), Sequence: i})
		if err != nil {
			t.Fatalf("Deliver A %d: %v", i, err)
		}
		if got := readU32LE(out[0]); got != uint32(i) {
			t.Errorf("A[%d] = %d, want %d", i, got, i)
		}
	}
	out, err := rt.Deliver(hB.OriginHash(), CausalEvent{OriginHash: idB.OriginHash(), Sequence: 1})
	if err != nil {
		t.Fatalf("Deliver B 1: %v", err)
	}
	if got := readU32LE(out[0]); got != 1 {
		t.Errorf("B[1] = %d, want 1", got)
	}
}

// -------------------------------------------------------------------------
// Sub-step 3: snapshot + restore round-trip
// -------------------------------------------------------------------------

func TestSnapshot_CounterRoundTrip(t *testing.T) {
	m := newLocalMesh(t)
	defer m.Shutdown()
	rt, err := NewDaemonRuntime(m)
	if err != nil {
		t.Fatalf("NewDaemonRuntime: %v", err)
	}
	defer rt.Close()
	if err := rt.Start(); err != nil {
		t.Fatalf("Start: %v", err)
	}

	id := newTestIdentity(t)
	defer id.Close()
	h, err := rt.Spawn("counter", id, &counterDaemon{}, nil)
	if err != nil {
		t.Fatalf("Spawn: %v", err)
	}

	// Drive the counter to 3.
	for i := uint64(1); i <= 3; i++ {
		if _, err := rt.Deliver(h.OriginHash(), CausalEvent{OriginHash: id.OriginHash(), Sequence: i}); err != nil {
			t.Fatalf("Deliver(%d): %v", i, err)
		}
	}

	snap, err := rt.Snapshot(h.OriginHash())
	if err != nil {
		t.Fatalf("Snapshot: %v", err)
	}
	if len(snap) == 0 {
		t.Fatalf("Snapshot returned empty bytes for stateful daemon")
	}

	// Tear the original down — restored instance must pick up
	// from the snapshot, not any live state.
	if err := rt.Stop(h.OriginHash()); err != nil {
		t.Fatalf("Stop: %v", err)
	}
	h.Close()
	if rt.DaemonCount() != 0 {
		t.Fatalf("DaemonCount after stop = %d, want 0", rt.DaemonCount())
	}

	restored, err := rt.SpawnFromSnapshot("counter", id, snap, &counterDaemon{}, nil)
	if err != nil {
		t.Fatalf("SpawnFromSnapshot: %v", err)
	}
	defer restored.Close()
	if restored.OriginHash() != h.OriginHash() {
		t.Errorf("restored.OriginHash = %#x, want %#x", restored.OriginHash(), h.OriginHash())
	}

	// One more delivery — counter should step from 3 to 4.
	out, err := rt.Deliver(restored.OriginHash(), CausalEvent{OriginHash: id.OriginHash(), Sequence: 4})
	if err != nil {
		t.Fatalf("Deliver after restore: %v", err)
	}
	if got := readU32LE(out[0]); got != 4 {
		t.Errorf("counter after restore = %d, want 4", got)
	}
}

func TestSnapshot_StatelessDaemonReturnsNil(t *testing.T) {
	m := newLocalMesh(t)
	defer m.Shutdown()
	rt, err := NewDaemonRuntime(m)
	if err != nil {
		t.Fatalf("NewDaemonRuntime: %v", err)
	}
	defer rt.Close()
	if err := rt.Start(); err != nil {
		t.Fatalf("Start: %v", err)
	}

	id := newTestIdentity(t)
	defer id.Close()
	h, err := rt.Spawn("echo", id, echoDaemon{}, nil)
	if err != nil {
		t.Fatalf("Spawn: %v", err)
	}
	defer h.Close()

	snap, err := rt.Snapshot(h.OriginHash())
	if err != nil {
		t.Fatalf("Snapshot: %v", err)
	}
	if snap != nil {
		t.Errorf("Snapshot of stateless daemon = %d bytes, want nil", len(snap))
	}
}

func TestSnapshot_UnknownOriginErrors(t *testing.T) {
	m := newLocalMesh(t)
	defer m.Shutdown()
	rt, err := NewDaemonRuntime(m)
	if err != nil {
		t.Fatalf("NewDaemonRuntime: %v", err)
	}
	defer rt.Close()
	if err := rt.Start(); err != nil {
		t.Fatalf("Start: %v", err)
	}
	_, err = rt.Snapshot(0xDEADBEEF)
	if err == nil {
		t.Fatalf("Snapshot(unknown) returned nil error")
	}
}

func TestSpawnFromSnapshot_CorruptedBytesErrors(t *testing.T) {
	m := newLocalMesh(t)
	defer m.Shutdown()
	rt, err := NewDaemonRuntime(m)
	if err != nil {
		t.Fatalf("NewDaemonRuntime: %v", err)
	}
	defer rt.Close()
	if err := rt.Start(); err != nil {
		t.Fatalf("Start: %v", err)
	}

	id := newTestIdentity(t)
	defer id.Close()
	_, err = rt.SpawnFromSnapshot("counter", id, []byte("not a real snapshot"), &counterDaemon{}, nil)
	if err == nil {
		t.Fatalf("SpawnFromSnapshot with garbage bytes returned nil error")
	}
	if !strings.Contains(err.Error(), "snapshot decode failed") {
		t.Errorf("err = %q, want 'snapshot decode failed'", err.Error())
	}
}

func TestSpawnFromSnapshot_MismatchedIdentityErrors(t *testing.T) {
	m := newLocalMesh(t)
	defer m.Shutdown()
	rt, err := NewDaemonRuntime(m)
	if err != nil {
		t.Fatalf("NewDaemonRuntime: %v", err)
	}
	defer rt.Close()
	if err := rt.Start(); err != nil {
		t.Fatalf("Start: %v", err)
	}

	orig, _ := GenerateIdentity()
	defer orig.Close()
	h, err := rt.Spawn("counter", orig, &counterDaemon{}, nil)
	if err != nil {
		t.Fatalf("Spawn: %v", err)
	}
	_, err = rt.Deliver(h.OriginHash(), CausalEvent{OriginHash: orig.OriginHash(), Sequence: 1})
	if err != nil {
		t.Fatalf("Deliver: %v", err)
	}
	snap, err := rt.Snapshot(h.OriginHash())
	if err != nil {
		t.Fatalf("Snapshot: %v", err)
	}
	_ = rt.Stop(h.OriginHash())
	h.Close()

	// Different identity — snapshot's entity_id mismatch.
	other, _ := GenerateIdentity()
	defer other.Close()
	_, err = rt.SpawnFromSnapshot("counter", other, snap, &counterDaemon{}, nil)
	if err == nil {
		t.Fatalf("SpawnFromSnapshot with wrong identity returned nil error")
	}
}

func TestSnapshot_EarlierVsLaterCapturesDifferentState(t *testing.T) {
	m := newLocalMesh(t)
	defer m.Shutdown()
	rt, err := NewDaemonRuntime(m)
	if err != nil {
		t.Fatalf("NewDaemonRuntime: %v", err)
	}
	defer rt.Close()
	if err := rt.Start(); err != nil {
		t.Fatalf("Start: %v", err)
	}

	id := newTestIdentity(t)
	defer id.Close()
	h, err := rt.Spawn("counter", id, &counterDaemon{}, nil)
	if err != nil {
		t.Fatalf("Spawn: %v", err)
	}
	evt := func(seq uint64) CausalEvent {
		return CausalEvent{OriginHash: id.OriginHash(), Sequence: seq}
	}
	for i := uint64(1); i <= 2; i++ {
		_, _ = rt.Deliver(h.OriginHash(), evt(i))
	}
	snapAt2, err := rt.Snapshot(h.OriginHash())
	if err != nil {
		t.Fatalf("Snapshot at 2: %v", err)
	}
	for i := uint64(3); i <= 5; i++ {
		_, _ = rt.Deliver(h.OriginHash(), evt(i))
	}
	snapAt5, err := rt.Snapshot(h.OriginHash())
	if err != nil {
		t.Fatalf("Snapshot at 5: %v", err)
	}
	_ = rt.Stop(h.OriginHash())
	h.Close()

	// Restore earlier snapshot — next event should be 3.
	h2, err := rt.SpawnFromSnapshot("counter", id, snapAt2, &counterDaemon{}, nil)
	if err != nil {
		t.Fatalf("SpawnFromSnapshot at 2: %v", err)
	}
	out, _ := rt.Deliver(h2.OriginHash(), evt(6))
	if got := readU32LE(out[0]); got != 3 {
		t.Errorf("restore-from-2 counter after one delivery = %d, want 3", got)
	}
	_ = rt.Stop(h2.OriginHash())
	h2.Close()

	// Restore later snapshot — next event should be 6.
	h5, err := rt.SpawnFromSnapshot("counter", id, snapAt5, &counterDaemon{}, nil)
	if err != nil {
		t.Fatalf("SpawnFromSnapshot at 5: %v", err)
	}
	defer h5.Close()
	out, _ = rt.Deliver(h5.OriginHash(), evt(7))
	if got := readU32LE(out[0]); got != 6 {
		t.Errorf("restore-from-5 counter after one delivery = %d, want 6", got)
	}
}

func bytesEqual(a, b []byte) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}
