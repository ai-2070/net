// Cross-language subnet stable-kind golden vectors (SSDK §7.3 / R4).
//
// Loads `net/crates/net/tests/cross_lang_subnet/stable_kinds.json` — the SAME
// fixture Rust generates from its canonical match and Node, Python, and C
// consume — and asserts this binding's ParseSubnetKind recovers every pinned
// kind. This is the Go consumer of the shared vocabulary and its drift guard: a
// renamed `subnet:` kind fails here (and in four other suites).
//
// Pure Go (no mesh, no cgo call) but in `package net`, so it runs in the same
// `go test ./...` pass as the FFI surface.

package net

import (
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"runtime"
	"testing"
)

type subnetKindFixture struct {
	Version    int      `json:"version"`
	Prefix     string   `json:"prefix"`
	AuthKinds  []string `json:"auth_kinds"`
	LocalKinds []string `json:"local_kinds"`
	FactKinds  []string `json:"fact_kinds"`
	Access     []string `json:"access"`
}

func loadSubnetKindFixture(t *testing.T) subnetKindFixture {
	t.Helper()
	_, thisFile, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("runtime.Caller failed")
	}
	path := filepath.Join(filepath.Dir(thisFile),
		"..", "net", "crates", "net", "tests", "cross_lang_subnet", "stable_kinds.json")
	if _, err := os.Stat(path); err != nil {
		t.Skipf("subnet kind fixture not present (%v) — standalone checkout", err)
	}
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read subnet kind fixture: %v", err)
	}
	var f subnetKindFixture
	if err := json.Unmarshal(raw, &f); err != nil {
		t.Fatalf("parse subnet kind fixture: %v", err)
	}
	return f
}

// The fixture keeps the frozen shape every binding relies on.
func TestSubnetKindFixtureShape(t *testing.T) {
	f := loadSubnetKindFixture(t)
	if f.Version != 1 {
		t.Fatalf("fixture version = %d, want 1", f.Version)
	}
	if f.Prefix != "subnet:" {
		t.Fatalf("prefix = %q, want %q", f.Prefix, "subnet:")
	}
	if len(f.AuthKinds) == 0 || len(f.LocalKinds) == 0 {
		t.Fatal("fixture must pin at least one auth kind and one local kind")
	}
	wantFacts := []string{"descriptor", "gateway_advertisement", "export_policy", "revocation_floor"}
	if len(f.FactKinds) != len(wantFacts) {
		t.Fatalf("fact_kinds = %v, want %v", f.FactKinds, wantFacts)
	}
	for i, want := range wantFacts {
		if f.FactKinds[i] != want {
			t.Fatalf("fact_kinds[%d] = %q, want %q", i, f.FactKinds[i], want)
		}
	}
	if len(f.Access) != 2 || f.Access[0] != "sameOrg" || f.Access[1] != "granted" {
		t.Fatalf("access = %v, want [sameOrg granted]", f.Access)
	}
}

// Every pinned kind round-trips through ParseSubnetKind verbatim.
func TestSubnetKindsParseVerbatim(t *testing.T) {
	f := loadSubnetKindFixture(t)
	for _, kind := range append(append([]string{}, f.AuthKinds...), f.LocalKinds...) {
		wire := "subnet:" + kind
		if got := ParseSubnetKind(wire); got != kind {
			t.Errorf("ParseSubnetKind(%q) = %q, want %q", wire, got, kind)
		}
	}
}

// Kinds are globally unique across both bands — a collision would make a
// classification ambiguous across bindings.
func TestSubnetKindsAreUnique(t *testing.T) {
	f := loadSubnetKindFixture(t)
	seen := make(map[string]bool)
	for _, kind := range append(append([]string{}, f.AuthKinds...), f.LocalKinds...) {
		if seen[kind] {
			t.Errorf("duplicate stable kind: %q", kind)
		}
		seen[kind] = true
	}
}

// A non-subnet message never yields a kind — notably an `org:` wire string, so
// the two vocabularies cannot be confused.
func TestParseSubnetKindRejectsNonSubnet(t *testing.T) {
	for _, wire := range []string{
		"org:credentials:signature_invalid",
		"some unrelated failure",
		"",
	} {
		if got := ParseSubnetKind(wire); got != "" {
			t.Errorf("ParseSubnetKind(%q) = %q, want empty", wire, got)
		}
	}
}

// The registration-failure shape WRAPS the envelope rather than leading with
// it; the kind is still recovered. This is the exact message
// net_subnet_serve_exported produces for an unknown export name.
func TestParseSubnetKindFindsWrappedEnvelope(t *testing.T) {
	wire := `subnet-exported serve registration failed: invalid protected registration: ` +
		`subnet:unknown_export_name: no configured subnet export named "nope"`
	if got := ParseSubnetKind(wire); got != "unknown_export_name" {
		t.Fatalf("ParseSubnetKind(wrapped) = %q, want %q", got, "unknown_export_name")
	}
}

// An unfamiliar kind is returned as data, never remapped onto a known one.
func TestParseSubnetKindPassesUnknownThrough(t *testing.T) {
	if got := ParseSubnetKind("subnet:kind_from_the_future"); got != "kind_from_the_future" {
		t.Fatalf("ParseSubnetKind(future) = %q, want verbatim", got)
	}
}

// SubnetError satisfies errors.Is(err, ErrSubnet) without parsing, and carries
// the kind for callers that want to branch finely.
func TestSubnetErrorSentinelAndKind(t *testing.T) {
	err := newSubnetError("subnet:invalid_format: bad artifact")
	if !errors.Is(err, ErrSubnet) {
		t.Fatal("errors.Is(err, ErrSubnet) = false, want true")
	}
	if err.Kind != "invalid_format" {
		t.Fatalf("Kind = %q, want %q", err.Kind, "invalid_format")
	}
	// errors.As recovers the typed error from a wrapped chain, which is how a
	// caller reaches Kind without parsing the message.
	var se *SubnetError
	if !errors.As(error(err), &se) || se.Kind != "invalid_format" {
		t.Fatalf("errors.As did not recover the SubnetError kind")
	}
}

// The C ABI access constants must match the Go values, or a provider would
// announce under the wrong access mode with no compile error.
func TestSubnetAccessConstants(t *testing.T) {
	if SubnetAccessSameOrg != 0 {
		t.Errorf("SubnetAccessSameOrg = %d, want 0 (NET_SUBNET_ACCESS_SAME_ORG)", SubnetAccessSameOrg)
	}
	if SubnetAccessGranted != 1 {
		t.Errorf("SubnetAccessGranted = %d, want 1 (NET_SUBNET_ACCESS_GRANTED)", SubnetAccessGranted)
	}
}

// The export map refuses an ambiguous configuration before a node exists.
func TestBuildSubnetExportMapRefusesAmbiguity(t *testing.T) {
	e := SubnetNamedExport{
		Name:   "factory",
		Access: SubnetAccessGranted,
		Binding: SubnetExportBinding{
			Subnet:        SubnetRef{AuthorityHex: "d7", Path: SubnetPath{Levels: []uint8{3}}},
			TopologyEpoch: 0,
		},
	}
	if _, err := buildSubnetExportMap([]SubnetNamedExport{e, e}); !errors.Is(err, ErrSubnet) {
		t.Fatalf("duplicate name err = %v, want a SubnetError", err)
	} else if k := ParseSubnetKind(err.Error()); k != "duplicate_export_name" {
		t.Fatalf("duplicate name kind = %q, want duplicate_export_name", k)
	}

	anon := e
	anon.Name = ""
	if _, err := buildSubnetExportMap([]SubnetNamedExport{anon}); !errors.Is(err, ErrSubnet) {
		t.Fatalf("empty name err = %v, want a SubnetError", err)
	}

	// A well-formed map builds, and an unknown lookup misses.
	m, err := buildSubnetExportMap([]SubnetNamedExport{e})
	if err != nil {
		t.Fatalf("buildSubnetExportMap: %v", err)
	}
	if _, ok := m["factory"]; !ok {
		t.Fatal("configured export missing from the map")
	}
	if _, ok := m["nope"]; ok {
		t.Fatal("unknown export name resolved")
	}
}

// The authority hex and path shape are marshaling facts checked before the
// binding crosses; a bad one never reaches Rust.
func TestSubnetExportToCRejectsMalformedBinding(t *testing.T) {
	bad := SubnetNamedExport{
		Name:   "x",
		Access: SubnetAccessGranted,
		Binding: SubnetExportBinding{
			Subnet: SubnetRef{AuthorityHex: "not-hex", Path: SubnetPath{}},
		},
	}
	if _, err := bad.toC(); ParseSubnetKind(errString(err)) != "invalid_id_hex" {
		t.Fatalf("bad authority hex err = %v, want subnet:invalid_id_hex", err)
	}

	deep := bad
	deep.Binding.Subnet.AuthorityHex = hex64()
	deep.Binding.Subnet.Path = SubnetPath{Levels: []uint8{1, 2, 3, 4, 5}}
	if _, err := deep.toC(); ParseSubnetKind(errString(err)) != "path_too_deep" {
		t.Fatalf("deep path err = %v, want subnet:path_too_deep", err)
	}

	ok := bad
	ok.Binding.Subnet.AuthorityHex = hex64()
	ok.Binding.Subnet.Path = SubnetPath{Levels: []uint8{3, 9}}
	ref, err := ok.toC()
	if err != nil {
		t.Fatalf("well-formed binding rejected: %v", err)
	}
	if int(ref.path.depth) != 2 {
		t.Fatalf("depth = %d, want 2", int(ref.path.depth))
	}
	// The inactive tail must be zero — the canonical form Rust enforces.
	if ref.path.levels[2] != 0 || ref.path.levels[3] != 0 {
		t.Fatal("inactive path levels must be zero")
	}
}

func hex64() string {
	s := ""
	for i := 0; i < 32; i++ {
		s += "d7"
	}
	return s
}

func errString(err error) string {
	if err == nil {
		return ""
	}
	return err.Error()
}
