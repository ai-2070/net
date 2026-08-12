package net

import (
	"go/ast"
	"go/parser"
	"go/token"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing"
)

// TestRecoverCallbackContainsAndCounts covers the helper every
// trampoline's deferred guard calls.
func TestRecoverCallbackContainsAndCounts(t *testing.T) {
	before := CallbackPanicCount()

	// The no-panic path must be free: a deferred `recover()` returns
	// nil on a normal return, and the guard must not count that as a
	// contained panic or tell the caller to fail.
	if recoverCallback("test.NoPanic", nil) {
		t.Fatal("recoverCallback reported containment for a nil recover value")
	}
	if got := CallbackPanicCount(); got != before {
		t.Fatalf("CallbackPanicCount moved on a non-panic: %d -> %d", before, got)
	}

	// A real panic, routed the way a trampoline routes it.
	code := 0
	func() {
		defer func() {
			if recoverCallback("test.Panic", recover()) {
				code = -1
			}
		}()
		var m map[string]string
		m["boom"] = "nil map write" // the exact shape peer input reaches
	}()
	if code != -1 {
		t.Fatal("panic was not converted into a failure code")
	}
	if got := CallbackPanicCount(); got != before+1 {
		t.Fatalf("CallbackPanicCount = %d, want %d", got, before+1)
	}

	// Containment must be repeatable — the goroutine that panicked
	// stays usable, which is the whole point.
	func() {
		defer func() { recoverCallback("test.Panic", recover()) }()
		panic("second")
	}()
	if got := CallbackPanicCount(); got != before+2 {
		t.Fatalf("second panic not counted: got %d, want %d", got, before+2)
	}
}

// trampolinesWithoutUserCode lists `//export`ed functions that do not
// invoke user-supplied code and therefore need no panic guard, with
// the reason each is exempt. Anything else must contain a
// recoverCallback defer.
//
// Keep this list short and justified. "It probably can't panic" is not
// a reason — the bar is "it does not call into code the application
// wrote", because that is what makes a panic reachable from a remote
// peer's input.
var trampolinesWithoutUserCode = map[string]string{
	// Drops a registry entry by id. Touches no application code.
	"goComputeFree": "registry bookkeeping only; calls no user code",
	// Already wraps the user's factory in its own recover — see the
	// `inst = nil` closure. Listed here so the audit below doesn't
	// demand a second, redundant top-level guard.
	"goComputeFactory": "wraps the user factory in its own recover",
	// The four nRPC/Org handler trampolines route through
	// safeCall*/handler wrappers that each install a recover before
	// touching user code.
	"go_net_rpc_handler_trampoline":          "user call goes through a safeCall* recover",
	"go_net_rpc_client_streaming_trampoline": "user call goes through a safeCall* recover",
	"go_net_rpc_duplex_trampoline":           "user call goes through a safeCall* recover",
	"go_net_rpc_streaming_trampoline":        "user call goes through a safeCall* recover",
	"go_net_org_handler_trampoline":          "user call goes through a safeCall* recover",
}

// TestEveryUserCallbackTrampolineContainsPanics is the standing guard
// for FFI-01.
//
// A Go panic that reaches an `//export` boundary kills the process:
// Rust's `catch_unwind` cannot translate an unwind that started in the
// Go runtime. Ten daemon and observer trampolines used to lack a
// guard, so an ordinary Go bug in user callback code — reachable by
// any peer that can send this node an event — terminated every
// unrelated daemon and service in the process along with it.
//
// Testing one trampoline would prove one trampoline. This parses the
// package and asserts the property holds for all of them, so a
// trampoline added later fails here rather than in production.
func TestEveryUserCallbackTrampolineContainsPanics(t *testing.T) {
	entries, err := os.ReadDir(".")
	if err != nil {
		t.Fatalf("read package dir: %v", err)
	}
	var files []string
	for _, e := range entries {
		name := e.Name()
		if !e.IsDir() && strings.HasSuffix(name, ".go") && !strings.HasSuffix(name, "_test.go") {
			files = append(files, name)
		}
	}
	sort.Strings(files)

	fset := token.NewFileSet()
	var checked, exempt int
	var unguarded []string

	for _, file := range files {
		src, err := os.ReadFile(file)
		if err != nil {
			t.Fatalf("read %s: %v", file, err)
		}
		parsed, err := parser.ParseFile(fset, filepath.Base(file), src, parser.ParseComments)
		if err != nil {
			// Build-tagged helper files may not parse standalone in
			// every configuration; skip rather than fail the guard.
			continue
		}
		for _, decl := range parsed.Decls {
			fn, ok := decl.(*ast.FuncDecl)
			if !ok || fn.Doc == nil || fn.Body == nil {
				continue
			}
			isExport := false
			for _, c := range fn.Doc.List {
				if strings.HasPrefix(c.Text, "//export ") {
					isExport = true
				}
			}
			if !isExport {
				continue
			}
			if _, ok := trampolinesWithoutUserCode[fn.Name.Name]; ok {
				exempt++
				continue
			}
			checked++

			// The guard must be the FIRST statement. A guard placed
			// after the handle lookup or the payload conversion
			// leaves exactly the frames peer input reaches
			// unprotected, which was the original defect.
			guarded := false
			if len(fn.Body.List) > 0 {
				if def, ok := fn.Body.List[0].(*ast.DeferStmt); ok {
					ast.Inspect(def, func(n ast.Node) bool {
						if id, ok := n.(*ast.Ident); ok && id.Name == "recoverCallback" {
							guarded = true
						}
						return true
					})
				}
			}
			if !guarded {
				unguarded = append(unguarded,
					fn.Name.Name+" ("+file+":"+
						fset.Position(fn.Pos()).String()[strings.LastIndex(
							fset.Position(fn.Pos()).String(), ":")+1:]+")")
			}
		}
	}

	if len(unguarded) > 0 {
		t.Fatalf(
			"FFI-01 regression: %d exported trampoline(s) invoke user code without a "+
				"top-level recoverCallback defer as their first statement:\n  %s\n\n"+
				"A panic in any of these kills the process — Rust's catch_unwind cannot "+
				"translate a Go unwind across cgo. Add:\n\n"+
				"    defer func() {\n"+
				"        if recoverCallback(\"<surface>\", recover()) {\n"+
				"            code = <failure value>\n"+
				"        }\n"+
				"    }()\n\n"+
				"as the first statement, and initialize any output pointers before it. "+
				"If the trampoline genuinely calls no user code, add it to "+
				"trampolinesWithoutUserCode with a reason.",
			len(unguarded), strings.Join(unguarded, "\n  "))
	}

	// Guard the guard: if a refactor renamed the trampolines or moved
	// them out of this package, the loop above would find nothing and
	// pass vacuously.
	if checked < 10 {
		t.Fatalf(
			"expected at least 10 guarded user-callback trampolines (3 compute, "+
				"6 meshos, 1 observer), found %d — did they move or get renamed? "+
				"A vacuous pass here means FFI-01 is unguarded.", checked)
	}
	if exempt != len(trampolinesWithoutUserCode) {
		t.Fatalf(
			"trampolinesWithoutUserCode names %d functions but %d were found; a stale "+
				"entry silently exempts nothing, or exempts the wrong thing",
			len(trampolinesWithoutUserCode), exempt)
	}
}

// TestMeshOsHandleIsDeletedOnlyByTheRustDestructor is the FFI-02 pin.
//
// The cgo.Handle behind a MeshOS daemon must be deleted from exactly
// one place: `goMeshOsDestroyUserCtx`, which Rust calls from
// `CDaemonBridge`'s Drop. That is the only instant at which the delete
// is provably safe — registry removal lets in-flight host Arc clones
// continue, so from Go, "teardown requested" and "no callback can
// arrive" are different moments with no signal between them.
//
// Two earlier shapes were wrong in ways this catches. Deleting in the
// handle's free closure raced an admitted callback (the original
// defect). Deleting via a Go-side callback guard closed the common
// interleavings but not the last one, and left two owners of the
// decision.
func TestMeshOsHandleIsDeletedOnlyByTheRustDestructor(t *testing.T) {
	src, err := os.ReadFile("meshos.go")
	if err != nil {
		t.Fatalf("read meshos.go: %v", err)
	}
	body := string(src)

	const del = "Delete()"
	if got := strings.Count(body, del); got != 2 {
		t.Fatalf(
			"found %d %s call sites in meshos.go, want exactly 2: the Rust-driven "+
				"destructor, and the registration-failure path where Rust never "+
				"took ownership. Any other delete cannot know whether a callback "+
				"is still in flight.",
			got, del)
	}

	// The destructor must be the one Rust calls, not something Go
	// schedules for itself.
	if !strings.Contains(body, "//export goMeshOsDestroyUserCtx") {
		t.Fatal("goMeshOsDestroyUserCtx is not exported to C; Rust cannot call it")
	}
	// Registration must go through v2, or Rust is never told about the
	// destructor and the handle leaks instead.
	if !strings.Contains(body, "net_meshos_register_daemon_with_vtable_v2") {
		t.Fatal(
			"MeshOS registration no longer uses the v2 entry point; without it " +
				"Rust never receives the destructor and the cgo.Handle leaks")
	}
	// The handle's own free closure must not delete: that is the
	// original defect's shape.
	freeClosure := body[strings.Index(body, "func newMeshOsDaemonHandle"):]
	freeClosure = freeClosure[:strings.Index(freeClosure, "\n}")]
	if strings.Contains(freeClosure, del) {
		t.Fatal(
			"newMeshOsDaemonHandle's free closure deletes the cgo.Handle again — " +
				"that is FFI-02, and it cannot know whether a callback is in flight")
	}
}

// The teardown steps must stay sequenced, not fused. Gating the native
// free on callbacks would call back into Rust from inside a Rust->Go
// callback; fusing the delete into the native free reintroduces
// FFI-02. Pin the shape so neither happens by accident.
func TestMeshOsFreeDoesNotDeleteTheHandleDirectly(t *testing.T) {
	src, err := os.ReadFile("meshos.go")
	if err != nil {
		t.Fatalf("read meshos.go: %v", err)
	}
	body := string(src)
	needle := "cgoHandle.Delete()"
	// Exactly one: the callback guard's free closure. Any other call
	// site is a delete that does not wait for in-flight callbacks.
	if got := strings.Count(body, needle); got != 1 {
		t.Fatalf(
			"found %d %s call sites in meshos.go, want exactly 1 (the "+
				"callback guard's free closure). A delete anywhere else does "+
				"not wait for in-flight Rust->Go callbacks and reopens FFI-02.",
			got, needle)
	}
}
