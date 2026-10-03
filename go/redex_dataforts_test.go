package net

// Tests for RedEX replication, greedy Dataforts and data gravity
// (go/redex_dataforts.go). Plan:
// docs/internal/plans/GO_BINDING_CONSOLIDATION_AND_BLOBS_PLAN.md, slice S3.
//
// Replication and greedy are proven by data arriving, not by a metric
// alone. Gravity is proven only as far as Go can reach: its configuration is
// forwarded and its install rules hold. Heat itself comes from reads served
// by the greedy cache, a path no binding exposes yet; the native witnesses
// are tests/dataforts_gravity_e2e.rs.

import (
	"errors"
	"fmt"
	"math"
	"strings"
	"testing"
	"time"
)

func waitUntil(t *testing.T, what string, timeout time.Duration, f func() bool) {
	t.Helper()
	deadline := time.Now().Add(timeout)
	for !f() {
		if time.Now().After(deadline) {
			t.Fatalf("timed out after %v: %s", timeout, what)
		}
		time.Sleep(50 * time.Millisecond)
	}
}

func TestRedexReplicatedFileRequiresEnable(t *testing.T) {
	r := NewRedex("")
	defer r.Free()
	_, err := r.OpenFile("go/repl/off", &RedexFileConfig{
		Replication: &RedexReplicationConfig{Factor: 1},
	})
	if !errors.Is(err, ErrRedex) {
		t.Fatalf("replicated OpenFile without EnableReplication: want ErrRedex, got %v", err)
	}
	if n := r.ReplicationRuntimeCount(); n != 0 {
		t.Fatalf("ReplicationRuntimeCount = %d after a refused open, want 0", n)
	}
	if text, err := r.ReplicationPrometheusText(); err != nil || text != "" {
		t.Fatalf("ReplicationPrometheusText with replication off = %q, %v; want \"\"", text, err)
	}
}

// Enabling installs an empty router; the count counts channel runtimes, so
// it moves only when a replicated channel opens, and disabling drains it.
func TestRedexReplicationRuntimeCountFollowsChannels(t *testing.T) {
	a, _, cleanup := meshHandshakePair(t)
	defer cleanup()
	r := NewRedex("")
	defer r.Free()

	if err := r.EnableReplication(a); err != nil {
		t.Fatalf("EnableReplication: %v", err)
	}
	if err := r.EnableReplication(a); err != nil {
		t.Fatalf("second EnableReplication (idempotent): %v", err)
	}
	if n := r.ReplicationRuntimeCount(); n != 0 {
		t.Fatalf("ReplicationRuntimeCount = %d right after enable, want 0 (empty router)", n)
	}

	f, err := r.OpenFile("go/repl/count", &RedexFileConfig{
		Replication: &RedexReplicationConfig{Factor: 1, HeartbeatMs: 150},
	})
	if err != nil {
		t.Fatalf("replicated OpenFile: %v", err)
	}
	defer f.Close()
	if n := r.ReplicationRuntimeCount(); n != 1 {
		t.Fatalf("ReplicationRuntimeCount = %d after opening a replicated channel, want 1", n)
	}
	text, err := r.ReplicationPrometheusText()
	if err != nil {
		t.Fatalf("ReplicationPrometheusText: %v", err)
	}
	if !strings.Contains(text, `channel="go/repl/count"`) {
		t.Fatalf("replication metrics do not name the channel:\n%s", text)
	}
	if seq, err := f.Append([]byte("x")); err != nil || seq != 0 {
		t.Fatalf("Append on a replicated channel = %d, %v; want 0, nil", seq, err)
	}

	if err := r.DisableReplication(); err != nil {
		t.Fatalf("DisableReplication: %v", err)
	}
	if n := r.ReplicationRuntimeCount(); n != 0 {
		t.Fatalf("ReplicationRuntimeCount = %d after disable, want 0", n)
	}
	if err := r.DisableReplication(); err != nil {
		t.Fatalf("second DisableReplication (idempotent): %v", err)
	}
	// The file stays open as a local log.
	if _, err := f.Append([]byte("y")); err != nil {
		t.Fatalf("Append after DisableReplication: %v", err)
	}
}

// The data-arrival witness: written on A, read back on B.
func TestRedexReplicationCarriesDataBetweenNodes(t *testing.T) {
	a, b, cleanup := meshHandshakePair(t)
	defer cleanup()
	rA, rB := NewRedex(""), NewRedex("")
	defer rA.Free()
	defer rB.Free()
	if err := rA.EnableReplication(a); err != nil {
		t.Fatalf("EnableReplication(a): %v", err)
	}
	defer rA.DisableReplication()
	if err := rB.EnableReplication(b); err != nil {
		t.Fatalf("EnableReplication(b): %v", err)
	}
	defer rB.DisableReplication()

	leader := a.NodeID()
	cfg := &RedexFileConfig{Replication: &RedexReplicationConfig{
		HeartbeatMs:  150,
		Placement:    PlacementPinned,
		PinnedNodes:  []uint64{a.NodeID(), b.NodeID()},
		LeaderPinned: &leader,
	}}
	const channel = "go/repl/pair"
	fA, err := rA.OpenFile(channel, cfg)
	if err != nil {
		t.Fatalf("OpenFile(a): %v", err)
	}
	defer fA.Close()
	fB, err := rB.OpenFile(channel, cfg)
	if err != nil {
		t.Fatalf("OpenFile(b): %v", err)
	}
	defer fB.Close()

	waitUntil(t, "A elected leader", 10*time.Second, func() bool {
		text, _ := rA.ReplicationPrometheusText()
		return strings.Contains(text, `dataforts_leader_changes_total{channel="`+channel+`"} 1`)
	})
	const n = 8
	for i := 0; i < n; i++ {
		if _, err := fA.Append([]byte(fmt.Sprintf("event-%d", i))); err != nil {
			t.Fatalf("Append(a): %v", err)
		}
	}
	waitUntil(t, "B caught up", 10*time.Second, func() bool {
		events, err := fB.ReadRange(0, n)
		return err == nil && len(events) == n
	})
	events, err := fB.ReadRange(n-1, n)
	if err != nil || len(events) != 1 {
		t.Fatalf("ReadRange(b) last = %v, %v", events, err)
	}
	if got := string(events[0].Payload); got != fmt.Sprintf("event-%d", n-1) {
		t.Fatalf("B's last event = %q, want %q", got, fmt.Sprintf("event-%d", n-1))
	}
}

// Greedy admission is event-driven: B caches a peer's channel once it
// observes that channel's events. Disabling removes the cache.
func TestRedexGreedyCachesAPeersChannel(t *testing.T) {
	a, b, cleanup := meshHandshakePair(t)
	defer cleanup()
	rB := NewRedex("")
	defer rB.Free()

	if err := rB.EnableGreedyDataforts(b, &GreedyConfig{IntentMatch: "disabled"}); err != nil {
		t.Fatalf("EnableGreedyDataforts: %v", err)
	}
	if err := rB.EnableGreedyDataforts(b, &GreedyConfig{IntentMatch: "disabled"}); err != nil {
		t.Fatalf("second EnableGreedyDataforts (idempotent): %v", err)
	}
	if n := rB.GreedyCachedChannelCount(); n != 0 {
		t.Fatalf("GreedyCachedChannelCount = %d before any event, want 0", n)
	}

	const channel = "go/greedy/observed"
	if err := a.RegisterChannel(ChannelConfig{Name: channel, Visibility: "global", Reliable: true}); err != nil {
		t.Fatalf("RegisterChannel: %v", err)
	}
	if err := b.SubscribeChannel(a.NodeID(), channel); err != nil {
		t.Fatalf("SubscribeChannel: %v", err)
	}
	waitUntil(t, "greedy cached A's channel", 10*time.Second, func() bool {
		_, err := a.Publish(channel, []byte("observed"), PublishConfig{
			Reliability: "reliable",
			OnFailure:   "best_effort",
		})
		if err != nil {
			t.Fatalf("Publish: %v", err)
		}
		return rB.GreedyCachedChannelCount() >= 1
	})
	text, err := rB.GreedyPrometheusText()
	if err != nil {
		t.Fatalf("GreedyPrometheusText: %v", err)
	}
	if !strings.Contains(text, "dataforts_greedy_") {
		t.Fatalf("greedy metrics missing the dataforts_greedy_ family:\n%s", text)
	}

	if err := rB.DisableGreedyDataforts(); err != nil {
		t.Fatalf("DisableGreedyDataforts: %v", err)
	}
	if err := rB.DisableGreedyDataforts(); err != nil {
		t.Fatalf("second DisableGreedyDataforts (idempotent): %v", err)
	}
	if n := rB.GreedyCachedChannelCount(); n != 0 {
		t.Fatalf("GreedyCachedChannelCount = %d after disable, want 0", n)
	}
	if text, err := rB.GreedyPrometheusText(); err != nil || text != "" {
		t.Fatalf("GreedyPrometheusText after disable = %q, %v; want \"\"", text, err)
	}
	// No new admissions once disabled.
	if _, err := a.Publish(channel, []byte("after"), PublishConfig{Reliability: "reliable", OnFailure: "best_effort"}); err != nil {
		t.Fatalf("Publish after disable: %v", err)
	}
	time.Sleep(300 * time.Millisecond)
	if n := rB.GreedyCachedChannelCount(); n != 0 {
		t.Fatalf("GreedyCachedChannelCount = %d after a post-disable publish, want 0", n)
	}
}

func TestRedexGreedyConfigRefusals(t *testing.T) {
	_, b, cleanup := meshHandshakePair(t)
	defer cleanup()
	r := NewRedex("")
	defer r.Free()

	// Parses, fails the native validation: NET_ERR_REDEX.
	err := r.EnableGreedyDataforts(b, &GreedyConfig{IntentMatch: "sometimes"})
	if !errors.Is(err, ErrRedex) {
		t.Fatalf("unknown intent_match: want ErrRedex, got %v", err)
	}
	// Cannot be encoded at all: refused before the C call.
	err = r.EnableGreedyDataforts(b, &GreedyConfig{BandwidthBudgetFraction: float32(math.NaN())})
	if !errors.Is(err, ErrInvalidRedexConfig) || !errors.Is(err, ErrRedex) {
		t.Fatalf("NaN budget: want ErrInvalidRedexConfig (and ErrRedex), got %v", err)
	}
	if text, err := r.GreedyPrometheusText(); err != nil || text != "" {
		t.Fatalf("a refused config installed greedy anyway: %q, %v", text, err)
	}
}

// Gravity: forwarding and install rules. (Heat emission needs greedy-cache
// reads, which no binding can drive yet; see the file comment.)
func TestRedexGravityInstallRules(t *testing.T) {
	_, b, cleanup := meshHandshakePair(t)
	defer cleanup()
	r := NewRedex("")
	defer r.Free()

	// Gravity rides on greedy.
	if err := r.EnableGravityForGreedy(b, nil); !errors.Is(err, ErrRedex) {
		t.Fatalf("gravity without greedy: want ErrRedex, got %v", err)
	}

	if err := r.EnableGreedyDataforts(b, &GreedyConfig{IntentMatch: "disabled"}); err != nil {
		t.Fatalf("EnableGreedyDataforts: %v", err)
	}
	off := false
	for _, cfg := range []*DataGravityConfig{
		nil,
		{TickIntervalMs: 50, EmitThresholdRatio: 1.5, DecayHalfLifeSecs: 60, NormalizationReferenceRate: 10},
		{Enabled: &off}, // carried but not emitting
	} {
		if err := r.EnableGravityForGreedy(b, cfg); err != nil {
			t.Fatalf("EnableGravityForGreedy(%+v): %v", cfg, err)
		}
	}
	nan := &DataGravityConfig{EmitThresholdRatio: float32(math.NaN())}
	if err := r.EnableGravityForGreedy(b, nan); !errors.Is(err, ErrInvalidRedexConfig) {
		t.Fatalf("NaN threshold: want ErrInvalidRedexConfig, got %v", err)
	}

	if err := r.DisableGravityForGreedy(); err != nil {
		t.Fatalf("DisableGravityForGreedy: %v", err)
	}
	if err := r.DisableGravityForGreedy(); err != nil {
		t.Fatalf("second DisableGravityForGreedy (idempotent): %v", err)
	}
	// Greedy keeps running without gravity.
	if text, err := r.GreedyPrometheusText(); err != nil || text == "" {
		t.Fatalf("greedy stopped with gravity: %q, %v", text, err)
	}
}

// Every Enable* clones the mesh reference only after its own checks pass,
// and refuses cleanly on a closed Redex or a shut-down mesh.
func TestRedexDatafortsLifecycleRefusals(t *testing.T) {
	a, _, cleanup := meshHandshakePair(t)
	defer cleanup()

	closed := NewRedex("")
	closed.Free()
	for name, err := range map[string]error{
		"EnableReplication":      closed.EnableReplication(a),
		"EnableGreedyDataforts":  closed.EnableGreedyDataforts(a, nil),
		"EnableGravityForGreedy": closed.EnableGravityForGreedy(a, nil),
		"DisableReplication":     closed.DisableReplication(),
	} {
		if !errors.Is(err, ErrRedex) || !errors.Is(err, ErrShuttingDown) {
			t.Fatalf("%s on a freed Redex: want ErrRedex + ErrShuttingDown, got %v", name, err)
		}
	}
	if n := closed.ReplicationRuntimeCount(); n != 0 {
		t.Fatalf("ReplicationRuntimeCount on a freed Redex = %d, want 0", n)
	}

	r := NewRedex("")
	defer r.Free()
	if err := r.EnableReplication(nil); !errors.Is(err, ErrRedex) {
		t.Fatalf("EnableReplication(nil): want ErrRedex, got %v", err)
	}
	gone, err := NewMeshNode(MeshConfig{BindAddr: reserveLocalUDPPort(t), PskHex: meshPsk})
	if err != nil {
		t.Fatalf("NewMeshNode: %v", err)
	}
	gone.Shutdown()
	if err := r.EnableReplication(gone); !errors.Is(err, ErrRedex) {
		t.Fatalf("EnableReplication on a shut-down mesh: want ErrRedex, got %v", err)
	}
}
