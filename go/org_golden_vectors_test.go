// Cross-language org error-vocabulary golden vectors (OSDK-L X1 / X3).
//
// Loads `net/crates/net/tests/cross_lang_org/error_vectors.json` — the fixture
// generated from Rust's single source (`OrgSdkError::to_wire`) — and asserts
// the Go binding's parseOrgError recovers the same domain + kind + is_local
// from every wire string. This is the Go consumer of the shared vocabulary and
// its drift guard: a renamed `org:` kind fails here (and four other suites).
//
// Pure Go (no mesh, no cgo call) — but it lives in `package net`, so it runs in
// the same `go test ./...` pass as the FFI surface.

package net

import (
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"runtime"
	"testing"
)

type orgErrorFixture struct {
	Prefix  string `json:"prefix"`
	Vectors []struct {
		Wire    string `json:"wire"`
		Domain  string `json:"domain"`
		Kind    string `json:"kind"`
		IsLocal bool   `json:"is_local"`
	} `json:"vectors"`
	Unclassified []struct {
		Wire          string `json:"wire"`
		ExpectDomain  string `json:"expect_domain"`
		ExpectIsLocal bool   `json:"expect_is_local"`
	} `json:"unclassified_cases"`
}

func loadOrgErrorFixture(t *testing.T) orgErrorFixture {
	t.Helper()
	_, thisFile, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("runtime.Caller failed")
	}
	path := filepath.Join(filepath.Dir(thisFile),
		"..", "net", "crates", "net", "tests", "cross_lang_org", "error_vectors.json")
	if _, err := os.Stat(path); err != nil {
		t.Skipf("org error fixture not present (%v) — standalone checkout", err)
	}
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read org error fixture: %v", err)
	}
	var f orgErrorFixture
	if err := json.Unmarshal(raw, &f); err != nil {
		t.Fatalf("parse org error fixture: %v", err)
	}
	return f
}

// Every canonical vector's domain + kind + is_local is recovered from its wire
// string, and the rpc domain exposes an unwrappable *RpcError.
func TestOrgErrorVocabulary_GoldenVectors(t *testing.T) {
	f := loadOrgErrorFixture(t)
	if len(f.Vectors) == 0 {
		t.Fatal("fixture had no vectors — parser or regeneration regressed")
	}
	for _, v := range f.Vectors {
		oe := parseOrgError(v.Wire)
		if string(oe.Domain) != v.Domain {
			t.Errorf("wire %q: domain = %q, want %q", v.Wire, oe.Domain, v.Domain)
		}
		if oe.Kind != v.Kind {
			t.Errorf("wire %q: kind = %q, want %q", v.Wire, oe.Kind, v.Kind)
		}
		if oe.IsLocal() != v.IsLocal {
			t.Errorf("wire %q: is_local = %v, want %v", v.Wire, oe.IsLocal(), v.IsLocal)
		}
		if oe.Domain == OrgDomainRPC {
			var re *RpcError
			if !errors.As(oe, &re) {
				t.Errorf("wire %q: rpc domain must Unwrap to *RpcError", v.Wire)
			}
		}
	}
}

// A wire this build cannot classify is `unknown` — never a canonical domain,
// and never local. This is the guard that a binding meeting an unfamiliar
// vocabulary says so rather than counterfeiting a remote admission result.
func TestOrgErrorVocabulary_Unclassified(t *testing.T) {
	f := loadOrgErrorFixture(t)
	if len(f.Unclassified) == 0 {
		t.Fatal("fixture had no unclassified cases")
	}
	for _, v := range f.Unclassified {
		oe := parseOrgError(v.Wire)
		if string(oe.Domain) != v.ExpectDomain {
			t.Errorf("wire %q: domain = %q, want %q (must never impersonate a domain)",
				v.Wire, oe.Domain, v.ExpectDomain)
		}
		if oe.IsLocal() != v.ExpectIsLocal {
			t.Errorf("wire %q: is_local = %v, want %v", v.Wire, oe.IsLocal(), v.ExpectIsLocal)
		}
		if oe.Kind != "" {
			t.Errorf("wire %q: unclassified must expose no kind, got %q", v.Wire, oe.Kind)
		}
		if !errors.Is(oe, ErrOrgUnclassified) {
			t.Errorf("wire %q: unknown domain must match ErrOrgUnclassified", v.Wire)
		}
	}
}

// The domain sentinels classify without re-parsing — the identity-precedent
// errors.Is path (distinct from the rich parse).
func TestOrgErrorSentinels(t *testing.T) {
	cases := []struct {
		wire     string
		sentinel error
	}{
		{"org:credentials:signature_invalid: detail", ErrOrgCredentials},
		{"org:discovery:no_authorized_provider: detail", ErrOrgDiscovery},
		{"org:admission_denied:denied", ErrOrgAdmissionDenied},
		{"org:rpc:timeout: detail", ErrOrgRPC},
		{"not-an-org-error", ErrOrgUnclassified},
	}
	for _, c := range cases {
		oe := parseOrgError(c.wire)
		if !errors.Is(oe, c.sentinel) {
			t.Errorf("wire %q: errors.Is(_, %v) = false", c.wire, c.sentinel)
		}
	}
}
