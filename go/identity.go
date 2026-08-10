// Package net — identity + permission-token surface.
//
// Mirrors the Rust SDK's `Identity` / `PermissionToken` one-for-one,
// matching the PyO3 / NAPI shape so cross-binding fixtures round-
// trip. Tokens cross the C boundary as opaque `[]byte` buffers
// (169 bytes each); entity ids as 32-byte slices. The Go side owns
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

	ErrTokenInvalidFormat        = errors.New("token: invalid_format")
	ErrTokenInvalidSignature     = errors.New("token: invalid_signature")
	ErrTokenExpired              = errors.New("token: expired")
	ErrTokenNotYetValid          = errors.New("token: not_yet_valid")
	ErrTokenDelegationExhausted  = errors.New("token: delegation_exhausted")
	ErrTokenDelegationNotAllowed = errors.New("token: delegation_not_allowed")
	ErrTokenNotAuthorized        = errors.New("token: not_authorized")
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
	case -12:
		return ErrInvalidArgument
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

// ---------------------------------------------------------------------------
// Issuer generation + durable issuer state
//
// A token carries the credential epoch of the identity that signed it, and a
// verifier rejects it once its revocation floor for that issuer exceeds the
// epoch. That is only usable if an issuer can restart still knowing which
// epoch it is on, and the 32-byte seed cannot carry one: IdentityFromSeed
// comes back at generation zero, which for an issuer that has already
// published floor N means it can mint nothing a verifier will accept.
//
// Node and Python have had this since it landed; Go had the C declarations
// and no way to reach them.
// ---------------------------------------------------------------------------

// IdentityStateSize is the number of bytes ToState writes, as reported by
// the linked libnet rather than by this package's header.
//
// Size your own storage from this, not from a constant you copied. The two
// agree in a matched build; if they ever disagree, the library is right and
// the header is stale — which is exactly the case a hard-coded 37 would turn
// into a buffer overrun rather than a mismatch you can see.
func IdentityStateSize() int {
	return int(C.net_identity_state_size())
}

// IssuerGeneration is this issuer's current credential epoch.
//
// Every token Issue mints carries it, and a verifier rejects that token once
// its revocation floor for this entity exceeds it.
//
// Returns 0 for a closed identity, which is indistinguishable from a genuine
// generation zero — the same shape as NodeID and OriginHash, and deliberate:
// zero is the epoch that claims the least, so a caller that ignores the
// distinction under-claims rather than over-claims.
func (id *Identity) IssuerGeneration() uint32 {
	id.mu.RLock()
	defer id.mu.RUnlock()
	if id.handle == nil {
		return 0
	}
	return uint32(C.net_identity_generation(id.handle))
}

// AtGeneration returns the same key at a later generation, as a NEW Identity.
//
// The receiver is unchanged and both must be closed separately — rotation is
// explicit at the call site rather than something that happens to a caller
// mid-issuance. The returned identity has its own empty token cache; the C
// ABI hands out owning pointers, so sharing one across two independently
// freeable handles would make one Close observable through the other.
//
// `next` equal to IssuerGeneration is accepted and idempotent at every
// generation including 1<<32 - 1, so re-applying a persisted generation on
// restart is never an error. Going backwards returns ErrIdentity. There is no
// generation above 1<<32 - 1 to name, so an issuer there can re-apply but not
// advance; past that, rotate the identity key.
//
// Rotation order:
//
//  1. build the generation-N identity here;
//  2. persist ToState atomically and durably;
//  3. distribute verifier floor N;
//  4. start issuing from the returned identity.
//
// Never publish floor N before step 2 lands. A crash in between leaves an
// issuer that has announced a floor it has no durable state to satisfy — it
// can mint nothing a verifier accepts, and only a key rotation gets it back.
func (id *Identity) AtGeneration(next uint32) (*Identity, error) {
	id.mu.RLock()
	defer id.mu.RUnlock()
	if id.handle == nil {
		return nil, ErrShuttingDown
	}

	var handle *C.net_identity_t
	code := C.net_identity_at_generation(id.handle, C.uint32_t(next), &handle)
	if err := identityErrorFromCode(code); err != nil {
		// The C ABI collapses every rotation rejection into
		// NET_ERR_IDENTITY, whose sentinel reads "malformed input" — true
		// of a bad seed, unhelpful here. Going backwards is the only way
		// the rotation check fails, and this handle's generation is
		// immutable (rotation mints a new handle), so the real reason can
		// be named without another round trip or a TOCTOU window. Wraps
		// ErrIdentity, so errors.Is keeps working.
		if errors.Is(err, ErrIdentity) {
			current := uint32(C.net_identity_generation(id.handle))
			if next < current {
				return nil, fmt.Errorf(
					"identity: generation %d is below this issuer's current %d; "+
						"rotation only moves forward, though re-applying %d is fine: %w",
					next, current, current, ErrIdentity)
			}
		}
		return nil, err
	}

	rotated := &Identity{handle: handle}
	runtime.SetFinalizer(rotated, (*Identity).free)
	return rotated, nil
}

// ToState serializes the full issuer state — version, seed, generation.
//
// Secret material, exactly like ToSeed: these bytes contain the ed25519
// signing seed. Encrypt at rest and write atomically; a torn write here is an
// issuer that cannot come back.
//
// The buffer is sized from the linked library, so a build that grows the
// state format cannot overflow a buffer this package allocated.
func (id *Identity) ToState() ([]byte, error) {
	id.mu.RLock()
	defer id.mu.RUnlock()
	if id.handle == nil {
		return nil, ErrShuttingDown
	}
	out := make([]byte, IdentityStateSize())
	if len(out) == 0 {
		return nil, fmt.Errorf("identity: libnet reports a zero-byte state size: %w", ErrIdentity)
	}
	code := C.net_identity_to_state(id.handle, (*C.uint8_t)(unsafe.Pointer(&out[0])))
	if err := identityErrorFromCode(code); err != nil {
		return nil, err
	}
	return out, nil
}

// IdentityFromState restores an issuer — key AND generation — from ToState
// output.
//
// This is the restart path for anything that rotates. IdentityFromSeed
// restores the key only and comes back at generation zero, which for a
// rotated issuer means below its own published floor.
//
// Returns ErrIdentity on a wrong length or a version this build does not
// understand, rather than parsing what it can: a partial parse of credential
// state is how an issuer silently comes back on the wrong epoch.
func IdentityFromState(state []byte) (*Identity, error) {
	// Guard before indexing — &state[0] panics on an empty slice, and the
	// C side would have rejected the length anyway.
	if len(state) == 0 {
		return nil, fmt.Errorf("identity: state is empty, want %d bytes: %w",
			IdentityStateSize(), ErrIdentity)
	}

	var handle *C.net_identity_t
	code := C.net_identity_from_state(
		(*C.uint8_t)(unsafe.Pointer(&state[0])),
		C.size_t(len(state)),
		&handle,
	)
	if err := identityErrorFromCode(code); err != nil {
		return nil, err
	}
	id := &Identity{handle: handle}
	runtime.SetFinalizer(id, (*Identity).free)
	return id, nil
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

// VerifySignature reports whether `signature` is a valid detached
// ed25519 signature over `msg` for the 32-byte `entityID`.
//
// The verifying half of Sign. Go exposed signing and no verification
// for an arbitrary message, so a signature produced here could only be
// checked from Rust — and the Go tests asserted the signature's length
// rather than a round trip, which passes for any 64 bytes.
//
// Strict verification: the malleable (R, S+L) variant is rejected, so
// one logical message cannot appear under two byte encodings.
//
// A `false` with a nil error means the signature did not verify. An
// error means an argument was malformed (wrong-length id or
// signature), never that verification failed.
func VerifySignature(entityID, msg, signature []byte) (bool, error) {
	if len(entityID) != 32 {
		return false, fmt.Errorf("entity id must be 32 bytes, got %d", len(entityID))
	}
	if len(signature) != 64 {
		return false, fmt.Errorf("signature must be 64 bytes, got %d", len(signature))
	}

	var msgPtr *C.uint8_t
	if len(msg) > 0 {
		msgPtr = (*C.uint8_t)(unsafe.Pointer(&msg[0]))
	}
	var valid C.int
	code := C.net_verify_signature(
		(*C.uint8_t)(unsafe.Pointer(&entityID[0])),
		C.size_t(len(entityID)),
		msgPtr,
		C.size_t(len(msg)),
		(*C.uint8_t)(unsafe.Pointer(&signature[0])),
		C.size_t(len(signature)),
		&valid,
	)
	runtime.KeepAlive(entityID)
	runtime.KeepAlive(msg)
	runtime.KeepAlive(signature)
	if err := identityErrorFromCode(code); err != nil {
		return false, err
	}
	return valid != 0, nil
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
// Returns the serialized 169-byte token; treat it as opaque bytes
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
	ChannelHash     uint64   `json:"channel_hash"`
	NotBefore       uint64   `json:"not_before"`
	NotAfter        uint64   `json:"not_after"`
	DelegationDepth uint8    `json:"delegation_depth"`

	// IssuerGeneration is the issuer generation this token was minted
	// under. The revocation registry rejects tokens below the issuer's
	// monotonic floor, so this is what explains a refusal that
	// otherwise looks like a valid credential being rejected for no
	// visible reason.
	IssuerGeneration uint32 `json:"issuer_generation"`

	Nonce        uint64 `json:"nonce"`
	SignatureHex string `json:"signature_hex"`
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

// ChannelHash hashes a channel name to its canonical 64-bit substrate
// identifier (used for ACL/storage/config keys; the wire NetHeader
// fast-path hint is the low 16 bits of this value).
func ChannelHash(channel string) (uint64, error) {
	cChannel := C.CString(channel)
	defer C.free(unsafe.Pointer(cChannel))
	var hash C.uint64_t
	code := C.net_channel_hash(cChannel, &hash)
	if err := identityErrorFromCode(code); err != nil {
		return 0, err
	}
	return uint64(hash), nil
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
