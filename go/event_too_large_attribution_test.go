// Size/limit parity for `EventTooLargeError`: the two numbers Go
// reports are the ones the core attributed, not numbers Go worked
// out for itself.
//
// Both cases below were wrong before the repair, and wrong in
// different ways, because `sendErrorFromCode` reconstructed the pair
// instead of reading it:
//
//   - the limit came from `MaxEventSize()`, the SINGLE-PACKET
//     constant, so a refusal measured against the 64,832-byte
//     fragmentation ceiling — which is the limit that applies to a
//     peer reached over RTC that advertises fragment reassembly —
//     was reported as 8,104;
//   - the size came from a first-match scan of the caller's own
//     slice for an element above `MaxEventSize()`, so a batch the
//     core refused for its SECOND event was attributed to its
//     first: `[9000, 64833]` came back as `{Size: 9000, Limit:
//     8104}` when the refusal was `{Size: 64833, Limit: 64832}`.
//
// Neither number is recoverable on this side of the boundary. The
// limit is a property of the resolved peer (which Go cannot see) and
// the refused element is the core's choice (which Go cannot infer),
// so both have to travel with the refusal. They now do, through the
// send entry points' `out_size` / `out_limit`.
//
// See `event_too_large_attribution_testhelpers.go` for why these
// drive the attribution seam rather than a live send, and for which
// test covers the live leg.

//go:build test_helpers

package net

import (
	"errors"
	"strconv"
	"strings"
	"testing"
)

// The fragmentation ceiling, recomputed from the wire protocol's
// published constants the same way `wantMaxEventSize` is, rather
// than copied from an error message:
//
//	MAX_FRAGMENTS_PER_GROUP 8   (net/crates/net/wire/src/protocol.rs)
//	MAX_FRAGMENTED_EVENT_SIZE = MAX_EVENT_SIZE * 8 = 64832
const (
	wireMaxFragmentsPerGroup = 8

	wantMaxFragmentedEventSize = wantMaxEventSize * wireMaxFragmentsPerGroup
)

// TestTaggedRefusalReportsTheAppliedCeilingNotTheSinglePacketLimit is
// Kyra's first case: a 64,833-byte send to a peer that reassembles
// fragments is refused at 64,832, and Go must say 64,832.
//
// The assertion is written against `MaxEventSize()` as well as
// against the literal, because reporting the single-packet constant
// is the specific defect — a caller that read 8,104 here would
// conclude its 64,833-byte event needed splitting into nine pieces
// when the transport would have carried eight.
func TestTaggedRefusalReportsTheAppliedCeilingNotTheSinglePacketLimit(t *testing.T) {
	refused := make([]byte, wantMaxFragmentedEventSize+1)

	err := eventTooLargeAttribution([][]byte{refused}, 0, wantMaxFragmentedEventSize)
	if err == nil {
		t.Fatal("the attribution seam returned no error for an EventTooLarge refusal")
	}
	if !errors.Is(err, ErrEventTooLarge) {
		t.Fatalf("got %v (%T), want ErrEventTooLarge", err, err)
	}

	var typed *EventTooLargeError
	if !errors.As(err, &typed) {
		t.Fatalf("%v is not an *EventTooLargeError", err)
	}
	if typed.Size != len(refused) {
		t.Fatalf("EventTooLargeError.Size = %d, want %d", typed.Size, len(refused))
	}
	if typed.Limit != wantMaxFragmentedEventSize {
		t.Fatalf(
			"EventTooLargeError.Limit = %d, want %d (the fragmentation "+
				"ceiling that applied); MaxEventSize() is %d and "+
				"reporting it here is the defect — it tells a caller to "+
				"split an event the transport would have carried",
			typed.Limit, wantMaxFragmentedEventSize, MaxEventSize(),
		)
	}
	if typed.Limit == MaxEventSize() {
		t.Fatalf(
			"EventTooLargeError.Limit is the single-packet constant %d; "+
				"the limit must be the one the core applied",
			MaxEventSize(),
		)
	}

	// The message a caller logs carries the applied limit too. A
	// correct pair behind a message naming 8,104 is the same
	// operational lie in a different field.
	if shown := typed.Error(); !strings.Contains(shown, strconv.Itoa(wantMaxFragmentedEventSize)) {
		t.Fatalf("Error() = %q, want it to name the %d-byte limit",
			shown, wantMaxFragmentedEventSize)
	}
}

// TestMixedBatchRefusalIsAttributedToTheRefusedEvent is Kyra's second
// case: the batch `[9000, 64833]` sent to a peer that reassembles
// fragments is refused for the SECOND event. 9,000 bytes is a legal
// event for that peer — it fragments into two pieces — so attributing
// the refusal to it names a payload that was never the problem.
//
// The first element is asserted to be above `MaxEventSize()` because
// that is precisely what made the old first-match scan pick it: a
// batch whose first element was under the single-packet limit would
// have been attributed correctly by accident.
func TestMixedBatchRefusalIsAttributedToTheRefusedEvent(t *testing.T) {
	const fragmentsCleanly = 9000

	admissible := make([]byte, fragmentsCleanly)
	refused := make([]byte, wantMaxFragmentedEventSize+1)

	if len(admissible) <= MaxEventSize() {
		t.Fatalf(
			"the first element is %d bytes, under the %d-byte "+
				"single-packet limit — this case only discriminates when "+
				"it is above it, because that is what a first-match scan "+
				"latches onto",
			len(admissible), MaxEventSize(),
		)
	}

	err := eventTooLargeAttribution(
		[][]byte{admissible, refused}, 1, wantMaxFragmentedEventSize,
	)
	if err == nil {
		t.Fatal("the attribution seam returned no error for an EventTooLarge refusal")
	}

	var typed *EventTooLargeError
	if !errors.As(err, &typed) {
		t.Fatalf("got %v (%T), want an *EventTooLargeError", err, err)
	}
	if typed.Size != len(refused) {
		t.Fatalf(
			"EventTooLargeError.Size = %d, want %d — the element the core "+
				"refused, not the first one above the %d-byte "+
				"single-packet limit (%d bytes), which this peer carries "+
				"fragmented",
			typed.Size, len(refused), MaxEventSize(), len(admissible),
		)
	}
	if typed.Limit != wantMaxFragmentedEventSize {
		t.Fatalf("EventTooLargeError.Limit = %d, want %d",
			typed.Limit, wantMaxFragmentedEventSize)
	}
}
