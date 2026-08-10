package net

// Issuer generation + durable issuer state through the Go binding.
//
// The Go mirror of `bindings/node/test/identity_generation.test.ts` and
// `src/ffi/identity_state_tests.rs`. It is the same C ABI underneath, so the
// interesting cases are the same ones: the epoch rides on every token, the
// seed cannot carry it, and only ToState/IdentityFromState round-trips it.
//
// Go had the C declarations for months with no wrapper, so this suite is the
// first thing that has ever called them from here — the round-trip assertions
// matter more than usual, because "it compiles" proved nothing about whether
// the arguments were in the right order.
//
// The state encoding is core's, byte-for-byte shared with the Rust, Python,
// Node and C surfaces: a file written by one has to be readable by the rest.
// `TestIdentityStateSizeMatchesTheHeader` is the guard on that shared shape.

import (
	"bytes"
	"encoding/binary"
	"errors"
	"math"
	"testing"
)

const generationChannel = "issuer/rotation"

// issueFrom mints a token so the test can read back the generation the
// signer stamped on it. That stamp is the whole point of the epoch — a
// generation the tokens do not carry is not observable by any verifier.
func issueFrom(t *testing.T, signer, subject *Identity) *ParsedToken {
	t.Helper()
	subjectID, err := subject.EntityID()
	if err != nil {
		t.Fatalf("subject entity id: %v", err)
	}
	raw, err := signer.IssueToken(IssueTokenRequest{
		Subject:    subjectID,
		Scope:      []string{"publish"},
		Channel:    generationChannel,
		TTLSeconds: 3600,
	})
	if err != nil {
		t.Fatalf("issue token: %v", err)
	}
	parsed, err := ParseToken(raw)
	if err != nil {
		t.Fatalf("parse token: %v", err)
	}
	return parsed
}

func mustGenerate(t *testing.T) *Identity {
	t.Helper()
	id, err := GenerateIdentity()
	if err != nil {
		t.Fatalf("generate: %v", err)
	}
	t.Cleanup(id.Close)
	return id
}

func TestIdentityStateSizeMatchesTheHeader(t *testing.T) {
	// The library is the authority and the header is a copy, so this
	// catches the copy going stale — which is how a caller sizing a
	// buffer from the constant would under-allocate.
	if got, want := IdentityStateSize(), 37; got != want {
		t.Fatalf("libnet reports a %d-byte identity state, header says %d; "+
			"the two must agree or a caller sizing from the header overruns", got, want)
	}
}

// The encoding is core's, not Go's. A state file written here has to be
// readable by the Rust, Python, Node and C surfaces, and the only way to
// check that without a second runtime in this process is to assert the bytes
// against the layout all five agree on:
//
//	offset  size  field
//	     0     1  version (currently 1)
//	     1    32  ed25519 seed
//	    33     4  issuer generation, uint32 little-endian
//
// A Go-side encoding drift would round-trip perfectly through
// IdentityFromState and be unreadable everywhere else, which is the failure
// this catches and the Go-to-Go tests cannot.
func TestStateBytesMatchTheCrossBindingLayout(t *testing.T) {
	id := mustGenerate(t)
	rotated, err := id.AtGeneration(0x04030201)
	if err != nil {
		t.Fatalf("rotate: %v", err)
	}
	defer rotated.Close()

	state, err := rotated.ToState()
	if err != nil {
		t.Fatalf("to state: %v", err)
	}
	if len(state) != 37 {
		t.Fatalf("state is %d bytes, the shared layout is 37", len(state))
	}

	if state[0] != 1 {
		t.Errorf("version byte is %d, want 1", state[0])
	}

	seed, err := rotated.ToSeed()
	if err != nil {
		t.Fatalf("to seed: %v", err)
	}
	if !bytes.Equal(state[1:33], seed) {
		t.Error("bytes 1..33 are not the ed25519 seed; the layout drifted")
	}

	// Little-endian, deliberately asymmetric so a byte-order flip cannot
	// pass by coincidence.
	got := binary.LittleEndian.Uint32(state[33:37])
	if got != 0x04030201 {
		t.Errorf("generation decodes to %#x, want %#x — check byte order",
			got, uint32(0x04030201))
	}
	if want := []byte{0x01, 0x02, 0x03, 0x04}; !bytes.Equal(state[33:37], want) {
		t.Errorf("generation bytes are %v, want %v (little-endian)", state[33:37], want)
	}
}

func TestIssuerGenerationStartsAtZeroAndStampsEveryToken(t *testing.T) {
	id := mustGenerate(t)
	subject := mustGenerate(t)

	if got := id.IssuerGeneration(); got != 0 {
		t.Fatalf("a fresh identity should be at generation 0, got %d", got)
	}
	if got := issueFrom(t, id, subject).IssuerGeneration; got != 0 {
		t.Fatalf("token from a generation-0 issuer carries %d", got)
	}
}

func TestAtGenerationReturnsANewIdentityAndLeavesTheOldOne(t *testing.T) {
	id := mustGenerate(t)
	subject := mustGenerate(t)

	rotated, err := id.AtGeneration(3)
	if err != nil {
		t.Fatalf("rotate to 3: %v", err)
	}
	defer rotated.Close()

	if got := rotated.IssuerGeneration(); got != 3 {
		t.Fatalf("rotated identity is at %d, want 3", got)
	}
	// The receiver is untouched. Rotation being explicit at the call site
	// is the point: nothing happens to a caller mid-issuance.
	if got := id.IssuerGeneration(); got != 0 {
		t.Fatalf("the source identity moved to %d; it must stay at 0", got)
	}

	// Same key, so tokens from both verify against one entity id...
	origID, _ := id.EntityID()
	rotatedID, _ := rotated.EntityID()
	if !bytes.Equal(origID, rotatedID) {
		t.Fatal("rotation changed the entity id; it must be the same key")
	}
	// ...but they claim different epochs.
	if got := issueFrom(t, rotated, subject).IssuerGeneration; got != 3 {
		t.Fatalf("token from the rotated issuer carries %d, want 3", got)
	}
	if got := issueFrom(t, id, subject).IssuerGeneration; got != 0 {
		t.Fatalf("token from the source issuer carries %d, want 0", got)
	}
}

func TestStateRoundTripPreservesTheGeneration(t *testing.T) {
	id := mustGenerate(t)
	subject := mustGenerate(t)

	rotated, err := id.AtGeneration(6)
	if err != nil {
		t.Fatalf("rotate to 6: %v", err)
	}
	defer rotated.Close()

	state, err := rotated.ToState()
	if err != nil {
		t.Fatalf("to state: %v", err)
	}
	if len(state) != IdentityStateSize() {
		t.Fatalf("ToState wrote %d bytes, want %d", len(state), IdentityStateSize())
	}

	restored, err := IdentityFromState(state)
	if err != nil {
		t.Fatalf("from state: %v", err)
	}
	defer restored.Close()

	if got := restored.IssuerGeneration(); got != 6 {
		t.Fatalf("restored issuer is at generation %d, want 6", got)
	}
	origID, _ := rotated.EntityID()
	restoredID, _ := restored.EntityID()
	if !bytes.Equal(origID, restoredID) {
		t.Fatal("state round-trip lost the entity id")
	}
	if got := issueFrom(t, restored, subject).IssuerGeneration; got != 6 {
		t.Fatalf("token from the restored issuer carries %d, want 6", got)
	}
}

// The reason ToState exists at all. An issuer that persists only the seed
// comes back at zero, which for one that has already published floor N means
// every token it mints is below its own floor — it can mint nothing a
// verifier accepts, and the failure looks like a verifier bug.
func TestSeedRoundTripLosesTheGenerationAndStateDoesNot(t *testing.T) {
	id := mustGenerate(t)

	rotated, err := id.AtGeneration(4)
	if err != nil {
		t.Fatalf("rotate to 4: %v", err)
	}
	defer rotated.Close()

	seed, err := rotated.ToSeed()
	if err != nil {
		t.Fatalf("to seed: %v", err)
	}
	seedOnly, err := IdentityFromSeed(seed)
	if err != nil {
		t.Fatalf("from seed: %v", err)
	}
	defer seedOnly.Close()

	if got := seedOnly.IssuerGeneration(); got != 0 {
		t.Fatalf("seed-only restore came back at generation %d; the seed "+
			"cannot carry an epoch, so it must be 0 — if this changed, the "+
			"reason for ToState changed with it", got)
	}
}

func TestRotationIsMonotonicAndIdempotent(t *testing.T) {
	id := mustGenerate(t)

	at5, err := id.AtGeneration(5)
	if err != nil {
		t.Fatalf("rotate to 5: %v", err)
	}
	defer at5.Close()

	// Backwards is refused...
	back, err := at5.AtGeneration(4)
	if err == nil {
		back.Close()
		t.Fatal("rotating backwards from 5 to 4 succeeded; it must not")
	}
	if !errors.Is(err, ErrIdentity) {
		t.Fatalf("backwards rotation returned %v, want an ErrIdentity", err)
	}
	// The C ABI collapses every rotation failure into NET_ERR_IDENTITY,
	// whose sentinel reads "malformed input". The wrapper names the real
	// cause; if it stops doing so, a caller is back to guessing.
	for _, want := range []string{"below", "4", "5"} {
		if !contains(err.Error(), want) {
			t.Errorf("the error should say what went wrong and name both "+
				"generations; %q is missing %q", err.Error(), want)
		}
	}

	// ...re-applying the current generation is not a rotation and is fine...
	same, err := at5.AtGeneration(5)
	if err != nil {
		t.Fatalf("re-applying generation 5 failed: %v — restart has to be "+
			"able to re-apply a persisted generation", err)
	}
	defer same.Close()
	if got := same.IssuerGeneration(); got != 5 {
		t.Fatalf("re-apply produced generation %d, want 5", got)
	}

	// ...and forwards works.
	forward, err := at5.AtGeneration(6)
	if err != nil {
		t.Fatalf("rotate 5 -> 6: %v", err)
	}
	defer forward.Close()
	if got := forward.IssuerGeneration(); got != 6 {
		t.Fatalf("forward rotation produced %d, want 6", got)
	}
}

// An issuer at the ceiling is still perfectly usable — it just cannot go
// further. Rejecting a re-apply there would deny the restart path to the one
// issuer that most needs it, since the only generation nameable at the
// ceiling is the ceiling itself.
func TestGenerationCeilingIsUsableIncludingAcrossARestart(t *testing.T) {
	id := mustGenerate(t)
	subject := mustGenerate(t)

	ceiling, err := id.AtGeneration(math.MaxUint32)
	if err != nil {
		t.Fatalf("rotate to the ceiling: %v", err)
	}
	defer ceiling.Close()
	if got := ceiling.IssuerGeneration(); got != math.MaxUint32 {
		t.Fatalf("ceiling identity is at %d, want %d", got, uint32(math.MaxUint32))
	}

	// Usable for issuance...
	if got := issueFrom(t, ceiling, subject).IssuerGeneration; got != math.MaxUint32 {
		t.Fatalf("token from the ceiling issuer carries %d", got)
	}

	// ...and for the restart path.
	state, err := ceiling.ToState()
	if err != nil {
		t.Fatalf("to state at the ceiling: %v", err)
	}
	restored, err := IdentityFromState(state)
	if err != nil {
		t.Fatalf("an issuer at the ceiling could not restore its own state: %v", err)
	}
	defer restored.Close()
	if got := restored.IssuerGeneration(); got != math.MaxUint32 {
		t.Fatalf("restored ceiling issuer is at %d", got)
	}
	reapplied, err := restored.AtGeneration(math.MaxUint32)
	if err != nil {
		t.Fatalf("re-applying the ceiling failed: %v", err)
	}
	defer reapplied.Close()

	// Backwards is still backwards here.
	back, err := ceiling.AtGeneration(math.MaxUint32 - 1)
	if err == nil {
		back.Close()
		t.Fatal("rotating backwards from the ceiling succeeded; it must not")
	}
}

func TestFromStateRefusesMalformedRatherThanParsingPartOfIt(t *testing.T) {
	id := mustGenerate(t)
	rotated, err := id.AtGeneration(2)
	if err != nil {
		t.Fatalf("rotate to 2: %v", err)
	}
	defer rotated.Close()

	good, err := rotated.ToState()
	if err != nil {
		t.Fatalf("to state: %v", err)
	}

	cases := map[string][]byte{
		"empty":          {},
		"nil":            nil,
		"one byte short": good[:len(good)-1],
		"one byte long":  append(append([]byte{}, good...), 0),
		"seed only":      good[1:33],
	}
	for name, state := range cases {
		t.Run(name, func(t *testing.T) {
			restored, err := IdentityFromState(state)
			if err == nil {
				restored.Close()
				t.Fatal("accepted; a partial parse of credential state is how " +
					"an issuer silently comes back on the wrong epoch")
			}
			if !errors.Is(err, ErrIdentity) {
				t.Fatalf("returned %v, want an ErrIdentity", err)
			}
		})
	}

	// An unrecognized version is refused too, rather than being read as
	// whatever this build happens to understand.
	future := append([]byte{}, good...)
	future[0] = 0xFF
	restored, err := IdentityFromState(future)
	if err == nil {
		restored.Close()
		t.Fatal("accepted a state with an unknown version byte")
	}
	if !errors.Is(err, ErrIdentity) {
		t.Fatalf("future version returned %v, want an ErrIdentity", err)
	}
}

// A closed identity must answer rather than crash. `IssuerGeneration`
// deliberately returns 0 — the epoch that claims the least — matching NodeID
// and OriginHash, while the fallible calls report ErrShuttingDown.
func TestGenerationSurfaceOnAClosedIdentity(t *testing.T) {
	id, err := GenerateIdentity()
	if err != nil {
		t.Fatalf("generate: %v", err)
	}
	rotated, err := id.AtGeneration(9)
	if err != nil {
		t.Fatalf("rotate: %v", err)
	}
	rotated.Close()

	if got := rotated.IssuerGeneration(); got != 0 {
		t.Errorf("closed identity reported generation %d, want 0", got)
	}
	if _, err := rotated.ToState(); !errors.Is(err, ErrShuttingDown) {
		t.Errorf("ToState on a closed identity returned %v, want ErrShuttingDown", err)
	}
	if _, err := rotated.AtGeneration(10); !errors.Is(err, ErrShuttingDown) {
		t.Errorf("AtGeneration on a closed identity returned %v, want ErrShuttingDown", err)
	}
	id.Close()
	// Close is idempotent, as everywhere else in this package.
	rotated.Close()
}

func contains(haystack, needle string) bool {
	return bytes.Contains([]byte(haystack), []byte(needle))
}
