// MCP bridge helper + consent/pin binding tests (`MCP_BRIDGE_SDK_PLAN.md`
// P3).
//
// Build the cdylib first:
//   cargo build --release -p net-mcp-ffi
// then run with the link path on LD_LIBRARY_PATH (see the go-tests CI job).
//
// The helpers are the bridge's one Rust implementation — these tests pin
// the classification parity vectors (same inputs -> same status in every
// binding), the secret-negative rule (no env value crosses back), and the
// concurrent pin-store contract (locked mutations lose nothing).

package net

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
)

const mcpSecret = "ghp_this-value-must-never-cross"

// ---------------------------------------------------------------------------
// Classify
// ---------------------------------------------------------------------------

func TestClassifyParityVectors(t *testing.T) {
	cases := []struct {
		program string
		args    []string
		envs    map[string]string
		want    string
	}{
		{"npx", []string{"-y", "some-server"}, map[string]string{"GITHUB_TOKEN": mcpSecret}, "credentialed"},
		{"npx", []string{"-y", "@modelcontextprotocol/server-github"}, nil, "external_api"},
		{"uvx", []string{"mcp-server-time"}, map[string]string{"TZ": "UTC"}, "unknown"},
	}
	for _, c := range cases {
		got, err := Classify(c.program, c.args, c.envs, "", false)
		if err != nil {
			t.Fatalf("Classify(%q) error: %v", c.program, err)
		}
		if got != c.want {
			t.Errorf("Classify(%q, %v) = %q, want %q", c.program, c.args, got, c.want)
		}
	}
}

func TestClassifyOverrides(t *testing.T) {
	if _, err := Classify("uvx", []string{"t"}, nil, "no-credentials", false); err == nil {
		t.Error("downward override without force must error")
	}
	got, err := Classify("uvx", []string{"t"}, nil, "no-credentials", true)
	if err != nil || got != "none" {
		t.Errorf("forced downgrade = (%q, %v), want (none, nil)", got, err)
	}
	got, err = Classify("uvx", []string{"t"}, nil, "credentialed", false)
	if err != nil || got != "credentialed" {
		t.Errorf("upward override = (%q, %v), want (credentialed, nil)", got, err)
	}
	if _, err := Classify("uvx", []string{"t"}, nil, "bogus", false); err == nil {
		t.Error("unknown override must error")
	}
}

// ---------------------------------------------------------------------------
// LowerTool
// ---------------------------------------------------------------------------

func TestLowerToolProducesDescriptorAndMetadata(t *testing.T) {
	tool := `{"name":"echo","description":"echo it back","inputSchema":{"type":"object","properties":{"message":{"type":"string"}}}}`
	lowered, err := LowerTool(tool, "2.0.0", "credentialed", "provider_local")
	if err != nil {
		t.Fatalf("LowerTool error: %v", err)
	}
	if lowered.ToolID != "echo" || lowered.McpName != "echo" {
		t.Errorf("ids = (%q, %q), want (echo, echo)", lowered.ToolID, lowered.McpName)
	}
	if lowered.BridgeMetadata["tool::echo::compat_tier"] != "mcp_bridge" {
		t.Errorf("compat_tier = %q", lowered.BridgeMetadata["tool::echo::compat_tier"])
	}
	if lowered.BridgeMetadata["tool::echo::credential_status"] != "credentialed" {
		t.Errorf("credential_status = %q", lowered.BridgeMetadata["tool::echo::credential_status"])
	}
	var desc map[string]interface{}
	if err := json.Unmarshal(lowered.Descriptor, &desc); err != nil {
		t.Fatalf("descriptor not valid JSON: %v", err)
	}
	if desc["tool_id"] != "echo" {
		t.Errorf("descriptor.tool_id = %v", desc["tool_id"])
	}
}

func TestLowerToolSanitizesNonChannelSafeNames(t *testing.T) {
	lowered, err := LowerTool(`{"name":"getCaps","inputSchema":{"type":"object"}}`, "1.0.0", "none", "")
	if err != nil {
		t.Fatalf("LowerTool error: %v", err)
	}
	if lowered.McpName != "getCaps" {
		t.Errorf("mcpName = %q, want getCaps", lowered.McpName)
	}
	if lowered.ToolID == "getCaps" || lowered.ToolID[:7] != "getcaps" {
		t.Errorf("toolId = %q, want a sanitized getcaps_* id", lowered.ToolID)
	}
}

func TestLowerToolRejectsGarbageStatus(t *testing.T) {
	tool := `{"name":"echo","inputSchema":{"type":"object"}}`
	if _, err := LowerTool(tool, "1.0.0", "totally-fine-trust-me", ""); err == nil {
		t.Error("unknown credential_status must error, never be silently gated")
	}
	if _, err := LowerTool(tool, "1.0.0", "none", "anything"); err == nil {
		t.Error("unknown substitutability must error")
	}
}

func TestSecretNegativeNoEnvValueCrosses(t *testing.T) {
	status, err := Classify("npx", []string{"srv"}, map[string]string{"API_KEY": mcpSecret}, "", false)
	if err != nil || status != "credentialed" {
		t.Fatalf("classify = (%q, %v)", status, err)
	}
	lowered, err := LowerTool(`{"name":"srv.call","description":"calls things","inputSchema":{"type":"object"}}`, "1.0.0", status, "")
	if err != nil {
		t.Fatalf("LowerTool error: %v", err)
	}
	blob, _ := json.Marshal(lowered)
	if strings.Contains(string(blob), mcpSecret) {
		t.Errorf("the env value leaked into the lowered DTO")
	}
}

// ---------------------------------------------------------------------------
// Consent gate
// ---------------------------------------------------------------------------

func TestCredentialRequiresConsentNeverTrustsWire(t *testing.T) {
	for _, status := range []string{"credentialed", "external_api", "unknown", "none", "", "bogus"} {
		if !CredentialRequiresConsent(status) {
			t.Errorf("CredentialRequiresConsent(%q) = false, want true (wire is never trusted)", status)
		}
	}
}

func TestCanonicalizeCapID(t *testing.T) {
	for _, spelling := range []string{"0x2a/echo", "0X2A/echo", " 42/echo", "42 /echo"} {
		got, err := CanonicalizeCapID(spelling)
		if err != nil {
			t.Fatalf("CanonicalizeCapID(%q) error: %v", spelling, err)
		}
		if got != "42/echo" {
			t.Errorf("CanonicalizeCapID(%q) = %q, want 42/echo", spelling, got)
		}
	}
	if _, err := CanonicalizeCapID("bareword"); err == nil {
		t.Error("a missing provider must error")
	}
}

func TestConsentPolicyGatesUntilAdmitted(t *testing.T) {
	p, err := NewConsentPolicy()
	if err != nil {
		t.Fatalf("NewConsentPolicy: %v", err)
	}
	defer p.Close()

	if d, _ := p.Decide("b/echo", "none"); d != "requires_approval" {
		t.Errorf("empty policy decide = %q, want requires_approval", d)
	}
	if err := p.Allow("b/echo"); err != nil {
		t.Fatalf("Allow: %v", err)
	}
	if d, _ := p.Decide("b/echo", "credentialed"); d != "allowed" {
		t.Errorf("allowed decide = %q, want allowed", d)
	}
	if req, _ := p.RequiresApproval("b/other", "credentialed"); !req {
		t.Error("a different capability must still require approval")
	}

	// A pin under the hex spelling admits the decimal spelling — identity
	// canonicalization runs in the Rust core.
	if err := p.Pin("0x2a/echo"); err != nil {
		t.Fatalf("Pin: %v", err)
	}
	if pinned, _ := p.IsPinned("42/echo"); !pinned {
		t.Error("a pin under 0x2a/echo must admit 42/echo")
	}
	if d, _ := p.Decide("42/echo", "external_api"); d != "allowed" {
		t.Errorf("pinned decide = %q, want allowed", d)
	}
	ids, _ := p.Pinned()
	if len(ids) != 1 || ids[0] != "42/echo" {
		t.Errorf("Pinned() = %v, want [42/echo]", ids)
	}
	if err := p.Unpin("42/echo"); err != nil {
		t.Fatalf("Unpin: %v", err)
	}
	if pinned, _ := p.IsPinned("42/echo"); pinned {
		t.Error("unpin must remove the pin")
	}
}

// ---------------------------------------------------------------------------
// Pin store
// ---------------------------------------------------------------------------

func TestPinStoreRequestApproveReject(t *testing.T) {
	path := filepath.Join(t.TempDir(), "pins.json")
	s := OpenPinStore(path)

	if state, err := s.Request("b/echo"); err != nil || state != "pending" {
		t.Fatalf("Request = (%q, %v), want (pending, nil)", state, err)
	}
	if ok, _ := s.IsApproved("b/echo"); ok {
		t.Error("a request must not grant consent")
	}
	if changed, err := s.Approve("b/echo"); err != nil || !changed {
		t.Fatalf("Approve = (%v, %v), want (true, nil)", changed, err)
	}
	// A later request never disturbs an approved pin.
	if state, _ := s.Request("b/echo"); state != "approved" {
		t.Errorf("Request after approve = %q, want approved", state)
	}
	if changed, _ := s.Reject("b/echo"); !changed {
		t.Error("reject of an existing pin returns true")
	}
	if changed, _ := s.Reject("b/echo"); changed {
		t.Error("reject of an absent pin returns false")
	}
	if _, ok, _ := s.State("b/echo"); ok {
		t.Error("state of a removed pin must be absent")
	}
}

func TestPinStoreSharedAndFormatCompatible(t *testing.T) {
	path := filepath.Join(t.TempDir(), "pins.json")
	a := OpenPinStore(path)
	b := OpenPinStore(path)

	if _, err := a.Approve("b/secret"); err != nil {
		t.Fatalf("Approve: %v", err)
	}
	// A second handle on the same file sees it — the model for "approved in
	// one place, honored by the shim in another".
	if ok, _ := b.IsApproved("b/secret"); !ok {
		t.Error("a second handle must see the approval")
	}
	rows, _ := b.List()
	if len(rows) != 1 || rows[0].CapID != "b/secret" || rows[0].State != "approved" {
		t.Errorf("List = %v, want one approved b/secret", rows)
	}
}

func TestPinStoreCorruptErrors(t *testing.T) {
	path := filepath.Join(t.TempDir(), "pins.json")
	if err := os.WriteFile(path, []byte("{ not valid json"), 0o600); err != nil {
		t.Fatal(err)
	}
	s := OpenPinStore(path)
	if _, err := s.List(); err == nil {
		t.Error("a corrupt store must error, not reset")
	}
	if _, err := s.Approve("b/echo"); err == nil {
		t.Error("a corrupt store must error on mutate")
	}
}

func TestPinStoreConcurrentMutationsLoseNothing(t *testing.T) {
	// P3 acceptance: concurrent access, no corruption. Every Approve runs
	// under the Rust core's cross-process advisory lock, so N goroutines
	// hammering one store must not lose an update to a stale-snapshot race.
	path := filepath.Join(t.TempDir(), "pins.json")
	const n = 40

	var wg sync.WaitGroup
	errs := make([]error, n)
	for i := 0; i < n; i++ {
		wg.Add(1)
		go func(i int) {
			defer wg.Done()
			s := OpenPinStore(path)
			_, errs[i] = s.Approve(fmt.Sprintf("node/tool%d", i))
		}(i)
	}
	wg.Wait()
	for i, err := range errs {
		if err != nil {
			t.Fatalf("concurrent approve %d: %v", i, err)
		}
	}

	approved, err := OpenPinStore(path).Approved()
	if err != nil {
		t.Fatalf("Approved: %v", err)
	}
	if len(approved) != n {
		t.Fatalf("approved count = %d, want %d (lost updates)", len(approved), n)
	}
	seen := map[string]bool{}
	for _, id := range approved {
		seen[id] = true
	}
	for i := 0; i < n; i++ {
		if !seen[fmt.Sprintf("node/tool%d", i)] {
			t.Errorf("node/tool%d was lost", i)
		}
	}
}
