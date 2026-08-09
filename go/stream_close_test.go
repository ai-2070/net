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

import "testing"

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
