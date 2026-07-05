// Package net — MCP bridge pure helpers + the graduated consent / pin
// surface (`MCP_BRIDGE_SDK_PLAN.md` P3).
//
// Compiled into the `libnet_mcp_ffi` cdylib (separate from `libnet`).
// Build with `cargo build --release -p net-mcp-ffi`.
//
// This is the Go face of exactly what the Python and Node bindings expose
// — one Rust implementation, three faces (doctrine #1: no logic in
// bindings). Identity canonicalization, the consent decision, and the pin
// store's atomic-save + cross-process lock protocol all live in Rust; this
// file marshals arguments and results across cgo.
//
//   - Classify / LowerTool are the pure helpers: no mesh, no process, no
//     secret ever crosses (the forwarding / keychain internals are never
//     bound).
//   - ConsentPolicy is the allowlist / pin gate.
//   - PinStore is the path-scoped, cross-process-locked consent store —
//     the same file the `net mcp pin` CLI and a running `net mcp serve`
//     shim use.
package net

/*
#cgo LDFLAGS: -L${SRCDIR}/../net/crates/net/target/release -lnet_mcp_ffi
#cgo darwin LDFLAGS: -framework Security -framework CoreFoundation
#include <stdlib.h>

typedef struct ConsentPolicyHandle ConsentPolicyHandle;

extern const char* net_mcp_last_error_message(void);
extern const char* net_mcp_last_error_kind(void);
extern void        net_mcp_clear_last_error(void);
extern void        net_mcp_free_string(char* s);

extern char* net_mcp_classify(const char* program, const char* args_json,
                              const char* envs_json, const char* credential_override,
                              int force);
extern char* net_mcp_lower_tool(const char* tool_json, const char* server_version,
                                const char* credential_status, const char* substitutability);

extern int   net_mcp_credential_requires_consent(const char* status);
extern char* net_mcp_cap_id_canonicalize(const char* display);

extern ConsentPolicyHandle* net_mcp_consent_policy_new(void);
extern void  net_mcp_consent_policy_free(ConsentPolicyHandle* policy);
extern int   net_mcp_consent_policy_allow(ConsentPolicyHandle* policy, const char* cap_id);
extern int   net_mcp_consent_policy_pin(ConsentPolicyHandle* policy, const char* cap_id);
extern int   net_mcp_consent_policy_unpin(ConsentPolicyHandle* policy, const char* cap_id);
extern int   net_mcp_consent_policy_is_pinned(ConsentPolicyHandle* policy, const char* cap_id);
extern char* net_mcp_consent_policy_decide(ConsentPolicyHandle* policy, const char* cap_id,
                                           const char* credential_status);
extern char* net_mcp_consent_policy_pinned(ConsentPolicyHandle* policy);

extern char* net_mcp_pin_request(const char* store_path, const char* cap_id);
extern int   net_mcp_pin_approve(const char* store_path, const char* cap_id);
extern int   net_mcp_pin_reject(const char* store_path, const char* cap_id);
extern int   net_mcp_pin_is_approved(const char* store_path, const char* cap_id);
extern char* net_mcp_pin_state(const char* store_path, const char* cap_id);
extern char* net_mcp_pin_list(const char* store_path);
*/
import "C"

import (
	"encoding/json"
	"errors"
	"fmt"
	"runtime"
	"sort"
	"strings"
	"sync"
	"unsafe"
)

// ErrMcp is the umbrella for any MCP-bridge FFI failure.
var ErrMcp = errors.New("mcp")

// McpError carries the thread-local last-error detail (message + kind) the
// C ABI recorded for the most recent failure.
type McpError struct {
	// Kind is a stable tag: "invalid_arg", "classify_error", "pins_error",
	// "encode_error", "runtime_panic", etc.
	Kind    string
	Message string
}

func (e *McpError) Error() string {
	if e.Kind != "" {
		return fmt.Sprintf("mcp: %s: %s", e.Kind, e.Message)
	}
	return fmt.Sprintf("mcp: %s", e.Message)
}

func (e *McpError) Unwrap() error { return ErrMcp }

// pinThread locks the calling goroutine to its OS thread and returns the
// unlock func; use as `defer pinThread()()`. The Rust side records its
// last-error in a thread_local!, and lastMcpError drains it with SEPARATE cgo
// calls (message, kind, clear). Without pinning, Go may migrate the goroutine
// between the failing FFI call and the drain, so the getters would read a
// different OS thread's slot — losing the error (yielding a nil error on a
// real failure), returning a stale error from an unrelated operation, or
// clearing an in-flight error another goroutine was about to read. Holding the
// OS thread across the whole call + drain keeps them on one thread. (Counted
// and reentrancy-safe, so nesting through helpers is fine.)
func pinThread() func() {
	runtime.LockOSThread()
	return runtime.UnlockOSThread
}

// lastMcpError drains the thread-local last-error pair into a Go error. Call
// immediately after a C function signalled failure (NULL / -1), before any
// other FFI call on this goroutine's thread overwrites the thread-local. The
// caller must hold its OS thread (see pinThread) for the whole call + drain so
// the thread-local is read on the thread the failure was recorded on.
func lastMcpError() error {
	e := &McpError{}
	if p := C.net_mcp_last_error_message(); p != nil {
		e.Message = C.GoString(p)
	}
	if p := C.net_mcp_last_error_kind(); p != nil {
		e.Kind = C.GoString(p)
	}
	C.net_mcp_clear_last_error()
	if e.Message == "" && e.Kind == "" {
		return nil
	}
	return e
}

// takeString copies an owned C string returned by the FFI into a Go string
// and frees the C allocation. A NULL pointer yields ("", false).
func takeString(cs *C.char) (string, bool) {
	if cs == nil {
		return "", false
	}
	defer C.net_mcp_free_string(cs)
	return C.GoString(cs), true
}

// checkNoNUL rejects strings containing a NUL byte, which C.CString would
// silently truncate at — a cap id or store path could otherwise be
// reinterpreted as a shorter, different value once it crosses into C.
// (JSON args marshaled via encoding/json are NUL-escaped, so only the
// raw-passed strings need this.)
func checkNoNUL(args ...string) error {
	for _, s := range args {
		if strings.IndexByte(s, 0) >= 0 {
			return &McpError{Kind: "invalid_arg", Message: "argument contains a NUL byte"}
		}
	}
	return nil
}

// cstr is C.CString + a cleanup func; the caller must call free().
func cstr(s string) (*C.char, func()) {
	cs := C.CString(s)
	return cs, func() { C.free(unsafe.Pointer(cs)) }
}

// optCstr is like cstr but passes NULL for the empty string (the FFI reads
// NULL as "use the default").
func optCstr(s string) (*C.char, func()) {
	if s == "" {
		return nil, func() {}
	}
	return cstr(s)
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

// Classify scores a wrapped MCP server's credential exposure, returning the
// status label ("credentialed" / "external_api" / "unknown" / "none"). Only
// the env KEYS drive detection; the values are never inspected beyond
// presence and never appear in the result.
//
// override is "" (detect) / "credentialed" / "no-credentials"; a downward
// override needs force=true, mirroring `net wrap --no-credentials --force`.
func Classify(program string, args []string, envs map[string]string, override string, force bool) (string, error) {
	defer pinThread()()
	if err := checkNoNUL(program, override); err != nil {
		return "", err
	}
	argsJSON, err := json.Marshal(args)
	if err != nil {
		return "", fmt.Errorf("mcp: marshal args: %w", err)
	}
	if envs == nil {
		envs = map[string]string{}
	}
	envsJSON, err := json.Marshal(envs)
	if err != nil {
		return "", fmt.Errorf("mcp: marshal envs: %w", err)
	}

	cProgram, freeProgram := cstr(program)
	defer freeProgram()
	cArgs, freeArgs := cstr(string(argsJSON))
	defer freeArgs()
	cEnvs, freeEnvs := cstr(string(envsJSON))
	defer freeEnvs()
	cOverride, freeOverride := optCstr(override)
	defer freeOverride()

	var cForce C.int
	if force {
		cForce = 1
	}

	label, ok := takeString(C.net_mcp_classify(cProgram, cArgs, cEnvs, cOverride, cForce))
	if !ok {
		return "", lastMcpError()
	}
	return label, nil
}

// LoweredTool is the result of lowering one MCP tools/list entry: the
// channel-safe service id, the original MCP name (what tools/call uses),
// the Net discovery descriptor, and the tool::<id>::<field> bridge
// metadata (classification labels only, never a secret).
type LoweredTool struct {
	ToolID         string            `json:"tool_id"`
	McpName        string            `json:"mcp_name"`
	Descriptor     json.RawMessage   `json:"descriptor"`
	BridgeMetadata map[string]string `json:"bridge_metadata"`
}

// LowerTool lowers one MCP tools/list entry (its JSON object) to the Net
// discovery shape. credentialStatus is the exact label the classifier
// produced (trusted local input — "none" is accepted verbatim);
// substitutability is "" (provider_local) / "provider_equivalent".
func LowerTool(toolJSON, serverVersion, credentialStatus, substitutability string) (LoweredTool, error) {
	defer pinThread()()
	var out LoweredTool
	if err := checkNoNUL(toolJSON, serverVersion, credentialStatus, substitutability); err != nil {
		return out, err
	}

	cTool, freeTool := cstr(toolJSON)
	defer freeTool()
	cVersion, freeVersion := cstr(serverVersion)
	defer freeVersion()
	cStatus, freeStatus := cstr(credentialStatus)
	defer freeStatus()
	cSub, freeSub := optCstr(substitutability)
	defer freeSub()

	js, ok := takeString(C.net_mcp_lower_tool(cTool, cVersion, cStatus, cSub))
	if !ok {
		return out, lastMcpError()
	}
	if err := json.Unmarshal([]byte(js), &out); err != nil {
		return out, fmt.Errorf("mcp: decode lowered tool: %w", err)
	}
	return out, nil
}

// ---------------------------------------------------------------------------
// Consent gate
// ---------------------------------------------------------------------------

// CredentialRequiresConsent reports whether a wire-declared credential
// status requires local consent. A wire "none" is NOT trusted (gates like
// "unknown"), so "none" / "" / anything unrecognised returns true.
func CredentialRequiresConsent(status string) bool {
	// No error channel here; a NUL-bearing status would truncate in C, so gate
	// it (require consent) rather than risk under-gating a reinterpreted value.
	if strings.IndexByte(status, 0) >= 0 {
		return true
	}
	cStatus, free := optCstr(status)
	defer free()
	return C.net_mcp_credential_requires_consent(cStatus) != 0
}

// CanonicalizeCapID returns the canonical provider/capability display id
// (trims whitespace, rewrites a 0x-hex node id to decimal), so "0x2a/echo"
// and "42/echo" return the same string. Errors on a missing/empty half.
func CanonicalizeCapID(display string) (string, error) {
	defer pinThread()()
	if err := checkNoNUL(display); err != nil {
		return "", err
	}
	cDisplay, free := cstr(display)
	defer free()
	s, ok := takeString(C.net_mcp_cap_id_canonicalize(cDisplay))
	if !ok {
		return "", lastMcpError()
	}
	return s, nil
}

// ConsentPolicy is the consumer-side consent gate: a config allowlist plus
// a pinned set. With no entries, EVERY discovered capability requires
// approval. The decision logic lives in Rust; this wraps the opaque handle.
//
// The Rust handle has no interior lock and its mutators take &mut, so `mu`
// serializes every FFI access and the free: without it, concurrent
// Allow/Pin/Unpin (or a mutator racing a reader) would create aliasing &mut
// in Rust — a data race — and two concurrent Close calls would double-free.
// A *ConsentPolicy may therefore be shared across goroutines.
type ConsentPolicy struct {
	mu  sync.Mutex
	ptr *C.ConsentPolicyHandle
}

// NewConsentPolicy allocates an empty policy. Call Close (or rely on the
// finalizer) to free it.
func NewConsentPolicy() (*ConsentPolicy, error) {
	defer pinThread()()
	ptr := C.net_mcp_consent_policy_new()
	if ptr == nil {
		return nil, lastMcpError()
	}
	p := &ConsentPolicy{ptr: ptr}
	runtime.SetFinalizer(p, (*ConsentPolicy).Close)
	return p, nil
}

// Close frees the underlying handle. Idempotent and safe to call
// concurrently: the mutex serializes it against in-flight methods and a
// second Close, so the handle is freed at most once.
func (p *ConsentPolicy) Close() {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.ptr != nil {
		C.net_mcp_consent_policy_free(p.ptr)
		p.ptr = nil
		runtime.SetFinalizer(p, nil)
	}
}

func (p *ConsentPolicy) mutate(capID string, fn func(*C.ConsentPolicyHandle, *C.char) C.int) error {
	if err := checkNoNUL(capID); err != nil {
		return err
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	defer pinThread()()
	// Keep p reachable until the cgo call returns: p's finalizer frees the
	// Rust handle, and without this the GC could run it (freeing p.ptr) while
	// the C call is still dereferencing it — a use-after-free. Every method
	// that passes p.ptr to cgo does the same.
	defer runtime.KeepAlive(p)
	cCap, free := cstr(capID)
	defer free()
	if fn(p.ptr, cCap) != 0 {
		return lastMcpError()
	}
	return nil
}

// Allow allowlists a capability (a standing pre-approval).
func (p *ConsentPolicy) Allow(capID string) error {
	return p.mutate(capID, func(h *C.ConsentPolicyHandle, c *C.char) C.int {
		return C.net_mcp_consent_policy_allow(h, c)
	})
}

// Pin records an approved pin.
func (p *ConsentPolicy) Pin(capID string) error {
	return p.mutate(capID, func(h *C.ConsentPolicyHandle, c *C.char) C.int {
		return C.net_mcp_consent_policy_pin(h, c)
	})
}

// Unpin removes a pin.
func (p *ConsentPolicy) Unpin(capID string) error {
	return p.mutate(capID, func(h *C.ConsentPolicyHandle, c *C.char) C.int {
		return C.net_mcp_consent_policy_unpin(h, c)
	})
}

// IsPinned reports whether the capability is pinned.
func (p *ConsentPolicy) IsPinned(capID string) (bool, error) {
	if err := checkNoNUL(capID); err != nil {
		return false, err
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	defer pinThread()()
	defer runtime.KeepAlive(p)
	cCap, free := cstr(capID)
	defer free()
	rc := C.net_mcp_consent_policy_is_pinned(p.ptr, cCap)
	if rc < 0 {
		return false, lastMcpError()
	}
	return rc != 0, nil
}

// Decide returns "allowed" or "requires_approval" for the capability with
// the given wire credential status (the SDK enum's stable string).
func (p *ConsentPolicy) Decide(capID, credentialStatus string) (string, error) {
	if err := checkNoNUL(capID, credentialStatus); err != nil {
		return "", err
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	defer pinThread()()
	defer runtime.KeepAlive(p)
	cCap, freeCap := cstr(capID)
	defer freeCap()
	cStatus, freeStatus := cstr(credentialStatus)
	defer freeStatus()
	s, ok := takeString(C.net_mcp_consent_policy_decide(p.ptr, cCap, cStatus))
	if !ok {
		// A NULL decision is a failure — never report it as ("", nil), which
		// RequiresApproval would read as "not requires_approval" = allowed. If
		// the FFI signalled failure but left no error detail, synthesize one so
		// the decision fails closed rather than silently opening the gate.
		if err := lastMcpError(); err != nil {
			return "", err
		}
		return "", &McpError{Kind: "invalid_arg", Message: "consent decision unavailable"}
	}
	return s, nil
}

// RequiresApproval is the boolean convenience over Decide. It fails CLOSED:
// on any error the capability is treated as requiring approval, and only an
// explicit "allowed" decision clears the gate — an unexpected or empty
// decision string requires approval rather than being mistaken for allowed.
func (p *ConsentPolicy) RequiresApproval(capID, credentialStatus string) (bool, error) {
	decision, err := p.Decide(capID, credentialStatus)
	if err != nil {
		return true, err
	}
	return decision != "allowed", nil
}

// Pinned returns the pinned capabilities' display ids, sorted.
func (p *ConsentPolicy) Pinned() ([]string, error) {
	p.mu.Lock()
	defer p.mu.Unlock()
	defer pinThread()()
	defer runtime.KeepAlive(p)
	js, ok := takeString(C.net_mcp_consent_policy_pinned(p.ptr))
	if !ok {
		return nil, lastMcpError()
	}
	var ids []string
	if err := json.Unmarshal([]byte(js), &ids); err != nil {
		return nil, fmt.Errorf("mcp: decode pinned: %w", err)
	}
	return ids, nil
}

// ---------------------------------------------------------------------------
// Pin store
// ---------------------------------------------------------------------------

// PinRecord is one entry from PinStore.List.
type PinRecord struct {
	CapID string `json:"cap_id"`
	State string `json:"state"`
}

// PinStore is a path-scoped handle on the machine-shared pin store — the
// same file the `net mcp pin` CLI and a running `net mcp serve` shim use.
// Reads load a fresh snapshot; every mutation runs the Rust core's full
// locked load->apply->save transaction, so concurrent access (another
// process, another goroutine) can never be clobbered by a stale snapshot.
type PinStore struct {
	path string
}

// OpenPinStore returns a handle on the pin store file at path. The file
// need not exist — a missing store reads as empty and is created on the
// first mutation.
func OpenPinStore(path string) *PinStore {
	return &PinStore{path: path}
}

// Path returns the store's file path.
func (s *PinStore) Path() string { return s.path }

func (s *PinStore) stringOp(fn func(cPath, cCap *C.char) *C.char, capID string) (string, bool, error) {
	defer pinThread()()
	if err := checkNoNUL(s.path, capID); err != nil {
		return "", false, err
	}
	cPath, freePath := cstr(s.path)
	defer freePath()
	cCap, freeCap := cstr(capID)
	defer freeCap()
	out, ok := takeString(fn(cPath, cCap))
	if !ok {
		return "", false, lastMcpError()
	}
	return out, true, nil
}

func (s *PinStore) intOp(fn func(cPath, cCap *C.char) C.int, capID string) (bool, error) {
	defer pinThread()()
	if err := checkNoNUL(s.path, capID); err != nil {
		return false, err
	}
	cPath, freePath := cstr(s.path)
	defer freePath()
	cCap, freeCap := cstr(capID)
	defer freeCap()
	rc := fn(cPath, cCap)
	if rc < 0 {
		return false, lastMcpError()
	}
	return rc != 0, nil
}

// Request records a pin request (the model-callable verb): writes a
// "pending" record if none exists, else leaves the record untouched.
// Returns the resulting state ("pending" / "approved").
func (s *PinStore) Request(capID string) (string, error) {
	state, _, err := s.stringOp(func(cPath, cCap *C.char) *C.char {
		return C.net_mcp_pin_request(cPath, cCap)
	}, capID)
	return state, err
}

// Approve approves a pin (operator verb). Returns whether this changed the
// stored state.
func (s *PinStore) Approve(capID string) (bool, error) {
	return s.intOp(func(cPath, cCap *C.char) C.int {
		return C.net_mcp_pin_approve(cPath, cCap)
	}, capID)
}

// Reject removes a pin (operator verb). Returns whether a record existed.
func (s *PinStore) Reject(capID string) (bool, error) {
	return s.intOp(func(cPath, cCap *C.char) C.int {
		return C.net_mcp_pin_reject(cPath, cCap)
	}, capID)
}

// IsApproved reports whether the capability is approved (fresh snapshot).
func (s *PinStore) IsApproved(capID string) (bool, error) {
	return s.intOp(func(cPath, cCap *C.char) C.int {
		return C.net_mcp_pin_is_approved(cPath, cCap)
	}, capID)
}

// State returns the capability's state ("pending" / "approved"), or "" and
// ok=false when there is no record.
func (s *PinStore) State(capID string) (state string, ok bool, err error) {
	raw, gotString, err := s.stringOp(func(cPath, cCap *C.char) *C.char {
		return C.net_mcp_pin_state(cPath, cCap)
	}, capID)
	if err != nil || !gotString {
		return "", false, err
	}
	if raw == "" {
		return "", false, nil // no record
	}
	return raw, true, nil
}

// List returns all records, sorted by cap id.
func (s *PinStore) List() ([]PinRecord, error) {
	defer pinThread()()
	if err := checkNoNUL(s.path); err != nil {
		return nil, err
	}
	cPath, freePath := cstr(s.path)
	defer freePath()
	js, ok := takeString(C.net_mcp_pin_list(cPath))
	if !ok {
		return nil, lastMcpError()
	}
	var rows []PinRecord
	if err := json.Unmarshal([]byte(js), &rows); err != nil {
		return nil, fmt.Errorf("mcp: decode pin list: %w", err)
	}
	return rows, nil
}

// Approved returns the display ids of every approved capability, sorted —
// a convenience derived from List.
func (s *PinStore) Approved() ([]string, error) {
	return s.idsInState("approved")
}

// Pending returns the display ids of every pending capability, sorted.
func (s *PinStore) Pending() ([]string, error) {
	return s.idsInState("pending")
}

func (s *PinStore) idsInState(want string) ([]string, error) {
	rows, err := s.List()
	if err != nil {
		return nil, err
	}
	ids := make([]string, 0, len(rows))
	for _, r := range rows {
		if r.State == want {
			ids = append(ids, r.CapID)
		}
	}
	sort.Strings(ids)
	return ids, nil
}
