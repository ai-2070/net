// Package net — identity + permission-token surface.
//
// Mirrors the Rust SDK's `Identity` / `PermissionToken` one-for-one,
// matching the PyO3 / NAPI shape so cross-binding fixtures round-
// trip. Tokens cross the C boundary as opaque `[]byte` buffers
// (159 bytes each); entity ids as 32-byte slices. The Go side owns
// token storage; `net_free_bytes` is invoked inline on the return
// path via `freeBytes`.
//
// This file tracks the Stage G-1 surface of
// `docs/SDK_GO_PARITY_PLAN.md`.

package net

/*
#include "net.h"
#include <stdlib.h>
#include <string.h>
*/
import "C"

import (
	"encoding/json"
	"errors"
	"fmt"
	"runtime"
	"sync"
	"unsafe"
)

// ---------------------------------------------------------------------------
// Errors — one sentinel per `TokenError` kind so callers can
// `errors.Is(err, net.ErrTokenExpired)` without parsing the message.
// ---------------------------------------------------------------------------

var (
	// ErrIdentity covers malformed inputs at the identity layer
	// (wrong seed length, invalid entity id, unknown scope name,
	// bad channel name). Token-validity failures have their own
	// sentinels below.
	ErrIdentity = errors.New("identity: malformed input")

	ErrTokenInvalidFormat         = errors.New("token: invalid_format")
	ErrTokenInvalidSignature      = errors.New("token: invalid_signature")
	ErrTokenExpired               = errors.New("token: expired")
	ErrTokenNotYetValid           = errors.New("token: not_yet_valid")
	ErrTokenDelegationExhausted   = errors.New("token: delegation_exhausted")
	ErrTokenDelegationNotAllowed  = errors.New("token: delegation_not_allowed")
	ErrTokenNotAuthorized         = errors.New("token: not_authorized")
)

func identityErrorFromCode(code C.int) error {
	switch code {
	case 0:
		return nil
	case -1:
		return ErrNullPointer
	case -2:
		return ErrInvalidUTF8
	case -3:
		return ErrInvalidJSON
	case -120:
		return ErrIdentity
	case -121:
		return ErrTokenInvalidFormat
	case -122:
		return ErrTokenInvalidSignature
	case -123:
		return ErrTokenExpired
	case -124:
		return ErrTokenNotYetValid
	case -125:
		return ErrTokenDelegationExhausted
	case -126:
		return ErrTokenDelegationNotAllowed
	case -127:
		return ErrTokenNotAuthorized
	default:
		return fmt.Errorf("identity unknown error (code %d)", code)
	}
}

// ---------------------------------------------------------------------------
// Identity — ed25519 keypair + local token cache
// ---------------------------------------------------------------------------

// Identity is an ed25519 keypair plus a local `TokenCache`. Cheap to
// move between goroutines — both inner pieces are reference-counted
// on the Rust side. Always call `Close` (or rely on the finalizer)
// to release the underlying handle.
type Identity struct {
	mu     sync.RWMutex
	handle *C.net_identity_t
}

// GenerateIdentity creates a fresh ed25519 identity with a new
// keypair and empty token cache.
func GenerateIdentity() (*Identity, error) {
	var handle *C.net_identity_t
	code := C.net_identity_generate(&handle)
	if err := identityErrorFromCode(code); err != nil {
		return nil, err
	}
	id := &Identity{handle: handle}
	runtime.SetFinalizer(id, (*Identity).free)
	return id, nil
}

// IdentityFromSeed rehydrates an identity from a 32-byte ed25519
// seed. The persisted form IS the seed — it round-trips through
// `ToSeed`.
func IdentityFromSeed(seed []byte) (*Identity, error) {
	if len(seed) != 32 {
		return nil, ErrIdentity
	}
	var handle *C.net_identity_t
	code := C.net_identity_from_seed(
		(*C.uint8_t)(unsafe.Pointer(&seed[0])),
		C.size_t(32),
		&handle,
	)
	if err := identityErrorFromCode(code); err != nil {
		return nil, err
	}
	id := &Identity{handle: handle}
	runtime.SetFinalizer(id, (*Identity).free)
	return id, nil
}

func (id *Identity) free() {
	id.mu.Lock()
	defer id.mu.Unlock()
	if id.handle != nil {
		C.net_identity_free(id.handle)
		id.handle = nil
		runtime.SetFinalizer(id, nil)
	}
}

// Close releases the underlying handle. Safe to call more than once.
func (id *Identity) Close() {
	id.free()
}

// ToSeed returns the 32-byte ed25519 seed. Treat as secret.
func (id *Identity) ToSeed() ([]byte, error) {
	id.mu.RLock()
	defer id.mu.RUnlock()
	if id.handle == nil {
		return nil, ErrShuttingDown
	}
	out := make([]byte, 32)
	code := C.net_identity_to_seed(id.handle, (*C.uint8_t)(unsafe.Pointer(&out[0])))
	if err := identityErrorFromCode(code); err != nil {
		return nil, err
	}
	return out, nil
}

// EntityID returns the 32-byte ed25519 public key.
func (id *Identity) EntityID() ([]byte, error) {
	id.mu.RLock()
	defer id.mu.RUnlock()
	if id.handle == nil {
		return nil, ErrShuttingDown
	}
	out := make([]byte, 32)
	code := C.net_identity_entity_id(id.handle, (*C.uint8_t)(unsafe.Pointer(&out[0])))
	if err := identityErrorFromCode(code); err != nil {
		return nil, err
	}
	return out, nil
}

// NodeID returns the 64-bit node id derived from the entity id.
func (id *Identity) NodeID() uint64 {
	id.mu.RLock()
	defer id.mu.RUnlock()
	if id.handle == nil {
		return 0
	}
	return uint64(C.net_identity_node_id(id.handle))
}

// OriginHash returns the 64-bit origin hash used in packet headers.
//
// Pre-2026-05-11 this returned uint32, truncating the upper 32 bits
// of the canonical u64 origin_hash the Rust substrate emits. The Go
// header was widened to match the canonical FFI signature; callers
// that previously read the truncated low 32 bits will now see the
// full 64-bit value.
func (id *Identity) OriginHash() uint64 {
	id.mu.RLock()
	defer id.mu.RUnlock()
	if id.handle == nil {
		return 0
	}
	return uint64(C.net_identity_origin_hash(id.handle))
}

// Sign signs `msg` with the identity's ed25519 secret key.
// Returns a 64-byte signature.
func (id *Identity) Sign(msg []byte) ([]byte, error) {
	id.mu.RLock()
	defer id.mu.RUnlock()
	if id.handle == nil {
		return nil, ErrShuttingDown
	}
	out := make([]byte, 64)
	var msgPtr *C.uint8_t
	if len(msg) > 0 {
		msgPtr = (*C.uint8_t)(unsafe.Pointer(&msg[0]))
	}
	code := C.net_identity_sign(
		id.handle,
		msgPtr,
		C.size_t(len(msg)),
		(*C.uint8_t)(unsafe.Pointer(&out[0])),
	)
	if err := identityErrorFromCode(code); err != nil {
		return nil, err
	}
	return out, nil
}

// IssueTokenRequest describes a token the identity is issuing as
// signer. `Scope` is any non-empty subset of
// `{"publish", "subscribe", "admin", "delegate"}`.
type IssueTokenRequest struct {
	Subject         []byte // 32 bytes
	Scope           []string
	Channel         string
	TTLSeconds      uint32
	DelegationDepth uint8
}

// IssueToken issues a permission token to `req.Subject` for `req.Channel`.
// Returns the serialized 159-byte token; treat it as opaque bytes
// (persist / ship / hand to peers as-is).
func (id *Identity) IssueToken(req IssueTokenRequest) ([]byte, error) {
	id.mu.RLock()
	defer id.mu.RUnlock()
	if id.handle == nil {
		return nil, ErrShuttingDown
	}
	if len(req.Subject) != 32 {
		return nil, ErrIdentity
	}
	// json.Marshal(nil) produces `"null"`, which the Rust scope
	// parser rejects as "not a list" and reports as a generic
	// ErrIdentity. Short-circuit with a clearer error so callers
	// get a readable signal instead of the catch-all.
	if req.Scope == nil {
		return nil, fmt.Errorf("%w: scope must not be nil", ErrIdentity)
	}
	scopeJSON, err := json.Marshal(req.Scope)
	if err != nil {
		return nil, fmt.Errorf("scope marshal: %w", err)
	}
	cScope := C.CString(string(scopeJSON))
	defer C.free(unsafe.Pointer(cScope))
	cChannel := C.CString(req.Channel)
	defer C.free(unsafe.Pointer(cChannel))

	var outPtr *C.uint8_t
	var outLen C.size_t
	code := C.net_identity_issue_token(
		id.handle,
		(*C.uint8_t)(unsafe.Pointer(&req.Subject[0])),
		C.size_t(len(req.Subject)),
		cScope,
		cChannel,
		C.uint32_t(req.TTLSeconds),
		C.uint8_t(req.DelegationDepth),
		&outPtr,
		&outLen,
	)
	if err := identityErrorFromCode(code); err != nil {
		return nil, err
	}
	return consumeBytes(outPtr, outLen), nil
}

// InstallToken inserts a token received from another issuer into
// this identity's cache. Signature verification runs on insert;
// malformed / tampered tokens return the relevant `ErrToken*`
// sentinel.
func (id *Identity) InstallToken(token []byte) error {
	id.mu.RLock()
	defer id.mu.RUnlock()
	if id.handle == nil {
		return ErrShuttingDown
	}
	if len(token) == 0 {
		return ErrTokenInvalidFormat
	}
	code := C.net_identity_install_token(
		id.handle,
		(*C.uint8_t)(unsafe.Pointer(&token[0])),
		C.size_t(len(token)),
	)
	return identityErrorFromCode(code)
}

// LookupToken retrieves a cached token by `(subject, channel)`.
// Returns `(nil, nil)` on miss — distinct from an error path.
func (id *Identity) LookupToken(subject []byte, channel string) ([]byte, error) {
	id.mu.RLock()
	defer id.mu.RUnlock()
	if id.handle == nil {
		return nil, ErrShuttingDown
	}
	if len(subject) != 32 {
		return nil, ErrIdentity
	}
	cChannel := C.CString(channel)
	defer C.free(unsafe.Pointer(cChannel))

	var outPtr *C.uint8_t
	var outLen C.size_t
	code := C.net_identity_lookup_token(
		id.handle,
		(*C.uint8_t)(unsafe.Pointer(&subject[0])),
		C.size_t(len(subject)),
		cChannel,
		&outPtr,
		&outLen,
	)
	if err := identityErrorFromCode(code); err != nil {
		return nil, err
	}
	if outPtr == nil || outLen == 0 {
		return nil, nil
	}
	return consumeBytes(outPtr, outLen), nil
}

// TokenCacheLen returns the number of tokens currently cached on
// this identity. Testing aid.
func (id *Identity) TokenCacheLen() uint32 {
	id.mu.RLock()
	defer id.mu.RUnlock()
	if id.handle == nil {
		return 0
	}
	return uint32(C.net_identity_token_cache_len(id.handle))
}

// ---------------------------------------------------------------------------
// Module-level token helpers
// ---------------------------------------------------------------------------

// ParsedToken is the JSON shape returned by `ParseToken`. Hex fields
// are 64 / 128 character strings; scope is lowercase role names.
type ParsedToken struct {
	IssuerHex       string   `json:"issuer_hex"`
	SubjectHex      string   `json:"subject_hex"`
	Scope           []string `json:"scope"`
	ChannelHash     uint32   `json:"channel_hash"`
	NotBefore       uint64   `json:"not_before"`
	NotAfter        uint64   `json:"not_after"`
	DelegationDepth uint8    `json:"delegation_depth"`
	Nonce           uint64   `json:"nonce"`
	SignatureHex    string   `json:"signature_hex"`
}

// ParseToken decodes a serialized `PermissionToken`. Returns
// `ErrTokenInvalidFormat` on bad length / layout. Does NOT verify
// the signature — use `VerifyToken` for that.
func ParseToken(token []byte) (*ParsedToken, error) {
	if len(token) == 0 {
		return nil, ErrTokenInvalidFormat
	}
	var outJSON *C.char
	var outLen C.size_t
	code := C.net_parse_token(
		(*C.uint8_t)(unsafe.Pointer(&token[0])),
		C.size_t(len(token)),
		&outJSON,
		&outLen,
	)
	if err := identityErrorFromCode(code); err != nil {
		return nil, err
	}
	defer C.net_free_string(outJSON)
	raw := C.GoStringN(outJSON, C.int(outLen))
	var parsed ParsedToken
	if err := json.Unmarshal([]byte(raw), &parsed); err != nil {
		return nil, fmt.Errorf("parse token json: %w", err)
	}
	return &parsed, nil
}

// VerifyToken returns `true` when the token's ed25519 signature
// matches the issuer; `false` on tampered / wrong-subject bytes.
// Time-bound validity is a separate check — use `TokenIsExpired`.
func VerifyToken(token []byte) (bool, error) {
	if len(token) == 0 {
		return false, ErrTokenInvalidFormat
	}
	var ok C.int
	code := C.net_verify_token(
		(*C.uint8_t)(unsafe.Pointer(&token[0])),
		C.size_t(len(token)),
		&ok,
	)
	if err := identityErrorFromCode(code); err != nil {
		return false, err
	}
	return ok == 1, nil
}

// TokenIsExpired returns `true` if the token's `not_after` has
// passed (host wall-clock).
func TokenIsExpired(token []byte) (bool, error) {
	if len(token) == 0 {
		return false, ErrTokenInvalidFormat
	}
	var expired C.int
	code := C.net_token_is_expired(
		(*C.uint8_t)(unsafe.Pointer(&token[0])),
		C.size_t(len(token)),
		&expired,
	)
	if err := identityErrorFromCode(code); err != nil {
		return false, err
	}
	return expired == 1, nil
}

// DelegateToken re-issues `parent` to `newSubject` with
// `restrictedScope` intersected against the parent's scope. The
// parent must include `"delegate"` and have
// `delegation_depth > 0`; `signer` must be the subject of the
// parent.
func DelegateToken(
	signer *Identity,
	parent []byte,
	newSubject []byte,
	restrictedScope []string,
) ([]byte, error) {
	if signer == nil {
		return nil, ErrNullPointer
	}
	signer.mu.RLock()
	defer signer.mu.RUnlock()
	if signer.handle == nil {
		return nil, ErrShuttingDown
	}
	if len(parent) == 0 {
		return nil, ErrTokenInvalidFormat
	}
	if len(newSubject) != 32 {
		return nil, ErrIdentity
	}
	// json.Marshal(nil) produces `"null"`, which the Rust scope
	// parser rejects as "not a list" and reports as a generic
	// ErrIdentity. Short-circuit with a clearer error so callers
	// passing `nil` (usually a programming mistake) get a readable
	// signal. A caller who genuinely wants "no scope" passes
	// `[]string{}` — delegate's intersection with an empty set
	// yields an empty-scope child, which is a valid (if useless)
	// token.
	if restrictedScope == nil {
		return nil, fmt.Errorf("%w: restrictedScope must not be nil", ErrIdentity)
	}
	scopeJSON, err := json.Marshal(restrictedScope)
	if err != nil {
		return nil, fmt.Errorf("scope marshal: %w", err)
	}
	cScope := C.CString(string(scopeJSON))
	defer C.free(unsafe.Pointer(cScope))

	var outPtr *C.uint8_t
	var outLen C.size_t
	code := C.net_delegate_token(
		signer.handle,
		(*C.uint8_t)(unsafe.Pointer(&parent[0])),
		C.size_t(len(parent)),
		(*C.uint8_t)(unsafe.Pointer(&newSubject[0])),
		C.size_t(len(newSubject)),
		cScope,
		&outPtr,
		&outLen,
	)
	if err := identityErrorFromCode(code); err != nil {
		return nil, err
	}
	return consumeBytes(outPtr, outLen), nil
}

// ChannelHash hashes a channel name to its canonical 32-bit substrate
// identifier (used for ACL/storage/config keys; the wire NetHeader
// fast-path hint is the low 16 bits of this value).
func ChannelHash(channel string) (uint32, error) {
	cChannel := C.CString(channel)
	defer C.free(unsafe.Pointer(cChannel))
	var hash C.uint32_t
	code := C.net_channel_hash(cChannel, &hash)
	if err := identityErrorFromCode(code); err != nil {
		return 0, err
	}
	return uint32(hash), nil
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

// consumeBytes copies a Rust-allocated byte buffer into an owned Go
// `[]byte`, then releases the Rust allocation. The out pointer must
// not be NULL and the out length must be >0 — callers check those
// preconditions before calling.
func consumeBytes(ptr *C.uint8_t, length C.size_t) []byte {
	if ptr == nil || length == 0 {
		return nil
	}
	// GoBytes copies the buffer into Go memory; we can free the Rust
	// allocation immediately after.
	out := C.GoBytes(unsafe.Pointer(ptr), C.int(length))
	C.net_free_bytes(ptr, length)
	return out
}
