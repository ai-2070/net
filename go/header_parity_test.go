package net

// Parity guard for the two hand-maintained C headers:
//
//   - go/net.h                       — what cgo actually compiles against
//   - net/crates/net/include/net.go.h — the crate's published header
//
// The two are siblings by design (comment wording may differ), but
// their FUNCTIONAL surface — function declarations, NET_* constants,
// typedef names — must stay identical. They have drifted before:
// `NET_ERR_INTERIOR_NUL` and `net_predicate_redact_trace_metadata_keys`
// existed only in the crate header until this guard landed, and the
// stage-5 `net_traversal_stats_v2_t` addition had to be made twice by
// hand. Pure text parsing — no cgo, no built cdylib required.

import (
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
)

var (
	blockCommentRe = regexp.MustCompile(`(?s)/\*.*?\*/`)
	lineCommentRe  = regexp.MustCompile(`//[^\n]*`)
	fnDeclRe       = regexp.MustCompile(`(?s)\b(net_\w+)\s*\(([^;{)]*(?:\([^)]*\))?[^;{)]*)\)\s*;`)
	// Enum-style constants: `NET_X = <value>` inside enum blocks.
	constRe = regexp.MustCompile(`\b(NET_\w+)\s*=\s*(-?\w+)`)
	// Preprocessor-style constants: `#define NET_X <value>`
	// (cubic round 4: NET_STREAM_* / NET_COMPUTE_* live as defines
	// and were invisible to the guard). Valueless guards like
	// `#define NET_SDK_H` are deliberately NOT matched — include
	// guards aren't part of the constant surface.
	defineConstRe = regexp.MustCompile(`(?m)^\s*#define\s+(NET_\w+)\s+(-?\w+)\s*$`)
	typedefRe     = regexp.MustCompile(`}\s*(\w+_t)\s*;`)
	// Inline struct typedefs (`typedef struct [tag] { … } name_t;`):
	// capture the body so field-level drift — a reordered, retyped, or
	// resized field — is caught, not just the typedef name. Opaque
	// forward decls (`typedef struct net_x_s net_x_t;`) have no `{` and
	// don't match. The stage-5 net_traversal_stats_v2_t was added to
	// both headers by hand; a name-only check would miss exactly the
	// kind of hand-copy slip this guards.
	structTypedefRe = regexp.MustCompile(`typedef\s+struct\s*(?:\w+\s*)?\{([^}]*)\}\s*(\w+_t)\s*;`)
	// One field inside a struct body: `<type> <name>[<n>];`.
	structFieldRe = regexp.MustCompile(`([A-Za-z_][A-Za-z0-9_ ]*?)\s+([A-Za-z_]\w*)\s*(\[\s*\d+\s*\])?\s*;`)
	whitespaceRe  = regexp.MustCompile(`\s+`)
)

type headerSurface struct {
	fns      map[string]string   // name -> normalized parameter list
	rets     map[string]string   // name -> normalized return type
	consts   map[string]string   // name -> value
	typedefs map[string]bool     // opaque + inline typedef names
	structs  map[string][]string // inline struct name -> ordered "type name[arr]" fields
}

func parseHeader(t *testing.T, path string) headerSurface {
	t.Helper()
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read %s: %v", path, err)
	}
	src := blockCommentRe.ReplaceAllString(string(raw), "")
	src = lineCommentRe.ReplaceAllString(src, "")

	s := headerSurface{
		fns:      map[string]string{},
		rets:     map[string]string{},
		consts:   map[string]string{},
		typedefs: map[string]bool{},
		structs:  map[string][]string{},
	}
	for _, idx := range fnDeclRe.FindAllStringSubmatchIndex(src, -1) {
		name := src[idx[2]:idx[3]]
		args := strings.TrimSpace(whitespaceRe.ReplaceAllString(src[idx[4]:idx[5]], " "))
		s.fns[name] = args
		s.rets[name] = returnTypeBefore(src, idx[2])
	}
	for _, m := range constRe.FindAllStringSubmatch(src, -1) {
		s.consts[m[1]] = m[2]
	}
	for _, m := range defineConstRe.FindAllStringSubmatch(src, -1) {
		s.consts[m[1]] = m[2]
	}
	for _, m := range typedefRe.FindAllStringSubmatch(src, -1) {
		s.typedefs[m[1]] = true
	}
	for _, m := range structTypedefRe.FindAllStringSubmatch(src, -1) {
		body, name := m[1], m[2]
		var fields []string
		for _, f := range structFieldRe.FindAllStringSubmatch(body, -1) {
			typ := whitespaceRe.ReplaceAllString(strings.TrimSpace(f[1]), " ")
			arr := whitespaceRe.ReplaceAllString(f[3], "")
			fields = append(fields, typ+" "+f[2]+arr)
		}
		s.structs[name] = fields
	}
	return s
}

// returnTypeBefore is the declaration text between the previous `;`, `{`
// or `}` and a function name, minus preprocessor lines, with whitespace and
// pointer spacing normalized ("char *" and "char*" compare equal). The
// return type is part of the C ABI: `int net_x(...)` and `void net_x(...)`
// must not compare equal just because their parameters do.
func returnTypeBefore(src string, nameAt int) string {
	start := strings.LastIndexAny(src[:nameAt], ";{}") + 1
	var kept []string
	for _, line := range strings.Split(src[start:nameAt], "\n") {
		if !strings.HasPrefix(strings.TrimSpace(line), "#") {
			kept = append(kept, line)
		}
	}
	ret := strings.TrimSpace(whitespaceRe.ReplaceAllString(strings.Join(kept, " "), " "))
	ret = strings.ReplaceAll(ret, " *", "*")
	return strings.TrimPrefix(ret, "extern ")
}

// requireCrateTree skips a test that reads the Rust crate (sources or
// canonical headers) when the Go module is checked out on its own, as
// TestHeaderParityWithCrateHeader does. In the monorepo, and so in CI, the
// tree is always there.
func requireCrateTree(t *testing.T) {
	t.Helper()
	if _, err := os.Stat(filepath.Join("..", "net", "crates", "net", "src")); err != nil {
		t.Skipf("crate tree not present (%v) — standalone checkout", err)
	}
}

func onlyIn(a, b map[string]string) []string {
	var out []string
	for k := range a {
		if _, ok := b[k]; !ok {
			out = append(out, k)
		}
	}
	return out
}

func TestHeaderParityWithCrateHeader(t *testing.T) {
	crateHeader := filepath.Join("..", "net", "crates", "net", "include", "net.go.h")
	if _, err := os.Stat(crateHeader); err != nil {
		// Vendored / standalone checkouts of the Go module don't
		// carry the crate tree; the guard only means something in
		// the monorepo (where both headers can be edited).
		t.Skipf("crate header not present (%v) — standalone checkout", err)
	}
	goHeader := parseHeader(t, "net.h")
	crate := parseHeader(t, crateHeader)

	if len(goHeader.fns) == 0 || len(crate.fns) == 0 {
		t.Fatal("header parser matched zero declarations — parser regressed")
	}
	// Pin the #define pass specifically: NET_STREAM_TIMEOUT exists as
	// a define (not an enum member) in both headers, so its absence
	// here means the define regex regressed and define-style drift
	// would go unchecked again.
	for name, surf := range map[string]headerSurface{"go/net.h": goHeader, "include/net.go.h": crate} {
		if _, ok := surf.consts["NET_STREAM_TIMEOUT"]; !ok {
			t.Fatalf("%s: define-style constants not parsed (NET_STREAM_TIMEOUT missing)", name)
		}
		// Pin the struct-body pass: net_traversal_stats_v2_t is the one
		// inline struct with fields. Empty means the struct/field regex
		// regressed and field-level drift would go unchecked again.
		if len(surf.structs["net_traversal_stats_v2_t"]) == 0 {
			t.Fatalf("%s: struct fields not parsed (net_traversal_stats_v2_t empty) — parser regressed", name)
		}
	}

	if miss := onlyIn(crate.fns, goHeader.fns); len(miss) > 0 {
		t.Errorf("functions declared in include/net.go.h but missing from go/net.h: %v", miss)
	}
	if extra := onlyIn(goHeader.fns, crate.fns); len(extra) > 0 {
		t.Errorf("functions declared in go/net.h but missing from include/net.go.h: %v", extra)
	}
	for name, args := range crate.fns {
		if got, ok := goHeader.fns[name]; ok && got != args {
			t.Errorf("signature drift for %s:\n  include: (%s)\n  go:      (%s)", name, args, got)
		}
		if got, ok := goHeader.rets[name]; ok && got != crate.rets[name] {
			t.Errorf("return type drift for %s: include %q, go %q", name, crate.rets[name], got)
		}
	}

	if miss := onlyIn(crate.consts, goHeader.consts); len(miss) > 0 {
		t.Errorf("constants in include/net.go.h but missing from go/net.h: %v", miss)
	}
	if extra := onlyIn(goHeader.consts, crate.consts); len(extra) > 0 {
		t.Errorf("constants in go/net.h but missing from include/net.go.h: %v", extra)
	}
	for name, val := range crate.consts {
		if got, ok := goHeader.consts[name]; ok && got != val {
			t.Errorf("constant value drift for %s: include=%s go=%s", name, val, got)
		}
	}

	for name := range crate.typedefs {
		if !goHeader.typedefs[name] {
			t.Errorf("typedef %s in include/net.go.h but missing from go/net.h", name)
		}
	}
	for name := range goHeader.typedefs {
		if !crate.typedefs[name] {
			t.Errorf("typedef %s in go/net.h but missing from include/net.go.h", name)
		}
	}

	// Struct field-level parity: a name-only typedef check misses a
	// field reordered / retyped / resized in one header but not the
	// other, which silently corrupts the cgo ABI (Go reads at the
	// wrong offsets). Compare the ordered field list of every inline
	// struct. The Rust `#[repr(C)]` side is cross-checked separately in
	// the crate (ffi::mesh traversal-stats ABI test).
	for name, crateFields := range crate.structs {
		goFields, ok := goHeader.structs[name]
		if !ok {
			t.Errorf("struct %s in include/net.go.h but missing from go/net.h", name)
			continue
		}
		if len(crateFields) != len(goFields) {
			t.Errorf("struct %s field-count drift: include=%d go=%d\n  include: %v\n  go:      %v",
				name, len(crateFields), len(goFields), crateFields, goFields)
			continue
		}
		for i := range crateFields {
			if crateFields[i] != goFields[i] {
				t.Errorf("struct %s field %d drift: include=%q go=%q", name, i, crateFields[i], goFields[i])
			}
		}
	}
	for name := range goHeader.structs {
		if _, ok := crate.structs[name]; !ok {
			t.Errorf("struct %s in go/net.h but missing from include/net.go.h", name)
		}
	}
}

// TestHeaderParserReadsReturnTypes pins the return-type pass: without it the
// parity checks compare parameters only, and `int net_x(...)` against
// `void net_x(...)` passes (cubic review, PR #1165).
func TestHeaderParserReadsReturnTypes(t *testing.T) {
	h := parseHeader(t, "net.h")
	for name, want := range map[string]string{
		"net_blob_publish":                         "int",
		"net_blob_free_buffer":                     "void",
		"net_mesh_blob_adapter_new":                "net_mesh_blob_adapter_t*",
		"net_mesh_blob_adapter_prometheus_text":    "char*",
		"net_blob_register_callback_adapter_owned": "int",
	} {
		if got := h.rets[name]; got != want {
			t.Errorf("return type of %s = %q, want %q", name, got, want)
		}
	}
	src := "#include <x.h>\nint net_a(void);\n/* c */ const char *  net_b(int x);\n"
	src = blockCommentRe.ReplaceAllString(src, "")
	got := map[string]string{}
	for _, idx := range fnDeclRe.FindAllStringSubmatchIndex(src, -1) {
		got[src[idx[2]:idx[3]]] = returnTypeBefore(src, idx[2])
	}
	if got["net_a"] != "int" || got["net_b"] != "const char*" {
		t.Errorf("returnTypeBefore = %v; want net_a int, net_b const char*", got)
	}
}
