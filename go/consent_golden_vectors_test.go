// Cross-language consent-surface parity fixture test
// (`MCP_BRIDGE_SDK_PLAN.md` conformance).
//
// Loads `net/crates/net/tests/cross_lang_mcp/consent_vectors.json` — the
// fixture the Rust source-of-truth verifier
// (`sdk/tests/consent_golden_vectors.rs`) validates — and asserts the Go
// consent wrappers agree with the core.

package net

import (
	"encoding/json"
	"os"
	"path/filepath"
	"runtime"
	"testing"
)

type consentFixture struct {
	CapIDCanonicalize []struct {
		Name     string `json:"name"`
		Input    string `json:"input"`
		Expected string `json:"expected"`
	} `json:"cap_id_canonicalize"`
	CapIDInvalid []struct {
		Name  string `json:"name"`
		Input string `json:"input"`
	} `json:"cap_id_invalid"`
	CredentialRequiresConsent []struct {
		Name     string `json:"name"`
		Status   string `json:"status"`
		Expected bool   `json:"expected"`
	} `json:"credential_requires_consent"`
	ConsentDecision []struct {
		Name string `json:"name"`
		Ops  []struct {
			Op    string `json:"op"`
			CapID string `json:"cap_id"`
		} `json:"ops"`
		CapID            string `json:"cap_id"`
		CredentialStatus string `json:"credential_status"`
		Expected         string `json:"expected"`
	} `json:"consent_decision"`
}

func loadConsentFixture(t *testing.T) consentFixture {
	t.Helper()
	_, thisFile, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("runtime.Caller failed")
	}
	dir := filepath.Dir(thisFile)
	path := filepath.Join(dir, "..", "net", "crates", "net", "tests", "cross_lang_mcp", "consent_vectors.json")
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read fixture: %v", err)
	}
	var f consentFixture
	if err := json.Unmarshal(raw, &f); err != nil {
		t.Fatalf("parse fixture: %v", err)
	}
	return f
}

func TestCapIDCanonicalizeGoldenVectors(t *testing.T) {
	for _, c := range loadConsentFixture(t).CapIDCanonicalize {
		got, err := CanonicalizeCapID(c.Input)
		if err != nil {
			t.Fatalf("[%s] CanonicalizeCapID(%q): %v", c.Name, c.Input, err)
		}
		if got != c.Expected {
			t.Errorf("[%s] = %q, want %q", c.Name, got, c.Expected)
		}
	}
}

func TestCapIDInvalidGoldenVectors(t *testing.T) {
	for _, c := range loadConsentFixture(t).CapIDInvalid {
		if _, err := CanonicalizeCapID(c.Input); err == nil {
			t.Errorf("[%s] %q must be rejected", c.Name, c.Input)
		}
	}
}

func TestCredentialRequiresConsentGoldenVectors(t *testing.T) {
	for _, c := range loadConsentFixture(t).CredentialRequiresConsent {
		if got := CredentialRequiresConsent(c.Status); got != c.Expected {
			t.Errorf("[%s] CredentialRequiresConsent(%q) = %v, want %v", c.Name, c.Status, got, c.Expected)
		}
	}
}

func TestConsentDecisionGoldenVectors(t *testing.T) {
	for _, c := range loadConsentFixture(t).ConsentDecision {
		p, err := NewConsentPolicy()
		if err != nil {
			t.Fatalf("[%s] NewConsentPolicy: %v", c.Name, err)
		}
		for _, op := range c.Ops {
			switch op.Op {
			case "allow":
				err = p.Allow(op.CapID)
			case "pin":
				err = p.Pin(op.CapID)
			case "unpin":
				err = p.Unpin(op.CapID)
			default:
				t.Fatalf("[%s] unknown op %q", c.Name, op.Op)
			}
			if err != nil {
				t.Fatalf("[%s] op %s(%q): %v", c.Name, op.Op, op.CapID, err)
			}
		}
		got, err := p.Decide(c.CapID, c.CredentialStatus)
		if err != nil {
			t.Fatalf("[%s] Decide: %v", c.Name, err)
		}
		if got != c.Expected {
			t.Errorf("[%s] decide = %q, want %q", c.Name, got, c.Expected)
		}
		p.Close()
	}
}
