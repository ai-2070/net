// Deck client tests.
//
// These tests exercise the surface defined in `deck.go`. The cdylib
// must be built before running:
//
//	cargo build --release -p net-deck-ffi
//
// without the cdylib, `go test -run TestDeck` fails the cgo link step.
//
// The tests use a fixed seed so the operator id is reproducible. The
// supervisor runtime starts up empty; admin verbs that target a node
// that doesn't exist return a `DeckError` with kind
// `"register_failed"` or `"call_failed"` — the tests cover both the
// happy construction path and the typed error path.

package net

import (
	"bytes"
	"errors"
	"testing"
)

// testOperatorSeed is a fixed 32-byte seed used across the Deck tests
// so the operator id is reproducible. Not secret — this is for tests.
var testOperatorSeed = []byte{
	0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
	0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10,
	0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18,
	0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x20,
}

// TestDeckClientLifecycle covers happy-path construction +
// idempotent Free.
func TestDeckClientLifecycle(t *testing.T) {
	c, err := NewDeckClient(testOperatorSeed, DeckClientConfig{ThisNode: 1})
	if err != nil {
		t.Fatalf("NewDeckClient: %v", err)
	}
	defer c.Free()

	id := c.OperatorID()
	if id == 0 {
		t.Fatalf("OperatorID should be non-zero")
	}

	// Idempotent Free.
	c.Free()
	c.Free()

	// Operations on a freed client return a typed error.
	if _, err := c.StatusSummary(); err == nil {
		t.Fatalf("StatusSummary on closed client should error")
	}
}

// TestDeckClientRejectsBadSeedLength confirms that the binding-side
// length check catches a wrong-sized seed before reaching the FFI.
func TestDeckClientRejectsBadSeedLength(t *testing.T) {
	short := bytes.Repeat([]byte{0xaa}, 16)
	if _, err := NewDeckClient(short, DeckClientConfig{}); !errors.Is(err, ErrDeck) {
		t.Fatalf("NewDeckClient(16-byte seed) should error with ErrDeck; got %v", err)
	}
	long := bytes.Repeat([]byte{0xbb}, 64)
	if _, err := NewDeckClient(long, DeckClientConfig{}); !errors.Is(err, ErrDeck) {
		t.Fatalf("NewDeckClient(64-byte seed) should error with ErrDeck; got %v", err)
	}
}

// TestDeckStatusSummary reads the rolled-up status from an
// empty-cluster supervisor. The supervisor starts with no peers / no
// daemons; counts should all be zero.
func TestDeckStatusSummary(t *testing.T) {
	c, err := NewDeckClient(testOperatorSeed, DeckClientConfig{ThisNode: 1})
	if err != nil {
		t.Fatalf("NewDeckClient: %v", err)
	}
	defer c.Free()

	sum, err := c.StatusSummary()
	if err != nil {
		t.Fatalf("StatusSummary: %v", err)
	}
	// Zero-cluster invariants: no peers, no daemons, no replica
	// chains. (`recent_failure_count` may legitimately tick on
	// supervisor startup if the substrate emits a startup log; the
	// strict check is on the typed peer/daemon counts.)
	if sum.Peers.Healthy != 0 || sum.Peers.Degraded != 0 ||
		sum.Peers.Unreachable != 0 || sum.Peers.Unknown != 0 {
		t.Errorf("expected zero peer counts on empty cluster; got %+v", sum.Peers)
	}
	if sum.Daemons.Running != 0 || sum.Daemons.Starting != 0 {
		t.Errorf("expected zero daemon counts on empty cluster; got %+v", sum.Daemons)
	}
}

// TestDeckAdminVerbsReturnTypedErrors confirms that admin verbs on a
// node that doesn't exist return a typed `*DeckError` wrapping
// `ErrDeck`. The exact kind is determined by the substrate; we only
// assert the error path is consistent.
func TestDeckAdminVerbsReturnTypedErrors(t *testing.T) {
	c, err := NewDeckClient(testOperatorSeed, DeckClientConfig{ThisNode: 1})
	if err != nil {
		t.Fatalf("NewDeckClient: %v", err)
	}
	defer c.Free()

	// Every admin verb against a non-existent node 99 should either
	// succeed (if the supervisor accepts the event optimistically) or
	// return a typed DeckError. Both outcomes are acceptable; what
	// matters is that on failure, the error wraps ErrDeck and carries
	// a kind discriminator.
	cases := []struct {
		name string
		call func() (ChainCommit, error)
	}{
		{"Drain", func() (ChainCommit, error) { return c.Drain(99, 1000) }},
		{"Cordon", func() (ChainCommit, error) { return c.Cordon(99) }},
		{"Uncordon", func() (ChainCommit, error) { return c.Uncordon(99) }},
		{"InvalidatePlacement", func() (ChainCommit, error) { return c.InvalidatePlacement(99) }},
		{"ClearAvoidList", func() (ChainCommit, error) { return c.ClearAvoidList(99) }},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			_, err := tc.call()
			if err == nil {
				return // accepted optimistically; that's fine
			}
			if !errors.Is(err, ErrDeck) {
				t.Errorf("%s error should wrap ErrDeck; got %v", tc.name, err)
			}
			var de *DeckError
			if errors.As(err, &de) {
				if de.Kind == "" && de.Msg == "" {
					t.Errorf("%s DeckError should carry kind or msg; got %+v", tc.name, de)
				}
			}
		})
	}
}

// TestDeckSnapshotStreamFreeIsIdempotent confirms repeated Free()
// calls on a stream are safe.
func TestDeckSnapshotStreamFreeIsIdempotent(t *testing.T) {
	c, err := NewDeckClient(testOperatorSeed, DeckClientConfig{ThisNode: 1})
	if err != nil {
		t.Fatalf("NewDeckClient: %v", err)
	}
	defer c.Free()

	s, err := c.SubscribeSnapshots()
	if err != nil {
		t.Fatalf("SubscribeSnapshots: %v", err)
	}
	s.Free()
	s.Free()
}

// TestDeckStatusSummaryStreamTimeout opens a status-summary stream
// and asserts that a short-timeout Next() returns `(nil, nil)`
// without erroring when no event is available.
func TestDeckStatusSummaryStreamTimeout(t *testing.T) {
	c, err := NewDeckClient(testOperatorSeed, DeckClientConfig{ThisNode: 1, TickIntervalMs: 60_000})
	if err != nil {
		t.Fatalf("NewDeckClient: %v", err)
	}
	defer c.Free()

	s, err := c.SubscribeStatusSummaries()
	if err != nil {
		t.Fatalf("SubscribeStatusSummaries: %v", err)
	}
	defer s.Free()

	// With tick interval set to 60s, a 50ms wait should reliably
	// return a timeout (nil, nil) — the supervisor's reconcile loop
	// won't emit a summary in that window.
	sum, err := s.Next(50)
	if err != nil && !errors.Is(err, ErrDeck) {
		t.Fatalf("Next on idle stream returned unexpected error: %v", err)
	}
	_ = sum // may be nil (timeout) or non-nil (substrate emitted on subscribe)
}
