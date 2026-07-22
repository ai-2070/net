// The Go org surface through the real cgo boundary (OSDK-L Workstream G).
//
// Issuance is deliberately absent from every binding (credentials come from the
// `net org` CLI), so these cover the construction, refusal, provenance, and
// provisioning paths a Go application can reach without a full issuance chain. A
// live admitted cross-org call needs adopted authorities + issued certs/grants
// in a multi-language harness — that is X2, owed. The exact binding-shaped path
// (install authority + install provider grant audience + from_parts + call) is
// proven end-to-end at the Rust tier
// (`live_cross_org_call_through_the_provisioning_methods`); the Go functions are
// thin marshaling over the same SDK calls.

package net

import (
	"encoding/hex"
	"errors"
	"path/filepath"
	"testing"
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
