// Organization capability auth — the verb facade (OSDK-L Workstream G).
//
// Two verbs (OrgCall, ServeOrg), five concepts (OrgCredentials, OrgClient,
// OrgAccess, OrgCaller, OrgError), and four error domains — the same surface
// the Rust, Node, and Python SDKs expose, over the `libnet_org` C ABI. Every
// authority decision already happened in Rust; this file is marshaling.
//
// The one rule that shapes the API: audience secrets cross as file PATHS, never
// as []byte. A discovery key must never enter Go's heap. Build credentials from
// public credential bytes plus secret-file paths:
//
//	creds, err := net.NewOrgCredentials(net.OrgCredentialsConfig{
//		Membership:          membershipBytes,
//		Dispatcher:          dispatcherBytes,
//		Grants:              [][]byte{grantBytes},
//		AudienceSecretPaths: []string{"/etc/net/grants/abc.audience"},
//	})
//	client, err := net.NewOrgClient(node, creds) // consumes creds
//	defer client.Close()                         // the withdrawal step — see below
//	resp, err := net.OrgCall[Req, Resp](ctx, client, "customer.read", req)
//
// Disposal is not hygiene. While an OrgClient is un-closed its consumer-audience
// lease stays installed, so the node keeps ingest authority for those grants —
// it can still open and store inbound private announcements for a credential set
// the application has logically finished with. Close every client; a finalizer
// backstop exists but must not be relied on.
package net

/*
#cgo LDFLAGS: -L${SRCDIR}/../net/crates/net/target/release -lnet_org -lnet
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

// Return + access constants (mirror include/net_org.h; the Rust
// bindings/go/org-ffi/src/lib.rs is the source of truth, guarded by a
// header↔Rust numeric mirror test).
#define NET_ORG_OK                     0
#define NET_ORG_ERR_NULL              -1
#define NET_ORG_ERR_INVALID_UTF8      -2
#define NET_ORG_ERR_CREDENTIALS       -3
#define NET_ORG_ERR_DISCOVERY         -4
#define NET_ORG_ERR_ADMISSION_DENIED  -5
#define NET_ORG_ERR_RPC               -6
#define NET_ORG_ERR_CLOSED            -7
#define NET_ORG_ERR_UNCLASSIFIED      -8
#define NET_ORG_ERR_NO_DISPATCHER     -9
#define NET_ORG_ERR_ALREADY_SERVING  -10
#define NET_ORG_ERR_SERVE            -11
#define NET_ORG_ERR_PROVISION        -12
#define NET_ORG_ACCESS_SAME_ORG        0
#define NET_ORG_ACCESS_GRANTED         1

// Opaque handle types from `libnet_org`.
typedef struct NetOrgCredentials NetOrgCredentials;
typedef struct NetOrgClient      NetOrgClient;
typedef struct NetOrgServeHandle NetOrgServeHandle;

// The provider-verified admission facts — five 32-byte ids, an exact
// projection of the Rust OrgCaller. 160 bytes, no padding.
typedef struct {
    uint8_t caller[32];
    uint8_t acting_org[32];
    uint8_t provider_org[32];
    uint8_t provider[32];
    uint8_t capability[32];
} net_org_caller_t;

// Handler dispatcher — Rust calls back into Go via this fn pointer to invoke a
// registered handler, passing the verified caller.
typedef int (*NetOrgHandlerFn)(
    uint64_t handler_id, const net_org_caller_t* caller,
    const uint8_t* req_ptr, size_t req_len,
    uint8_t** out_resp_ptr, size_t* out_resp_len, char** out_err);

// Imported FFI surface from `net-org-ffi`.
extern uint32_t net_org_abi_version(void);
extern int      net_org_check_abi_version(uint32_t expected);
extern void     net_org_free_cstring(char* s);
extern void     net_org_response_free(uint8_t* ptr, size_t len);

extern int net_org_credentials_new(
    const uint8_t* membership_ptr, size_t membership_len,
    const uint8_t* dispatcher_ptr, size_t dispatcher_len,
    const uint8_t* const* grant_ptrs, const size_t* grant_lens, size_t grant_count,
    const char* const* audience_secret_paths, size_t audience_secret_count,
    NetOrgCredentials** out_creds, char** out_err);
extern void net_org_credentials_free(NetOrgCredentials** credentials);

// `mesh_arc` is a void* from net_mesh_arc_clone; CONSUMED here (like
// net_rpc_new). `credentials` is CONSUMED and NULLed on both paths.
extern int  net_org_bind(void* mesh_arc, NetOrgCredentials** credentials,
                         NetOrgClient** out_client, char** out_err);
extern void net_org_client_free(NetOrgClient** client);

extern int net_org_call(
    NetOrgClient* client,
    const char* service_ptr, size_t service_len,
    const uint8_t* req_ptr, size_t req_len,
    uint64_t deadline_ms, uint64_t cancel_token,
    uint8_t** out_resp_ptr, size_t* out_resp_len, char** out_err);
extern uint64_t net_org_reserve_cancel_token(NetOrgClient* client);
extern int      net_org_cancel_call(NetOrgClient* client, uint64_t cancel_token);

extern void     net_org_set_handler_dispatcher(NetOrgHandlerFn dispatcher);
extern uint64_t net_org_reserve_handler_id(void);
extern int      net_org_serve(void* mesh_arc,
                              const char* service_ptr, size_t service_len,
                              int access, uint64_t handler_id,
                              NetOrgServeHandle** out_handle, char** out_err);
extern uint64_t net_org_serve_handle_id(const NetOrgServeHandle* handle);
extern void     net_org_serve_handle_close(NetOrgServeHandle* handle);
extern void     net_org_serve_handle_free(NetOrgServeHandle** handle);

extern int net_org_install_authority(void* mesh_arc,
                                     const char* dir_ptr, size_t dir_len,
                                     char** out_err);
extern int net_org_install_provider_grant_audience(
    void* mesh_arc,
    const uint8_t* grant_ptr, size_t grant_len,
    const char* secret_path_ptr, size_t secret_path_len,
    char** out_err);

// Trampoline Rust calls back through. Defined below as a Go //export function
// and registered via net_org_set_handler_dispatcher. cgo's auto-generated
// header from the //export pragma drops C `const` qualifiers (Go has no const),
// so this forward declaration must match the unqualified shape to avoid a
// `conflicting types` diagnostic.
int go_net_org_handler_trampoline(
    uint64_t handler_id, net_org_caller_t* caller,
    uint8_t* req_ptr, size_t req_len,
    uint8_t** out_resp_ptr, size_t* out_resp_len, char** out_err);
*/
import "C"

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"runtime"
	"strings"
	"sync"
	"sync/atomic"
	"unsafe"
)

// orgABIVersion is the ABI this file was written against. init() hard-fails if
// the loaded libnet_org is older (X3 drift guard).
const orgABIVersion uint32 = 0x0001

func init() {
	if C.net_org_check_abi_version(C.uint32_t(orgABIVersion)) != C.NET_ORG_OK {
		panic(fmt.Sprintf(
			"net: libnet_org ABI mismatch — header expects >= 0x%04x, library is 0x%04x",
			orgABIVersion, uint32(C.net_org_abi_version())))
	}
}

// =========================================================================
// Errors (G6).
// =========================================================================

// OrgDomain is the load-bearing fact in an org failure: WHERE the refusal
// happened. Credentials and Discovery are local (nothing was sent);
// AdmissionDenied is remote (a provider's admission engine refused); RPC is
// transport or a non-admission server error. Unknown is the parser/ABI
// fallback — it never impersonates a canonical domain (§D5a).
type OrgDomain string

const (
	OrgDomainCredentials     OrgDomain = "credentials"
	OrgDomainDiscovery       OrgDomain = "discovery"
	OrgDomainAdmissionDenied OrgDomain = "admission_denied"
	OrgDomainRPC             OrgDomain = "rpc"
	OrgDomainUnknown         OrgDomain = "unknown"
)

// Sentinels for errors.Is without parsing. An *OrgError matches its domain's
// sentinel via its Is method; the provisioning/serve sentinels wrap plain
// startup failures.
var (
	// ErrOrgClosed is returned by an operation on a closed OrgClient.
	ErrOrgClosed = errors.New("net.OrgClient: handle is closed")
	// ErrOrgCredentials matches any local credential-domain failure.
	ErrOrgCredentials = errors.New("org: local credential failure")
	// ErrOrgDiscovery matches "no authorized, directly-reachable provider".
	ErrOrgDiscovery = errors.New("org: no authorized provider")
	// ErrOrgAdmissionDenied matches a remote admission refusal.
	ErrOrgAdmissionDenied = errors.New("org: provider admission denied")
	// ErrOrgRPC matches a transport / non-admission server error.
	ErrOrgRPC = errors.New("org: transport or server error")
	// ErrOrgUnclassified matches the parser/ABI fallback domain.
	ErrOrgUnclassified = errors.New("org: unclassified error")
	// ErrOrgAlreadyServing is returned when the service is already served.
	ErrOrgAlreadyServing = errors.New("org: service already served on this node")
	// ErrOrgProvision wraps a provisioning (install) failure.
	ErrOrgProvision = errors.New("org: provisioning failed")
)

// OrgError is a classified organization failure. Domain says where the refusal
// happened; Kind is the finer token within the domain (e.g. "signature_invalid"),
// empty when unclassified. For the RPC domain, Unwrap returns the underlying
// *RpcError so `errors.As(err, &rpcErr)` works. errors.Is against the domain
// sentinels above works without parsing.
type OrgError struct {
	Domain  OrgDomain
	Kind    string
	Message string
	Wire    string
	rpc     *RpcError
}

func (e *OrgError) Error() string {
	if e.Wire != "" {
		return e.Wire
	}
	if e.Kind != "" {
		return fmt.Sprintf("org:%s:%s", e.Domain, e.Kind)
	}
	return fmt.Sprintf("org:%s", e.Domain)
}

// IsLocal reports whether the refusal means nothing left this process.
func (e *OrgError) IsLocal() bool {
	return e.Domain == OrgDomainCredentials || e.Domain == OrgDomainDiscovery
}

// Unwrap exposes the underlying *RpcError for the rpc domain (nil otherwise).
func (e *OrgError) Unwrap() error {
	if e.rpc != nil {
		return e.rpc
	}
	return nil
}

// Is matches the domain sentinels so `errors.Is(err, ErrOrgCredentials)` works.
func (e *OrgError) Is(target error) bool {
	switch target {
	case ErrOrgCredentials:
		return e.Domain == OrgDomainCredentials
	case ErrOrgDiscovery:
		return e.Domain == OrgDomainDiscovery
	case ErrOrgAdmissionDenied:
		return e.Domain == OrgDomainAdmissionDenied
	case ErrOrgRPC:
		return e.Domain == OrgDomainRPC
	case ErrOrgUnclassified:
		return e.Domain == OrgDomainUnknown
	}
	return false
}

// parseOrgError recovers the domain and kind from an `org:` wire string — the
// exact mirror of Rust's `parse_org_wire` (OSDK-L X1). Anything that does not
// match `org:<domain>:<kind>` with a domain this build knows classifies as
// `unknown` with no kind — deliberately, so a binding meeting an unfamiliar
// vocabulary never guesses a canonical domain.
func parseOrgError(wire string) *OrgError {
	rest, ok := strings.CutPrefix(wire, "org:")
	if !ok {
		return &OrgError{Domain: OrgDomainUnknown, Message: wire, Wire: wire}
	}
	parts := strings.SplitN(rest, ":", 3)
	if len(parts) < 2 || parts[1] == "" {
		return &OrgError{Domain: OrgDomainUnknown, Message: wire, Wire: wire}
	}
	domainTok, kind := parts[0], parts[1]
	message := ""
	if len(parts) == 3 {
		message = strings.TrimSpace(parts[2])
	}
	var domain OrgDomain
	switch domainTok {
	case "credentials":
		domain = OrgDomainCredentials
	case "discovery":
		domain = OrgDomainDiscovery
	case "admission_denied":
		domain = OrgDomainAdmissionDenied
	case "rpc":
		domain = OrgDomainRPC
	default:
		// "unknown" or an unrecognized token — never impersonate a domain.
		return &OrgError{Domain: OrgDomainUnknown, Message: wire, Wire: wire}
	}
	oe := &OrgError{Domain: domain, Kind: kind, Message: message, Wire: wire}
	if domain == OrgDomainRPC {
		// The kind token IS the nRPC kind; expose the underlying error.
		oe.rpc = &RpcError{Kind: RpcKind(kind), Message: message}
	}
	return oe
}

// readOrgCError reads and frees a CString returned via an out_err out-param.
func readOrgCError(p *C.char) string {
	if p == nil {
		return ""
	}
	defer C.net_org_free_cstring(p)
	return C.GoString(p)
}

// orgErrorFromCall turns a (code, out_err) pair into a Go error. Domain codes
// carry the `org:` wire (parsed richly); infra / provisioning / serve codes
// carry a plain message wrapped in the matching sentinel.
func orgErrorFromCall(code C.int, errPtr *C.char) error {
	if code == C.NET_ORG_OK {
		readOrgCError(errPtr) // free any stray message
		return nil
	}
	msg := readOrgCError(errPtr)
	switch code {
	case C.NET_ORG_ERR_NULL, C.NET_ORG_ERR_INVALID_UTF8:
		if msg == "" {
			msg = "invalid argument"
		}
		return fmt.Errorf("net.org: %s", msg)
	case C.NET_ORG_ERR_PROVISION:
		if msg == "" {
			msg = "provisioning failed"
		}
		return fmt.Errorf("%w: %s", ErrOrgProvision, msg)
	case C.NET_ORG_ERR_ALREADY_SERVING:
		return fmt.Errorf("%w: %s", ErrOrgAlreadyServing, strings.TrimSpace(msg))
	case C.NET_ORG_ERR_NO_DISPATCHER, C.NET_ORG_ERR_SERVE:
		if msg == "" {
			msg = "serve failed"
		}
		return fmt.Errorf("net.org serve: %s", msg)
	}
	// Call-domain codes: the `org:` wire is authoritative.
	return parseOrgError(msg)
}

// =========================================================================
// OrgCredentials (G2) — public bytes + audience-secret PATHS.
// =========================================================================

// OrgCredentialsConfig is the input to NewOrgCredentials. Public credentials
// cross as bytes; audience secrets cross ONLY as file paths — there is no bytes
// field for a secret, so a discovery key can never be a Go []byte.
type OrgCredentialsConfig struct {
	// Membership is the OrgMembershipCert wire bytes (required).
	Membership []byte
	// Dispatcher is the OrgDispatcherGrant wire bytes (required).
	Dispatcher []byte
	// Grants are OrgCapabilityGrant wire bytes (may be empty).
	Grants [][]byte
	// AudienceSecretPaths are paths to 0600 secret files (may be empty). The
	// raw discovery key is loaded, validated, and zeroized entirely in Rust.
	AudienceSecretPaths []string
}

// OrgCredentials is a validated, closed credential set. It is consumed by
// NewOrgClient; call Close only if you build one you never bind.
type OrgCredentials struct {
	mu     sync.Mutex
	handle *C.NetOrgCredentials
}

// NewOrgCredentials validates a credential set (signature + binding checks run
// in Rust). Audience secrets are supplied as file paths, never bytes.
func NewOrgCredentials(cfg OrgCredentialsConfig) (*OrgCredentials, error) {
	if len(cfg.Membership) == 0 || len(cfg.Dispatcher) == 0 {
		return nil, errors.New("net.NewOrgCredentials: membership and dispatcher are required")
	}

	memC := C.CBytes(cfg.Membership)
	defer C.free(memC)
	dispC := C.CBytes(cfg.Dispatcher)
	defer C.free(dispC)

	// Grant pointer + length arrays (backing byte buffers freed on return).
	var grantByteBufs []unsafe.Pointer
	defer func() {
		for _, b := range grantByteBufs {
			C.free(b)
		}
	}()
	var grantPtrs []*C.uint8_t
	var grantLens []C.size_t
	var grantPtrsHead **C.uint8_t
	var grantLensHead *C.size_t
	if len(cfg.Grants) > 0 {
		grantPtrs = make([]*C.uint8_t, len(cfg.Grants))
		grantLens = make([]C.size_t, len(cfg.Grants))
		for i, g := range cfg.Grants {
			gb := C.CBytes(g)
			grantByteBufs = append(grantByteBufs, gb)
			grantPtrs[i] = (*C.uint8_t)(gb)
			grantLens[i] = C.size_t(len(g))
		}
		grantPtrsHead = (**C.uint8_t)(unsafe.Pointer(&grantPtrs[0]))
		grantLensHead = (*C.size_t)(unsafe.Pointer(&grantLens[0]))
	}

	// Audience-secret PATH array (C strings freed on return).
	var pathCStrs []*C.char
	defer func() {
		for _, p := range pathCStrs {
			C.free(unsafe.Pointer(p))
		}
	}()
	var pathHead **C.char
	if len(cfg.AudienceSecretPaths) > 0 {
		pathCStrs = make([]*C.char, len(cfg.AudienceSecretPaths))
		for i, p := range cfg.AudienceSecretPaths {
			pathCStrs[i] = C.CString(p)
		}
		pathHead = (**C.char)(unsafe.Pointer(&pathCStrs[0]))
	}

	var out *C.NetOrgCredentials
	var errPtr *C.char
	code := C.net_org_credentials_new(
		(*C.uint8_t)(memC), C.size_t(len(cfg.Membership)),
		(*C.uint8_t)(dispC), C.size_t(len(cfg.Dispatcher)),
		grantPtrsHead, grantLensHead, C.size_t(len(cfg.Grants)),
		pathHead, C.size_t(len(cfg.AudienceSecretPaths)),
		&out, &errPtr,
	)
	// Keep the pointer/len slices alive across the cgo call.
	runtime.KeepAlive(grantPtrs)
	runtime.KeepAlive(grantLens)
	runtime.KeepAlive(pathCStrs)

	if err := orgErrorFromCall(code, errPtr); err != nil {
		return nil, err
	}
	creds := &OrgCredentials{handle: out}
	runtime.SetFinalizer(creds, (*OrgCredentials).finalize)
	return creds, nil
}

// take removes the handle so bind can consume it exactly once, clearing the
// finalizer. Returns nil if already taken/closed.
func (c *OrgCredentials) take() *C.NetOrgCredentials {
	c.mu.Lock()
	defer c.mu.Unlock()
	h := c.handle
	if h == nil {
		return nil
	}
	c.handle = nil
	runtime.SetFinalizer(c, nil)
	return h
}

// Close frees an unconsumed credential set. A set consumed by NewOrgClient is
// already released, so this is then a no-op. Idempotent.
func (c *OrgCredentials) Close() {
	h := c.take()
	if h != nil {
		C.net_org_credentials_free(&h)
	}
}

func (c *OrgCredentials) finalize() { c.Close() }

// =========================================================================
// OrgClient (G3) — bind, call, close.
// =========================================================================

// OrgClient is a bound organization credential set. Close it when done — that
// releases the consumer-audience lease (the withdrawal step); a leaked client
// retains ingest authority for its grants until the finalizer backstop runs.
type OrgClient struct {
	mu     sync.RWMutex
	handle *C.NetOrgClient
	closed atomic.Bool
}

// NewOrgClient binds a credential set to a mesh node. It CONSUMES creds (on
// both success and failure) — do not reuse or Close it afterward. Requires an
// installed node authority (see InstallOrgAuthority) and a durable identity
// (a seeded NewMeshNode); an ephemeral node is refused persistent_identity_required.
func NewOrgClient(node *MeshNode, creds *OrgCredentials) (*OrgClient, error) {
	if node == nil {
		return nil, errors.New("net.NewOrgClient: node must be non-nil")
	}
	if creds == nil {
		return nil, errors.New("net.NewOrgClient: creds must be non-nil")
	}
	credsHandle := creds.take()
	if credsHandle == nil {
		return nil, errors.New("net.NewOrgClient: credentials already consumed or closed")
	}
	arcPtr := node.arcClonePtr()
	if arcPtr == nil {
		// The bind would have consumed both; since we never called it, release
		// the credential handle we took so it does not leak.
		C.net_org_credentials_free(&credsHandle)
		return nil, errors.New("net.NewOrgClient: node is shutting down or freed")
	}

	ch := credsHandle
	var out *C.NetOrgClient
	var errPtr *C.char
	// net_org_bind CONSUMES arcPtr and ch (NULLing ch) on both paths.
	code := C.net_org_bind(arcPtr, &ch, &out, &errPtr)
	if err := orgErrorFromCall(code, errPtr); err != nil {
		return nil, err
	}
	client := &OrgClient{handle: out}
	runtime.SetFinalizer(client, (*OrgClient).finalize)
	return client, nil
}

// withHandle holds the read lock across the whole cgo call so a concurrent
// Close cannot free the handle mid-flight. Returns ErrOrgClosed if closed.
func (c *OrgClient) withHandle(fn func(h *C.NetOrgClient)) error {
	c.mu.RLock()
	defer c.mu.RUnlock()
	if c.handle == nil {
		return ErrOrgClosed
	}
	fn(c.handle)
	runtime.KeepAlive(c)
	return nil
}

// Close releases the consumer-audience lease and frees the handle. Idempotent;
// the finalizer calls it as a backstop, but do not rely on that. Teardown
// order: orgClient.Close() → serveHandle.Close() → node.Shutdown().
func (c *OrgClient) Close() {
	if c.closed.Swap(true) {
		return
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	runtime.SetFinalizer(c, nil)
	if c.handle != nil {
		C.net_org_client_free(&c.handle) // NULLs c.handle
	}
}

func (c *OrgClient) finalize() { c.Close() }

// IsClosed reports whether Close has been called.
func (c *OrgClient) IsClosed() bool { return c.closed.Load() }

// CallBytes calls a protected service with raw bytes — for callers who marshal
// themselves. The typed OrgCall wraps this with JSON.
//
// ctx carries a real deadline and cancellation: the deadline becomes the call's
// hard deadline, and cancelling ctx drops the one in-flight call (never a
// retry — a signed proof is never resent). A caller-cancelled call returns
// ctx.Err().
func (c *OrgClient) CallBytes(ctx context.Context, service string, req []byte) ([]byte, error) {
	deadlineMs := contextDeadlineMs(ctx)
	cService := stringToCBytes(service)
	defer C.free(cService.ptr)
	cReq, freeReq := bytesToCBytes(req)
	defer freeReq()

	cancelToken, stopWatcher := installOrgCancelWatcher(ctx, c)
	defer stopWatcher()

	var outResp *C.uint8_t
	var outRespLen C.size_t
	var outErr *C.char
	var code C.int
	if err := c.withHandle(func(h *C.NetOrgClient) {
		code = C.net_org_call(
			h,
			(*C.char)(cService.ptr), cService.len,
			cReq.ptr, cReq.len,
			C.uint64_t(deadlineMs), C.uint64_t(cancelToken),
			&outResp, &outRespLen, &outErr,
		)
	}); err != nil {
		return nil, err
	}
	return readOrgCallResult(ctx, code, outResp, outRespLen, outErr)
}

func readOrgCallResult(
	ctx context.Context,
	code C.int,
	respPtr *C.uint8_t,
	respLen C.size_t,
	errPtr *C.char,
) ([]byte, error) {
	if code != C.NET_ORG_OK {
		err := orgErrorFromCall(code, errPtr)
		// A caller-cancelled call surfaces as ctx.Err(), not org:rpc:cancelled.
		if ctx != nil && ctx.Err() != nil {
			var oe *OrgError
			if errors.As(err, &oe) && oe.Domain == OrgDomainRPC && oe.Kind == "cancelled" {
				return nil, ctx.Err()
			}
		}
		return nil, err
	}
	if respLen == 0 || respPtr == nil {
		return []byte{}, nil
	}
	defer C.net_org_response_free(respPtr, respLen)
	src := unsafe.Slice((*byte)(unsafe.Pointer(respPtr)), int(respLen))
	out := make([]byte, int(respLen))
	copy(out, src)
	return out, nil
}

// installOrgCancelWatcher reserves a cancel token and spawns a goroutine that
// fires net_org_cancel_call on ctx.Done(). Returns the token and an idempotent
// stop. Mirrors mesh_rpc.go's installCancelWatcher.
func installOrgCancelWatcher(ctx context.Context, c *OrgClient) (uint64, func()) {
	if ctx == nil || ctx.Done() == nil || c == nil {
		return 0, func() {}
	}
	var token uint64
	if err := c.withHandle(func(h *C.NetOrgClient) {
		token = uint64(C.net_org_reserve_cancel_token(h))
	}); err != nil {
		return 0, func() {}
	}
	if token == 0 {
		return 0, func() {}
	}
	stop := make(chan struct{})
	done := make(chan struct{})
	go func() {
		defer close(done)
		select {
		case <-ctx.Done():
			// Re-acquire the read lock so a concurrent Close can't free the
			// handle out from under us. A closed handle is a quiet drop.
			_ = c.withHandle(func(h *C.NetOrgClient) {
				C.net_org_cancel_call(h, C.uint64_t(token))
			})
		case <-stop:
		}
	}()
	return token, func() {
		select {
		case <-stop:
		default:
			close(stop)
		}
		<-done
	}
}

// OrgCall calls a protected service with JSON marshaling. Free function because
// Go forbids type params on methods (matching TypedCall).
func OrgCall[Req, Resp any](ctx context.Context, c *OrgClient, service string, req Req) (Resp, error) {
	var zero Resp
	body, err := jsonEncodeTyped(req)
	if err != nil {
		return zero, err
	}
	respBody, err := c.CallBytes(ctx, service, body)
	if err != nil {
		return zero, err
	}
	return jsonDecodeTyped[Resp](respBody)
}

// =========================================================================
// OrgCaller + OrgAccess (G5).
// =========================================================================

// OrgAccess selects who may call a served service and how it is announced.
type OrgAccess int

const (
	// OrgAccessSameOrg admits members of this node's own organization. Its
	// value (0) is the C ABI's NET_ORG_ACCESS_SAME_ORG, pinned by
	// TestOrgAccessConstants and the Rust header↔const mirror.
	OrgAccessSameOrg OrgAccess = 0
	// OrgAccessGranted admits members of another org holding a capability grant
	// (C ABI NET_ORG_ACCESS_GRANTED = 1).
	OrgAccessGranted OrgAccess = 1
)

// OrgCaller is the set of provider-verified facts about an admitted call — an
// exact projection of the Rust OrgCaller. Every field was verified by the
// admission engine before the handler ran; none is caller-claimed.
type OrgCaller struct {
	// Caller is the acting entity S.
	Caller [32]byte
	// ActingOrg is the organization S acted for.
	ActingOrg [32]byte
	// ProviderOrg is this provider's owner organization.
	ProviderOrg [32]byte
	// Provider is this exact provider node.
	Provider [32]byte
	// Capability is the capability that was invoked.
	Capability [32]byte
}

// IsSameOrg reports whether the call came from this provider's own organization.
func (c OrgCaller) IsSameOrg() bool { return c.ActingOrg == c.ProviderOrg }

func orgCallerFromC(c *C.net_org_caller_t) OrgCaller {
	var oc OrgCaller
	oc.Caller = *(*[32]byte)(unsafe.Pointer(&c.caller))
	oc.ActingOrg = *(*[32]byte)(unsafe.Pointer(&c.acting_org))
	oc.ProviderOrg = *(*[32]byte)(unsafe.Pointer(&c.provider_org))
	oc.Provider = *(*[32]byte)(unsafe.Pointer(&c.provider))
	oc.Capability = *(*[32]byte)(unsafe.Pointer(&c.capability))
	return oc
}

// =========================================================================
// serve (G5) — Variant-A trampoline.
// =========================================================================

// OrgHandler answers an admitted request, receiving the verified caller. Return
// AppError(code, body) to surface a typed application status; any other error
// becomes an internal server error.
type OrgHandler func(caller OrgCaller, req []byte) ([]byte, error)

var (
	orgHandlerRegistry sync.Map // handlerID (uint64) -> OrgHandler
	orgDispatcherOnce  sync.Once
)

func registerOrgDispatcher() {
	orgDispatcherOnce.Do(func() {
		C.net_org_set_handler_dispatcher(
			(C.NetOrgHandlerFn)(C.go_net_org_handler_trampoline),
		)
	})
}

//export go_net_org_handler_trampoline
func go_net_org_handler_trampoline(
	handlerID C.uint64_t,
	caller *C.net_org_caller_t,
	reqPtr *C.uint8_t,
	reqLen C.size_t,
	outRespPtr **C.uint8_t,
	outRespLen *C.size_t,
	outErr **C.char,
) C.int {
	val, ok := orgHandlerRegistry.Load(uint64(handlerID))
	if !ok {
		writeCError(outErr, fmt.Sprintf("no org handler registered for id %d", uint64(handlerID)))
		return -1
	}
	handler, _ := val.(OrgHandler)

	req, okLen := goBytesChecked(reqPtr, reqLen)
	if !okLen {
		writeCError(outErr, fmt.Sprintf("request body length %d exceeds the maximum", uint64(reqLen)))
		return -1
	}

	resp, err := safeCallOrgHandler(handler, orgCallerFromC(caller), req)
	if err != nil {
		writeCError(outErr, err.Error())
		return -1
	}

	if len(resp) == 0 {
		*outRespPtr = nil
		*outRespLen = 0
		return 0
	}
	respBuf := C.malloc(C.size_t(len(resp)))
	if respBuf == nil {
		writeCError(outErr, "C.malloc returned NULL for org response buffer")
		return -1
	}
	C.memmove(respBuf, unsafe.Pointer(&resp[0]), C.size_t(len(resp)))
	*outRespPtr = (*C.uint8_t)(respBuf)
	*outRespLen = C.size_t(len(resp))
	return 0
}

func safeCallOrgHandler(h OrgHandler, caller OrgCaller, req []byte) (resp []byte, err error) {
	defer func() {
		if r := recover(); r != nil {
			err = fmt.Errorf("org handler panicked: %v", r)
		}
	}()
	return h(caller, req)
}

// OrgServeHandle is a live protected-service registration. Close it to
// unregister; teardown order is orgClient.Close() → serveHandle.Close() →
// node.Shutdown().
type OrgServeHandle struct {
	handle    *C.NetOrgServeHandle
	handlerID uint64
	closed    atomic.Bool
}

// ServeOrgBytes registers a protected service with a raw byte handler. The
// typed ServeOrg wraps this with JSON. Requires an installed node authority; a
// Granted service also needs its provider grant audience installed (see
// InstallProviderGrantAudience).
func ServeOrgBytes(node *MeshNode, service string, access OrgAccess, handler OrgHandler) (*OrgServeHandle, error) {
	if node == nil {
		return nil, errors.New("net.ServeOrgBytes: node must be non-nil")
	}
	if handler == nil {
		return nil, errors.New("net.ServeOrgBytes: handler must be non-nil")
	}
	registerOrgDispatcher()

	arcPtr := node.arcClonePtr()
	if arcPtr == nil {
		return nil, errors.New("net.ServeOrgBytes: node is shutting down or freed")
	}

	// Reserve the id and store the callable BEFORE serving — pre-registration
	// closes the request-arrives-before-store race.
	hID := uint64(C.net_org_reserve_handler_id())
	orgHandlerRegistry.Store(hID, handler)

	cService := stringToCBytes(service)
	defer C.free(cService.ptr)

	var out *C.NetOrgServeHandle
	var errPtr *C.char
	code := C.net_org_serve(
		arcPtr,
		(*C.char)(cService.ptr), cService.len,
		C.int(access), C.uint64_t(hID),
		&out, &errPtr,
	)
	if err := orgErrorFromCall(code, errPtr); err != nil {
		orgHandlerRegistry.Delete(hID)
		return nil, err
	}
	sh := &OrgServeHandle{handle: out, handlerID: hID}
	runtime.SetFinalizer(sh, (*OrgServeHandle).finalize)
	return sh, nil
}

// ServeOrg registers a protected service with a JSON-typed handler. Free
// function because Go forbids method type params (matching TypedServe).
func ServeOrg[Req, Resp any](
	node *MeshNode,
	service string,
	access OrgAccess,
	handler func(caller OrgCaller, req Req) (Resp, error),
) (*OrgServeHandle, error) {
	shim := func(caller OrgCaller, reqBytes []byte) ([]byte, error) {
		var req Req
		if err := json.Unmarshal(reqBytes, &req); err != nil {
			body := mustMarshalBody(struct {
				Err    string `json:"error"`
				Detail string `json:"detail"`
			}{Err: "invalid_request", Detail: err.Error()})
			return nil, AppError(NrpcTypedBadRequest, body)
		}
		resp, err := handler(caller, req)
		if err != nil {
			return nil, err
		}
		return jsonEncodeTyped(resp)
	}
	return ServeOrgBytes(node, service, access, shim)
}

// Close unregisters the service and frees the handle. Idempotent; in-flight
// handlers continue but no new requests are dispatched.
func (s *OrgServeHandle) Close() {
	if s.closed.Swap(true) {
		return
	}
	runtime.SetFinalizer(s, nil)
	C.net_org_serve_handle_close(s.handle)
	C.net_org_serve_handle_free(&s.handle) // NULLs s.handle
	orgHandlerRegistry.Delete(s.handlerID)
}

func (s *OrgServeHandle) finalize() { s.Close() }

// =========================================================================
// Provisioning (G-prov2 / §D9) — node startup, distinct from adoption/issuance.
// =========================================================================

// InstallOrgAuthority installs an adopted node authority from the directory
// `net node adopt` wrote. Required before NewOrgClient can succeed or a service
// can serve. This is node startup — adoption and issuance stay in the CLI.
func InstallOrgAuthority(node *MeshNode, dir string) error {
	if node == nil {
		return errors.New("net.InstallOrgAuthority: node must be non-nil")
	}
	arcPtr := node.arcClonePtr()
	if arcPtr == nil {
		return errors.New("net.InstallOrgAuthority: node is shutting down or freed")
	}
	cDir := stringToCBytes(dir)
	defer C.free(cDir.ptr)
	var errPtr *C.char
	code := C.net_org_install_authority(arcPtr, (*C.char)(cDir.ptr), cDir.len, &errPtr)
	return orgErrorFromCall(code, errPtr)
}

// InstallProviderGrantAudience installs a provider grant audience so a Granted
// service can seal envelopes. The grant is wire bytes; its secret is a file
// PATH, never bytes. A SameOrg provider does not need this.
func InstallProviderGrantAudience(node *MeshNode, grantBytes []byte, secretPath string) error {
	if node == nil {
		return errors.New("net.InstallProviderGrantAudience: node must be non-nil")
	}
	arcPtr := node.arcClonePtr()
	if arcPtr == nil {
		return errors.New("net.InstallProviderGrantAudience: node is shutting down or freed")
	}
	cGrant, freeGrant := bytesToCBytes(grantBytes)
	defer freeGrant()
	cPath := stringToCBytes(secretPath)
	defer C.free(cPath.ptr)
	var errPtr *C.char
	code := C.net_org_install_provider_grant_audience(
		arcPtr,
		cGrant.ptr, cGrant.len,
		(*C.char)(cPath.ptr), cPath.len,
		&errPtr,
	)
	return orgErrorFromCall(code, errPtr)
}
