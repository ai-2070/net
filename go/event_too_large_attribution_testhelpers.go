// Test-only bridge to the C-ABI refusal-attribution seam every send
// entry point shares, for the size/limit parity tests in
// `event_too_large_attribution_test.go`.
//
// Gated with the `test_helpers` build tag so Go production builds
// (plain `go build` / `go test`) never compile this file or reference
// `net_mesh_test_send_refusal_attribution`; the symbol itself is gated
// at the Rust layer behind the core crate's `fixtures` cargo feature,
// which `net-ffi/test-helpers` turns on — so a `libnet` built without
// that feature does not export it either. Running these tests
// therefore requires:
//
//  1. Build with the feature on so the symbol reaches `libnet`:
//     `cargo build --release -p net-ffi --features
//     net-ffi/test-helpers` (what the go-tests CI job runs).
//  2. Run `go test -tags test_helpers` so this file is compiled into
//     the test binary and the extern reference resolves.
//
// Same posture as `groups_testhelpers.go`: a `libnet` without the
// feature paired with a `-tags test_helpers` binary fails to link,
// which makes the test posture explicit rather than implicit.
//
// WHY A SEAM AND NOT A LIVE SEND. The 64,832-byte fragmentation
// ceiling only applies when the resolved peer address is RTC, and the
// cdylib this binding links is built without the `webrtc` feature, so
// no send through it can be refused at that limit — nor attributed to
// a batch's second element, since without the ceiling every batch is
// refused at its first element above `MaxEventSize()`. Those two live
// attributions are covered on the Rust side
// (`net/crates/net/tests/rtc_repairs.rs`); what these tests cover is
// the other half — that whatever pair the core attributed arrives in
// Go intact instead of being reconstructed from the single-packet
// accessor and a scan of the caller's own slice, which is what this
// binding used to do and what made a 64,833-byte tagged refusal
// report 8,104 and a `[9000, 64833]` batch report `{9000, 8104}`.
//
// The real oversize send against a real handshaken pair — the leg
// that proves the out-params are wired through `net_mesh_send`
// itself — is `TestSendRefusesOversizePayloadWithTypedError`, which
// needs no build tag.

//go:build test_helpers

package net

/*
#include "net.h"

// The prototype is deliberately NOT in net.h since that header is
// consumed by production callers, and this symbol does not exist in a
// production build. Declared inline here so only this test-only TU
// references it.
extern int net_mesh_test_send_refusal_attribution(
    const uint8_t* const* payloads,
    const size_t* lens,
    size_t count,
    size_t attributed_index,
    size_t limit,
    size_t* out_size,
    size_t* out_limit
);
*/
import "C"

// eventTooLargeAttribution drives the seam with the attribution a
// send would have produced — the batch, which element of it the core
// refused, and the limit that applied — and returns the result
// through `sendErrorFromCode`, the same mapping every Send method
// returns through.
//
// It sends nothing. The payload array is real and is collected by the
// same `collect_payloads` the send entry points use, so a Go layer
// that reconstructs the refusal from it instead of reading the
// out-params produces a visibly different answer.
func eventTooLargeAttribution(payloads [][]byte, attributedIndex, limit int) error {
	ptrs, lens, count, release := payloadPtrs(payloads)
	defer release()
	var refusedSize, refusedLimit C.size_t
	code := C.net_mesh_test_send_refusal_attribution(
		ptrs, lens, count,
		C.size_t(attributedIndex), C.size_t(limit),
		&refusedSize, &refusedLimit,
	)
	return sendErrorFromCode(code, refusedSize, refusedLimit)
}
