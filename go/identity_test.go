// Tests for the Identity + PermissionToken surface (Stage G-1).
//
// Direct port of `bindings/python/tests/test_identity.py`. Same 22
// assertions, same helper layout — when a behaviour differs between
// Go and Python, this file is the canonical regression site for the
// Go binding.

package net

import (
	"bytes"
	"errors"
	"strings"
	"testing"
	"time"
)

// ---------------------------------------------------------------------------
// generate / seed round-trip
// ---------------------------------------------------------------------------

func TestGenerate_ProducesValidEntityID(t *testing.T) {
	id, err := GenerateIdentity()
	if err != nil {
		t.Fatalf("generate: %v", err)
	}
	defer id.Close()

	eid, err := id.EntityID()
	if err != nil {
		t.Fatalf("entity id: %v", err)
	}
	if len(eid) != 32 {
		t.Fatalf("want 32-byte entity id, got %d", len(eid))
	}
}

func TestGenerate_YieldsUniqueIdentities(t *testing.T) {
	a, err := GenerateIdentity()
	if err != nil {
		t.Fatalf("generate a: %v", err)
	}
	defer a.Close()
	b, err := GenerateIdentity()
	if err != nil {
		t.Fatalf("generate b: %v", err)
	}
	defer b.Close()

	aID, _ := a.EntityID()
	bID, _ := b.EntityID()
	if bytes.Equal(aID, bID) {
		t.Fatal("two generated identities shared an entity id")
	}
}

func TestSeedRoundTrip(t *testing.T) {
	original, err := GenerateIdentity()
	if err != nil {
		t.Fatalf("generate: %v", err)
	}
	defer original.Close()

	seed, err := original.ToSeed()
	if err != nil {
		t.Fatalf("to seed: %v", err)
	}
	if len(seed) != 32 {
		t.Fatalf("want 32-byte seed, got %d", len(seed))
	}

	restored, err := IdentityFromSeed(seed)
	if err != nil {
		t.Fatalf("from seed: %v", err)
	}
	defer restored.Close()

	origID, _ := original.EntityID()
	restoredID, _ := restored.EntityID()
	if !bytes.Equal(origID, restoredID) {
		t.Fatal("seed round-trip lost entity id")
	}
	if original.NodeID() != restored.NodeID() {
		t.Fatal("seed round-trip lost node id")
	}
	if original.OriginHash() != restored.OriginHash() {
		t.Fatal("seed round-trip lost origin hash")
	}
}

func TestFromSeed_RejectsWrongLength(t *testing.T) {
	for _, sz := range []int{0, 16, 31, 33, 64} {
		seed := make([]byte, sz)
		if _, err := IdentityFromSeed(seed); !errors.Is(err, ErrIdentity) {
			// 0-length hits Go's own length check, which also returns
			// ErrIdentity — either path is fine.
			t.Fatalf("seed len=%d: want ErrIdentity, got %v", sz, err)
		}
	}
}

func TestSign_Returns64Bytes(t *testing.T) {
	id, err := GenerateIdentity()
	if err != nil {
		t.Fatalf("generate: %v", err)
	}
	defer id.Close()

	sig, err := id.Sign([]byte("hello world"))
	if err != nil {
		t.Fatalf("sign: %v", err)
	}
	if len(sig) != 64 {
		t.Fatalf("want 64-byte signature, got %d", len(sig))
	}
}

func TestSign_EmptyMessage(t *testing.T) {
	id, err := GenerateIdentity()
	if err != nil {
		t.Fatalf("generate: %v", err)
	}
	defer id.Close()
	sig, err := id.Sign(nil)
	if err != nil {
		t.Fatalf("sign empty: %v", err)
	}
	if len(sig) != 64 {
		t.Fatalf("want 64-byte signature, got %d", len(sig))
	}
}

// Sign then verify, in one round trip, through the Go binding.
//
// The two tests above assert the signature's LENGTH, which passes for
// any 64 bytes — including 64 zeros. Nothing here checked that a
// signature produced through this binding actually verifies, because
// until VerifySignature there was no way to check it outside Rust.
func TestVerifySignature_RoundTripAndRejections(t *testing.T) {
	id, err := GenerateIdentity()
	if err != nil {
		t.Fatalf("generate: %v", err)
	}
	defer id.Close()

	entityID, err := id.EntityID()
	if err != nil {
		t.Fatalf("entity id: %v", err)
	}
	msg := []byte("the exact bytes that were signed")
	sig, err := id.Sign(msg)
	if err != nil {
		t.Fatalf("sign: %v", err)
	}

	ok, err := VerifySignature(entityID, msg, sig)
	if err != nil {
		t.Fatalf("verify: %v", err)
	}
	if !ok {
		t.Fatal("a freshly produced signature must verify")
	}

	// Wrong message.
	if ok, err := VerifySignature(entityID, []byte("different bytes"), sig); err != nil || ok {
		t.Fatalf("a signature must not verify against another message (ok=%v err=%v)", ok, err)
	}

	// Wrong key.
	other, err := GenerateIdentity()
	if err != nil {
		t.Fatalf("generate other: %v", err)
	}
	defer other.Close()
	otherID, err := other.EntityID()
	if err != nil {
		t.Fatalf("entity id: %v", err)
	}
	if ok, err := VerifySignature(otherID, msg, sig); err != nil || ok {
		t.Fatalf("a signature must not verify under another entity (ok=%v err=%v)", ok, err)
	}

	// The all-zero signature a length check accepts.
	if ok, err := VerifySignature(entityID, msg, make([]byte, 64)); err != nil || ok {
		t.Fatalf("64 zero bytes must not verify (ok=%v err=%v)", ok, err)
	}

	// Tampered.
	tampered := append([]byte(nil), sig...)
	tampered[0] ^= 0xff
	if ok, err := VerifySignature(entityID, msg, tampered); err != nil || ok {
		t.Fatalf("a tampered signature must not verify (ok=%v err=%v)", ok, err)
	}
}

// An empty message is a legitimate thing to sign, and a nil slice must
// not be confused with a missing argument.
func TestVerifySignature_EmptyMessage(t *testing.T) {
	id, err := GenerateIdentity()
	if err != nil {
		t.Fatalf("generate: %v", err)
	}
	defer id.Close()

	entityID, err := id.EntityID()
	if err != nil {
		t.Fatalf("entity id: %v", err)
	}
	sig, err := id.Sign(nil)
	if err != nil {
		t.Fatalf("sign: %v", err)
	}
	ok, err := VerifySignature(entityID, nil, sig)
	if err != nil {
		t.Fatalf("verify empty: %v", err)
	}
	if !ok {
		t.Fatal("an empty message must verify against its own signature")
	}
}

// A malformed argument is an error, never a `false` verdict. A caller
// that cannot tell them apart treats its own bug as a failed check.
func TestVerifySignature_MalformedArgumentsError(t *testing.T) {
	id, err := GenerateIdentity()
	if err != nil {
		t.Fatalf("generate: %v", err)
	}
	defer id.Close()

	entityID, err := id.EntityID()
	if err != nil {
		t.Fatalf("entity id: %v", err)
	}
	msg := []byte("payload")
	sig, err := id.Sign(msg)
	if err != nil {
		t.Fatalf("sign: %v", err)
	}

	for _, n := range []int{0, 31, 33} {
		if _, err := VerifySignature(make([]byte, n), msg, sig); err == nil {
			t.Fatalf("a %d-byte entity id must be an error, not a false verdict", n)
		}
	}
	for _, n := range []int{0, 63, 65} {
		if _, err := VerifySignature(entityID, msg, make([]byte, n)); err == nil {
			t.Fatalf("a %d-byte signature must be an error, not a false verdict", n)
		}
	}
}

// ---------------------------------------------------------------------------
// issue / parse / verify
// ---------------------------------------------------------------------------

func issueTestToken(t *testing.T, issuer *Identity, subjectID []byte, scope []string, channel string, ttl uint32, depth uint8) []byte {
	t.Helper()
	token, err := issuer.IssueToken(IssueTokenRequest{
		Subject:         subjectID,
		Scope:           scope,
		Channel:         channel,
		TTLSeconds:      ttl,
		DelegationDepth: depth,
	})
	if err != nil {
		t.Fatalf("issue token: %v", err)
	}
	if len(token) == 0 {
		t.Fatal("issue token produced empty bytes")
	}
	return token
}

func TestIssueParseRoundtrip_MatchesFields(t *testing.T) {
	issuer, _ := GenerateIdentity()
	defer issuer.Close()
	subject, _ := GenerateIdentity()
	defer subject.Close()

	subID, _ := subject.EntityID()
	token := issueTestToken(t, issuer, subID, []string{"publish", "subscribe"}, "sensors/temp", 3600, 0)

	parsed, err := ParseToken(token)
	if err != nil {
		t.Fatalf("parse token: %v", err)
	}

	issuerID, _ := issuer.EntityID()
	if parsed.IssuerHex != toHex(issuerID) {
		t.Fatalf("issuer mismatch")
	}
	if parsed.SubjectHex != toHex(subID) {
		t.Fatalf("subject mismatch")
	}
	if !hasAll(parsed.Scope, "publish", "subscribe") {
		t.Fatalf("scope mismatch: %v", parsed.Scope)
	}
	wantHash, _ := ChannelHash("sensors/temp")
	if parsed.ChannelHash != wantHash {
		t.Fatalf("channel hash mismatch: %d vs %d", parsed.ChannelHash, wantHash)
	}
	if parsed.DelegationDepth != 0 {
		t.Fatalf("delegation depth mismatch")
	}
	if parsed.NotAfter <= parsed.NotBefore {
		t.Fatalf("not_after not greater than not_before")
	}
	if len(parsed.SignatureHex) != 128 {
		t.Fatalf("signature hex should be 128 chars, got %d", len(parsed.SignatureHex))
	}
}

func TestVerifyToken_AcceptsValid(t *testing.T) {
	issuer, _ := GenerateIdentity()
	defer issuer.Close()
	subject, _ := GenerateIdentity()
	defer subject.Close()

	subID, _ := subject.EntityID()
	token := issueTestToken(t, issuer, subID, []string{"publish"}, "topic", 60, 0)

	ok, err := VerifyToken(token)
	if err != nil {
		t.Fatalf("verify: %v", err)
	}
	if !ok {
		t.Fatal("valid token failed verification")
	}
}

func TestVerifyToken_RejectsTampered(t *testing.T) {
	issuer, _ := GenerateIdentity()
	defer issuer.Close()
	subject, _ := GenerateIdentity()
	defer subject.Close()

	subID, _ := subject.EntityID()
	token := issueTestToken(t, issuer, subID, []string{"publish"}, "topic", 60, 0)
	// Flip a byte in the signature.
	token[len(token)-1] ^= 0x01

	ok, err := VerifyToken(token)
	if err != nil {
		t.Fatalf("verify: %v", err)
	}
	if ok {
		t.Fatal("tampered token verified as valid")
	}
}

func TestTokenIsExpired_FalseForFresh(t *testing.T) {
	issuer, _ := GenerateIdentity()
	defer issuer.Close()
	subject, _ := GenerateIdentity()
	defer subject.Close()

	subID, _ := subject.EntityID()
	token := issueTestToken(t, issuer, subID, []string{"publish"}, "topic", 3600, 0)

	expired, err := TokenIsExpired(token)
	if err != nil {
		t.Fatalf("is expired: %v", err)
	}
	if expired {
		t.Fatal("fresh token reported as expired")
	}
}

// Regression for a cubic-flagged bug: the previous impl walked
// `PermissionToken::is_valid()` and matched on Err(Expired), which
// short-circuits on signature check. A tampered + expired token
// therefore reported NOT expired. TokenIsExpired is documented as
// a pure time check; this test locks that in.
func TestTokenIsExpired_ReportsExpiredEvenWhenSignatureTampered(t *testing.T) {
	issuer, _ := GenerateIdentity()
	defer issuer.Close()
	subject, _ := GenerateIdentity()
	defer subject.Close()

	subID, _ := subject.EntityID()
	// 1-second TTL. Sleep 2.5s below — must cover a full seconds-
	// truncation boundary since `not_after` is seconds, so 1.3s
	// could leave `current == not_after` on certain issue-time
	// phases (flake).
	raw := issueTestToken(t, issuer, subID, []string{"publish"}, "topic", 1, 0)
	token := make([]byte, len(raw))
	copy(token, raw)
	// Tamper the signature byte.
	token[len(token)-1] ^= 0xFF

	// Wait past TTL with margin.
	time.Sleep(2500 * time.Millisecond)

	expired, err := TokenIsExpired(token)
	if err != nil {
		t.Fatalf("is expired: %v", err)
	}
	if !expired {
		t.Fatal(
			"TokenIsExpired must be a pure time check; " +
				"short-circuiting on signature validity is the " +
				"cubic-flagged bug",
		)
	}
}

func TestIssueToken_RejectsUnknownScope(t *testing.T) {
	issuer, _ := GenerateIdentity()
	defer issuer.Close()
	subject, _ := GenerateIdentity()
	defer subject.Close()

	subID, _ := subject.EntityID()
	_, err := issuer.IssueToken(IssueTokenRequest{
		Subject:    subID,
		Scope:      []string{"bogus"},
		Channel:    "topic",
		TTLSeconds: 60,
	})
	if !errors.Is(err, ErrIdentity) {
		t.Fatalf("want ErrIdentity, got %v", err)
	}
}

func TestIssueToken_RejectsShortSubject(t *testing.T) {
	issuer, _ := GenerateIdentity()
	defer issuer.Close()

	_, err := issuer.IssueToken(IssueTokenRequest{
		Subject:    make([]byte, 16),
		Scope:      []string{"publish"},
		Channel:    "topic",
		TTLSeconds: 60,
	})
	if !errors.Is(err, ErrIdentity) {
		t.Fatalf("want ErrIdentity, got %v", err)
	}
}

func TestParseToken_RejectsBadFormat(t *testing.T) {
	_, err := ParseToken(make([]byte, 16))
	if !errors.Is(err, ErrTokenInvalidFormat) {
		t.Fatalf("want ErrTokenInvalidFormat, got %v", err)
	}
}

// ---------------------------------------------------------------------------
// token cache install / lookup
// ---------------------------------------------------------------------------

func TestInstallAndLookupToken(t *testing.T) {
	issuer, _ := GenerateIdentity()
	defer issuer.Close()
	subject, _ := GenerateIdentity()
	defer subject.Close()

	subID, _ := subject.EntityID()
	token := issueTestToken(t, issuer, subID, []string{"subscribe"}, "sensors/temp", 600, 0)

	holder, _ := GenerateIdentity()
	defer holder.Close()
	if holder.TokenCacheLen() != 0 {
		t.Fatalf("fresh identity should have empty cache, got %d", holder.TokenCacheLen())
	}
	if err := holder.InstallToken(token); err != nil {
		t.Fatalf("install: %v", err)
	}
	if holder.TokenCacheLen() != 1 {
		t.Fatalf("want cache len 1, got %d", holder.TokenCacheLen())
	}

	found, err := holder.LookupToken(subID, "sensors/temp")
	if err != nil {
		t.Fatalf("lookup: %v", err)
	}
	if !bytes.Equal(found, token) {
		t.Fatal("looked-up token differs from installed token")
	}
}

func TestLookupTokenMiss_ReturnsNil(t *testing.T) {
	holder, _ := GenerateIdentity()
	defer holder.Close()
	other, _ := GenerateIdentity()
	defer other.Close()

	otherID, _ := other.EntityID()
	found, err := holder.LookupToken(otherID, "not/there")
	if err != nil {
		t.Fatalf("lookup: %v", err)
	}
	if found != nil {
		t.Fatalf("miss should return nil, got %d bytes", len(found))
	}
}

func TestInstallToken_RejectsTampered(t *testing.T) {
	issuer, _ := GenerateIdentity()
	defer issuer.Close()
	subject, _ := GenerateIdentity()
	defer subject.Close()

	subID, _ := subject.EntityID()
	raw := issueTestToken(t, issuer, subID, []string{"subscribe"}, "c", 60, 0)
	raw[len(raw)-1] ^= 0x02

	holder, _ := GenerateIdentity()
	defer holder.Close()
	err := holder.InstallToken(raw)
	if !errors.Is(err, ErrTokenInvalidSignature) {
		t.Fatalf("want ErrTokenInvalidSignature, got %v", err)
	}
}

// ---------------------------------------------------------------------------
// delegation
// ---------------------------------------------------------------------------

func TestDelegationChain_ExhaustsAtDepthZero(t *testing.T) {
	a, _ := GenerateIdentity()
	defer a.Close()
	b, _ := GenerateIdentity()
	defer b.Close()
	c, _ := GenerateIdentity()
	defer c.Close()
	d, _ := GenerateIdentity()
	defer d.Close()
	e, _ := GenerateIdentity()
	defer e.Close()

	bID, _ := b.EntityID()
	cID, _ := c.EntityID()
	dID, _ := d.EntityID()
	eID, _ := e.EntityID()

	tokenAB := issueTestToken(t, a, bID, []string{"publish", "delegate"}, "chain", 3600, 2)

	tokenBC, err := DelegateToken(b, tokenAB, cID, []string{"publish", "delegate"})
	if err != nil {
		t.Fatalf("B delegate to C: %v", err)
	}
	parsedBC, _ := ParseToken(tokenBC)
	if parsedBC.DelegationDepth != 1 {
		t.Fatalf("BC depth: want 1, got %d", parsedBC.DelegationDepth)
	}
	if parsedBC.SubjectHex != toHex(cID) {
		t.Fatalf("BC subject mismatch")
	}

	tokenCD, err := DelegateToken(c, tokenBC, dID, []string{"publish"})
	if err != nil {
		t.Fatalf("C delegate to D: %v", err)
	}
	parsedCD, _ := ParseToken(tokenCD)
	if parsedCD.DelegationDepth != 0 {
		t.Fatalf("CD depth: want 0, got %d", parsedCD.DelegationDepth)
	}

	_, err = DelegateToken(d, tokenCD, eID, []string{"publish"})
	if !errors.Is(err, ErrTokenDelegationExhausted) {
		t.Fatalf("want ErrTokenDelegationExhausted, got %v", err)
	}
}

func TestDelegation_RequiresDelegateScopeOnParent(t *testing.T) {
	a, _ := GenerateIdentity()
	defer a.Close()
	b, _ := GenerateIdentity()
	defer b.Close()
	c, _ := GenerateIdentity()
	defer c.Close()

	bID, _ := b.EntityID()
	cID, _ := c.EntityID()

	tokenAB := issueTestToken(t, a, bID, []string{"publish"}, "topic", 3600, 2)
	_, err := DelegateToken(b, tokenAB, cID, []string{"publish"})
	if !errors.Is(err, ErrTokenDelegationNotAllowed) {
		t.Fatalf("want ErrTokenDelegationNotAllowed, got %v", err)
	}
}

func TestDelegation_RejectsUnauthorizedSigner(t *testing.T) {
	a, _ := GenerateIdentity()
	defer a.Close()
	b, _ := GenerateIdentity()
	defer b.Close()
	c, _ := GenerateIdentity()
	defer c.Close()
	stranger, _ := GenerateIdentity()
	defer stranger.Close()

	bID, _ := b.EntityID()
	cID, _ := c.EntityID()

	tokenAB := issueTestToken(t, a, bID, []string{"publish", "delegate"}, "topic", 3600, 2)
	_, err := DelegateToken(stranger, tokenAB, cID, []string{"publish"})
	if !errors.Is(err, ErrTokenNotAuthorized) {
		t.Fatalf("want ErrTokenNotAuthorized, got %v", err)
	}
}

// Regression for a cubic-flagged P2: `json.Marshal(nil)` produces
// `"null"`, which the Rust scope parser rejects as "not a list" and
// maps to the generic `ErrIdentity`. Callers passing `nil` (usually
// a programming mistake — meant to pass `[]string{}`) got an opaque
// signal. The Go wrapper now short-circuits with a clear error that
// wraps `ErrIdentity` and mentions the nil.
func TestDelegateToken_RejectsNilScope(t *testing.T) {
	a, _ := GenerateIdentity()
	defer a.Close()
	b, _ := GenerateIdentity()
	defer b.Close()
	c, _ := GenerateIdentity()
	defer c.Close()

	bID, _ := b.EntityID()
	cID, _ := c.EntityID()
	tokenAB := issueTestToken(t, a, bID, []string{"publish", "delegate"}, "topic", 3600, 2)

	_, err := DelegateToken(b, tokenAB, cID, nil)
	if !errors.Is(err, ErrIdentity) {
		t.Fatalf("want err wrapping ErrIdentity, got %v", err)
	}
	// Message should mention the specific cause so callers don't
	// have to dig — confirms we're not just returning bare
	// `ErrIdentity`.
	if !strings.Contains(err.Error(), "nil") {
		t.Fatalf("err should mention nil scope, got %q", err.Error())
	}
}

func TestIssueToken_RejectsNilScope(t *testing.T) {
	issuer, _ := GenerateIdentity()
	defer issuer.Close()
	subject, _ := GenerateIdentity()
	defer subject.Close()

	subID, _ := subject.EntityID()
	_, err := issuer.IssueToken(IssueTokenRequest{
		Subject:    subID,
		Scope:      nil,
		Channel:    "topic",
		TTLSeconds: 60,
	})
	if !errors.Is(err, ErrIdentity) {
		t.Fatalf("want err wrapping ErrIdentity, got %v", err)
	}
	if !strings.Contains(err.Error(), "nil") {
		t.Fatalf("err should mention nil scope, got %q", err.Error())
	}
}

// ---------------------------------------------------------------------------
// miscellany
// ---------------------------------------------------------------------------

func TestChannelHash_IsStable(t *testing.T) {
	h1, err := ChannelHash("sensors/temp")
	if err != nil {
		t.Fatalf("hash: %v", err)
	}
	h2, _ := ChannelHash("sensors/temp")
	if h1 != h2 {
		t.Fatal("channel hash not stable across calls")
	}
}

func TestChannelHash_DiffersAcrossNames(t *testing.T) {
	a, _ := ChannelHash("a")
	b, _ := ChannelHash("b")
	if a == b {
		t.Fatal("expected different hashes for different names")
	}
}

// ---------------------------------------------------------------------------
// Local helpers (kept package-local — testing-only, no exports).
// ---------------------------------------------------------------------------

const hexChars = "0123456789abcdef"

func toHex(b []byte) string {
	out := make([]byte, len(b)*2)
	for i, v := range b {
		out[2*i] = hexChars[v>>4]
		out[2*i+1] = hexChars[v&0x0f]
	}
	return string(out)
}

func hasAll(values []string, required ...string) bool {
	set := make(map[string]struct{}, len(values))
	for _, v := range values {
		set[v] = struct{}{}
	}
	for _, r := range required {
		if _, ok := set[r]; !ok {
			return false
		}
	}
	return true
}
