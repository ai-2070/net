package net

// Capability sensing through the Go binding (SENSING_OLB_COMPLETION_PLAN B2).
// Every behaviour asserted is the Rust SDK's, forwarded over the C ABI:
//
//   - the cgo preamble's constants match include/net_sensing.h (org-ffi pins
//     the header against Rust, so all three agree);
//   - each fixture vector classifies through the Rust classifier exactly as
//     Rust classifies it, and a live refusal carries the Rust wire (C5);
//   - a ProvideReadiness Set reaches a consumer's snapshot through real
//     signed attestations;
//   - Close from another goroutine ends a pending Changed as "watch_closed",
//     and a cancelled ctx ends it without closing the watch (B1-pre).

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"
)

func sensingCrateRoot(t *testing.T) string {
	t.Helper()
	_, thisFile, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("runtime.Caller failed")
	}
	return filepath.Join(filepath.Dir(thisFile), "..", "net", "crates", "net")
}

type sensingVector struct {
	Kind     string `json:"kind"`
	Terminal bool   `json:"terminal"`
	Wire     string `json:"wire"`
}

func sensingVectors(t *testing.T) []sensingVector {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join(sensingCrateRoot(t), "tests", "cross_lang_sensing", "error_vectors.json"))
	if err != nil {
		t.Fatalf("read fixture: %v", err)
	}
	var doc struct {
		Vectors []sensingVector `json:"vectors"`
	}
	if err := json.Unmarshal(raw, &doc); err != nil {
		t.Fatalf("decode fixture: %v", err)
	}
	if len(doc.Vectors) == 0 {
		t.Fatal("the fixture has no vectors")
	}
	return doc.Vectors
}

func TestSensingErrorVectorsClassifyThroughRust(t *testing.T) {
	for _, v := range sensingVectors(t) {
		got := ClassifySensingError(v.Wire)
		if got == nil {
			t.Errorf("%q did not classify", v.Wire)
			continue
		}
		if got.Kind != v.Kind || got.Terminal != v.Terminal {
			t.Errorf("%q: got {%s %v}, fixture {%s %v}", v.Wire, got.Kind, got.Terminal, v.Kind, v.Terminal)
		}
	}
	if ClassifySensingError("org:rpc:timeout: x") != nil {
		t.Error("a non-sensing wire must not classify")
	}
}

func TestSensingNextIncarnationIsPersistedAndMonotonic(t *testing.T) {
	path := filepath.Join(t.TempDir(), "incarnation")
	first, err := SensingNextIncarnation(path)
	if err != nil {
		t.Fatalf("first: %v", err)
	}
	second, err := SensingNextIncarnation(path)
	if err != nil {
		t.Fatalf("second: %v", err)
	}
	if first != 1 || second != 2 {
		t.Fatalf("got %d then %d, want 1 then 2", first, second)
	}
	if err := os.WriteFile(path, []byte("garbage"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := SensingNextIncarnation(path); err == nil {
		t.Fatal("a corrupt counter must fail closed, not restart at 1")
	}
}

func TestSensingRefusalWithoutSensingIsTheRustWire(t *testing.T) {
	node, err := NewMeshNode(MeshConfig{BindAddr: reserveLocalUDPPort(t), PskHex: strings.Repeat("42", 32)})
	if err != nil {
		t.Fatalf("node: %v", err)
	}
	t.Cleanup(func() { _ = node.Shutdown() })
	_, err = WatchSensing(node, "nrpc:anything", SensingQueryOptions{})
	var se *SensingError
	if !errors.As(err, &se) {
		t.Fatalf("want *SensingError, got %v", err)
	}
	var disabled sensingVector
	for _, v := range sensingVectors(t) {
		if v.Kind == "disabled" {
			disabled = v
		}
	}
	if se.Kind != "disabled" || !se.Terminal || se.Wire != disabled.Wire {
		t.Fatalf("got %+v, want the fixture's disabled vector %+v", se, disabled)
	}
}

func TestSensingLiveValueReachesTheConsumerAndCloseEndsAPendingWait(t *testing.T) {
	outdir := t.TempDir()
	m := genSubnetScenario(t, outdir)
	path := func(rel string) string { return filepath.Join(outdir, rel) }

	newNode := func(seed, name string) (*MeshNode, string) {
		addr := reserveLocalUDPPort(t)
		node, err := NewMeshNode(MeshConfig{
			BindAddr:               addr,
			PskHex:                 m.PskHex,
			IdentitySeedHex:        seed,
			HeartbeatMs:            200,
			EnableSensing:          true,
			SensingIncarnationPath: path(name + ".incarnation"),
		})
		if err != nil {
			t.Fatalf("%s construction: %v", name, err)
		}
		t.Cleanup(func() { _ = node.Shutdown() })
		return node, addr
	}
	provider, providerAddr := newNode(m.Provider.SeedHex, "provider")
	consumer, _ := newNode(m.Caller.SeedHex, "consumer")
	quiet, _ := newNode(m.ForeignCaller.SeedHex, "quiet")

	if err := InstallOrgAuthority(provider, path(m.Provider.AuthorityDir)); err != nil {
		t.Fatalf("provider authority: %v", err)
	}
	if err := shareOwnerAudience(path(m.Provider.AuthorityDir), path(m.Caller.AuthorityDir)); err != nil {
		t.Fatalf("share audience: %v", err)
	}
	if err := InstallOrgAuthority(consumer, path(m.Caller.AuthorityDir)); err != nil {
		t.Fatalf("consumer authority: %v", err)
	}
	if err := InstallOrgAuthority(quiet, path(m.ForeignCaller.AuthorityDir)); err != nil {
		t.Fatalf("quiet authority: %v", err)
	}
	if err := handshakeNodes(consumer, provider, providerAddr); err != nil {
		t.Fatalf("handshake: %v", err)
	}
	for _, node := range []*MeshNode{consumer, provider, quiet} {
		if err := node.Start(); err != nil {
			t.Fatalf("start: %v", err)
		}
	}
	const service = "sensing.go.live"
	const tag = "nrpc:" + service
	handle, err := ServeOrgBytes(provider, service, OrgAccessSameOrg, func(OrgCaller, []byte) ([]byte, error) {
		return []byte("ok"), nil
	})
	if err != nil {
		t.Fatalf("serve: %v", err)
	}
	t.Cleanup(func() { handle.Close() })

	start := 40 * time.Millisecond
	ready, err := ProvideReadiness(provider, tag, ReadinessValue{State: ReadinessReady, EstimatedStart: &start})
	if err != nil {
		t.Fatalf("provide: %v", err)
	}
	t.Cleanup(func() { ready.Close() })
	providerID := provider.NodeID()

	watch, err := WatchSensing(consumer, tag, SensingQueryOptions{})
	if err != nil {
		t.Fatalf("watch: %v", err)
	}
	t.Cleanup(func() { watch.Close() })

	until := func(what string, predicate func(SensedProvider) bool) {
		t.Helper()
		deadline := time.Now().Add(60 * time.Second)
		for time.Now().Before(deadline) {
			snap, err := watch.Snapshot()
			if err != nil {
				t.Fatalf("%s: snapshot: %v", what, err)
			}
			for _, row := range snap.Providers {
				if row.NodeID == providerID && predicate(row) {
					snap.Release()
					return
				}
			}
			snap.Release()
			_ = provider.AnnounceCapabilities(CapabilitySet{})
			_ = consumer.AnnounceCapabilities(CapabilitySet{})
			ctx, cancel := context.WithTimeout(context.Background(), 500*time.Millisecond)
			_ = watch.Changed(ctx)
			cancel()
		}
		t.Fatalf("%s within 60s", what)
	}
	until("the provider never read ready", func(r SensedProvider) bool { return r.Readiness == "ready" })
	if _, err := ready.Set(ReadinessValue{State: ReadinessNotReady, Reason: 3}); err != nil {
		t.Fatalf("set: %v", err)
	}
	until("a not_ready set never reached the consumer", func(r SensedProvider) bool { return r.Viability == "not_viable" })

	// A cancelled ctx ends the wait without closing the watch.
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if err := watch.Changed(ctx); !errors.Is(err, context.Canceled) {
		t.Fatalf("a cancelled ctx: got %v", err)
	}
	if _, err := watch.Snapshot(); err != nil {
		t.Fatalf("the watch must still be open after a cancelled wait: %v", err)
	}

	// B1-pre on the QUIET node: no peers and no providers, so only the 1 s
	// population floor could wake the wait. Close from another goroutine ends
	// the pending Changed as watch_closed, never as a wake.
	quietWatch, err := WatchSensing(quiet, "nrpc:quiet.capability", SensingQueryOptions{})
	if err != nil {
		t.Fatalf("quiet watch: %v", err)
	}
	if snap, err := quietWatch.Snapshot(); err == nil {
		snap.Release()
	} else {
		t.Fatalf("quiet snapshot: %v", err)
	}
	result := make(chan error, 1)
	go func() { result <- quietWatch.Changed(context.Background()) }()
	select {
	case err := <-result:
		t.Fatalf("precondition: the wait must be parked, it returned %v", err)
	case <-time.After(100 * time.Millisecond):
	}
	if !quietWatch.Close() {
		t.Fatal("Close must report that it closed the watch")
	}
	select {
	case err := <-result:
		var se *SensingError
		if !errors.As(err, &se) || se.Kind != "watch_closed" || !se.Terminal {
			t.Fatalf("a wait pending at Close must end as watch_closed, got %v", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("a pending wait did not end promptly on Close")
	}
	if quietWatch.Close() {
		t.Fatal("a second Close must report false")
	}

	if !ready.Close() {
		t.Fatal("the provider's Close must report that it removed the registration")
	}
}

// A conflicting boot-counter configuration is refused before any node exists:
// a path silently skipped in favour of an explicit value would let a stale
// incarnation sign as a later boot.
func TestSensingIncarnationSourcesAreMutuallyExclusive(t *testing.T) {
	_, err := NewMeshNode(MeshConfig{
		BindAddr:               reserveLocalUDPPort(t),
		PskHex:                 strings.Repeat("42", 32),
		EnableSensing:          true,
		SensingIncarnationPath: filepath.Join(t.TempDir(), "incarnation"),
		SensingIncarnation:     7,
	})
	if err == nil || !strings.Contains(err.Error(), "mutually exclusive") {
		t.Fatalf("both sources must be refused, got %v", err)
	}
}

// Negative bounds are refused instead of being read as "absent", and a positive
// sub-millisecond bound rounds UP so it never becomes Rust's unsatisfiable 0.
func TestSensingDurationsAreValidatedBeforeTheABI(t *testing.T) {
	negative := -time.Millisecond
	if _, err := WatchSensing(nil, "nrpc:x", SensingQueryOptions{Within: &negative}); err == nil ||
		!strings.Contains(err.Error(), "must not be negative") {
		t.Fatalf("a negative Within must be refused, got %v", err)
	}
	if _, err := WatchSensing(nil, "nrpc:x", SensingQueryOptions{StartWithin: &negative}); err == nil ||
		!strings.Contains(err.Error(), "must not be negative") {
		t.Fatalf("a negative StartWithin must be refused, got %v", err)
	}
	if _, err := CallServiceSensed(nil, "x", nil, nil, -time.Second); err == nil ||
		!strings.Contains(err.Error(), "must not be negative") {
		t.Fatalf("a negative timeout must be refused, got %v", err)
	}
	for _, c := range []struct {
		in   time.Duration
		want int64
	}{
		{0, 0},
		{500 * time.Microsecond, 1},
		{time.Millisecond, 1},
		{1500 * time.Microsecond, 2},
		{2 * time.Second, 2000},
	} {
		got, err := durationMs("bound", c.in)
		if err != nil || got != c.want {
			t.Errorf("durationMs(%v) = %d, %v; want %d", c.in, got, err, c.want)
		}
	}
}
