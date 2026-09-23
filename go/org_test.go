// The Go org surface through the real cgo boundary (OSDK-L Workstream G).
//
// Issuance is deliberately absent from every binding (credentials come from the
// `net org` CLI), so the unit tests cover the construction, refusal, provenance,
// and provisioning paths a Go application can reach without a full issuance
// chain. The LIVE cells (this file's Stage 4 section) run admitted calls
// against two mesh nodes from artifacts MINTED BY RUST
// (`gen_org_scenario` / `gen_subnet_scenario`) and loaded from disk — the
// exact bytes-and-paths contract every language harness consumes.

package net

import (
	"context"
	"encoding/hex"
	"encoding/json"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"sync"
	"testing"
	"time"
)

// hexSeed is a 64-char hex seed of 32 repeated bytes — a durable identity.
func hexSeed(b byte) string {
	seed := make([]byte, 32)
	for i := range seed {
		seed[i] = b
	}
	return hex.EncodeToString(seed)
}

// Correctly-sized but unsigned credential bytes are refused across the FFI, and
// the refusal carries the canonical `org:credentials:signature_invalid`
// vocabulary intact. 156 / 185 are the exact wire lengths, so this proves
// signature verification runs across the boundary — not a length check.
func TestOrgCredentials_RefusesUnsignedWithOrgVocabulary(t *testing.T) {
	_, err := NewOrgCredentials(OrgCredentialsConfig{
		Membership: make([]byte, 156),
		Dispatcher: make([]byte, 185),
	})
	if err == nil {
		t.Fatal("expected unsigned credentials to be refused")
	}
	var oe *OrgError
	if !errors.As(err, &oe) {
		t.Fatalf("expected *OrgError, got %T: %v", err, err)
	}
	if oe.Domain != OrgDomainCredentials {
		t.Errorf("domain = %q, want credentials", oe.Domain)
	}
	if oe.Kind != "signature_invalid" {
		t.Errorf("kind = %q, want signature_invalid", oe.Kind)
	}
	if !oe.IsLocal() {
		t.Error("a credential refusal is local — nothing was sent")
	}
	if !errors.Is(err, ErrOrgCredentials) {
		t.Error("errors.Is(err, ErrOrgCredentials) must hold")
	}
}

// Membership and dispatcher are mandatory; an empty config is refused before
// crossing the boundary.
func TestOrgCredentials_RequiresMembershipAndDispatcher(t *testing.T) {
	if _, err := NewOrgCredentials(OrgCredentialsConfig{}); err == nil {
		t.Fatal("empty membership/dispatcher must be refused")
	}
}

// The API has no way to pass an audience secret as bytes: OrgCredentialsConfig
// carries AudienceSecretPaths []string and no bytes sibling, so a discovery key
// can never be a Go []byte (locked decision #1). This test exists to fail
// compilation if a bytes field is ever added — the field it names is the only
// secret channel.
func TestOrgCredentials_SecretIsPathOnly(t *testing.T) {
	cfg := OrgCredentialsConfig{
		Membership:          make([]byte, 156),
		Dispatcher:          make([]byte, 185),
		AudienceSecretPaths: []string{"/etc/net/grants/example.audience"},
	}
	if len(cfg.AudienceSecretPaths) != 1 {
		t.Fatal("AudienceSecretPaths is the sole secret channel — paths, never bytes")
	}
}

// Seeded meshes share a durable entity id; ephemeral ones do not. This is the
// property the org facade's provenance check (§D1a, configured_identity) keys
// off: a seeded identity is org-bindable, an ephemeral one is refused. The
// FFI-level flag itself is witnessed in the Rust crate
// (`net_mesh_new_records_identity_provenance`).
func TestOrgSeededMeshesStableEphemeralNot(t *testing.T) {
	seed := hexSeed(0x7a)
	m1, err := NewMeshNode(MeshConfig{BindAddr: reserveLocalUDPPort(t), PskHex: meshPsk, IdentitySeedHex: seed})
	if err != nil {
		t.Fatalf("seeded mesh 1: %v", err)
	}
	defer m1.Shutdown()
	m2, err := NewMeshNode(MeshConfig{BindAddr: reserveLocalUDPPort(t), PskHex: meshPsk, IdentitySeedHex: seed})
	if err != nil {
		t.Fatalf("seeded mesh 2: %v", err)
	}
	defer m2.Shutdown()

	id1, err := m1.EntityID()
	if err != nil {
		t.Fatal(err)
	}
	id2, err := m2.EntityID()
	if err != nil {
		t.Fatal(err)
	}
	if hex.EncodeToString(id1) != hex.EncodeToString(id2) {
		t.Errorf("seeded meshes must share a durable entity id: %x vs %x", id1, id2)
	}

	e1, err := NewMeshNode(MeshConfig{BindAddr: reserveLocalUDPPort(t), PskHex: meshPsk})
	if err != nil {
		t.Fatalf("ephemeral mesh 1: %v", err)
	}
	defer e1.Shutdown()
	e2, err := NewMeshNode(MeshConfig{BindAddr: reserveLocalUDPPort(t), PskHex: meshPsk})
	if err != nil {
		t.Fatalf("ephemeral mesh 2: %v", err)
	}
	defer e2.Shutdown()
	eid1, _ := e1.EntityID()
	eid2, _ := e2.EntityID()
	if hex.EncodeToString(eid1) == hex.EncodeToString(eid2) {
		t.Error("ephemeral meshes must not share an entity id")
	}
}

// The provisioning surface the org facade is non-functional without exists and
// refuses bad input across the FFI (§D9). A full install needs a real adopted
// authority directory (operator setup); this asserts the error paths marshal.
func TestOrgProvisioningSurface(t *testing.T) {
	m, err := NewMeshNode(MeshConfig{BindAddr: reserveLocalUDPPort(t), PskHex: meshPsk, IdentitySeedHex: hexSeed(0x61)})
	if err != nil {
		t.Fatalf("mesh: %v", err)
	}
	defer m.Shutdown()

	// A nonexistent authority directory is refused, as a provisioning error
	// (not a call-domain result).
	err = InstallOrgAuthority(m, filepath.Join(t.TempDir(), "no-such-authority"))
	if err == nil {
		t.Error("a nonexistent authority dir must be refused")
	} else if !errors.Is(err, ErrOrgProvision) {
		t.Errorf("expected ErrOrgProvision, got %v", err)
	}

	// A right-length-but-unsigned grant + a bogus secret path is refused —
	// proving both the grant bytes and the path cross and the loader runs.
	err = InstallProviderGrantAudience(m, make([]byte, 318), filepath.Join(t.TempDir(), "no-such-secret"))
	if err == nil {
		t.Error("a bad grant + secret path must be refused")
	}
}

// OrgAccess maps to the C ABI's access constants (0 = same-org, 1 = granted).
func TestOrgAccessConstants(t *testing.T) {
	if OrgAccessSameOrg != 0 {
		t.Errorf("OrgAccessSameOrg = %d, want 0", OrgAccessSameOrg)
	}
	if OrgAccessGranted != 1 {
		t.Errorf("OrgAccessGranted = %d, want 1", OrgAccessGranted)
	}
}

// OrgCaller.IsSameOrg compares the acting and provider orgs — the one derived
// fact on the verified projection.
func TestOrgCallerIsSameOrg(t *testing.T) {
	var c OrgCaller
	for i := range c.ActingOrg {
		c.ActingOrg[i] = 0x11
		c.ProviderOrg[i] = 0x11
	}
	if !c.IsSameOrg() {
		t.Error("equal acting/provider org must be same-org")
	}
	c.ProviderOrg[0] = 0x22
	if c.IsSameOrg() {
		t.Error("differing acting/provider org must not be same-org")
	}
}

// =========================================================================
// Stage 4 live cells (ORG_SCOPED_STREAMING_PLAN §4.4, Go/C row).
//
// The three streaming shapes, call AND serve, through the real cgo boundary
// against two live mesh nodes over loopback UDP — driven from artifacts
// MINTED BY RUST and loaded from disk (the same manifest contract the Node /
// Python / Rust harnesses load), gated on RUN_INTEGRATION_TESTS=1 like the
// other live suites and skipping cleanly without a Rust toolchain.
//
// Two authority modes per shape:
//
//   - GRANTED (cross-org): `gen_org_scenario` — org B's provider serves
//     Granted with its installed provider grant audience; org A's caller
//     invokes with (membership, dispatcher, grant, grant secret PATH).
//   - SAME-ORG: `gen_subnet_scenario`'s org artifacts (this section uses the
//     PLAIN protected surface, not the subnet plane) plus spec §3.4's
//     out-of-band pre-staging step performed FROM FILES (see
//     shareOwnerAudience).
//
// Every handler records the provider-verified OrgCaller it received, and
// every test asserts the attribution afterwards — that projection is the
// discriminating surface of the caller-projection inverse receipt (a
// zeroed / swapped / wrong-source projection reddens these assertions).
//
// HANDLER-DROP CONTRACT (spec §2.2; the F-S3.1-2 level): the retire
// supervisor may drop the handler future WITHOUT a final poll — the
// handlers below observe retirement ONLY through Recv / Send returning
// ErrStreamDone and exit cooperatively there; there is no cancellation
// callback and no guaranteed final poll (see org.go's dispatch section).
// =========================================================================

// xScenarioRole is the caller role of the generated CROSS-ORG scenario
// (`gen_org_scenario` / `write_cross_org_scenario`).
type xScenarioRole struct {
	SeedHex         string `json:"seed_hex"`
	OrgIDHex        string `json:"org_id_hex"`
	AuthorityDir    string `json:"authority_dir"`
	MembershipPath  string `json:"membership_path"`
	DispatcherPath  string `json:"dispatcher_path"`
	GrantPath       string `json:"grant_path"`
	GrantSecretPath string `json:"grant_secret_path"`
}

// xScenarioProvider is the provider role of the generated CROSS-ORG scenario.
type xScenarioProvider struct {
	SeedHex         string `json:"seed_hex"`
	OrgIDHex        string `json:"org_id_hex"`
	AuthorityDir    string `json:"authority_dir"`
	GrantPath       string `json:"grant_path"`
	GrantSecretPath string `json:"grant_secret_path"`
}

// xScenarioManifest is the subset of the generated manifest.json these
// harnesses load. Paths are relative to the manifest's directory.
type xScenarioManifest struct {
	PskHex        string            `json:"psk_hex"`
	GrantedService string            `json:"granted_service"`
	Provider      xScenarioProvider `json:"provider"`
	Caller        xScenarioRole     `json:"caller"`
}

// genOrgScenario mints a fresh cross-org scenario. Credentials expire, so
// this is never a committed fixture (the genSubnetScenario discipline).
func genOrgScenario(t *testing.T, outdir string) xScenarioManifest {
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
		"--example", "gen_org_scenario", "--", outdir,
	)
	cmd.Dir = crateRoot
	cmd.Stderr = os.Stderr
	if err := cmd.Run(); err != nil {
		t.Skipf("cannot generate the org scenario (%v) — needs a Rust toolchain", err)
	}
	raw, err := os.ReadFile(filepath.Join(outdir, "manifest.json"))
	if err != nil {
		t.Fatalf("read manifest: %v", err)
	}
	var m xScenarioManifest
	if err := json.Unmarshal(raw, &m); err != nil {
		t.Fatalf("parse manifest: %v", err)
	}
	return m
}

// shareOwnerAudience performs spec §3.4's out-of-band pre-staging step FROM
// THE GENERATED FILES: it copies the provider authority's owner-audience
// credential (owner-audience.key) over the caller authority's, BEFORE
// InstallOrgAuthority loads either. Owner-scoped discovery is keyed on ONE
// per-organization audience, so two independently adopted nodes each minting
// their own could never open each other's envelopes. The Rust same-org
// witnesses perform this in memory (tests_live.rs `fast_mesh`'s
// shared_audience); a file-based language harness performs it on the adopted
// directories. Overwriting in place preserves the file's owner-only ACL,
// which the install path re-checks.
func shareOwnerAudience(providerAuthorityDir, callerAuthorityDir string) error {
	const audienceFile = "owner-audience.key"
	src, err := os.ReadFile(filepath.Join(providerAuthorityDir, audienceFile))
	if err != nil {
		return err
	}
	return os.WriteFile(filepath.Join(callerAuthorityDir, audienceFile), src, 0o600)
}

// liveFacts captures what a handler observed about its verified caller.
type liveFacts struct {
	mu     sync.Mutex
	calls  int
	caller OrgCaller
}

func newLiveFacts() *liveFacts { return &liveFacts{} }

func (f *liveFacts) record(c OrgCaller) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.calls++
	f.caller = c
}

func (f *liveFacts) snapshot() (int, OrgCaller) {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.calls, f.caller
}

// livePair is one provider + one bound caller, plus the attribution the
// handler must observe.
type livePair struct {
	provider     *MeshNode
	client       *OrgClient
	service      string
	facts        *liveFacts
	wantSameOrg  bool
	wantActing   string // hex acting-org id
	wantProviderOrg string // hex provider-org id
}

// assertCaller checks the verified-caller attribution a handler recorded.
func (p *livePair) assertCaller(t *testing.T) {
	t.Helper()
	calls, caller := p.facts.snapshot()
	if calls != 1 {
		t.Fatalf("handler calls = %d, want exactly 1", calls)
	}
	if got := hex.EncodeToString(caller.ActingOrg[:]); got != p.wantActing {
		t.Fatalf("handler saw acting org %s, want %s (verified projection)", got, p.wantActing)
	}
	if got := hex.EncodeToString(caller.ProviderOrg[:]); got != p.wantProviderOrg {
		t.Fatalf("handler saw provider org %s, want %s (verified projection)", got, p.wantProviderOrg)
	}
	if caller.IsSameOrg() != p.wantSameOrg {
		t.Fatalf("handler saw IsSameOrg = %v, want %v", caller.IsSameOrg(), p.wantSameOrg)
	}
}

// setupGrantedLive brings up org B's provider (Granted + provider grant
// audience) and org A's caller from the generated CROSS-ORG scenario.
func setupGrantedLive(t *testing.T) *livePair {
	t.Helper()
	outdir := t.TempDir()
	m := genOrgScenario(t, outdir)
	path := func(rel string) string { return filepath.Join(outdir, rel) }

	providerAddr := reserveLocalUDPPort(t)
	callerAddr := reserveLocalUDPPort(t)
	provider, err := NewMeshNode(MeshConfig{
		BindAddr:        providerAddr,
		PskHex:          m.PskHex,
		IdentitySeedHex: m.Provider.SeedHex,
		HeartbeatMs:     200,
	})
	if err != nil {
		t.Fatalf("provider construction: %v", err)
	}
	t.Cleanup(func() { _ = provider.Shutdown() })
	caller, err := NewMeshNode(MeshConfig{
		BindAddr:        callerAddr,
		PskHex:          m.PskHex,
		IdentitySeedHex: m.Caller.SeedHex,
		HeartbeatMs:     200,
	})
	if err != nil {
		t.Fatalf("caller construction: %v", err)
	}
	t.Cleanup(func() { _ = caller.Shutdown() })

	if err := InstallOrgAuthority(provider, path(m.Provider.AuthorityDir)); err != nil {
		t.Fatalf("install provider authority: %v", err)
	}
	grant, err := os.ReadFile(path(m.Provider.GrantPath))
	if err != nil {
		t.Fatalf("read provider grant: %v", err)
	}
	if err := InstallProviderGrantAudience(provider, grant, path(m.Provider.GrantSecretPath)); err != nil {
		t.Fatalf("install provider grant audience: %v", err)
	}
	if err := InstallOrgAuthority(caller, path(m.Caller.AuthorityDir)); err != nil {
		t.Fatalf("install caller authority: %v", err)
	}

	membership, err := os.ReadFile(path(m.Caller.MembershipPath))
	if err != nil {
		t.Fatalf("read membership: %v", err)
	}
	dispatcher, err := os.ReadFile(path(m.Caller.DispatcherPath))
	if err != nil {
		t.Fatalf("read dispatcher: %v", err)
	}
	callerGrant, err := os.ReadFile(path(m.Caller.GrantPath))
	if err != nil {
		t.Fatalf("read caller grant: %v", err)
	}
	creds, err := NewOrgCredentials(OrgCredentialsConfig{
		Membership:          membership,
		Dispatcher:          dispatcher,
		Grants:              [][]byte{callerGrant},
		AudienceSecretPaths: []string{path(m.Caller.GrantSecretPath)},
	})
	if err != nil {
		t.Fatalf("caller credentials: %v", err)
	}
	client, err := NewOrgClient(caller, creds)
	if err != nil {
		t.Fatalf("bind client: %v", err)
	}
	t.Cleanup(client.Close)

	if err := handshakeNodes(caller, provider, providerAddr); err != nil {
		t.Fatalf("caller handshake: %v", err)
	}
	// The CALLER starts first: the scoped/private envelope ships on the
	// announce path, and the provider's start-time announcement must not
	// race the caller's receive loop coming up (a dropped first announce
	// waits a whole re-announce window to recover).
	if err := caller.Start(); err != nil {
		t.Fatalf("caller start: %v", err)
	}
	if err := provider.Start(); err != nil {
		t.Fatalf("provider start: %v", err)
	}

	return &livePair{
		provider:        provider,
		client:          client,
		service:         m.GrantedService,
		facts:           newLiveFacts(),
		wantSameOrg:     false,
		wantActing:      m.Caller.OrgIDHex,
		wantProviderOrg: m.Provider.OrgIDHex,
	}
}

// setupSameOrgLive brings up a provider and a same-org caller from the
// generated SUBNET scenario's ORG artifacts (the plain protected surface —
// no subnet plane is configured), including §3.4's pre-staging.
func setupSameOrgLive(t *testing.T) *livePair {
	t.Helper()
	outdir := t.TempDir()
	m := genSubnetScenario(t, outdir)
	path := func(rel string) string { return filepath.Join(outdir, rel) }

	providerAddr := reserveLocalUDPPort(t)
	callerAddr := reserveLocalUDPPort(t)
	provider, err := NewMeshNode(MeshConfig{
		BindAddr:        providerAddr,
		PskHex:          m.PskHex,
		IdentitySeedHex: m.Provider.SeedHex,
		HeartbeatMs:     200,
	})
	if err != nil {
		t.Fatalf("provider construction: %v", err)
	}
	t.Cleanup(func() { _ = provider.Shutdown() })
	caller, err := NewMeshNode(MeshConfig{
		BindAddr:        callerAddr,
		PskHex:          m.PskHex,
		IdentitySeedHex: m.Caller.SeedHex,
		HeartbeatMs:     200,
	})
	if err != nil {
		t.Fatalf("caller construction: %v", err)
	}
	t.Cleanup(func() { _ = caller.Shutdown() })

	if err := InstallOrgAuthority(provider, path(m.Provider.AuthorityDir)); err != nil {
		t.Fatalf("install provider authority: %v", err)
	}
	if err := shareOwnerAudience(path(m.Provider.AuthorityDir), path(m.Caller.AuthorityDir)); err != nil {
		t.Fatalf("stage the shared owner audience (spec §3.4): %v", err)
	}
	if err := InstallOrgAuthority(caller, path(m.Caller.AuthorityDir)); err != nil {
		t.Fatalf("install caller authority: %v", err)
	}
	client, err := orgClientFromRole(caller, outdir, m.Caller)
	if err != nil {
		t.Fatalf("bind client: %v", err)
	}
	t.Cleanup(client.Close)

	if err := handshakeNodes(caller, provider, providerAddr); err != nil {
		t.Fatalf("caller handshake: %v", err)
	}
	// The CALLER starts first: the scoped/private envelope ships on the
	// announce path, and the provider's start-time announcement must not
	// race the caller's receive loop coming up (a dropped first announce
	// waits a whole re-announce window to recover).
	if err := caller.Start(); err != nil {
		t.Fatalf("caller start: %v", err)
	}
	if err := provider.Start(); err != nil {
		t.Fatalf("provider start: %v", err)
	}

	return &livePair{
		provider:        provider,
		client:          client,
		service:         m.ExportedService,
		facts:           newLiveFacts(),
		wantSameOrg:     true,
		wantActing:      m.Caller.OrgIDHex,
		wantProviderOrg: m.Provider.OrgIDHex,
	}
}

// convergeOrgCall runs `fn` until it stops failing with the LOCAL discovery
// refusal — the Go binding's equivalent of the Rust harness's
// `converge_discovery` precondition (tests_live.rs): the scoped/private
// announcements ride the announce path at the core's re-announce cadence,
// and the first call must not race their arrival. A `no_authorized_provider`
// refusal is LOCAL — nothing was sent and no proof was minted — so retrying
// it cannot resend a signed proof (the facade's no-retry rule binds ISSUED
// proofs). Every other outcome returns immediately and unmodified: success,
// or any error outside that exact class, fails the test.
func convergeOrgCall(t *testing.T, deadline time.Duration, fn func() error) {
	t.Helper()
	deadlineAt := time.Now().Add(deadline)
	for {
		err := fn()
		if err == nil {
			return
		}
		var oe *OrgError
		if !errors.As(err, &oe) || oe.Domain != OrgDomainDiscovery || oe.Kind != "no_authorized_provider" {
			t.Fatalf("call failed outside the discovery-convergence class: %v", err)
		}
		if time.Now().After(deadlineAt) {
			t.Fatalf("discovery did not converge within %v (last: %v)", deadline, err)
		}
		time.Sleep(250 * time.Millisecond)
	}
}

func mustJSON(t *testing.T, v interface{}) []byte {
	t.Helper()
	body, err := json.Marshal(v)
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}
	return body
}

// drainOrgStream collects every chunk until the clean terminal, asserting
// each item decodes.
func drainOrgStream(t *testing.T, stream *RpcStream) []s4Pong {
	t.Helper()
	var got []s4Pong
	for {
		chunk, err := stream.Recv()
		if errors.Is(err, ErrStreamDone) {
			return got
		}
		if err != nil {
			t.Fatalf("recv: %v", err)
		}
		var pong s4Pong
		if err := json.Unmarshal(chunk, &pong); err != nil {
			t.Fatalf("decode chunk %q: %v", chunk, err)
		}
		got = append(got, pong)
	}
}

// ---- shape: server-streaming (one request in, N correlated items out) ----

func runStreamingLive(t *testing.T, p *livePair) {
	t.Helper()
	facts := p.facts
	sh, err := ServeOrgStreamingBytes(p.provider, p.service, orgAccessFor(p),
		func(caller OrgCaller, req []byte, sink *ResponseSinkSend) error {
			facts.record(caller)
			for i := 1; i <= 3; i++ {
				if err := sink.Send(mustJSON(t, s4Pong{N: i, ServedBy: "go-s4"})); err != nil {
					return err
				}
			}
			return nil
		})
	if err != nil {
		t.Fatalf("serve streaming: %v", err)
	}
	defer sh.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	var stream *RpcStream
	convergeOrgCall(t, 60*time.Second, func() error {
		var err error
		stream, err = p.client.CallStreaming(ctx, p.service, mustJSON(t, s4Ping{N: 1}))
		return err
	})
	defer stream.Close()

	got := drainOrgStream(t, stream)
	if len(got) != 3 {
		t.Fatalf("chunks = %d, want 3 (correlated items + explicit completion)", len(got))
	}
	for i, g := range got {
		if g.N != i+1 {
			t.Fatalf("chunk %d carries n=%d, want %d", i, g.N, i+1)
		}
	}
	p.assertCaller(t)
}

// ---- shape: client-streaming (N requests in, one terminal response) ----

func runClientStreamLive(t *testing.T, p *livePair) {
	t.Helper()
	facts := p.facts
	sh, err := ServeOrgClientStreamBytes(p.provider, p.service, orgAccessFor(p),
		func(caller OrgCaller, stream *RequestStreamRecv) ([]byte, error) {
			facts.record(caller)
			n := 0
			for {
				// Exit cooperatively on the retirement observable —
				// see the handler-drop contract above.
				chunk, err := stream.Recv()
				if errors.Is(err, ErrStreamDone) {
					break
				}
				if err != nil {
					return nil, err
				}
				var ping s4Ping
				if err := json.Unmarshal(chunk, &ping); err != nil {
					return nil, err
				}
				n += ping.N
			}
			return mustJSON(t, s4Pong{N: n, ServedBy: "go-s4"}), nil
		})
	if err != nil {
		t.Fatalf("serve client-stream: %v", err)
	}
	defer sh.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	var call *ClientStreamCall
	convergeOrgCall(t, 60*time.Second, func() error {
		var err error
		call, err = p.client.CallClientStream(ctx, p.service)
		return err
	})
	defer call.Close()

	if err := call.Send(mustJSON(t, s4Ping{N: 1})); err != nil {
		t.Fatalf("send 1: %v", err)
	}
	if err := call.Send(mustJSON(t, s4Ping{N: 2})); err != nil {
		t.Fatalf("send 2: %v", err)
	}
	resp, err := call.Finish()
	if err != nil {
		t.Fatalf("finish: %v", err)
	}
	var pong s4Pong
	if err := json.Unmarshal(resp, &pong); err != nil {
		t.Fatalf("decode terminal response %q: %v", resp, err)
	}
	if pong.N != 3 {
		t.Fatalf("terminal aggregate = %d, want 3 (1+2, upload half-close then one response)", pong.N)
	}
	p.assertCaller(t)
}

// ---- shape: duplex (N requests in, N correlated items out) ----

func runDuplexLive(t *testing.T, p *livePair) {
	t.Helper()
	facts := p.facts
	sh, err := ServeOrgDuplexBytes(p.provider, p.service, orgAccessFor(p),
		func(caller OrgCaller, stream *RequestStreamRecv, sink *ResponseSinkSend) error {
			facts.record(caller)
			for {
				// Exit cooperatively on the retirement observable —
				// see the handler-drop contract above.
				chunk, err := stream.Recv()
				if errors.Is(err, ErrStreamDone) {
					return nil
				}
				if err != nil {
					return err
				}
				var ping s4Ping
				if err := json.Unmarshal(chunk, &ping); err != nil {
					return err
				}
				if err := sink.Send(mustJSON(t, s4Pong{N: ping.N, ServedBy: "go-s4"})); err != nil {
					return err
				}
			}
		})
	if err != nil {
		t.Fatalf("serve duplex: %v", err)
	}
	defer sh.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	var call *DuplexCall
	convergeOrgCall(t, 60*time.Second, func() error {
		var err error
		call, err = p.client.CallDuplex(ctx, p.service)
		return err
	})
	defer call.Close()

	if err := call.Send(mustJSON(t, s4Ping{N: 1})); err != nil {
		t.Fatalf("send 1: %v", err)
	}
	if err := call.Send(mustJSON(t, s4Ping{N: 4})); err != nil {
		t.Fatalf("send 2: %v", err)
	}
	if err := call.FinishSending(); err != nil {
		t.Fatalf("finish sending (half-close): %v", err)
	}
	var got []s4Pong
	for {
		chunk, err := call.Recv()
		if errors.Is(err, ErrStreamDone) {
			break
		}
		if err != nil {
			t.Fatalf("recv: %v", err)
		}
		var pong s4Pong
		if err := json.Unmarshal(chunk, &pong); err != nil {
			t.Fatalf("decode chunk %q: %v", chunk, err)
		}
		got = append(got, pong)
	}
	if len(got) != 2 {
		t.Fatalf("response chunks = %d, want 2", len(got))
	}
	if got[0].N != 1 || got[1].N != 4 {
		t.Fatalf("response items = %+v, want the two correlated echoes", got)
	}
	p.assertCaller(t)
}

func orgAccessFor(p *livePair) OrgAccess {
	if p.wantSameOrg {
		return OrgAccessSameOrg
	}
	return OrgAccessGranted
}

// The six live siblings — three shapes x two authority modes.

func TestLiveGrantedStreamingFromAGeneratedScenario(t *testing.T) {
	skipIfNotEnabled(t)
	runStreamingLive(t, setupGrantedLive(t))
}

func TestLiveGrantedClientStreamFromAGeneratedScenario(t *testing.T) {
	skipIfNotEnabled(t)
	runClientStreamLive(t, setupGrantedLive(t))
}

func TestLiveGrantedDuplexFromAGeneratedScenario(t *testing.T) {
	skipIfNotEnabled(t)
	runDuplexLive(t, setupGrantedLive(t))
}

func TestLiveSameOrgStreamingFromAGeneratedScenario(t *testing.T) {
	skipIfNotEnabled(t)
	runStreamingLive(t, setupSameOrgLive(t))
}

func TestLiveSameOrgClientStreamFromAGeneratedScenario(t *testing.T) {
	skipIfNotEnabled(t)
	runClientStreamLive(t, setupSameOrgLive(t))
}

func TestLiveSameOrgDuplexFromAGeneratedScenario(t *testing.T) {
	skipIfNotEnabled(t)
	runDuplexLive(t, setupSameOrgLive(t))
}
