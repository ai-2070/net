package net

// Parity guard for `NET_ERR_MESH_EVENT_TOO_LARGE` (-118).
//
// The gap this closes: the C ABI defined -118 and the Rust FFI
// returned it, but `meshErrorFromCode` had no arm for it, so every
// oversize send surfaced in Go as `mesh unknown error (code -118)` —
// untypeable, unmatched by `errors.Is`, and indistinguishable from a
// variant this binding predates. The refusal's two numbers (the
// offending size and the transport's per-event limit) were discarded
// crossing the ABI as well.
//
// These tests therefore assert three separate things, and each one
// fails on a different regression:
//
//   1. The Go arm is keyed to the header constant that cgo actually
//      compiles (`netErrMeshEventTooLarge` is `C.NET_ERR_MESH_EVENT_
//      TOO_LARGE` from `go/net.h`), and its value is -118. A
//      renumbered enum breaks the build at the const, and a header
//      edited to a different number fails here.
//   2. The limit crosses the ABI: `net_mesh_max_event_size()` is a
//      real cgo call into the linked cdylib, and it agrees with
//      `MAX_PAYLOAD_SIZE - 4` computed from the wire protocol's own
//      published constants.
//   3. A REAL oversize send on a REAL handshaken pair returns the
//      typed error carrying both numbers. That is the end-to-end
//      leg: it exercises the Rust arm in `stream_err_to_code`, the
//      integer crossing the ABI, and the Go mapping, and it would
//      have failed before this repair.
//
// Every test here needs cgo and the built `libnet` cdylib, like the
// rest of this package (see `cgo_disabled.go`).

import (
	"errors"
	"strings"
	"testing"
)

// Recomputed from the wire protocol's published constants rather than
// copied from the error message, so a silent change to either one is
// a disagreement rather than a matching pair of edits:
//
//	MAX_PACKET_SIZE  8192   (net/crates/net/wire/src/protocol.rs)
//	HEADER_SIZE        68
//	TAG_SIZE           16
//	EventFrame::LEN_SIZE 4
//
// MAX_EVENT_SIZE = 8192 - 68 - 16 - 4 = 8104.
const (
	wireMaxPacketSize   = 8192
	wireHeaderSize      = 68
	wireTagSize         = 16
	wireEventFrameLenSz = 4

	wantMaxEventSize = wireMaxPacketSize - wireHeaderSize - wireTagSize - wireEventFrameLenSz
)

// TestEventTooLargeHeaderParity pins the error code Go maps from to
// the value declared in the header cgo compiles against.
//
// `netErrMeshEventTooLarge` is not a transcribed literal — it is
// `C.NET_ERR_MESH_EVENT_TOO_LARGE`, so this test reads the header
// through cgo. The literal below is the contract with the Rust side
// (`ffi::mesh::NET_ERR_MESH_EVENT_TOO_LARGE`) and with every other
// language binding; changing the wire number is a breaking change
// that must be made deliberately in all of them.
func TestEventTooLargeHeaderParity(t *testing.T) {
	if got := netErrMeshEventTooLarge; got != -118 {
		t.Fatalf(
			"NET_ERR_MESH_EVENT_TOO_LARGE = %d in go/net.h, want -118 — "+
				"the C ABI, the Rust ffi::mesh constant and every "+
				"binding must agree on this number",
			got,
		)
	}
	// The mapping exists at all. Before this repair the default arm
	// caught -118 and produced an untypeable `fmt.Errorf`.
	err := meshErrorFromCode(netErrMeshEventTooLarge)
	if !errors.Is(err, ErrEventTooLarge) {
		t.Fatalf(
			"meshErrorFromCode(%d) = %v, want ErrEventTooLarge — "+
				"a missing arm here is what made oversize sends arrive "+
				"as a generic unknown error",
			netErrMeshEventTooLarge, err,
		)
	}
	if strings.Contains(err.Error(), "unknown error") {
		t.Fatalf("code %d still falls through to the unknown-error arm: %v",
			netErrMeshEventTooLarge, err)
	}
}

// TestMaxEventSizeCrossesTheABI checks the limit the ABI used to
// discard is now readable, and that the number the cdylib reports is
// the one the wire protocol actually enforces.
func TestMaxEventSizeCrossesTheABI(t *testing.T) {
	got := MaxEventSize()
	if got != wantMaxEventSize {
		t.Fatalf(
			"net_mesh_max_event_size() = %d, want %d "+
				"(MAX_PACKET_SIZE %d - HEADER_SIZE %d - TAG_SIZE %d - "+
				"EventFrame::LEN_SIZE %d)",
			got, wantMaxEventSize, wireMaxPacketSize, wireHeaderSize,
			wireTagSize, wireEventFrameLenSz,
		)
	}
}

// TestSendRefusesOversizePayloadWithTypedError is the end-to-end leg:
// a real handshaken pair, a real stream, a real oversize send.
//
// The payload is one byte over the limit, which makes the assertion
// discriminating in both directions — a boundary-off-by-one on either
// side of the ABI shows up as either a missing refusal or a refusal
// of the companion at-limit payload sent below.
func TestSendRefusesOversizePayloadWithTypedError(t *testing.T) {
	a, b, cleanup := meshHandshakePair(t)
	defer cleanup()

	stream, err := a.OpenStream(b.NodeID(), 0x5118, StreamConfig{
		Reliability: "reliable",
		WindowBytes: WindowBytesOf(1 << 16),
	})
	if err != nil {
		t.Fatalf("open_stream: %v", err)
	}
	defer stream.Close()

	limit := MaxEventSize()
	oversize := make([]byte, limit+1)

	// A small payload rides in front of the oversize one so the test
	// also pins that the refusal attributes the RIGHT element of the
	// batch, not merely "something in there was too big".
	sendErr := stream.Send([][]byte{[]byte("ok"), oversize})
	if sendErr == nil {
		t.Fatalf("Send accepted a %d-byte payload over the %d-byte limit",
			len(oversize), limit)
	}
	if !errors.Is(sendErr, ErrEventTooLarge) {
		t.Fatalf("Send returned %v (%T), want ErrEventTooLarge", sendErr, sendErr)
	}

	var typed *EventTooLargeError
	if !errors.As(sendErr, &typed) {
		t.Fatalf("Send error %v is not an *EventTooLargeError", sendErr)
	}
	if typed.Size != len(oversize) {
		t.Fatalf("EventTooLargeError.Size = %d, want %d (the oversize "+
			"element, not the 2-byte one in front of it)",
			typed.Size, len(oversize))
	}
	if typed.Limit != limit {
		t.Fatalf("EventTooLargeError.Limit = %d, want %d", typed.Limit, limit)
	}

	// Retrying cannot clear it, and the retrying senders must not
	// absorb it the way they absorb backpressure.
	if err := stream.SendWithRetry([][]byte{oversize}, 3); !errors.Is(err, ErrEventTooLarge) {
		t.Fatalf("SendWithRetry returned %v, want ErrEventTooLarge "+
			"propagated immediately", err)
	}
	if err := stream.SendBlocking([][]byte{oversize}); !errors.Is(err, ErrEventTooLarge) {
		t.Fatalf("SendBlocking returned %v, want ErrEventTooLarge "+
			"propagated immediately", err)
	}

	// The boundary itself is admitted: exactly `limit` bytes is a
	// legal event. Without this the refusal above would also be
	// satisfied by a limit that is simply too low.
	if err := stream.Send([][]byte{make([]byte, limit)}); err != nil {
		t.Fatalf("Send refused an exactly-at-limit %d-byte payload: %v",
			limit, err)
	}
}
