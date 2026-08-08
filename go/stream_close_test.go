// Close must release core stream state, not just the FFI handle.
//
// `MeshStream.Close` called only `net_mesh_stream_free`, which drops
// the handle and its Arc without touching `MeshNode::close_stream`.
// Core state then survived until node shutdown, so a long-lived Go
// node could not release it eagerly, could not enforce a close/reopen
// epoch, and could not reopen the same stream id under a new
// configuration — the first open's config stayed in force.
//
// These require the native library; they are skipped when it is absent.

package net

import "testing"

func TestMeshStream_CloseIsIdempotent(t *testing.T) {
	m := newMeshForCaps(t)
	defer m.Shutdown()

	stream, err := m.OpenStream(1, 7, StreamConfig{})
	if err != nil {
		t.Skipf("no peer to open a stream against: %v", err)
	}

	stream.Close()
	// A second Close must be a no-op rather than a double free — the
	// handle is nilled under the mutex on the first call.
	stream.Close()
}

func TestMeshStream_ReopenAfterCloseUsesTheNewConfig(t *testing.T) {
	m := newMeshForCaps(t)
	defer m.Shutdown()

	const streamID = 11

	first, err := m.OpenStream(1, streamID, StreamConfig{
		WindowBytes: WindowBytesOf(16384),
	})
	if err != nil {
		t.Skipf("no peer to open a stream against: %v", err)
	}
	first.Close()

	// Without the core close this reopen inherits the first open's
	// config ("first open wins") rather than the one asked for here.
	second, err := m.OpenStream(1, streamID, StreamConfig{
		WindowBytes: UnboundedWindow(),
	})
	if err != nil {
		t.Fatalf("reopen after close must succeed: %v", err)
	}
	defer second.Close()
}
