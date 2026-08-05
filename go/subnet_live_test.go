// S4 — the Go live cell for the subnet-exported surface.
//
// The Go twin of bindings/node/test/subnet_live.test.ts and
// bindings/python/tests/test_subnet_live.py: a provider inside a protected
// subnet serves a NAMED export over real transport, a same-org caller invokes
// it with organization authority only, and a foreign-org caller is refused —
// all from artifacts MINTED BY RUST and loaded from disk. The
// `gen_subnet_scenario` example writes the whole chain; this consumes the SAME
// manifest the Node, Python, and C harnesses load.
//
// Ten points, all proven here:
//
//	 1 provider construction: roots, attachment, named exports
//	 2 local refusal of an unknown export, before announcement
//	 3 serve through the frozen named-export API
//	 4 caller construction from real generated org credentials
//	 5 live public discovery
//	 6 a successful CallExported
//	 7 verified caller + organization attribution at the handler
//	 8 fail-closed for a foreign-org caller
//	 9 that denial is not retried
//	10 clean close, with no callback racing teardown
//
// Env: needs a Rust toolchain (to generate the scenario) and the libnet_org
// cdylib on the link path; skips cleanly otherwise.

package net

import (
	"context"
	"encoding/hex"
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"sync/atomic"
	"testing"
	"time"
)

type s4Authority struct {
	AuthorityHex             string   `json:"authority_hex"`
	RootHexes                []string `json:"root_hexes"`
	MaximumGrantLifetimeSecs uint64   `json:"maximum_grant_lifetime_secs"`
}

type s4Binding struct {
	AuthorityHex  string   `json:"authority_hex"`
	Path          []uint32 `json:"path"`
	TopologyEpoch uint32   `json:"topology_epoch"`
}

type s4Provider struct {
	SeedHex                string     `json:"seed_hex"`
	EntityIDHex            string     `json:"entity_id_hex"`
	OrgIDHex               string     `json:"org_id_hex"`
	AuthorityDir           string     `json:"authority_dir"`
	Attachment             []uint32   `json:"attachment"`
	GatewayCredentialsPath string     `json:"gateway_credentials_path"`
	BoundaryPaths          [][]uint32 `json:"boundary_paths"`
}

type s4Caller struct {
	SeedHex        string `json:"seed_hex"`
	EntityIDHex    string `json:"entity_id_hex"`
	OrgIDHex       string `json:"org_id_hex"`
	AuthorityDir   string `json:"authority_dir"`
	MembershipPath string `json:"membership_path"`
	DispatcherPath string `json:"dispatcher_path"`
}

type s4Manifest struct {
	PskHex            string        `json:"psk_hex"`
	ExportedService   string        `json:"exported_service"`
	ExportName        string        `json:"export_name"`
	UnknownExportName string        `json:"unknown_export_name"`
	ExportAccess      string        `json:"export_access"`
	SubnetAuthorities []s4Authority `json:"subnet_authorities"`
	ExportBinding     s4Binding     `json:"export_binding"`
	Provider          s4Provider    `json:"provider"`
	Caller            s4Caller      `json:"caller"`
	ForeignCaller     s4Caller      `json:"foreign_caller"`
}

type s4Ping struct {
	N int `json:"n"`
}

type s4Pong struct {
	N        int    `json:"n"`
	ServedBy string `json:"servedBy"`
}

// genSubnetScenario mints a fresh scenario. Credentials expire, so this is
// never a committed fixture.
func genSubnetScenario(t *testing.T, outdir string) s4Manifest {
	t.Helper()
	_, thisFile, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("runtime.Caller failed")
	}
	crateRoot := filepath.Join(filepath.Dir(thisFile), "..", "net", "crates", "net")
	if _, err := os.Stat(crateRoot); err != nil {
		t.Skipf("crate root not present (%v) — standalone checkout", err)
	}
	cmd := exec.Command(
		"cargo", "run", "-q", "-p", "net-mesh-sdk",
		"--features", "net,cortex,fixtures",
		"--example", "gen_subnet_scenario", "--", outdir,
	)
	cmd.Dir = crateRoot
	cmd.Stderr = os.Stderr
	if err := cmd.Run(); err != nil {
		t.Skipf("cannot generate the subnet scenario (%v) — needs a Rust toolchain", err)
	}
	raw, err := os.ReadFile(filepath.Join(outdir, "manifest.json"))
	if err != nil {
		t.Fatalf("read manifest: %v", err)
	}
	var m s4Manifest
	if err := json.Unmarshal(raw, &m); err != nil {
		t.Fatalf("parse manifest: %v", err)
	}
	return m
}

func TestLiveSubnetExportedCallFromAGeneratedScenario(t *testing.T) {
	outdir := t.TempDir()
	m := genSubnetScenario(t, outdir)
	path := func(rel string) string { return filepath.Join(outdir, rel) }

	// Reserved bind addresses — the Go binding exposes no LocalAddr(), so
	// each node binds where the test says and the test remembers it.
	providerAddr := reserveLocalUDPPort(t)
	callerAddr := reserveLocalUDPPort(t)
	foreignAddr := reserveLocalUDPPort(t)

	// ---- (1) provider construction: roots, attachment, named export ----
	//
	// Every subnet input is CONSTRUCTION state, validated by Rust before the
	// node exists. Application code names them; it builds no authority object.
	provider, err := NewMeshNode(MeshConfig{
		BindAddr:          providerAddr,
		PskHex:            m.PskHex,
		IdentitySeedHex:   m.Provider.SeedHex,
		HeartbeatMs:       200,
		SubnetAuthorities: toSubnetAuthorityConfigs(m.SubnetAuthorities),
		SubnetAttachment:  m.Provider.Attachment,
		SubnetExports: []SubnetNamedExport{{
			Name:   m.ExportName,
			Access: accessFromWire(t, m.ExportAccess),
			Binding: SubnetExportBinding{
				Subnet: SubnetRef{
					AuthorityHex: m.ExportBinding.AuthorityHex,
					Path:         SubnetPath{Levels: m.ExportBinding.Path},
				},
				TopologyEpoch: m.ExportBinding.TopologyEpoch,
			},
		}},
	})
	if err != nil {
		t.Fatalf("provider construction from the manifest: %v", err)
	}
	defer provider.Shutdown()

	// A caller presents organization authority ONLY: it names no subnet, joins
	// no subnet, and needs no trust anchor of its own.
	caller, err := NewMeshNode(MeshConfig{
		BindAddr:        callerAddr,
		PskHex:          m.PskHex,
		IdentitySeedHex: m.Caller.SeedHex,
		HeartbeatMs:     200,
	})
	if err != nil {
		t.Fatalf("caller construction: %v", err)
	}
	defer caller.Shutdown()

	foreign, err := NewMeshNode(MeshConfig{
		BindAddr:        foreignAddr,
		PskHex:          m.PskHex,
		IdentitySeedHex: m.ForeignCaller.SeedHex,
		HeartbeatMs:     200,
	})
	if err != nil {
		t.Fatalf("foreign caller construction: %v", err)
	}
	defer foreign.Shutdown()

	for _, r := range []struct {
		node *MeshNode
		dir  string
	}{
		{provider, m.Provider.AuthorityDir},
		{caller, m.Caller.AuthorityDir},
		{foreign, m.ForeignCaller.AuthorityDir},
	} {
		if err := InstallOrgAuthority(r.node, path(r.dir)); err != nil {
			t.Fatalf("install org authority %s: %v", r.dir, err)
		}
	}

	// Gateway provisioning from the generated artifacts — wholesale.
	creds, err := os.ReadFile(path(m.Provider.GatewayCredentialsPath))
	if err != nil {
		t.Fatalf("read gateway credentials: %v", err)
	}
	if err := InstallSubnetGatewayCredentials(provider, [][]byte{creds}); err != nil {
		t.Fatalf("install gateway credentials: %v", err)
	}
	boundaries := make([]SubnetPath, 0, len(m.Provider.BoundaryPaths))
	for _, p := range m.Provider.BoundaryPaths {
		boundaries = append(boundaries, SubnetPath{Levels: p})
	}
	if err := DeclareSubnetBoundaries(provider, SubnetBoundaryDeclaration{
		AuthorityHex:  m.ExportBinding.AuthorityHex,
		TopologyEpoch: m.ExportBinding.TopologyEpoch,
		Boundaries:    boundaries,
	}); err != nil {
		t.Fatalf("declare boundaries: %v", err)
	}

	// Every accept() must complete before any start().
	if err := handshakeNodes(caller, provider, providerAddr); err != nil {
		t.Fatalf("caller handshake: %v", err)
	}
	if err := handshakeNodes(foreign, provider, providerAddr); err != nil {
		t.Fatalf("foreign handshake: %v", err)
	}
	for _, n := range []*MeshNode{provider, caller, foreign} {
		if err := n.Start(); err != nil {
			t.Fatalf("start: %v", err)
		}
	}

	// ---- (2) an unknown export is refused LOCALLY, before the service is
	//          registered or announced ----
	if _, err := ServeSubnetExported[s4Ping, s4Pong](
		provider, m.ExportedService, m.UnknownExportName,
		func(OrgCaller, s4Ping) (s4Pong, error) { return s4Pong{}, nil },
	); err == nil {
		t.Fatal("an unconfigured export name must be refused")
	} else if k := ParseSubnetKind(err.Error()); k != "unknown_export_name" {
		t.Fatalf("unknown export kind = %q, want unknown_export_name (err %v)", k, err)
	}

	// ---- (3) serve through the frozen named-export API ----
	//
	// The handler runs on a cgo callback thread, so these are atomics: a plain
	// int and bool would be a data race with the assertions below, which `go
	// test -race` would (rightly) fail.
	var calls atomic.Int64
	var attributionOK atomic.Bool
	handle, err := ServeSubnetExported[s4Ping, s4Pong](
		provider, m.ExportedService, m.ExportName,
		func(c OrgCaller, req s4Ping) (s4Pong, error) {
			calls.Add(1)
			// ---- (7) attribution: the provider's VERIFIED view, checked
			// against the identities the manifest itself declares.
			attributionOK.Store(
				hex.EncodeToString(c.Caller[:]) == m.Caller.EntityIDHex &&
					hex.EncodeToString(c.ActingOrg[:]) == m.Caller.OrgIDHex &&
					hex.EncodeToString(c.ProviderOrg[:]) == m.Provider.OrgIDHex &&
					c.IsSameOrg(),
			)
			return s4Pong{N: req.N + 1, ServedBy: "go-s4"}, nil
		},
	)
	if err != nil {
		t.Fatalf("serve the configured export: %v", err)
	}

	// ---- (4) caller credentials, from the generated files ----
	client, err := orgClientFromRole(caller, outdir, m.Caller)
	if err != nil {
		t.Fatalf("bind caller: %v", err)
	}
	defer client.Close()

	// ---- (5) live public discovery, and (6) the call ----
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	var reply s4Pong
	var lastErr error
	deadline := time.Now().Add(45 * time.Second)
	admitted := false
	for time.Now().Before(deadline) && !admitted {
		_ = provider.AnnounceCapabilities(CapabilitySet{})
		_ = caller.AnnounceCapabilities(CapabilitySet{})
		reply, lastErr = CallExported[s4Ping, s4Pong](ctx, client, m.ExportedService, s4Ping{N: 1})
		if lastErr == nil {
			admitted = true
			break
		}
		time.Sleep(500 * time.Millisecond)
	}
	if !admitted {
		t.Fatalf("the exported call was never admitted; last error: %v", lastErr)
	}
	if reply.N != 2 || reply.ServedBy != "go-s4" {
		t.Fatalf("reply = %+v, want {2 go-s4}", reply)
	}
	if got := calls.Load(); got != 1 {
		t.Fatalf("handler ran %d times, want exactly 1", got)
	}
	// ---- (7) ----
	if !attributionOK.Load() {
		t.Fatal("the handler must see the verified caller and organization attribution")
	}

	// ---- (8) fail-closed: a FOREIGN-org caller with valid credentials ----
	//
	// Its membership and dispatcher grant are correctly signed — by the WRONG
	// organization. That is what makes this a boundary test, not a decoder test.
	foreignClient, err := orgClientFromRole(foreign, outdir, m.ForeignCaller)
	if err != nil {
		t.Fatalf("bind foreign caller: %v", err)
	}
	defer foreignClient.Close()

	before := calls.Load()
	mustNotBeServed(t, ctx, foreignClient, m.ExportedService, s4Ping{N: 50})
	if got := calls.Load(); got != before {
		t.Fatalf("the handler ran for a refused caller (%d -> %d)", before, got)
	}

	// ---- (9) the denial is not retried ----
	//
	// A signed proof is never resent. Observed provider-side: a second refused
	// call still never reaches the handler.
	mustNotBeServed(t, ctx, foreignClient, m.ExportedService, s4Ping{N: 51})
	if got := calls.Load(); got != before {
		t.Fatalf("a retry smuggled a refused caller into the handler (%d -> %d)", before, got)
	}

	// ---- (10) clean close, no callback racing teardown ----
	handle.Close()
	handle.Close() // idempotent
	mustNotBeServed(t, ctx, client, m.ExportedService, s4Ping{N: 99})
	if got := calls.Load(); got != 1 {
		t.Fatalf("a handler invocation landed after close (calls = %d)", got)
	}
}

// mustNotBeServed asserts a call is NOT served, bounded.
//
// A refused caller fails locally and fast, but a call after teardown is
// different: the provider is still an announced candidate for a while, so the
// request goes out and simply gets no reply. Unbounded, that turns a correct
// refusal into a hung test. Either outcome — an error, or nothing within the
// bound — proves "not served"; a RETURNED reply is the failure.
func mustNotBeServed(
	t *testing.T, parent context.Context, c *OrgClient, service string, req s4Ping,
) {
	t.Helper()
	ctx, cancel := context.WithTimeout(parent, 8*time.Second)
	defer cancel()
	reply, err := CallExported[s4Ping, s4Pong](ctx, c, service, req)
	if err == nil {
		t.Fatalf("the call must not be served, got %+v", reply)
	}
}

func toSubnetAuthorityConfigs(in []s4Authority) []SubnetAuthorityConfig {
	out := make([]SubnetAuthorityConfig, 0, len(in))
	for _, a := range in {
		out = append(out, SubnetAuthorityConfig{
			AuthorityHex:             a.AuthorityHex,
			RootHexes:                a.RootHexes,
			MaximumGrantLifetimeSecs: a.MaximumGrantLifetimeSecs,
		})
	}
	return out
}

func accessFromWire(t *testing.T, wire string) SubnetExportAccess {
	t.Helper()
	switch wire {
	case "sameOrg", "same_org":
		return SubnetAccessSameOrg
	case "granted":
		return SubnetAccessGranted
	default:
		t.Fatalf("manifest access %q is not a known spelling", wire)
		return SubnetAccessSameOrg
	}
}

func orgClientFromRole(node *MeshNode, outdir string, role s4Caller) (*OrgClient, error) {
	membership, err := os.ReadFile(filepath.Join(outdir, role.MembershipPath))
	if err != nil {
		return nil, err
	}
	dispatcher, err := os.ReadFile(filepath.Join(outdir, role.DispatcherPath))
	if err != nil {
		return nil, err
	}
	creds, err := NewOrgCredentials(OrgCredentialsConfig{
		Membership: membership,
		Dispatcher: dispatcher,
	})
	if err != nil {
		return nil, err
	}
	return NewOrgClient(node, creds)
}

// handshakeNodes runs the a2a routed handshake: the acceptor waits while the
// connector dials — both BEFORE either node is started.
//
// `acceptorAddr` is passed rather than read off the node: the Go binding has no
// `LocalAddr()`, so a test binds to a reserved address and remembers it. No
// sleep before Connect — both handshake helpers retry with backoff, so a
// Connect that races ahead of Accept is absorbed rather than failing.
func handshakeNodes(connector, acceptor *MeshNode, acceptorAddr string) error {
	acceptorPub, err := acceptor.PublicKey()
	if err != nil {
		return err
	}
	accepted := make(chan error, 1)
	go func() {
		_, e := acceptor.Accept(connector.NodeID())
		accepted <- e
	}()
	if err := connector.Connect(acceptorAddr, acceptorPub, acceptor.NodeID()); err != nil {
		return err
	}
	select {
	case err := <-accepted:
		return err
	case <-time.After(10 * time.Second):
		return context.DeadlineExceeded
	}
}
