package net

import (
	"bytes"
	"errors"
	"testing"
	"time"
)

func TestStreamIDFromLabelMarksAStream(t *testing.T) {
	id, err := StreamIDFromLabel("store/my-game.world")
	if err != nil {
		t.Fatalf("stream id: %v", err)
	}
	again, _ := StreamIDFromLabel("store/my-game.world")
	if id != again {
		t.Fatalf("derivation is not stable: %#x vs %#x", id, again)
	}
	if id&(1<<49) == 0 || id&(1<<48) != 0 {
		t.Fatalf("%#x: want bit 49 set and bit 48 clear", id)
	}
}

// An inbox delivers each event with the peer that sent it — what
// RecvShard cannot — refuses a second receiver, and Close wakes a Recv
// parked in another goroutine.
func TestStreamInboxDeliversEachEventWithItsSender(t *testing.T) {
	a, b, cleanup := meshHandshakePair(t)
	defer cleanup()

	sid, err := StreamIDFromLabel("store/go-inbox-test")
	if err != nil {
		t.Fatalf("stream id: %v", err)
	}
	inbox, err := b.OpenStreamInbox(sid, 16)
	if err != nil {
		t.Fatalf("open inbox: %v", err)
	}
	defer inbox.Close()
	if _, err := b.OpenStreamInbox(sid, 16); !errors.Is(err, ErrStreamOccupied) {
		t.Fatalf("second receiver: want ErrStreamOccupied, got %v", err)
	}

	stream, err := a.OpenStream(b.NodeID(), sid, StreamConfig{Reliability: "reliable"})
	if err != nil {
		t.Fatalf("open stream: %v", err)
	}
	defer stream.Close()
	if err := stream.Send([][]byte{[]byte("from-go")}); err != nil {
		t.Fatalf("send: %v", err)
	}

	got, err := inbox.Recv(5 * time.Second)
	if err != nil || got == nil {
		t.Fatalf("recv: event=%v err=%v", got, err)
	}
	if got.PeerNodeID != a.NodeID() {
		t.Fatalf("sender: got %#x, want %#x", got.PeerNodeID, a.NodeID())
	}
	if !bytes.Equal(got.Payload, []byte("from-go")) || got.StreamID != sid {
		t.Fatalf("event: %+v", got)
	}
	if none, err := inbox.Recv(20 * time.Millisecond); none != nil || err != nil {
		t.Fatalf("empty inbox: event=%v err=%v", none, err)
	}

	done := make(chan time.Duration, 1)
	go func() {
		started := time.Now()
		_, _ = inbox.Recv(10 * time.Second)
		done <- time.Since(started)
	}()
	time.Sleep(100 * time.Millisecond)
	inbox.Close()
	select {
	case waited := <-done:
		if waited > 5*time.Second {
			t.Fatalf("Close did not wake the parked Recv (waited %v)", waited)
		}
	case <-time.After(8 * time.Second):
		t.Fatal("Close did not wake the parked Recv")
	}

	again, err := b.OpenStreamInbox(sid, 16)
	if err != nil {
		t.Fatalf("closing released the stream: %v", err)
	}
	again.Close()
}
