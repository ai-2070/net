// Stream inbox — every event on one stream, with the peer whose session
// authenticated it.
//
// `RecvShard` returns events without a sender. A StreamInbox is the
// receive path for anything that decides by who is asking: a Go host
// serving the browser package's store, for one. Pull-based on purpose —
// no Go callback runs on the mesh's receive thread.

package net

/*
#include "net.h"
#include <stdlib.h>
*/
import "C"

import (
	"runtime"
	"sync"
	"time"
	"unsafe"
)

// StreamData is one event from a StreamInbox.
type StreamData struct {
	// PeerNodeID is the peer whose session decrypted this event — the
	// authenticated sender, never a value the packet carries.
	PeerNodeID uint64
	StreamID   uint64
	Payload    []byte
}

// StreamInbox receives every event on one stream id, from any peer.
//
// At most the capacity given to OpenStreamInbox wait; beyond that an
// arriving event is dropped and counted (Dropped) rather than stalling
// the mesh. Close stops it, and the stream's events go back to
// RecvShard.
type StreamInbox struct {
	mu       sync.RWMutex
	handle   *C.net_mesh_stream_inbox_t
	streamID uint64
}

// OpenStreamInbox starts receiving streamID with each event's sender.
// One inbox per stream id per node: a second returns ErrStreamOccupied.
// A capacity of 0 means 1.
func (m *MeshNode) OpenStreamInbox(streamID uint64, capacity uint32) (*StreamInbox, error) {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return nil, ErrShuttingDown
	}
	var handle *C.net_mesh_stream_inbox_t
	code := C.net_mesh_open_stream_inbox(m.handle, C.uint64_t(streamID), C.uint32_t(capacity), &handle)
	if err := meshErrorFromCode(code); err != nil {
		return nil, err
	}
	inbox := &StreamInbox{handle: handle, streamID: streamID}
	runtime.SetFinalizer(inbox, (*StreamInbox).Close)
	return inbox, nil
}

// StreamID is the stream this inbox receives.
func (i *StreamInbox) StreamID() uint64 { return i.streamID }

// Recv returns the next event, waiting up to timeout. It returns
// (nil, nil) on timeout and once the inbox is closed — Close wakes a
// Recv waiting in another goroutine at once.
func (i *StreamInbox) Recv(timeout time.Duration) (*StreamData, error) {
	i.mu.RLock()
	defer i.mu.RUnlock()
	if i.handle == nil {
		return nil, nil
	}
	ms := timeout.Milliseconds()
	if ms < 0 {
		ms = 0
	}
	if ms > int64(^uint32(0)) {
		ms = int64(^uint32(0))
	}
	var (
		from   C.uint64_t
		buf    *C.uint8_t
		length C.size_t
	)
	code := C.net_mesh_stream_inbox_recv(i.handle, C.uint32_t(ms), &from, &buf, &length)
	if code == 0 {
		return nil, nil
	}
	if code < 0 {
		return nil, meshErrorFromCode(code)
	}
	var payload []byte
	if buf != nil && length > 0 {
		payload = C.GoBytes(unsafe.Pointer(buf), C.int(length))
		C.net_free_bytes(buf, length)
	}
	return &StreamData{PeerNodeID: uint64(from), StreamID: i.streamID, Payload: payload}, nil
}

// Dropped counts events dropped because the inbox was full.
func (i *StreamInbox) Dropped() uint64 {
	i.mu.RLock()
	defer i.mu.RUnlock()
	if i.handle == nil {
		return 0
	}
	return uint64(C.net_mesh_stream_inbox_dropped(i.handle))
}

// Close stops receiving and releases the inbox. Idempotent.
func (i *StreamInbox) Close() {
	// Wake any Recv first, under the read lock it also holds — the
	// write lock below would otherwise wait out that Recv's timeout.
	i.mu.RLock()
	if i.handle != nil {
		C.net_mesh_stream_inbox_close(i.handle)
	}
	i.mu.RUnlock()

	i.mu.Lock()
	defer i.mu.Unlock()
	if i.handle != nil {
		C.net_mesh_stream_inbox_free(i.handle)
		i.handle = nil
		runtime.SetFinalizer(i, nil)
	}
}

// StreamIDFromLabel is the stream id a label names — the same derivation
// the browser package uses, so a Go node and a page that agree on a label
// open the same stream.
func StreamIDFromLabel(label string) (uint64, error) {
	cLabel := C.CString(label)
	defer C.free(unsafe.Pointer(cLabel))
	var id C.uint64_t
	if err := meshErrorFromCode(C.net_stream_id_from_label(cLabel, &id)); err != nil {
		return 0, err
	}
	return uint64(id), nil
}
