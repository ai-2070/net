// ABI-stability regression tests for the Go binding's error
// surface. These pin the wire-format SDK callers match on:
//
//   1. The `daemon:` / `migration:` / `traversal:` / `channel:`
//      message prefixes produced by `*DaemonError`,
//      `*DuplicateKindError`, `*MigrationError`, and the
//      package-level `Err*` sentinels.
//   2. The stable `MigrationErrorKind` string vocabulary — a
//      binding release that renames one of these breaks every
//      caller's switch statement, so each kind gets a hard-
//      coded equality assertion.
//   3. `parseMigrationError` correctly lifts every valid kind
//      out of a synthesized `daemon: migration: <kind>[: detail]`
//      message, and demotes unknown kinds to
//      `MigrationErrKindUnknown` (forward-compat contract).
//
// These tests don't touch any cgo handle and require no live
// mesh — they operate on the Go-side type surface only, so a
// rename in `migration.go` flags here before it propagates into
// an SDK release.
//
// Corresponds to TEST_COVERAGE_PLAN §P3-15.
//
// The FFI round-trip test that actually exercises the Go↔C
// uint64 boundary lives in `abi_stability_cgo_test.go`, gated
// on `//go:build test_helpers` — cgo directives aren't allowed
// inside `_test.go` files, so the helper lives in a paired
// non-test file with the same build tag.

package net

import (
	"errors"
	"os"
	"regexp"
	"strconv"
	"strings"
	"testing"
)

// TestABIStabilityDaemonErrorPrefix pins that *DaemonError always
// serializes with exactly the "daemon: " prefix. Any other prefix
// would break `classifyError`-style helpers on the Node side and
// the `migration_error_kind` parser on the Python side, since
// they share this envelope convention.
func TestABIStabilityDaemonErrorPrefix(t *testing.T) {
	cases := []struct {
		name string
		msg  string
		want string
	}{
		{"empty message", "", "daemon: "},
		{"simple detail", "factory not found", "daemon: factory not found"},
		{"nested prefix", "migration: not-ready", "daemon: migration: not-ready"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			e := &DaemonError{Message: tc.msg}
			if got := e.Error(); got != tc.want {
				t.Fatalf("DaemonError.Error() = %q, want %q — \"daemon: \" prefix is a wire contract", got, tc.want)
			}
		})
	}
}

// TestABIStabilityDuplicateKindErrorFormat pins the exact format
// string `DuplicateKindError` emits — callers who regex on the
// kind name (for structured dispatch) depend on it staying in
// single-quotes.
func TestABIStabilityDuplicateKindErrorFormat(t *testing.T) {
	e := &DuplicateKindError{Kind: "counter"}
	const want = "daemon: factory for kind 'counter' is already registered"
	if got := e.Error(); got != want {
		t.Fatalf("DuplicateKindError.Error() = %q, want %q", got, want)
	}
}

// TestABIStabilityTraversalSentinels pins every `ErrTraversal*`
// sentinel's `.Error()` value. `errors.Is` relies on pointer
// equality, but callers that log+match on substrings (common in
// operator tooling) need the string values stable too.
func TestABIStabilityTraversalSentinels(t *testing.T) {
	cases := []struct {
		name string
		err  error
		want string
	}{
		{"ReflexTimeout", ErrTraversalReflexTimeout, "traversal: reflex-timeout"},
		{"PeerNotReachable", ErrTraversalPeerNotReachable, "traversal: peer-not-reachable"},
		{"Transport", ErrTraversalTransport, "traversal: transport"},
		{"RendezvousNoRelay", ErrTraversalRendezvousNoRelay, "traversal: rendezvous-no-relay"},
		{"RendezvousRejected", ErrTraversalRendezvousRejected, "traversal: rendezvous-rejected"},
		{"PunchFailed", ErrTraversalPunchFailed, "traversal: punch-failed"},
		{"PortMapUnavailable", ErrTraversalPortMapUnavailable, "traversal: port-map-unavailable"},
		{"Unsupported", ErrTraversalUnsupported, "traversal: unsupported"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if tc.err == nil {
				t.Fatalf("sentinel is nil — removed without replacement?")
			}
			if got := tc.err.Error(); got != tc.want {
				t.Fatalf("sentinel %q = %q, want %q", tc.name, got, tc.want)
			}
			if !strings.HasPrefix(tc.err.Error(), "traversal: ") {
				t.Fatalf("sentinel %q lost the \"traversal: \" prefix", tc.name)
			}
		})
	}
}

// TestABIStabilityChannelAuthSentinel pins the channel error.
func TestABIStabilityChannelAuthSentinel(t *testing.T) {
	if ErrChannelAuth == nil {
		t.Fatal("ErrChannelAuth sentinel is nil")
	}
	const want = "channel: unauthorized"
	if got := ErrChannelAuth.Error(); got != want {
		t.Fatalf("ErrChannelAuth = %q, want %q", got, want)
	}
}

// TestABIStabilityMigrationErrorKinds pins every
// `MigrationErrorKind` string literal. The set + values are the
// cross-binding vocabulary: renaming "not-ready" to "notReady"
// breaks Go, Python, *and* Node callers simultaneously, so the
// assertion is deliberately strict (not lower/upper-case
// insensitive).
func TestABIStabilityMigrationErrorKinds(t *testing.T) {
	cases := []struct {
		kind MigrationErrorKind
		want string
	}{
		{MigrationErrKindNotReady, "not-ready"},
		{MigrationErrKindFactoryNotFound, "factory-not-found"},
		{MigrationErrKindComputeNotSupported, "compute-not-supported"},
		{MigrationErrKindStateFailed, "state-failed"},
		{MigrationErrKindAlreadyMigrating, "already-migrating"},
		{MigrationErrKindIdentityTransportFailed, "identity-transport-failed"},
		{MigrationErrKindNotReadyTimeout, "not-ready-timeout"},
		{MigrationErrKindDaemonNotFound, "daemon-not-found"},
		{MigrationErrKindTargetUnavailable, "target-unavailable"},
		{MigrationErrKindWrongPhase, "wrong-phase"},
		{MigrationErrKindSnapshotTooLarge, "snapshot-too-large"},
		{MigrationErrKindUnknown, "unknown"},
	}
	for _, tc := range cases {
		if string(tc.kind) != tc.want {
			t.Errorf("MigrationErrorKind %q bound to %q, want %q", tc.want, string(tc.kind), tc.want)
		}
	}
}

// TestABIStabilityParseMigrationErrorRoundTrip pins the parser:
// for every known kind, a synthesized `migration: <kind>[: detail]`
// message on a `DaemonError` lifts into a typed `*MigrationError`
// carrying exactly that kind. Unknown kinds demote to
// `MigrationErrKindUnknown` so SDK callers never see a nil or a
// surprise string.
func TestABIStabilityParseMigrationErrorRoundTrip(t *testing.T) {
	knownKinds := []MigrationErrorKind{
		MigrationErrKindNotReady,
		MigrationErrKindFactoryNotFound,
		MigrationErrKindComputeNotSupported,
		MigrationErrKindStateFailed,
		MigrationErrKindAlreadyMigrating,
		MigrationErrKindIdentityTransportFailed,
		MigrationErrKindNotReadyTimeout,
		MigrationErrKindDaemonNotFound,
		MigrationErrKindTargetUnavailable,
		MigrationErrKindWrongPhase,
		MigrationErrKindSnapshotTooLarge,
	}
	for _, k := range knownKinds {
		t.Run(string(k), func(t *testing.T) {
			// Tag-only form.
			d := &DaemonError{Message: "migration: " + string(k)}
			me := parseMigrationError(d)
			if me == nil {
				t.Fatalf("parseMigrationError returned nil for tag-only kind %q", k)
			}
			if me.Kind != k {
				t.Errorf("kind = %q, want %q", me.Kind, k)
			}
			if me.Detail != "" {
				t.Errorf("detail = %q, want empty for tag-only form", me.Detail)
			}

			// Tag + detail form.
			d2 := &DaemonError{Message: "migration: " + string(k) + ": boom"}
			me2 := parseMigrationError(d2)
			if me2 == nil {
				t.Fatalf("parseMigrationError returned nil for tag+detail kind %q", k)
			}
			if me2.Kind != k {
				t.Errorf("kind with detail = %q, want %q", me2.Kind, k)
			}
			if me2.Detail != "boom" {
				t.Errorf("detail = %q, want %q", me2.Detail, "boom")
			}

			// Errors.As must still reach the underlying *DaemonError
			// — callers who catch the broader type keep working.
			var de *DaemonError
			if !errors.As(me, &de) {
				t.Fatalf("errors.As(*MigrationError, *DaemonError) must succeed")
			}
			if !strings.HasPrefix(de.Error(), "daemon: migration: ") {
				t.Errorf("wrapped DaemonError prefix broken: %q", de.Error())
			}
		})
	}

	// Unknown kind demotes to MigrationErrKindUnknown (forward
	// compat — newer Rust vocabulary must not crash older Go).
	d := &DaemonError{Message: "migration: future-kind-never-seen"}
	me := parseMigrationError(d)
	if me == nil {
		t.Fatal("parseMigrationError must not return nil for a well-formed migration: message with unknown kind")
	}
	if me.Kind != MigrationErrKindUnknown {
		t.Errorf("unknown kind = %q, want MigrationErrKindUnknown (%q)", me.Kind, MigrationErrKindUnknown)
	}

	// Non-migration bodies must not lift — returning nil lets
	// `migrationErr` fall back to the raw *DaemonError.
	d2 := &DaemonError{Message: "factory not found"}
	if parseMigrationError(d2) != nil {
		t.Errorf("parseMigrationError must return nil for a non-migration-prefixed message")
	}
	if parseMigrationError(nil) != nil {
		t.Errorf("parseMigrationError(nil) must return nil")
	}
}

// TestABIStabilityTransportDeclsMatchCanonicalHeader pins go/net.h's
// copies of the transport functions to the canonical
// include/net_transport.h. header_parity_test.go keeps go/net.h and
// net.go.h in step, but neither of those is the transport surface's
// own header, so without this a parameter edit there would leave the
// Go mirror silently stale. (GO_BINDING_CONSOLIDATION_AND_BLOBS_PLAN.md,
// "Header closure".)
func TestABIStabilityTransportDeclsMatchCanonicalHeader(t *testing.T) {
	requireCrateTree(t)
	canonical := parseHeader(t, "../net/crates/net/include/net_transport.h")
	mirror := parseHeader(t, "net.h")
	if len(canonical.fns) == 0 {
		t.Fatal("parsed no functions from net_transport.h")
	}
	for name, params := range canonical.fns {
		got, ok := mirror.fns[name]
		if !ok {
			t.Errorf("go/net.h does not declare %s (declared in net_transport.h)", name)
			continue
		}
		if got != params {
			t.Errorf("%s parameters differ:\n  net_transport.h: (%s)\n  go/net.h:        (%s)", name, params, got)
		}
		if want, got := canonical.rets[name], mirror.rets[name]; got != want {
			t.Errorf("%s return type differs: net_transport.h %q, go/net.h %q", name, want, got)
		}
	}
}

// TestABIStabilityTransferCodes pins every transfer-band constant in
// net_transport.h to the Go sentinel callers match with errors.Is, and
// that each one is still an ErrTransfer.
func TestABIStabilityTransferCodes(t *testing.T) {
	want := map[string]error{
		"NET_ERR_TRANSFER_NOT_FOUND":            ErrTransferNotFound,
		"NET_ERR_TRANSFER_HASH_MISMATCH":        ErrTransferHashMismatch,
		"NET_ERR_TRANSFER_ALL_PEERS_FAILED":     ErrTransferAllPeersFailed,
		"NET_ERR_TRANSFER_ENGINE_NOT_INSTALLED": ErrTransferEngineNotInstalled,
		"NET_ERR_TRANSFER_BACKEND":              ErrTransferBackend,
		"NET_ERR_TRANSFER_INVALID_ARGUMENT":     ErrTransferInvalidArgument,
		"NET_ERR_DIR_INVALID_MANIFEST":          ErrDirInvalidManifest,
		"NET_ERR_DIR_PATH_INVALID":              ErrDirPathInvalid,
		"NET_ERR_DIR_IO":                        ErrDirIO,
		// Mapped, but to a message rather than a dedicated sentinel.
		"NET_ERR_TRANSFER_CANCELLED":     ErrTransfer,
		"NET_ERR_TRANSFER_NULL_POINTER":  ErrTransfer,
		"NET_ERR_TRANSFER_SHUTTING_DOWN": ErrTransfer,
		"NET_ERR_TRANSFER_PANIC":         ErrTransfer,
		// Not in the transport band: the feature-off transport stubs return
		// it, so net_transport.h declares it (guarded; net.go.h and
		// net_cortex.h declare the same value).
		"NET_ERR_FEATURE_NOT_BUILT": ErrFeatureNotBuilt,
	}
	h := parseHeader(t, "../net/crates/net/include/net_transport.h")
	seen := 0
	for name, raw := range h.consts {
		if !strings.HasPrefix(name, "NET_ERR_") {
			continue
		}
		seen++
		sentinel, ok := want[name]
		if !ok {
			t.Errorf("%s (%s) has no pinned Go mapping; add it here and to transferErrorFromInt", name, raw)
			continue
		}
		code, err := strconv.Atoi(raw)
		if err != nil {
			t.Fatalf("%s = %q is not an integer", name, raw)
		}
		got := transferErrorFromInt(code)
		if !errors.Is(got, sentinel) || !errors.Is(got, ErrTransfer) {
			t.Errorf("%s (%d) maps to %v, want %v (and ErrTransfer)", name, code, got, sentinel)
		}
		if strings.Contains(got.Error(), "unknown code") {
			t.Errorf("%s (%d) falls through to the unknown-code branch", name, code)
		}
	}
	if seen != len(want) {
		t.Errorf("net_transport.h has %d NET_ERR_* constants, the pin table has %d", seen, len(want))
	}
}

// TestABIStabilityCortexDeclsMatchCanonicalHeader pins go/net_cortex.h to
// the canonical include/net_cortex.h function-for-function. No other test
// compares that pair (header_parity_test.go covers net.h / net.go.h), so
// an S3/S4 declaration added to one copy only would otherwise go unseen
// until a C consumer of the other tripped on it.
func TestABIStabilityCortexDeclsMatchCanonicalHeader(t *testing.T) {
	requireCrateTree(t)
	canonical := parseHeader(t, "../net/crates/net/include/net_cortex.h")
	mirror := parseHeader(t, "net_cortex.h")
	if len(canonical.fns) == 0 {
		t.Fatal("parsed no functions from net_cortex.h")
	}
	for _, name := range onlyIn(canonical.fns, mirror.fns) {
		t.Errorf("%s is declared in include/net_cortex.h but not go/net_cortex.h", name)
	}
	for _, name := range onlyIn(mirror.fns, canonical.fns) {
		t.Errorf("%s is declared in go/net_cortex.h but not include/net_cortex.h", name)
	}
	for name, params := range canonical.fns {
		if got, ok := mirror.fns[name]; ok && got != params {
			t.Errorf("%s parameters differ:\n  include: (%s)\n  go:      (%s)", name, params, got)
		}
		if got, ok := mirror.rets[name]; ok && got != canonical.rets[name] {
			t.Errorf("%s return type differs: include %q, go %q", name, canonical.rets[name], got)
		}
	}
}

// TestABIStabilityBlobV3ArityMatchesRust pins the six S6 declarations in
// go/net.h to their Rust definitions in src/ffi/blob.rs by parameter count.
// The header-parity tests compare the two headers with each other but never
// with Rust, and cgo only checks the header: an extra or missing Rust
// parameter would link and then read garbage.
func TestABIStabilityBlobV3ArityMatchesRust(t *testing.T) {
	requireCrateTree(t)
	src, err := os.ReadFile("../net/crates/net/src/ffi/blob.rs")
	if err != nil {
		t.Fatalf("read src/ffi/blob.rs: %v", err)
	}
	h := parseHeader(t, "net.h")
	for _, name := range []string{
		"net_mesh_blob_adapter_new_v2",
		"net_mesh_blob_adapter_fetch_range",
		"net_mesh_blob_adapter_store_tree",
		"net_mesh_blob_adapter_repair_blob",
		"net_mesh_blob_adapter_tree_node_cache_stats",
		"net_blob_ref_describe",
	} {
		re := regexp.MustCompile(`pub unsafe extern "C" fn ` + name + `\(([^)]*)\)`)
		m := re.FindStringSubmatch(string(src))
		if m == nil {
			t.Errorf("%s: no Rust definition in src/ffi/blob.rs", name)
			continue
		}
		rustParams := 0
		for _, p := range strings.Split(m[1], ",") {
			if strings.TrimSpace(p) != "" {
				rustParams++
			}
		}
		params, ok := h.fns[name]
		if !ok {
			t.Errorf("%s: not declared in go/net.h", name)
			continue
		}
		headerParams := strings.Count(params, ",") + 1
		if rustParams != headerParams {
			t.Errorf("%s: Rust takes %d parameters, go/net.h declares %d (%s)", name, rustParams, headerParams, params)
		}
	}
}

// TestABIStabilityGreedyCacheForArityMatchesRust pins the gap G-A entry
// point: declared in go/net_cortex.h with the same parameter count as its
// Rust definition (both the dataforts and the feature-off stub).
func TestABIStabilityGreedyCacheForArityMatchesRust(t *testing.T) {
	requireCrateTree(t)
	src, err := os.ReadFile("../net/crates/net/src/ffi/cortex.rs")
	if err != nil {
		t.Fatal(err)
	}
	defs := regexp.MustCompile(`pub unsafe extern "C" fn net_redex_greedy_cache_for\(([^)]*)\)`).FindAllStringSubmatch(string(src), -1)
	if len(defs) != 2 {
		t.Fatalf("want the dataforts definition and its stub, found %d", len(defs))
	}
	h := parseHeader(t, "net_cortex.h")
	params, ok := h.fns["net_redex_greedy_cache_for"]
	if !ok {
		t.Fatal("net_redex_greedy_cache_for is not declared in go/net_cortex.h")
	}
	want := strings.Count(params, ",") + 1
	for _, d := range defs {
		got := 0
		for _, p := range strings.Split(d[1], ",") {
			if strings.TrimSpace(p) != "" {
				got++
			}
		}
		if got != want {
			t.Errorf("Rust definition takes %d parameters, the header declares %d", got, want)
		}
	}
}

// TestABIStabilityBlobOwnedRegistrationMatchesRust pins the S5b surface
// (gap G-B): the owned registration's arity, and the vtable's entry order,
// which C reads positionally. A field added, dropped or reordered on either
// side would make the substrate call the wrong Go trampoline.
func TestABIStabilityBlobOwnedRegistrationMatchesRust(t *testing.T) {
	requireCrateTree(t)
	srcBytes, err := os.ReadFile("../net/crates/net/src/ffi/blob.rs")
	if err != nil {
		t.Fatal(err)
	}
	src := string(srcBytes)
	def := regexp.MustCompile(`pub unsafe extern "C" fn net_blob_register_callback_adapter_owned\(([^)]*)\)`).FindStringSubmatch(src)
	if def == nil {
		t.Fatal("net_blob_register_callback_adapter_owned is not defined in src/ffi/blob.rs")
	}
	h := parseHeader(t, "net.h")
	params, ok := h.fns["net_blob_register_callback_adapter_owned"]
	if !ok {
		t.Fatal("net_blob_register_callback_adapter_owned is not declared in go/net.h")
	}
	got := 0
	for _, p := range strings.Split(def[1], ",") {
		if strings.TrimSpace(p) != "" {
			got++
		}
	}
	if want := strings.Count(params, ",") + 1; got != want {
		t.Errorf("Rust definition takes %d parameters, the header declares %d", got, want)
	}

	rustStruct := regexp.MustCompile(`(?s)pub struct NetBlobAdapterVtable \{(.*?)\n\}`).FindStringSubmatch(src)
	if rustStruct == nil {
		t.Fatal("NetBlobAdapterVtable not found in src/ffi/blob.rs")
	}
	var rustFields []string
	for _, m := range regexp.MustCompile(`(?m)^\s*pub (\w+):`).FindAllStringSubmatch(rustStruct[1], -1) {
		rustFields = append(rustFields, m[1])
	}
	hdr, err := os.ReadFile("net.h")
	if err != nil {
		t.Fatal(err)
	}
	cStruct := regexp.MustCompile(`(?s)typedef struct net_blob_adapter_vtable_s \{(.*?)\} net_blob_adapter_vtable_t;`).FindStringSubmatch(string(hdr))
	if cStruct == nil {
		t.Fatal("net_blob_adapter_vtable_t not found in go/net.h")
	}
	var cFields []string
	for _, m := range regexp.MustCompile(`\(\*(\w+)\)`).FindAllStringSubmatch(cStruct[1], -1) {
		cFields = append(cFields, m[1])
	}
	want := []string{"store", "fetch", "fetch_range", "exists", "free_buffer"}
	if strings.Join(rustFields, ",") != strings.Join(want, ",") {
		t.Errorf("Rust vtable fields = %v, want %v", rustFields, want)
	}
	if strings.Join(cFields, ",") != strings.Join(want, ",") {
		t.Errorf("C vtable fields = %v, want %v", cFields, want)
	}
}

// TestABIStabilityHeadersDeclareReturnedCodes pins the return codes C1's
// audit found declared nowhere (C_SDK_CONSUMER_VERIFICATION_PLAN.md) to the
// headers Go compiles against. Each is declared in go/net.h or
// go/net_cortex.h with the value the Rust FFI returns, and the blob band and
// NET_ERR_FEATURE_NOT_BUILT map to their Go sentinels. Before, the whole
// NET_ERR_BLOB_* band and the cortex read-your-writes codes existed only in
// Rust and as bare numbers in Go, and header comments told C callers to
// compare against names no header declared.
func TestABIStabilityHeadersDeclareReturnedCodes(t *testing.T) {
	requireCrateTree(t)
	rust := map[string]string{}
	re := regexp.MustCompile(`(?m)^\s*pub(?:\(crate\))? const (NET_ERR_\w+): c_int = (-?\d+);`)
	for _, f := range []string{"blob.rs", "cortex.rs", "mesh.rs"} {
		src, err := os.ReadFile("../net/crates/net/src/ffi/" + f)
		if err != nil {
			t.Fatal(err)
		}
		for _, m := range re.FindAllStringSubmatch(string(src), -1) {
			rust[m[1]] = m[2]
		}
	}
	blob := map[string]error{
		"NET_ERR_BLOB_DECODE":                 ErrBlobDecode,
		"NET_ERR_BLOB_DUPLICATE_ID":           ErrBlobDuplicateID,
		"NET_ERR_BLOB_NOT_REGISTERED":         ErrBlobNotRegistered,
		"NET_ERR_BLOB_NOT_FOUND":              ErrBlobNotFound,
		"NET_ERR_BLOB_HASH_MISMATCH":          ErrBlobHashMismatch,
		"NET_ERR_BLOB_BACKEND":                ErrBlobBackend,
		"NET_ERR_BLOB_UNSUPPORTED_SCHEME":     ErrBlobUnsupportedScheme,
		"NET_ERR_BLOB_PANIC":                  ErrBlob,
		"NET_ERR_BLOB_ADAPTER_NOT_CONFIGURED": ErrBlob,
		"NET_ERR_BLOB_ADAPTER_NOT_REGISTERED": ErrBlobNotRegistered,
		"NET_ERR_BLOB_UNAUTHORIZED":           ErrBlobUnauthorized,
		"NET_ERR_BLOB_INVALID_ARGUMENT":       ErrBlobInvalidArgument,
		"NET_ERR_FEATURE_NOT_BUILT":           ErrFeatureNotBuilt,
	}
	declared := map[string][]string{
		"net.h": {"NET_ERR_GANG_INVALID"},
		"net_cortex.h": {
			"NET_ERR_TIMEOUT", "NET_ERR_STREAM_ENDED", "NET_ERR_WRONG_ORIGIN",
			"NET_ERR_QUEUE_FULL", "NET_ERR_FOLD_STOPPED", "NET_ERR_FEATURE_NOT_BUILT",
			"NET_ERR_PANIC",
		},
	}
	for name := range blob {
		declared["net.h"] = append(declared["net.h"], name)
	}
	for file, names := range declared {
		h := parseHeader(t, file)
		for _, name := range names {
			got, ok := h.consts[name]
			if !ok {
				t.Errorf("go/%s does not declare %s", file, name)
				continue
			}
			want, ok := rust[name]
			if !ok {
				t.Errorf("%s is declared in go/%s but defined in no src/ffi file read here", name, file)
				continue
			}
			if got != want {
				t.Errorf("%s is %s in go/%s but %s in Rust", name, got, file, want)
			}
			sentinel, ok := blob[name]
			if !ok || file != "net.h" {
				continue
			}
			code, err := strconv.Atoi(got)
			if err != nil {
				t.Fatalf("%s = %q is not an integer", name, got)
			}
			if err := blobRegistryErrorFromInt(code); !errors.Is(err, sentinel) || !errors.Is(err, ErrBlob) {
				t.Errorf("%s (%d) maps to %v, want %v (and ErrBlob)", name, code, err, sentinel)
			}
		}
	}
}
