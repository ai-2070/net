// Subnet authority — the exported-service surface (SSDK S4d).
//
// Two ordinary verbs and one advanced namespace, over the subnet symbols in
// the `libnet_org` cdylib (see net/crates/net/include/net_subnet.h). Every
// authority decision already happened in Rust; this file is marshaling.
//
// The ordinary surface — a provider exporting one service, a caller invoking
// it with organization authority only:
//
//	handle, err := net.ServeSubnetExported[Req, Resp](node, "fleet.telemetry", "factory-export", handler)
//	defer handle.Close()
//
//	resp, err := net.CallExported[Req, Resp](ctx, orgClient, "fleet.telemetry", req)
//
// `exportName` is a provider-local label configured in MeshConfig.SubnetExports.
// It is never announced and never accepted from a caller: the application names
// a service and a local export, and constructs no authority objects — no roots,
// no credentials, no boundaries, no epochs. An unknown name fails locally,
// before anything is registered or announced.
//
// The caller side is deliberately CallExported, not CallSubnet: the caller
// presents organization authority, names no subnet, joins no subnet, and
// receives no subnet context.
//
// Administration (installing gateway credential sets, declaring boundaries,
// applying signed control facts) lives in the Subnet* functions at the bottom
// of this file — the operator surface, deliberately not beside the ordinary
// verbs. Every signed artifact is minted by `net-mesh subnet …` and crosses as
// opaque canonical wire bytes; nothing here signs, and no signing key type
// exists on this surface.
//
// # Known gap: subnet TRUST ANCHORS are not configurable from Go
//
// Declaring which authorities this node trusts (`subnet_authorities`), its
// security attachment path, and its control channel is CONFIG-TIME state on
// MeshNodeConfig — there is no post-construction installer for it. Go and C
// both receive their node from base libnet's JSON constructor, which cannot
// reach the SDK module that converts those DTOs, so neither binding can declare
// trust anchors today. Everything else works: this file serves exports, calls
// exported services, and performs every runtime administration verb.
//
// A node that must act as a subnet GATEWAY should be constructed from Rust,
// Node, or Python, which do expose the construction-time trust config. Closing
// this gap for Go and C means relocating that one conversion module into the
// core crate so base libnet can share it — deliberately deferred, not
// overlooked.
package net

/*
#cgo LDFLAGS: -L${SRCDIR}/../net/crates/net/target/release -lnet_org -lnet
#include <stdint.h>
#include <stdlib.h>
#include <stdbool.h>

// Mirrors include/net_subnet.h + the subnet codes in net_org.h. The Rust
// bindings/go/org-ffi/src/lib.rs is the source of truth; both headers are
// guarded against it by the numeric mirror tests there
// (`header_numeric_contract_matches_rust` for NET_ORG_*,
// `subnet_header_numeric_contract_matches_rust` for NET_SUBNET_*). This
// preamble is a third copy and is NOT machine-checked — a value changed
// here alone compiles. Keep it in sync by hand.
#define NET_ORG_OK                    0
#define NET_ORG_ERR_NO_DISPATCHER    -9
#define NET_ORG_ERR_ALREADY_SERVING -10
#define NET_ORG_ERR_SUBNET          -13
#define NET_SUBNET_ACCESS_SAME_ORG    0
#define NET_SUBNET_ACCESS_GRANTED     1

// A compact hierarchy path: depth 0..=4, levels[depth..] MUST be zero.
// 5 bytes, no padding (pinned by a Rust layout test).
typedef struct {
    uint8_t depth;
    uint8_t levels[4];
} net_subnet_path_t;

// An authority-qualified crossing — NOT the topology subnet id. 37 bytes.
typedef struct {
    uint8_t authority[32];
    net_subnet_path_t path;
} net_subnet_ref_t;

extern void net_org_free_cstring(char* s);

// `mesh_arc` is a void* from net_mesh_arc_clone; CONSUMED by each of these
// (mint a fresh clone per call and do NOT free it).
extern int net_subnet_install_gateway_credentials(
    void* mesh_arc,
    const uint8_t* const* set_ptrs, const size_t* set_lens, size_t set_count,
    char** out_err);

extern int net_subnet_declare_boundaries(
    void* mesh_arc,
    const uint8_t* authority, uint32_t topology_epoch,
    const net_subnet_path_t* boundaries, size_t boundary_count,
    char** out_err);

extern int net_subnet_apply_control_fact(
    void* mesh_arc,
    const uint8_t* fact_ptr, size_t fact_len,
    char** out_kind, bool* out_applied, char** out_err);

// `out_handle` is really NetOrgServeHandle** (declared in org.go's preamble).
// It crosses as void** here because each cgo file is its own translation unit,
// so org.go's opaque handle type is a DIFFERENT Go type in this file — the same
// reason mesh_arc travels as void*. org.go re-wraps the pointer.
extern int net_subnet_serve_exported(
    void* mesh_arc,
    const char* service_ptr, size_t service_len,
    const net_subnet_ref_t* export_ref, uint32_t topology_epoch,
    int access, uint64_t handler_id,
    void** out_handle, char** out_err);
*/
import "C"

import (
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"unsafe"
)

// =========================================================================
// Errors — the stable `subnet:<kind>` envelope.
// =========================================================================

// ErrSubnet matches any subnet provisioning / configuration / serve failure via
// errors.Is. These are always LOCAL and startup-shaped: configuration, decode,
// or install refused before (or without) any node-state mutation. A remote
// refusal of an exported CALL is not one of these — it surfaces through the org
// domains (ErrOrgAdmissionDenied and friends).
var ErrSubnet = errors.New("subnet: provisioning or configuration failure")

// SubnetError is a subnet failure carrying the stable kind token. Kind is a
// core reason code or a local configuration kind — the vocabulary pinned by
// net/crates/net/tests/cross_lang_subnet/stable_kinds.json — and is empty when
// the message carried no `subnet:` envelope.
type SubnetError struct {
	Kind    string
	Message string
	Wire    string
}

func (e *SubnetError) Error() string {
	if e.Wire != "" {
		return e.Wire
	}
	if e.Kind != "" {
		return "subnet:" + e.Kind
	}
	return "subnet: unknown failure"
}

// Is matches the ErrSubnet sentinel so errors.Is works without parsing.
func (e *SubnetError) Is(target error) bool { return target == ErrSubnet }

// ParseSubnetKind recovers the stable kind token from a `subnet:` wire string,
// returning "" when the message is not a subnet envelope. An unrecognized kind
// is returned verbatim: the kind is data, and substituting one this build does
// not know would be the counterfeit the org taxonomy's `unknown` rule exists to
// prevent.
//
// The registration-failure messages WRAP the envelope rather than leading with
// it (`… failed: subnet:unknown_export_name: …`), so this scans for the token
// anywhere in the message.
func ParseSubnetKind(wire string) string {
	idx := strings.Index(wire, "subnet:")
	if idx < 0 {
		return ""
	}
	rest := wire[idx+len("subnet:"):]
	if end := strings.IndexAny(rest, ": \t\n"); end >= 0 {
		rest = rest[:end]
	}
	return strings.TrimSpace(rest)
}

// newSubnetError builds a SubnetError from a wire string.
func newSubnetError(wire string) *SubnetError {
	return &SubnetError{Kind: ParseSubnetKind(wire), Message: wire, Wire: wire}
}

// readAndFreeCString reads and frees a CString handed back through an
// out-param — an out_err message or the control-fact outcome kind, both
// allocated by Rust and released through the same net_org_free_cstring.
// Its own copy (not org.go's) because each cgo file is a separate translation
// unit with its own C namespace.
func readAndFreeCString(p *C.char) string {
	if p == nil {
		return ""
	}
	defer C.net_org_free_cstring(p)
	return C.GoString(p)
}

// subnetErrorFromCall turns a (code, out_err) pair into a Go error.
func subnetErrorFromCall(code C.int, errPtr *C.char) error {
	msg := readAndFreeCString(errPtr)
	if code == C.NET_ORG_OK {
		return nil
	}
	switch code {
	case C.NET_ORG_ERR_ALREADY_SERVING:
		if msg == "" {
			return ErrOrgAlreadyServing
		}
		return fmt.Errorf("%w: %s", ErrOrgAlreadyServing, msg)
	case C.NET_ORG_ERR_NO_DISPATCHER:
		return fmt.Errorf("net: subnet handler dispatcher not registered: %s", msg)
	}
	if msg == "" {
		return fmt.Errorf("%w: code %d", ErrSubnet, int(code))
	}
	return newSubnetError(msg)
}

// =========================================================================
// The export configuration types (SSDK §3.3).
// =========================================================================

// SubnetExportAccess selects who may call a subnet-exported service. Its values
// are the C ABI's NET_SUBNET_ACCESS_* constants.
type SubnetExportAccess int

const (
	// SubnetAccessSameOrg admits members of this node's own organization.
	SubnetAccessSameOrg SubnetExportAccess = 0
	// SubnetAccessGranted admits another organization holding a capability
	// grant.
	SubnetAccessGranted SubnetExportAccess = 1
)

// SubnetPath is a compact hierarchy path: 0..=4 levels, each 0..=255. An empty
// Levels is the authority-root (global) path.
type SubnetPath struct {
	Levels []uint8 `json:"levels"`
}

// SubnetRef is an authority-qualified crossing. It is NOT the topology subnet
// id: equal paths under two different authorities are unrelated.
type SubnetRef struct {
	// AuthorityHex is the 32-byte authority entity id, 64 hex chars.
	AuthorityHex string `json:"authority_hex"`
	// Path is the path under that authority.
	Path SubnetPath `json:"path"`
}

// SubnetExportBinding is the pair an export captures: the exported crossing and
// the topology epoch it was declared under.
type SubnetExportBinding struct {
	Subnet        SubnetRef `json:"subnet"`
	TopologyEpoch uint32    `json:"topology_epoch"`
}

// SubnetNamedExport is one entry of the provider-local named-export map
// configured in MeshConfig.SubnetExports. Name is never announced and never
// accepted from a caller.
type SubnetNamedExport struct {
	Name    string              `json:"name"`
	Access  SubnetExportAccess  `json:"access"`
	Binding SubnetExportBinding `json:"binding"`
}

// toC converts a configured export into the C ABI's ref POD. Authority hex and
// path shape are checked here because they are marshaling facts (is this 32
// bytes of hex? are there at most 4 levels?); every AUTHORITY decision — is the
// binding trusted, is the epoch current, may this node export this crossing —
// happens in Rust when the serve registers.
func (e SubnetNamedExport) toC() (C.net_subnet_ref_t, error) {
	var ref C.net_subnet_ref_t
	raw, err := hex.DecodeString(e.Binding.Subnet.AuthorityHex)
	if err != nil || len(raw) != 32 {
		return ref, newSubnetError(fmt.Sprintf(
			"subnet:invalid_id_hex: export %q authority must be 64 hex chars", e.Name))
	}
	for i := 0; i < 32; i++ {
		ref.authority[i] = C.uint8_t(raw[i])
	}
	levels := e.Binding.Subnet.Path.Levels
	if len(levels) > 4 {
		return ref, newSubnetError(fmt.Sprintf(
			"subnet:path_too_deep: export %q path has %d levels (max 4)", e.Name, len(levels)))
	}
	ref.path.depth = C.uint8_t(len(levels))
	for i, l := range levels {
		ref.path.levels[i] = C.uint8_t(l)
	}
	return ref, nil
}

// buildSubnetExportMap freezes the configured exports into the lookup map a
// MeshNode retains. Called by NewMeshNode before the node exists.
//
// It checks only what makes the MAP well-formed — a non-empty, unique label —
// because an ambiguous map is a Go-side configuration error with no meaningful
// Rust counterpart. It deliberately does NOT re-implement authority validation:
// the binding is checked in Rust when the serve registers, so there is one
// authority checker, not two.
func buildSubnetExportMap(exports []SubnetNamedExport) (map[string]SubnetNamedExport, error) {
	if len(exports) == 0 {
		return nil, nil
	}
	out := make(map[string]SubnetNamedExport, len(exports))
	for _, e := range exports {
		if e.Name == "" {
			return nil, newSubnetError("subnet:empty_export_name: subnet export name must be non-empty")
		}
		if _, dup := out[e.Name]; dup {
			return nil, newSubnetError(fmt.Sprintf(
				"subnet:duplicate_export_name: subnet export %q configured twice", e.Name))
		}
		if e.Access != SubnetAccessSameOrg && e.Access != SubnetAccessGranted {
			return nil, newSubnetError(fmt.Sprintf(
				"subnet:invalid_access: export %q has access %d", e.Name, int(e.Access)))
		}
		out[e.Name] = e
	}
	return out, nil
}

// lookupSubnetExport resolves a provider-local export label. The map is
// immutable after construction, so this needs no lock.
func (m *MeshNode) lookupSubnetExport(name string) (SubnetNamedExport, bool) {
	if m == nil || m.subnetExports == nil {
		return SubnetNamedExport{}, false
	}
	e, ok := m.subnetExports[name]
	return e, ok
}

// =========================================================================
// The ordinary provider verb (SSDK §3.5).
// =========================================================================

// ServeSubnetExportedBytes registers a subnet-exported service with a raw byte
// handler — the cgo ownership/error seam the typed ServeSubnetExported wraps.
// The handler receives the same verified OrgCaller as ServeOrgBytes, so raw Go
// is a first-class route to the trampoline, not a fallback.
//
// exportName resolves against the map configured in MeshConfig.SubnetExports.
// An unknown name fails here — before any registration or announcement — with
// kind `unknown_export_name`. Announcement visibility is always public: the
// external caller proves organization authority and never joins this node's
// subnet.
//
// Requires an installed node authority (see InstallOrgAuthority).
func ServeSubnetExportedBytes(
	node *MeshNode,
	service string,
	exportName string,
	handler OrgHandler,
) (*OrgServeHandle, error) {
	if node == nil {
		return nil, errors.New("net.ServeSubnetExportedBytes: node must be non-nil")
	}
	if handler == nil {
		return nil, errors.New("net.ServeSubnetExportedBytes: handler must be non-nil")
	}

	// Resolve the NAME before touching the node: a pure lookup in the map this
	// mesh retained at construction. Failing here is what keeps an unknown name
	// from ever reaching registration or announcement.
	export, ok := node.lookupSubnetExport(exportName)
	if !ok {
		return nil, newSubnetError(fmt.Sprintf(
			"subnet:unknown_export_name: no configured subnet export named %q", exportName))
	}
	ref, err := export.toC()
	if err != nil {
		return nil, err
	}

	registerOrgDispatcher()

	arcPtr := node.arcClonePtr()
	if arcPtr == nil {
		return nil, errors.New("net.ServeSubnetExportedBytes: node is shutting down or freed")
	}

	// Reserve the id and store the callable BEFORE serving — pre-registration
	// closes the request-arrives-before-store race, exactly as ServeOrgBytes.
	hID := reserveOrgHandlerID()
	storeOrgHandler(hID, handler)

	cService := stringToCBytes(service)
	defer C.free(cService.ptr)

	var out unsafe.Pointer
	var errPtr *C.char
	code := C.net_subnet_serve_exported(
		arcPtr,
		(*C.char)(cService.ptr), cService.len,
		&ref, C.uint32_t(export.Binding.TopologyEpoch),
		C.int(export.Access), C.uint64_t(hID),
		&out, &errPtr,
	)
	if err := subnetErrorFromCall(code, errPtr); err != nil {
		deleteOrgHandler(hID)
		return nil, err
	}
	return newOrgServeHandleFromPtr(out, hID), nil
}

// ServeSubnetExported registers a subnet-exported service with a JSON-typed
// handler. Free function because Go forbids method type params (matching
// ServeOrg and TypedServe).
func ServeSubnetExported[Req, Resp any](
	node *MeshNode,
	service string,
	exportName string,
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
	return ServeSubnetExportedBytes(node, service, exportName, shim)
}

// =========================================================================
// Administration (SSDK §3.4) — the operator surface.
//
// Deliberately separate from the ordinary verbs above. Wholesale-replace
// semantics: pass every currently-held artifact, not a delta.
// =========================================================================

// InstallSubnetGatewayCredentials installs this node's own gateway credential
// sets — WHOLESALE REPLACE, so pass every set this node currently holds, not a
// delta. Every artifact decodes before anything installs: one malformed set
// refuses the whole batch with no node-state mutation.
func InstallSubnetGatewayCredentials(node *MeshNode, credentialSets [][]byte) error {
	if node == nil {
		return errors.New("net.InstallSubnetGatewayCredentials: node must be non-nil")
	}
	arcPtr := node.arcClonePtr()
	if arcPtr == nil {
		return errors.New("net.InstallSubnetGatewayCredentials: node is shutting down or freed")
	}

	ptrs := make([]*C.uint8_t, len(credentialSets))
	lens := make([]C.size_t, len(credentialSets))
	for i, set := range credentialSets {
		cSet, freeSet := bytesToCBytes(set)
		defer freeSet()
		ptrs[i] = cSet.ptr
		lens[i] = cSet.len
	}
	var ptrsArg **C.uint8_t
	var lensArg *C.size_t
	if len(credentialSets) > 0 {
		ptrsArg = &ptrs[0]
		lensArg = &lens[0]
	}

	var errPtr *C.char
	code := C.net_subnet_install_gateway_credentials(
		arcPtr, ptrsArg, lensArg, C.size_t(len(credentialSets)), &errPtr)
	return subnetErrorFromCall(code, errPtr)
}

// SubnetBoundaryDeclaration is this node's protected boundary inventory under
// one authority, at one topology epoch.
type SubnetBoundaryDeclaration struct {
	// AuthorityHex is the 32-byte authority entity id, 64 hex chars.
	AuthorityHex string `json:"authority_hex"`
	// TopologyEpoch is the epoch the boundaries are declared under.
	TopologyEpoch uint32 `json:"topology_epoch"`
	// Boundaries are the subtree roots whose edge is a protected boundary.
	Boundaries []SubnetPath `json:"boundaries"`
}

// DeclareSubnetBoundaries declares this node's protected boundary inventory —
// also WHOLESALE, so pass the complete inventory for this authority and epoch.
func DeclareSubnetBoundaries(node *MeshNode, decl SubnetBoundaryDeclaration) error {
	if node == nil {
		return errors.New("net.DeclareSubnetBoundaries: node must be non-nil")
	}
	authority, err := hex.DecodeString(decl.AuthorityHex)
	if err != nil || len(authority) != 32 {
		return newSubnetError("subnet:invalid_id_hex: authority must be 64 hex chars")
	}
	paths := make([]C.net_subnet_path_t, len(decl.Boundaries))
	for i, p := range decl.Boundaries {
		if len(p.Levels) > 4 {
			return newSubnetError(fmt.Sprintf(
				"subnet:path_too_deep: boundary %d has %d levels (max 4)", i, len(p.Levels)))
		}
		paths[i].depth = C.uint8_t(len(p.Levels))
		for j, l := range p.Levels {
			paths[i].levels[j] = C.uint8_t(l)
		}
	}

	arcPtr := node.arcClonePtr()
	if arcPtr == nil {
		return errors.New("net.DeclareSubnetBoundaries: node is shutting down or freed")
	}
	var pathsArg *C.net_subnet_path_t
	if len(paths) > 0 {
		pathsArg = &paths[0]
	}
	var errPtr *C.char
	code := C.net_subnet_declare_boundaries(
		arcPtr,
		(*C.uint8_t)(unsafe.Pointer(&authority[0])), C.uint32_t(decl.TopologyEpoch),
		pathsArg, C.size_t(len(paths)),
		&errPtr,
	)
	return subnetErrorFromCall(code, errPtr)
}

// SubnetControlOutcome is the projection of applying one signed control fact.
// Applied == false is an authenticated stale/idempotent outcome — the fact
// verified but changed nothing — NOT a transport or authority failure.
type SubnetControlOutcome struct {
	// Kind is "descriptor", "gateway_advertisement", "export_policy", or
	// "revocation_floor".
	Kind string
	// Applied reports whether any state changed.
	Applied bool
}

// ApplySubnetControlFact applies one signed control fact from its outer wire
// frame — the ONE door for floors and descriptive facts alike. The fact is an
// opaque canonical artifact minted by `net-mesh subnet …`; nothing here signs.
func ApplySubnetControlFact(node *MeshNode, fact []byte) (SubnetControlOutcome, error) {
	var out SubnetControlOutcome
	if node == nil {
		return out, errors.New("net.ApplySubnetControlFact: node must be non-nil")
	}
	arcPtr := node.arcClonePtr()
	if arcPtr == nil {
		return out, errors.New("net.ApplySubnetControlFact: node is shutting down or freed")
	}
	cFact, freeFact := bytesToCBytes(fact)
	defer freeFact()

	var kindPtr *C.char
	var applied C.bool
	var errPtr *C.char
	code := C.net_subnet_apply_control_fact(
		arcPtr, cFact.ptr, cFact.len, &kindPtr, &applied, &errPtr)
	if err := subnetErrorFromCall(code, errPtr); err != nil {
		return out, err
	}
	out.Kind = readAndFreeCString(kindPtr) // same free path (net_org_free_cstring)
	out.Applied = bool(applied)
	return out, nil
}

// =========================================================================
// The caller verb lives on OrgClient (org.go): a subnet-exported call is an
// organization call to a publicly discoverable service. CallExported is
// declared there so it shares the client handle, cancellation, and the four
// org error domains.
// =========================================================================
