// Tests for the groups surface — Stage 4 of SDK_GROUPS_SURFACE_PLAN.md.
//
// Requires the `test_helpers` build tag so
// `TestInjectSyntheticPeer` (from `groups_testhelpers.go`) is
// compiled in. Run with:
//
//   DYLD_LIBRARY_PATH=.../target/release go test -tags test_helpers ./net/...
//
// and the cdylib must be built with `--features test-helpers`.

//go:build test_helpers

package net

import (
	"errors"
	"strings"
	"testing"
)

// runtimeWithPeers builds a started DaemonRuntime with a factory
// registered under "noop" and `extraPeers` synthetic capability
// entries in the mesh's index so `place_with_spread` has enough
// placement candidates for multi-member groups.
func runtimeWithPeers(t *testing.T, extraPeers int) (*DaemonRuntime, *MeshNode) {
	t.Helper()
	mesh := newLocalMesh(t)
	for i := 1; i <= extraPeers; i++ {
		// Synthetic node IDs above 0x1000_0000 so they never
		// collide with real node IDs derived from ed25519 pubkeys.
		TestInjectSyntheticPeer(mesh, uint64(0x1000_0000_0000_0000)+uint64(i))
	}
	rt, err := NewDaemonRuntime(mesh)
	if err != nil {
		t.Fatalf("NewDaemonRuntime: %v", err)
	}
	if err := rt.RegisterFactoryFunc("noop", func() MeshDaemon {
		return &noopDaemon{}
	}); err != nil {
		t.Fatalf("RegisterFactoryFunc: %v", err)
	}
	if err := rt.Start(); err != nil {
		t.Fatalf("Start: %v", err)
	}
	return rt, mesh
}

type noopDaemon struct{}

func (noopDaemon) Process(CausalEvent) ([][]byte, error) { return nil, nil }

func seed(b byte) []byte {
	s := make([]byte, 32)
	for i := range s {
		s[i] = b
	}
	return s
}

// ----------------------------------------------------------------
// ReplicaGroup
// ----------------------------------------------------------------

func TestReplicaGroup_SpawnRegistersMembersAndReportsHealthy(t *testing.T) {
	rt, mesh := runtimeWithPeers(t, 3)
	defer mesh.Shutdown()
	defer rt.Close()

	g, err := NewReplicaGroup(rt, "noop", ReplicaGroupConfig{
		ReplicaCount: 3,
		GroupSeed:    seed(0x11),
		LBStrategy:   StrategyRoundRobin,
	})
	if err != nil {
		t.Fatalf("NewReplicaGroup: %v", err)
	}
	defer g.Close()

	if n := g.ReplicaCount(); n != 3 {
		t.Errorf("ReplicaCount = %d, want 3", n)
	}
	if n := g.HealthyCount(); n != 3 {
		t.Errorf("HealthyCount = %d, want 3", n)
	}
	if h := g.Health(); h.Status != "healthy" {
		t.Errorf("Health.Status = %q, want healthy", h.Status)
	}
	if n := rt.DaemonCount(); n != 3 {
		t.Errorf("rt.DaemonCount() = %d, want 3", n)
	}
	members := g.Replicas()
	if len(members) != 3 {
		t.Fatalf("len(Replicas()) = %d, want 3", len(members))
	}
	for _, m := range members {
		if !m.Healthy {
			t.Errorf("member %d not healthy", m.Index)
		}
		if m.OriginHash == 0 {
			t.Errorf("member %d has zero origin_hash", m.Index)
		}
	}
}

func TestReplicaGroup_RouteEventReturnsLiveMemberOrigin(t *testing.T) {
	rt, mesh := runtimeWithPeers(t, 3)
	defer mesh.Shutdown()
	defer rt.Close()

	g, err := NewReplicaGroup(rt, "noop", ReplicaGroupConfig{
		ReplicaCount: 3,
		GroupSeed:    seed(0x22),
		LBStrategy:   StrategyConsistentHash,
	})
	if err != nil {
		t.Fatalf("NewReplicaGroup: %v", err)
	}
	defer g.Close()

	live := make(map[uint64]bool)
	for _, m := range g.Replicas() {
		live[m.OriginHash] = true
	}
	for i := 0; i < 30; i++ {
		origin, err := g.RouteEvent("req-" + string(rune('a'+i%26)))
		if err != nil {
			t.Fatalf("RouteEvent iter %d: %v", i, err)
		}
		if !live[origin] {
			t.Errorf("RouteEvent iter %d returned unknown origin %#x", i, origin)
		}
	}
}

func TestReplicaGroup_ScaleUpAndDown(t *testing.T) {
	rt, mesh := runtimeWithPeers(t, 5)
	defer mesh.Shutdown()
	defer rt.Close()

	g, err := NewReplicaGroup(rt, "noop", ReplicaGroupConfig{
		ReplicaCount: 2,
		GroupSeed:    seed(0x33),
		LBStrategy:   StrategyRoundRobin,
	})
	if err != nil {
		t.Fatalf("NewReplicaGroup: %v", err)
	}
	defer g.Close()

	if err := g.ScaleTo(5); err != nil {
		t.Fatalf("ScaleTo(5): %v", err)
	}
	if g.ReplicaCount() != 5 {
		t.Errorf("ReplicaCount after scale-up = %d", g.ReplicaCount())
	}
	if rt.DaemonCount() != 5 {
		t.Errorf("rt.DaemonCount = %d after scale-up", rt.DaemonCount())
	}

	if err := g.ScaleTo(1); err != nil {
		t.Fatalf("ScaleTo(1): %v", err)
	}
	if g.ReplicaCount() != 1 {
		t.Errorf("ReplicaCount after scale-down = %d", g.ReplicaCount())
	}
	if rt.DaemonCount() != 1 {
		t.Errorf("rt.DaemonCount = %d after scale-down", rt.DaemonCount())
	}
}

func TestReplicaGroup_SpawnBeforeStartErrorsNotReady(t *testing.T) {
	mesh := newLocalMesh(t)
	defer mesh.Shutdown()
	rt, err := NewDaemonRuntime(mesh)
	if err != nil {
		t.Fatalf("NewDaemonRuntime: %v", err)
	}
	defer rt.Close()
	if err := rt.RegisterFactoryFunc("noop", func() MeshDaemon {
		return &noopDaemon{}
	}); err != nil {
		t.Fatalf("RegisterFactoryFunc: %v", err)
	}
	// Intentionally skip rt.Start()

	_, err = NewReplicaGroup(rt, "noop", ReplicaGroupConfig{
		ReplicaCount: 2,
		GroupSeed:    seed(0x44),
		LBStrategy:   StrategyRoundRobin,
	})
	if err == nil {
		t.Fatal("expected not-ready error")
	}
	var ge *GroupError
	if !errors.As(err, &ge) {
		t.Fatalf("err = %T %v, want *GroupError", err, err)
	}
	if ge.Kind != GroupErrNotReady {
		t.Errorf("Kind = %q, want %q", ge.Kind, GroupErrNotReady)
	}
}

func TestReplicaGroup_SpawnUnknownKindErrorsFactoryNotFound(t *testing.T) {
	rt, mesh := runtimeWithPeers(t, 2)
	defer mesh.Shutdown()
	defer rt.Close()

	_, err := NewReplicaGroup(rt, "never-registered", ReplicaGroupConfig{
		ReplicaCount: 2,
		GroupSeed:    seed(0x55),
		LBStrategy:   StrategyRoundRobin,
	})
	if err == nil {
		t.Fatal("expected factory-not-found error")
	}
	var ge *GroupError
	if !errors.As(err, &ge) {
		t.Fatalf("err = %T %v, want *GroupError", err, err)
	}
	if ge.Kind != GroupErrFactoryNotFound {
		t.Errorf("Kind = %q, want %q", ge.Kind, GroupErrFactoryNotFound)
	}
}

func TestReplicaGroup_InvalidSeedErrors(t *testing.T) {
	rt, mesh := runtimeWithPeers(t, 2)
	defer mesh.Shutdown()
	defer rt.Close()

	_, err := NewReplicaGroup(rt, "noop", ReplicaGroupConfig{
		ReplicaCount: 2,
		GroupSeed:    []byte("short"),
		LBStrategy:   StrategyRoundRobin,
	})
	if err == nil {
		t.Fatal("expected invalid-seed error")
	}
	var ge *GroupError
	if !errors.As(err, &ge) {
		t.Fatalf("err = %T %v, want *GroupError", err, err)
	}
	if ge.Kind != GroupErrInvalidConfig {
		t.Errorf("Kind = %q, want invalid-config", ge.Kind)
	}
	if !strings.Contains(ge.Error(), "group_seed must be 32 bytes") {
		t.Errorf("error message = %q, missing group_seed hint", ge.Error())
	}
}

// ----------------------------------------------------------------
// ForkGroup
// ----------------------------------------------------------------

func TestForkGroup_UniqueOriginsAndVerifiableLineage(t *testing.T) {
	rt, mesh := runtimeWithPeers(t, 4)
	defer mesh.Shutdown()
	defer rt.Close()

	g, err := NewForkGroup(rt, "noop", 0xABCDEF01, 42, ForkGroupConfig{
		ForkCount:  3,
		LBStrategy: StrategyRoundRobin,
	})
	if err != nil {
		t.Fatalf("NewForkGroup: %v", err)
	}
	defer g.Close()

	if g.ForkCount() != 3 {
		t.Errorf("ForkCount = %d, want 3", g.ForkCount())
	}
	if g.ParentOrigin() != 0xABCDEF01 {
		t.Errorf("ParentOrigin = %#x, want 0xABCDEF01", g.ParentOrigin())
	}
	if g.ForkSeq() != 42 {
		t.Errorf("ForkSeq = %d, want 42", g.ForkSeq())
	}
	if !g.VerifyLineage() {
		t.Error("VerifyLineage() = false, want true")
	}

	origins := make(map[uint64]bool)
	for _, m := range g.Members() {
		origins[m.OriginHash] = true
	}
	if len(origins) != 3 {
		t.Errorf("unique origins = %d, want 3", len(origins))
	}
	if records := g.ForkRecords(); len(records) != 3 {
		t.Errorf("len(ForkRecords) = %d, want 3", len(records))
	}
}

func TestForkGroup_ForkBeforeStartErrorsNotReady(t *testing.T) {
	mesh := newLocalMesh(t)
	defer mesh.Shutdown()
	rt, err := NewDaemonRuntime(mesh)
	if err != nil {
		t.Fatalf("NewDaemonRuntime: %v", err)
	}
	defer rt.Close()
	_ = rt.RegisterFactoryFunc("noop", func() MeshDaemon { return &noopDaemon{} })
	// No Start()

	_, err = NewForkGroup(rt, "noop", 0x1234, 1, ForkGroupConfig{
		ForkCount:  2,
		LBStrategy: StrategyRoundRobin,
	})
	if err == nil {
		t.Fatal("expected not-ready error")
	}
	var ge *GroupError
	if !errors.As(err, &ge) {
		t.Fatalf("err = %T %v", err, err)
	}
	if ge.Kind != GroupErrNotReady {
		t.Errorf("Kind = %q, want not-ready", ge.Kind)
	}
}

// ----------------------------------------------------------------
// StandbyGroup
// ----------------------------------------------------------------

func TestStandbyGroup_MemberZeroIsActive(t *testing.T) {
	rt, mesh := runtimeWithPeers(t, 3)
	defer mesh.Shutdown()
	defer rt.Close()

	g, err := NewStandbyGroup(rt, "noop", StandbyGroupConfig{
		MemberCount: 3,
		GroupSeed:   seed(0x77),
	})
	if err != nil {
		t.Fatalf("NewStandbyGroup: %v", err)
	}
	defer g.Close()

	if g.MemberCount() != 3 {
		t.Errorf("MemberCount = %d, want 3", g.MemberCount())
	}
	if g.StandbyCount() != 2 {
		t.Errorf("StandbyCount = %d, want 2", g.StandbyCount())
	}
	if g.ActiveIndex() != 0 {
		t.Errorf("ActiveIndex = %d, want 0", g.ActiveIndex())
	}
	if !g.ActiveHealthy() {
		t.Error("ActiveHealthy = false, want true")
	}
	if g.ActiveOrigin() == 0 {
		t.Error("ActiveOrigin = 0, want non-zero")
	}
	if g.BufferedEventCount() != 0 {
		t.Errorf("BufferedEventCount = %d, want 0", g.BufferedEventCount())
	}
	if r := g.MemberRole(0); r != "active" {
		t.Errorf("MemberRole(0) = %q, want active", r)
	}
	if r := g.MemberRole(1); r != "standby" {
		t.Errorf("MemberRole(1) = %q, want standby", r)
	}
	if r := g.MemberRole(99); r != "" {
		t.Errorf("MemberRole(99) = %q, want empty", r)
	}
}

func TestStandbyGroup_MemberCountBelowTwoRejected(t *testing.T) {
	rt, mesh := runtimeWithPeers(t, 3)
	defer mesh.Shutdown()
	defer rt.Close()

	_, err := NewStandbyGroup(rt, "noop", StandbyGroupConfig{
		MemberCount: 1,
		GroupSeed:   seed(0x88),
	})
	if err == nil {
		t.Fatal("expected invalid-config error")
	}
	var ge *GroupError
	if !errors.As(err, &ge) {
		t.Fatalf("err = %T %v", err, err)
	}
	if ge.Kind != GroupErrInvalidConfig {
		t.Errorf("Kind = %q, want invalid-config", ge.Kind)
	}
}

func TestStandbyGroup_UnknownKindErrorsFactoryNotFound(t *testing.T) {
	rt, mesh := runtimeWithPeers(t, 3)
	defer mesh.Shutdown()
	defer rt.Close()

	_, err := NewStandbyGroup(rt, "never-registered", StandbyGroupConfig{
		MemberCount: 2,
		GroupSeed:   seed(0x99),
	})
	if err == nil {
		t.Fatal("expected factory-not-found error")
	}
	var ge *GroupError
	if !errors.As(err, &ge) {
		t.Fatalf("err = %T %v", err, err)
	}
	if ge.Kind != GroupErrFactoryNotFound {
		t.Errorf("Kind = %q, want factory-not-found", ge.Kind)
	}
}
