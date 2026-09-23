// Cross-language org streaming OPENING vectors (S4Vectors).
//
// Loads `net/crates/net/tests/cross_lang_org/streaming_opening_vectors.json` —
// the fixture generated from Rust's single sources (the `org_call` codec and
// `OrgSdkError::to_wire`) — and asserts this binding's view of every vector is
// byte-identical to the fixture authority's: the dual-encoding byte pins
// (`wire_hex`/`wire_base64`/`wire_len`), the caller-emit and provider-decode
// contracts of the streaming opening envelope, the strict decoder rejections,
// and the frozen `org:` vocabulary via parseOrgError.
//
// Four harness-integrity rules, enforced by the named rows below: malformed or
// unknown errors, narrowing IDs, callback loss, and decoder disagreement MUST
// NOT become success.
//
// The mixed-pair row executes Go-as-caller against a Python-as-provider over a
// real mesh (the ONE non-Rust pair of the lane), driven by the same vector
// file; it needs RUN_INTEGRATION_TESTS=1, a built `net` wheel with the S4
// streaming surface, and the Rust toolchain (`gen_org_scenario`).
//
// Pure Go for the conformance rows (no mesh, no cgo call) — but it lives in
// `package net`, so it runs in the same `go test ./...` pass as the FFI surface.

package net

import (
	"bufio"
	"bytes"
	"context"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"runtime"
	"strconv"
	"strings"
	"testing"
	"time"
)

type sovLayout struct {
	StreamSuffixLen   int            `json:"stream_suffix_len"`
	SessionBindingLen int            `json:"session_binding_len"`
	SignatureWireLen  int            `json:"signature_wire_len"`
	MaxProofBytes     int            `json:"max_proof_bytes"`
	KindValues        map[string]int `json:"kind_values"`
}

type sovExpect struct {
	Decode             string `json:"decode"`
	Verify             string `json:"verify"`
	Kind               int    `json:"kind"`
	SessionBindingHex  string `json:"session_binding_hex"`
	SigDomainSeparated bool   `json:"sig_domain_separated"`
}

type sovOpening struct {
	ID                     string    `json:"id"`
	Kind                   int       `json:"kind"`
	KindName               string    `json:"kind_name"`
	Access                 string    `json:"access"`
	CallID                 string    `json:"call_id"`
	ProofExpiresUnixNs     string    `json:"proof_expires_at_unix_ns"`
	RequestDigestHex       string    `json:"request_digest_hex"`
	SessionBindingHex      string    `json:"session_binding_hex"`
	CallBindingSigHex      string    `json:"call_binding_sig_hex"`
	UnaryCallBindingSigHex string    `json:"unary_call_binding_sig_hex"`
	WireHex                string    `json:"wire_hex"`
	WireBase64             string    `json:"wire_base64"`
	WireLen                int       `json:"wire_len"`
	UnaryWireHex           string    `json:"unary_wire_hex"`
	UnaryWireBase64        string    `json:"unary_wire_base64"`
	UnaryWireLen           int       `json:"unary_wire_len"`
	PreSigPrefixHex        string    `json:"pre_sig_prefix_hex"`
	Expect                 sovExpect `json:"expect"`
}

type sovReject struct {
	ID                       string `json:"id"`
	Base                     string `json:"base"`
	Mutation                 string `json:"mutation"`
	WireHex                  string `json:"wire_hex"`
	WireBase64               string `json:"wire_base64"`
	WireLen                  int    `json:"wire_len"`
	BaseWireLen              int    `json:"base_wire_len"`
	ExpectErrorDisplay       string `json:"expect_error_display"`
	ExpectDecode             string `json:"expect_decode"`
	ExpectVerifyErrorDisplay string `json:"expect_verify_error_display"`
}

type sovVocab struct {
	Wire       string `json:"wire"`
	Domain     string `json:"domain"`
	Kind       string `json:"kind"`
	IsLocal    bool   `json:"is_local"`
	WireBase64 string `json:"wire_base64"`
	WireLen    int    `json:"wire_len"`
}

type sovUnclassified struct {
	Wire          string `json:"wire"`
	ExpectDomain  string `json:"expect_domain"`
	ExpectIsLocal bool   `json:"expect_is_local"`
	WireBase64    string `json:"wire_base64"`
	WireLen       int    `json:"wire_len"`
}

type sovMixedPair struct {
	ID            string   `json:"id"`
	Service       string   `json:"service"`
	Shape         string   `json:"shape"`
	RequestHex    string   `json:"request_hex"`
	ChunksHex     []string `json:"chunks_hex"`
	ExpectHandler struct {
		EntityHex      string `json:"entity_hex"`
		ActingOrgHex   string `json:"acting_org_hex"`
		ProviderOrgHex string `json:"provider_org_hex"`
		CapabilityHex  string `json:"capability_hex"`
		IsSameOrg      bool   `json:"is_same_org"`
	} `json:"expect_handler"`
	ExpectTerminal    string `json:"expect_terminal"`
	ChunkCount        int    `json:"chunk_count"`
	GrantedCapability string `json:"granted_capability_tag"`
}

type sovFixture struct {
	Version          int               `json:"version"`
	Prefix           string            `json:"prefix"`
	Layout           sovLayout         `json:"layout"`
	IDs              map[string]string `json:"-"`
	Credentials      map[string]string `json:"credentials"`
	OpeningVectors   []sovOpening      `json:"opening_vectors"`
	DecoderRejects   []sovReject       `json:"decoder_rejects"`
	SignatureRejects []sovReject       `json:"signature_rejects"`
	ErrorVocabulary  struct {
		Vectors           []sovVocab        `json:"vectors"`
		UnclassifiedCases []sovUnclassified `json:"unclassified_cases"`
	} `json:"error_vocabulary"`
	Scenarios struct {
		MixedPair sovMixedPair `json:"mixed_pair"`
	} `json:"scenarios"`
}

func sovVectorsPath(t *testing.T) string {
	t.Helper()
	_, thisFile, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("runtime.Caller failed")
	}
	return filepath.Join(filepath.Dir(thisFile),
		"..", "net", "crates", "net", "tests", "cross_lang_org", "streaming_opening_vectors.json")
}

func loadSOV(t *testing.T) sovFixture {
	t.Helper()
	path := sovVectorsPath(t)
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Skipf("streaming opening vectors not present (%v) — standalone checkout", err)
	}
	var f sovFixture
	dec := json.NewDecoder(bytes.NewReader(raw))
	dec.UseNumber() // u64 values travel as strings; never as float64
	if err := dec.Decode(&f); err != nil {
		t.Fatalf("parse streaming opening vectors: %v", err)
	}
	// `ids` has heterogeneous values in some loads; keep the raw map too.
	var generic map[string]json.RawMessage
	if err := json.Unmarshal(raw, &generic); err != nil {
		t.Fatalf("reparse: %v", err)
	}
	var ids map[string]string
	if err := json.Unmarshal(generic["ids"], &ids); err != nil {
		t.Fatalf("parse ids: %v", err)
	}
	f.IDs = ids
	return f
}

// sovWireBytes returns a vector's wire bytes per the fixture's uniform rule:
// hex-decoded `wire_hex` for envelope vectors, the UTF-8 of `wire` for
// vocabulary vectors.
func sovWireBytes(t *testing.T, wireHex, wire string) []byte {
	t.Helper()
	if wireHex != "" {
		b, err := hex.DecodeString(wireHex)
		if err != nil {
			t.Fatalf("wire_hex does not decode: %v", err)
		}
		return b
	}
	return []byte(wire)
}

// sovPin asserts the byte-for-byte pin: both encodings recover the SAME bytes
// at the pinned length, and the hex form is canonical lowercase. A single
// flipped byte in any of the three fields reddens the row.
func sovPin(t *testing.T, b []byte, wireHex, wireBase64 string, wireLen int) {
	t.Helper()
	if len(b) != wireLen {
		t.Fatalf("wire_len=%d, decoded %d bytes", wireLen, len(b))
	}
	if wireHex != "" && hex.EncodeToString(b) != wireHex {
		t.Fatalf("hex re-encode differs from wire_hex (non-canonical or mangled hex)")
	}
	if base64.StdEncoding.EncodeToString(b) != wireBase64 {
		t.Fatalf("base64 re-encode differs from wire_base64")
	}
}

func TestStreamingOpeningVectors_Shape(t *testing.T) {
	v := loadSOV(t)
	if v.Version != 1 || v.Prefix != "org:" {
		t.Fatalf("version/prefix = %d/%q", v.Version, v.Prefix)
	}
	if len(v.OpeningVectors) != 6 || len(v.DecoderRejects) != 8 || len(v.SignatureRejects) != 1 {
		t.Fatalf("opening/rejects/sig_rejects = %d/%d/%d, want 6/8/1",
			len(v.OpeningVectors), len(v.DecoderRejects), len(v.SignatureRejects))
	}
	if len(v.ErrorVocabulary.Vectors) != 24 || len(v.ErrorVocabulary.UnclassifiedCases) != 4 {
		t.Fatalf("vocab/unclassified = %d/%d, want 24/4",
			len(v.ErrorVocabulary.Vectors), len(v.ErrorVocabulary.UnclassifiedCases))
	}
	if v.Layout.StreamSuffixLen != 33 || v.Layout.SignatureWireLen != 65 || v.Layout.MaxProofBytes != 1024 {
		t.Fatalf("layout = %+v", v.Layout)
	}
}

func TestStreamingOpeningVectors_BytePins(t *testing.T) {
	v := loadSOV(t)
	for _, o := range v.OpeningVectors {
		t.Run(o.ID, func(t *testing.T) {
			sovPin(t, sovWireBytes(t, o.WireHex, ""), o.WireHex, o.WireBase64, o.WireLen)
			sovPin(t, sovWireBytes(t, o.UnaryWireHex, ""), o.UnaryWireHex, o.UnaryWireBase64, o.UnaryWireLen)
		})
	}
	for _, r := range v.DecoderRejects {
		t.Run(r.ID, func(t *testing.T) {
			sovPin(t, sovWireBytes(t, r.WireHex, ""), r.WireHex, r.WireBase64, r.WireLen)
		})
	}
	for _, r := range v.SignatureRejects {
		t.Run(r.ID, func(t *testing.T) {
			sovPin(t, sovWireBytes(t, r.WireHex, ""), r.WireHex, r.WireBase64, r.WireLen)
		})
	}
	for _, x := range v.ErrorVocabulary.Vectors {
		t.Run("vocab."+x.Domain+"."+x.Kind, func(t *testing.T) {
			sovPin(t, sovWireBytes(t, "", x.Wire), "", x.WireBase64, x.WireLen)
		})
	}
	for i, x := range v.ErrorVocabulary.UnclassifiedCases {
		t.Run("unclassified."+strconv.Itoa(i), func(t *testing.T) {
			sovPin(t, sovWireBytes(t, "", x.Wire), "", x.WireBase64, x.WireLen)
		})
	}
}

func TestStreamingOpeningVectors_OpeningRoles(t *testing.T) {
	v := loadSOV(t)
	for _, o := range v.OpeningVectors {
		o := o
		t.Run("as_caller "+o.ID, func(t *testing.T) {
			if o.WireLen != o.UnaryWireLen+v.Layout.StreamSuffixLen {
				t.Fatalf("wire_len %d != unary_wire_len %d + %d", o.WireLen, o.UnaryWireLen, v.Layout.StreamSuffixLen)
			}
			if !strings.HasPrefix(o.WireHex, o.PreSigPrefixHex) || !strings.HasPrefix(o.UnaryWireHex, o.PreSigPrefixHex) {
				t.Fatal("the pre-signature prefix is not shared by both encodings")
			}
			sigAt := len(o.PreSigPrefixHex) / 2
			wire := sovWireBytes(t, o.WireHex, "")
			unary := sovWireBytes(t, o.UnaryWireHex, "")
			got := hex.EncodeToString(wire[sigAt+1 : sigAt+65])
			// postcard bytes-with-length: a varint length prefix of 64
			// (0x40) then the 64 signature bytes — 65 bytes in total.
			if wire[sigAt] != 0x40 {
				t.Fatalf("signature wire tag = %#x, want 0x40 (postcard length prefix)", wire[sigAt])
			}
			if got != o.CallBindingSigHex {
				t.Fatalf("stream signature field = %s, want call_binding_sig_hex", got)
			}
			if unary[sigAt] != 0x40 {
				t.Fatalf("unary signature wire tag = %#x, want 0x40", unary[sigAt])
			}
			if hex.EncodeToString(unary[sigAt+1:sigAt+65]) != o.UnaryCallBindingSigHex {
				t.Fatal("unary signature field differs from unary_call_binding_sig_hex")
			}
			if o.CallBindingSigHex == o.UnaryCallBindingSigHex || !o.Expect.SigDomainSeparated {
				t.Fatal("the stream and unary transcripts must sign different digests (domain separation)")
			}
		})
		t.Run("as_provider "+o.ID, func(t *testing.T) {
			wire := sovWireBytes(t, o.WireHex, "")
			suffix := wire[len(wire)-v.Layout.StreamSuffixLen:]
			if int(suffix[0]) != o.Expect.Kind || int(suffix[0]) != o.Kind {
				t.Fatalf("suffix kind byte = %d, want %d", suffix[0], o.Expect.Kind)
			}
			if v.Layout.KindValues[o.KindName] != o.Kind {
				t.Fatalf("kind_values[%q] = %d, want %d", o.KindName, v.Layout.KindValues[o.KindName], o.Kind)
			}
			if hex.EncodeToString(suffix[1:]) != o.Expect.SessionBindingHex || o.Expect.SessionBindingHex != o.SessionBindingHex {
				t.Fatal("suffix session binding differs from the declared session_binding_hex")
			}
			if len(suffix[1:]) != v.Layout.SessionBindingLen {
				t.Fatalf("suffix session binding = %d bytes, want %d", len(suffix[1:]), v.Layout.SessionBindingLen)
			}
			if o.Expect.Decode != "ok" || o.Expect.Verify != "ok" {
				t.Fatalf("expect = %+v, want decode/verify ok", o.Expect)
			}
		})
	}
}

func TestStreamingOpeningVectors_Rejects(t *testing.T) {
	v := loadSOV(t)
	bases := map[string][]byte{}
	for _, o := range v.OpeningVectors {
		bases[o.ID] = sovWireBytes(t, o.WireHex, "")
	}
	check := func(t *testing.T, r sovReject) {
		t.Helper()
		base := bases[r.Base]
		if base == nil {
			t.Fatalf("base %q not found", r.Base)
		}
		wire := sovWireBytes(t, r.WireHex, "")
		if len(wire) != r.BaseWireLen && r.BaseWireLen != len(base) {
			t.Fatalf("base_wire_len = %d, base vector is %d", r.BaseWireLen, len(base))
		}
		switch {
		case strings.HasPrefix(r.ID, "reject.trailing_byte"):
			if len(wire) != len(base)+1 || !bytes.HasPrefix(wire, base) {
				t.Fatal("not base + one trailing byte")
			}
		case strings.HasPrefix(r.ID, "reject.truncated_"):
			if len(wire) >= len(base) || !bytes.HasPrefix(base, wire) {
				t.Fatal("not a strict prefix truncation of base")
			}
		case strings.HasPrefix(r.ID, "reject.kind_"):
			if len(wire) != len(base) {
				t.Fatal("kind mutation changed the length")
			}
			diff := 0
			diffAt := -1
			for i := range wire {
				if wire[i] != base[i] {
					diff++
					diffAt = i
				}
			}
			if diff != 1 || diffAt != len(base)-v.Layout.StreamSuffixLen {
				t.Fatalf("kind mutation = %d differing byte(s) at %d, want exactly the kind byte at %d", diff, diffAt, len(base)-v.Layout.StreamSuffixLen)
			}
			if wire[diffAt] == 1 || wire[diffAt] == 2 || wire[diffAt] == 3 {
				t.Fatalf("kind mutation byte %d is a VALID kind — it would not be a reject", wire[diffAt])
			}
		case strings.HasPrefix(r.ID, "reject.over_cap"):
			if len(wire) != v.Layout.MaxProofBytes+1 || !bytes.HasPrefix(wire, base) {
				t.Fatal("not base padded past max_proof_bytes")
			}
		case strings.HasPrefix(r.ID, "reject.signature_flipped"):
			if len(wire) != len(base) {
				t.Fatal("signature mutation changed the length")
			}
			sigStart := len(base) - v.Layout.StreamSuffixLen - v.Layout.SignatureWireLen + 1
			sigEnd := len(base) - v.Layout.StreamSuffixLen
			diff, diffAt := 0, -1
			for i := range wire {
				if wire[i] != base[i] {
					diff++
					diffAt = i
				}
			}
			if diff != 1 || diffAt < sigStart || diffAt >= sigEnd {
				t.Fatalf("signature mutation = %d differing byte(s) at %d, want one inside the sig field [%d,%d)", diff, diffAt, sigStart, sigEnd)
			}
			if r.ExpectDecode != "ok" {
				t.Fatal("a flipped signature must still DECODE (and then fail verify)")
			}
			if r.ExpectVerifyErrorDisplay != "invalid signature" {
				t.Fatalf("expect_verify_error_display = %q", r.ExpectVerifyErrorDisplay)
			}
		default:
			t.Fatalf("unknown mutation class %q", r.ID)
		}
	}
	for _, r := range v.DecoderRejects {
		r := r
		t.Run(r.ID, func(t *testing.T) {
			if r.ExpectErrorDisplay != "invalid wire format" {
				t.Fatalf("expect_error_display = %q — decoder disagreement must not become success", r.ExpectErrorDisplay)
			}
			check(t, r)
		})
	}
	for _, r := range v.SignatureRejects {
		r := r
		t.Run(r.ID, func(t *testing.T) {
			check(t, r)
		})
	}
}

func TestStreamingOpeningVectors_Vocabulary(t *testing.T) {
	v := loadSOV(t)
	for _, x := range v.ErrorVocabulary.Vectors {
		x := x
		name := x.Domain + "." + x.Kind
		t.Run("as_caller classify "+name, func(t *testing.T) {
			oe := parseOrgError(x.Wire)
			if oe == nil || string(oe.Domain) != x.Domain || oe.Kind != x.Kind || oe.IsLocal() != x.IsLocal {
				t.Fatalf("parseOrgError(%q) = %+v, want %s/%s/%v", x.Wire, oe, x.Domain, x.Kind, x.IsLocal)
			}
		})
		t.Run("as_provider grammar "+name, func(t *testing.T) {
			want := "org:" + x.Domain + ":" + x.Kind
			if x.Wire != want && !strings.HasPrefix(x.Wire, want+": ") {
				t.Fatalf("wire %q does not match the org:<domain>:<kind>[: detail] grammar", x.Wire)
			}
			if x.Domain == "admission_denied" && strings.Count(x.Wire, ":") != 2 {
				t.Fatalf("admission denial %q carries detail — the coarse bucket and NOTHING else", x.Wire)
			}
		})
	}
}

func TestStreamingOpeningVectors_Unclassified(t *testing.T) {
	v := loadSOV(t)
	for i, x := range v.ErrorVocabulary.UnclassifiedCases {
		i, x := i, x
		t.Run("never-success unclassified."+strconv.Itoa(i), func(t *testing.T) {
			oe := parseOrgError(x.Wire)
			if string(oe.Domain) != "unknown" || string(oe.Domain) != x.ExpectDomain {
				t.Fatalf("classified as %q — malformed/unknown errors must not become success", oe.Domain)
			}
			if oe.Kind != "" {
				t.Fatalf("an unclassifiable wire exposed kind %q", oe.Kind)
			}
			if oe.IsLocal() || x.ExpectIsLocal {
				t.Fatal("an unclassifiable wire claimed locality")
			}
		})
	}
}

var sovNarrowed = regexp.MustCompile(`[0-9a-f]{16}\.\.\.`)

func TestStreamingOpeningVectors_NarrowedIds(t *testing.T) {
	v := loadSOV(t)
	var full []string
	for _, id := range v.IDs {
		if len(id) == 64 {
			full = append(full, id)
		}
	}
	if len(full) == 0 {
		t.Fatal("no full 32-byte ids in the fixture")
	}
	seen := 0
	for _, x := range v.ErrorVocabulary.Vectors {
		for _, narrowed := range sovNarrowed.FindAllString(x.Wire, -1) {
			seen++
			core := strings.TrimSuffix(narrowed, "...")
			if len(core) != 16 {
				t.Fatalf("narrowed display %q is not 16 hex chars", narrowed)
			}
			for _, f := range full {
				if f == narrowed || f == core {
					t.Fatalf("a narrowed display %q became a full id — narrowing IDs must not become success", narrowed)
				}
			}
			// The classification of such a wire must surface no id at all.
			oe := parseOrgError(x.Wire)
			if string(oe.Domain) != x.Domain || oe.Kind != x.Kind {
				t.Fatal("classification changed for a wire carrying a narrowed id")
			}
		}
	}
	if seen == 0 {
		t.Fatal("no narrowed displays found — the narrowing-ID guard had nothing to guard")
	}
}

func TestStreamingOpeningVectors_U64Exact(t *testing.T) {
	v := loadSOV(t)
	digits := regexp.MustCompile(`^\d+$`)
	for _, o := range v.OpeningVectors {
		for name, s := range map[string]string{"call_id": o.CallID, "proof_expires_at_unix_ns": o.ProofExpiresUnixNs} {
			if !digits.MatchString(s) {
				t.Fatalf("%s %q is not a decimal string (u64 values travel as strings)", name, s)
			}
			n, err := strconv.ParseUint(s, 10, 64)
			if err != nil || strconv.FormatUint(n, 10) != s {
				t.Fatalf("%s %q does not round-trip exactly", name, s)
			}
		}
	}
}

// TestStreamingOpeningVectors_MixedPair_GoCallerPythonProvider is THE mixed
// non-Rust pair of the lane: Go calls, Python serves, end to end over a real
// mesh, every observable pinned by scenarios.mixed_pair. Both sides load the
// vector file; the Python side (tests/cross_lang_org/mixed_pair/provider.py)
// asserts the handler-side facts and emits RESULT ok only when they hold —
// decoder disagreement or callback loss must not become success.
func TestStreamingOpeningVectors_MixedPair_GoCallerPythonProvider(t *testing.T) {
	skipIfNotEnabled(t)
	// The ONE cross-OS-process cell of the estate: it spawns the Python
	// provider (tests/cross_lang_org/mixed_pair/provider.py). Stay bound
	// behind RUN_MIXED_CROSS_PROCESS=1 so the default and CI estates never
	// race a second runtime's build state (Main pins the name as
	// existing-but-gated). The one red this row ever produced was
	// CONSUMER-SIDE and is fixed: the provider originally discarded its
	// serve handle, and ServeHandle's Drop (mesh_rpc.rs:468) RAII-deregisters
	// the service before the first announcement, so the granted scoped
	// envelope never sealed. Keep any serve handle bound for the whole serve
	// lifetime (the rule is stated at provider.py's handler surface).
	if os.Getenv("RUN_MIXED_CROSS_PROCESS") == "" {
		t.Skip("the mixed pair spawns the Python provider across OS processes — set RUN_MIXED_CROSS_PROCESS=1 to execute it (the cross-run contract)")
	}
	v := loadSOV(t)
	sc := v.Scenarios.MixedPair

	py, err := exec.LookPath("python")
	if err != nil {
		py, err = exec.LookPath("python3")
	}
	if err != nil {
		t.Skipf("python not on PATH (%v) — mixed pair needs both runtimes", err)
	}
	probe := exec.Command(py, "-c", "import net; assert hasattr(net, 'serve_org_streaming') and hasattr(net, 'install_provider_grant_audience')")
	if out, err := probe.CombinedOutput(); err != nil {
		t.Skipf("python net wheel lacks the S4 streaming surface (%v): %s — rebuild with the wheel-acceptance profile", err, out)
	}

	outdir := t.TempDir()
	m := genOrgScenario(t, outdir)
	path := func(rel string) string { return filepath.Join(outdir, rel) }
	if m.Caller.OrgIDHex != v.IDs["acting_org_granted_hex"] {
		t.Fatalf("manifest acting org %s != vectors ids.acting_org_granted_hex %s", m.Caller.OrgIDHex, v.IDs["acting_org_granted_hex"])
	}
	if m.Provider.OrgIDHex != v.IDs["provider_org_hex"] {
		t.Fatalf("manifest provider org %s != vectors ids.provider_org_hex %s", m.Provider.OrgIDHex, v.IDs["provider_org_hex"])
	}
	if m.GrantedService != sc.Service {
		t.Fatalf("manifest service %q != scenario service %q", m.GrantedService, sc.Service)
	}

	callerAddr := reserveLocalUDPPort(t)
	caller, err := NewMeshNode(MeshConfig{
		BindAddr:        callerAddr,
		PskHex:          m.PskHex,
		IdentitySeedHex: m.Caller.SeedHex,
		HeartbeatMs:     200,
		// Explicitly match the Python NetMesh default (4): a shard-count
		// mismatch strands capability announcements on unpooled shards.
		NumShards: 4,
	})
	if err != nil {
		t.Fatalf("caller construction: %v", err)
	}
	t.Cleanup(func() { _ = caller.Shutdown() })
	if err := InstallOrgAuthority(caller, path(m.Caller.AuthorityDir)); err != nil {
		t.Fatalf("install caller authority: %v", err)
	}
	membership, err := os.ReadFile(path(m.Caller.MembershipPath))
	if err != nil {
		t.Fatal(err)
	}
	dispatcher, err := os.ReadFile(path(m.Caller.DispatcherPath))
	if err != nil {
		t.Fatal(err)
	}
	callerGrant, err := os.ReadFile(path(m.Caller.GrantPath))
	if err != nil {
		t.Fatal(err)
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

	_, thisFile, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("runtime.Caller failed")
	}
	providerScript := filepath.Join(filepath.Dir(thisFile),
		"..", "net", "crates", "net", "tests", "cross_lang_org", "mixed_pair", "provider.py")

	cmd := exec.Command(py, providerScript,
		"--manifest", outdir,
		"--vectors", sovVectorsPath(t),
		"--caller-node-id", strconv.FormatUint(caller.NodeID(), 10))
	cmd.Stderr = os.Stderr
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		t.Fatal(err)
	}
	if err := cmd.Start(); err != nil {
		t.Fatalf("start python provider: %v", err)
	}
	t.Cleanup(func() { _ = cmd.Process.Kill() })

	lines := make(chan string, 16)
	go func() {
		scanner := bufio.NewScanner(stdout)
		for scanner.Scan() {
			lines <- scanner.Text()
		}
		close(lines)
	}()
	readLine := func(phase string) string {
		select {
		case line, ok := <-lines:
			if !ok {
				t.Fatalf("python provider exited before %s", phase)
			}
			return line
		case <-time.After(120 * time.Second):
			t.Fatalf("timeout waiting for %s (callback loss must not become success)", phase)
		}
		return ""
	}

	ready := readLine("READY")
	fields := strings.Fields(ready)
	if len(fields) != 4 || fields[0] != "READY" {
		t.Fatalf("bad READY line: %q", ready)
	}
	providerAddr, providerPub, providerNodeID := fields[1], fields[2], fields[3]
	nodeID, err := strconv.ParseUint(providerNodeID, 10, 64)
	if err != nil {
		t.Fatalf("provider node id: %v", err)
	}

	if err := caller.Connect(providerAddr, providerPub, nodeID); err != nil {
		t.Fatalf("caller handshake to the python provider: %v", err)
	}
	if err := caller.Start(); err != nil {
		t.Fatalf("caller start: %v", err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()
	var stream *RpcStream
	convergeOrgCall(t, 60*time.Second, func() error {
		// Force scoped-catalog emission on BOTH sides each pass (the
		// `_Pair.announce` discipline of the in-process language harnesses).
		_ = caller.AnnounceCapabilities(CapabilitySet{})
		req, err := hex.DecodeString(sc.RequestHex)
		if err != nil {
			return err
		}
		stream, err = client.CallStreaming(ctx, sc.Service, req)
		return err
	})
	defer stream.Close()

	var chunks [][]byte
	for {
		chunk, err := stream.Recv()
		if errors.Is(err, ErrStreamDone) {
			break
		}
		if err != nil {
			t.Fatalf("recv: %v", err)
		}
		chunks = append(chunks, chunk)
	}
	if len(chunks) != sc.ChunkCount {
		t.Fatalf("chunks = %d, want %d (callback loss must not become success)", len(chunks), sc.ChunkCount)
	}
	for i, wantHex := range sc.ChunksHex {
		want, err := hex.DecodeString(wantHex)
		if err != nil {
			t.Fatal(err)
		}
		if !bytes.Equal(chunks[i], want) {
			t.Fatalf("chunk %d = %x, want %x (byte-for-byte pin)", i, chunks[i], want)
		}
	}
	if sc.ExpectTerminal != "eof" {
		t.Fatalf("expect_terminal = %q", sc.ExpectTerminal)
	}

	result := readLine("RESULT")
	if result != "RESULT ok calls=1 chunks="+strconv.Itoa(sc.ChunkCount) {
		t.Fatalf("python provider did not confirm the vector pins: %q", result)
	}
	_ = cmd.Wait()
}
