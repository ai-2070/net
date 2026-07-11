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
	constRe        = regexp.MustCompile(`\b(NET_\w+)\s*=\s*(-?\w+)`)
	typedefRe      = regexp.MustCompile(`}\s*(\w+_t)\s*;`)
	whitespaceRe   = regexp.MustCompile(`\s+`)
)

type headerSurface struct {
	fns      map[string]string // name -> normalized parameter list
	consts   map[string]string // name -> value
	typedefs map[string]bool
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
		consts:   map[string]string{},
		typedefs: map[string]bool{},
	}
	for _, m := range fnDeclRe.FindAllStringSubmatch(src, -1) {
		args := strings.TrimSpace(whitespaceRe.ReplaceAllString(m[2], " "))
		s.fns[m[1]] = args
	}
	for _, m := range constRe.FindAllStringSubmatch(src, -1) {
		s.consts[m[1]] = m[2]
	}
	for _, m := range typedefRe.FindAllStringSubmatch(src, -1) {
		s.typedefs[m[1]] = true
	}
	return s
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
}
