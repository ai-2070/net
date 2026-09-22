// Close must release core stream state, not just the FFI handle.
//
// `MeshStream.Close` called only `net_mesh_stream_free`, which drops
// the handle and its Arc without touching `MeshNode::close_stream`.
// Core state then survived until node shutdown, so a long-lived Go
// node could not release it eagerly, could not enforce a close/reopen
// epoch, and could not reopen the same stream id under a new
// configuration — the first open's config stayed in force.
//
// These originally opened against node id 1 on a lone mesh, which has
// no peer, so `OpenStream` always failed and both tests always hit
// their `t.Skipf`. They ran green having asserted nothing — the exact
// shape of the gap they were written to close. They now stand up a
// real handshaked pair via `meshHandshakePair` and open against the
// peer's actual node id, so a failure to open is a failure, not a
// skip.

package net

import (
	"errors"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"testing"
)

func TestMeshStream_CloseIsIdempotent(t *testing.T) {
	a, b, cleanup := meshHandshakePair(t)
	defer cleanup()

	stream, err := a.OpenStream(b.NodeID(), 7, StreamConfig{})
	if err != nil {
		t.Fatalf("open stream against the handshaked peer: %v", err)
	}

	stream.Close()
	// A second Close must be a no-op rather than a double free — the
	// handle is nilled under the mutex on the first call.
	stream.Close()
}

func TestMeshStream_ReopenAfterCloseUsesTheNewConfig(t *testing.T) {
	a, b, cleanup := meshHandshakePair(t)
	defer cleanup()

	const streamID = 11
	peer := b.NodeID()

	first, err := a.OpenStream(peer, streamID, StreamConfig{
		WindowBytes: WindowBytesOf(16384),
	})
	if err != nil {
		t.Fatalf("open stream against the handshaked peer: %v", err)
	}
	first.Close()

	// Without the core close this reopen inherits the first open's
	// config ("first open wins") rather than the one asked for here.
	second, err := a.OpenStream(peer, streamID, StreamConfig{
		WindowBytes: UnboundedWindow(),
	})
	if err != nil {
		t.Fatalf("reopen after close must succeed: %v", err)
	}
	defer second.Close()
}

// -117 must surface as the public [ErrSessionSuperseded] sentinel,
// and it must be the code the C header declares.
//
// Pre-fix `meshErrorFromCode` had no -117 arm, so every send, retry
// and blocking wrapper returned a freshly allocated
// `mesh unknown error (code -117)`: unmatchable by `errors.Is`, and
// indistinguishable from a genuinely unknown future code. The C
// constant existing in both headers did not close Go parity.
func TestSessionSupersededSentinelMatchesTheHeaderCode(t *testing.T) {
	code, ok := headerConstant(t, "NET_ERR_MESH_SESSION_SUPERSEDED")
	if !ok {
		t.Fatal("NET_ERR_MESH_SESSION_SUPERSEDED absent from go/net.h")
	}
	if code != -117 {
		t.Fatalf("NET_ERR_MESH_SESSION_SUPERSEDED = %d, want -117", code)
	}

	err := meshErrorFromCode(-117)
	if !errors.Is(err, ErrSessionSuperseded) {
		t.Fatalf("meshErrorFromCode(-117) = %v, want ErrSessionSuperseded", err)
	}
	// Distinct from the neighbouring stream errors: "the peer is gone
	// or this id is closed" and "the peer is here but this handle
	// belongs to a session it replaced" are different recoveries.
	if errors.Is(err, ErrNotConnected) || errors.Is(err, ErrBackpressure) {
		t.Fatal("ErrSessionSuperseded must not alias ErrNotConnected/ErrBackpressure")
	}
	// The message is the same stable prefix the N-API surface emits
	// (`ERR_SESSION_SUPERSEDED_PREFIX`), so a polyglot operator sees
	// one vocabulary.
	if got, want := ErrSessionSuperseded.Error(), "stream session superseded"; got != want {
		t.Fatalf("ErrSessionSuperseded = %q, want %q", got, want)
	}
	// Terminal, not retryable: `SendWithRetry` / `SendBlocking`
	// absorb exactly one sentinel — ErrBackpressure. The previous
	// probe compared the two sentinels with each other
	// (`errors.Is(ErrBackpressure, ErrSessionSuperseded)`): two
	// distinct constants, false by construction, so the leg could
	// never fire. Probed through the wrapper instead, on a real
	// stream whose send cannot succeed and cannot be backpressure.
	a, b, cleanup := meshHandshakePair(t)
	defer cleanup()
	peer := b.NodeID()
	stream, err := a.OpenStream(peer, 0x1170, StreamConfig{Reliability: "reliable"})
	if err != nil {
		t.Fatalf("open stream: %v", err)
	}
	stream.Close()
	got := stream.SendWithRetry([][]byte{[]byte("x")}, 3)
	if got == nil {
		t.Fatal("a closed stream's retry send must fail")
	}
	if errors.Is(got, ErrBackpressure) {
		t.Fatalf("the retry wrappers absorb only ErrBackpressure; a closed stream's %v must not ride that absorption", got)
	}
	// DEFERRED: the sharper claim — that a genuinely superseded
	// handle's -117 send is not folded into that same retry loop —
	// needs a real displaced session, and the Go binding generates
	// each node's identity internally. The displacement
	// `tests/rtc_repairs.rs::a_displaced_sessions_handle_cannot_address_its_successor`
	// drives (a second same-identity incarnation) cannot be built
	// from the exported Go surface.
}

// headerConstant reads one NET_* enum constant out of `go/net.h` —
// the header cgo actually compiles against — so the Go sentinel and
// the C ABI cannot drift apart silently.
func headerConstant(t *testing.T, name string) (int, bool) {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join(".", "net.h"))
	if err != nil {
		t.Fatalf("read net.h: %v", err)
	}
	re := regexp.MustCompile(`\b` + regexp.QuoteMeta(name) + `\s*=\s*(-?\d+)`)
	m := re.FindSubmatch(raw)
	if m == nil {
		return 0, false
	}
	value, err := strconv.Atoi(string(m[1]))
	if err != nil {
		t.Fatalf("parse %s: %v", name, err)
	}
	return value, true
}
