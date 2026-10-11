// SDK audit 2026-10-11: MeshRpc.Close must not deadlock against the ctx
// cancel watcher of an in-flight Call.
//
// The call holds the client's read lock across the blocking cgo call. The
// watcher used to re-take that read lock to fire the cancel — but a queued
// Close (waiting for the write lock) makes Go's RWMutex refuse new readers,
// so the watcher, the call and Close all blocked for good against a server
// that never answers. The cancel now fires against the handle the call
// itself publishes (inflightCancel), with no lock.

package net

import (
	"context"
	"errors"
	"testing"
	"time"
	"unsafe"
)

func TestMeshRpc_CloseThenCancelDoesNotDeadlockAnInflightCall(t *testing.T) {
	a, b, cleanup := meshHandshakePair(t)
	defer cleanup()

	rpcA, err := NewMeshRpc(a)
	if err != nil {
		t.Fatalf("NewMeshRpc(a): %v", err)
	}
	rpcB, err := NewMeshRpc(b)
	if err != nil {
		t.Fatalf("NewMeshRpc(b): %v", err)
	}
	defer rpcB.Close()

	release := make(chan struct{})
	defer close(release)
	serve, err := rpcB.Serve("audit.silent", func(req []byte) ([]byte, error) {
		<-release // a server that never answers while the test runs
		return req, nil
	})
	if err != nil {
		t.Fatalf("Serve: %v", err)
	}
	defer serve.Close()

	ctx, cancel := context.WithCancel(context.Background()) // no deadline
	defer cancel()
	callDone := make(chan error, 1)
	go func() {
		_, err := rpcA.Call(ctx, b.NodeID(), "audit.silent", []byte("x"))
		callDone <- err
	}()
	time.Sleep(300 * time.Millisecond) // let the call park in native code

	// Queue Close for the write lock FIRST, then cancel: the order that
	// left the watcher unable to take the read lock.
	closeDone := make(chan struct{})
	go func() {
		rpcA.Close()
		close(closeDone)
	}()
	time.Sleep(100 * time.Millisecond)
	cancel()

	select {
	case err := <-callDone:
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("call returned %v, want context.Canceled", err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("in-flight Call never returned after cancel with Close queued (deadlock)")
	}
	select {
	case <-closeDone:
	case <-time.After(10 * time.Second):
		t.Fatal("Close never returned")
	}
}

func TestInflightCancel_DeliversBeforeAndDuringTheCall(t *testing.T) {
	var fired []uint64
	ic := newInflightCancel(7, func(_ unsafe.Pointer, tok uint64) { fired = append(fired, tok) })

	// Not in flight: remembered, delivered on enter.
	ic.cancel()
	if len(fired) != 0 {
		t.Fatalf("cancel with no call in flight must not fire yet: %v", fired)
	}
	var h int
	ic.enter(unsafe.Pointer(&h))
	if len(fired) != 1 || fired[0] != 7 {
		t.Fatalf("enter after cancel must fire once: %v", fired)
	}
	ic.leave()

	// In flight: fired immediately; after leave, never again.
	ic2 := newInflightCancel(9, func(_ unsafe.Pointer, tok uint64) { fired = append(fired, tok) })
	ic2.enter(unsafe.Pointer(&h))
	ic2.cancel()
	ic2.leave()
	ic2.cancel()
	if len(fired) != 2 || fired[1] != 9 {
		t.Fatalf("in-flight cancel must fire exactly once: %v", fired)
	}
	var nilIC *inflightCancel
	nilIC.enter(unsafe.Pointer(&h)) // nil-safe: no watcher installed
	nilIC.leave()
}

// SDK audit 2026-10-11: the streaming constructors used the
// non-cancellable C exports, so ctx cancel / Close could not wake a Recv
// parked in native code — it stayed blocked until the server's handler
// timeout, holding an OS thread and the call slot, with no CANCEL sent.
func TestMeshRpc_CtxCancelWakesAParkedStreamRecv(t *testing.T) {
	for _, viaClose := range []bool{false, true} {
		name := "ctx-cancel"
		if viaClose {
			name = "close"
		}
		t.Run(name, func(t *testing.T) {
			a, b, cleanup := meshHandshakePair(t)
			defer cleanup()
			rpcA, err := NewMeshRpc(a)
			if err != nil {
				t.Fatalf("NewMeshRpc(a): %v", err)
			}
			defer rpcA.Close()
			rpcB, err := NewMeshRpc(b)
			if err != nil {
				t.Fatalf("NewMeshRpc(b): %v", err)
			}
			defer rpcB.Close()

			release := make(chan struct{})
			defer close(release)
			serve, err := rpcB.ServeStreaming("audit.silent.stream", func(_ []byte, _ *ResponseSinkSend) error {
				<-release // never sends a chunk while the test runs
				return nil
			})
			if err != nil {
				t.Fatalf("ServeStreaming: %v", err)
			}
			defer serve.Close()

			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			stream, err := rpcA.CallStreaming(ctx, b.NodeID(), "audit.silent.stream", []byte("x"), StreamOptions{})
			if err != nil {
				t.Fatalf("CallStreaming: %v", err)
			}
			recvDone := make(chan error, 1)
			go func() {
				_, err := stream.Recv()
				recvDone <- err
			}()
			time.Sleep(300 * time.Millisecond) // let Recv park in native code
			if viaClose {
				stream.Close()
			} else {
				cancel()
			}
			select {
			case err := <-recvDone:
				if err == nil {
					t.Fatal("Recv returned a chunk from a silent server")
				}
			case <-time.After(10 * time.Second):
				t.Fatal("parked Recv was not woken by teardown")
			}
			stream.Close()
		})
	}
}

// SDK audit 2026-10-11: a goroutine a server handler spawned could still be
// parked in native `next` (or past the old "invalidated" check) when the
// handler returned, and Rust frees the handle the moment the trampoline
// returns — the next chunk, END or CANCEL then wrote through freed memory.
// The trampoline now retires the wrapper: the parked Recv is woken with
// ErrStreamDone and waited for before control goes back to Rust.
func TestServerStream_ParkedRecvIsRetiredBeforeTheHandleIsFreed(t *testing.T) {
	a, b, cleanup := meshHandshakePair(t)
	defer cleanup()
	rpcA, err := NewMeshRpc(a)
	if err != nil {
		t.Fatalf("NewMeshRpc(a): %v", err)
	}
	defer rpcA.Close()
	rpcB, err := NewMeshRpc(b)
	if err != nil {
		t.Fatalf("NewMeshRpc(b): %v", err)
	}
	defer rpcB.Close()

	parked := make(chan struct{})
	reader := make(chan error, 1)
	serve, err := rpcB.ServeDuplex("audit.escaping.reader", func(stream *RequestStreamRecv, _ *ResponseSinkSend) error {
		go func() {
			close(parked)
			for { // drain the opening chunk, then park on the next
				if _, err := stream.Recv(); err != nil {
					reader <- err
					return
				}
			}
		}()
		<-parked
		time.Sleep(100 * time.Millisecond) // let the goroutine reach native next
		return nil                         // returns while the reader is parked
	})
	if err != nil {
		t.Fatalf("ServeDuplex: %v", err)
	}
	defer serve.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	call, err := rpcA.CallDuplex(ctx, b.NodeID(), "audit.escaping.reader", DuplexOptions{})
	if err != nil {
		t.Fatalf("CallDuplex: %v", err)
	}
	defer call.Close()
	if err := call.Send([]byte("open")); err != nil {
		t.Fatalf("first Send: %v", err)
	}

	select {
	case err := <-reader:
		if !errors.Is(err, ErrStreamDone) {
			t.Fatalf("escaped Recv returned %v, want ErrStreamDone", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("a Recv parked past its handler's return was not retired")
	}
	// Traffic after the handler returned must not touch a freed handle.
	_ = call.Send([]byte("late"))
	_ = call.FinishSending()
	time.Sleep(200 * time.Millisecond)
}

func TestServerHandleGate_RetireWaitsForInflightOps(t *testing.T) {
	var g serverHandleGate
	if !g.enter() {
		t.Fatal("enter on a fresh gate")
	}
	woken := make(chan struct{})
	done := make(chan struct{})
	go func() {
		g.retire(func() { close(woken) })
		close(done)
	}()
	<-woken
	select {
	case <-done:
		t.Fatal("retire returned while an op was still in flight")
	case <-time.After(50 * time.Millisecond):
	}
	g.leave()
	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("retire did not return after the op left")
	}
	if g.enter() {
		t.Fatal("enter after retire must refuse")
	}
}

// SDK audit 2026-10-11: Split while a Recv was parked on the same call
// blocked until that Recv returned — forever, on a quiet stream. It now
// refuses at once and leaves the call usable.
func TestDuplexSplitDoesNotParkBehindRecv(t *testing.T) {
	a, b, cleanup := meshHandshakePair(t)
	defer cleanup()
	rpcA, err := NewMeshRpc(a)
	if err != nil {
		t.Fatalf("NewMeshRpc(a): %v", err)
	}
	defer rpcA.Close()
	rpcB, err := NewMeshRpc(b)
	if err != nil {
		t.Fatalf("NewMeshRpc(b): %v", err)
	}
	defer rpcB.Close()

	release := make(chan struct{})
	serve, err := rpcB.ServeDuplex("audit.split.quiet", func(_ *RequestStreamRecv, _ *ResponseSinkSend) error {
		<-release // never answers while the test runs
		return nil
	})
	if err != nil {
		t.Fatalf("ServeDuplex: %v", err)
	}
	defer serve.Close()
	defer close(release)

	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	call, err := rpcA.CallDuplex(ctx, b.NodeID(), "audit.split.quiet", DuplexOptions{})
	if err != nil {
		t.Fatalf("CallDuplex: %v", err)
	}
	defer call.Close()

	recvDone := make(chan error, 1)
	go func() {
		_, err := call.Recv()
		recvDone <- err
	}()
	time.Sleep(200 * time.Millisecond) // parked in native next

	split := make(chan error, 1)
	go func() {
		_, _, err := call.Split()
		split <- err
	}()
	select {
	case err := <-split:
		if !errors.Is(err, ErrSplitWhileReceiving) {
			t.Fatalf("Split with a parked Recv returned %v, want ErrSplitWhileReceiving", err)
		}
	case <-time.After(3 * time.Second):
		t.Fatal("Split parked behind an in-flight Recv")
	}
	// The call is still usable: Close wakes the parked Recv.
	call.Close()
	select {
	case <-recvDone:
	case <-time.After(5 * time.Second):
		t.Fatal("Close did not wake the parked Recv")
	}
}

// Code review 2026-10-11 GO-1: Split ran requestFree while its own
// enter() was counted in flight, so the guard always fired the wake —
// a substrate cancel of the call the split halves had just taken over.
// Every DuplexStream.Recv then saw a cancellation instead of the reply.
func TestDuplexSplitHalvesStillCarryTheCall(t *testing.T) {
	a, b, cleanup := meshHandshakePair(t)
	defer cleanup()
	rpcA, err := NewMeshRpc(a)
	if err != nil {
		t.Fatalf("NewMeshRpc(a): %v", err)
	}
	defer rpcA.Close()
	rpcB, err := NewMeshRpc(b)
	if err != nil {
		t.Fatalf("NewMeshRpc(b): %v", err)
	}
	defer rpcB.Close()

	serve, err := rpcB.ServeDuplex("audit.split.echo", func(in *RequestStreamRecv, out *ResponseSinkSend) error {
		for {
			msg, err := in.Recv()
			if err != nil {
				return nil
			}
			if err := out.Send(msg); err != nil {
				return err
			}
		}
	})
	if err != nil {
		t.Fatalf("ServeDuplex: %v", err)
	}
	defer serve.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	call, err := rpcA.CallDuplex(ctx, b.NodeID(), "audit.split.echo", DuplexOptions{})
	if err != nil {
		t.Fatalf("CallDuplex: %v", err)
	}
	sink, stream, err := call.Split()
	if err != nil {
		t.Fatalf("Split: %v", err)
	}
	defer sink.Close()
	defer stream.Close()

	for i := 0; i < 3; i++ {
		want := []byte{'p', 'i', 'n', 'g', byte('0' + i)}
		if err := sink.Send(want); err != nil {
			t.Fatalf("sink.Send #%d: %v", i, err)
		}
		got, err := stream.Recv()
		if err != nil {
			t.Fatalf("stream.Recv #%d after Split: %v (the split cancelled its own call)", i, err)
		}
		if string(got) != string(want) {
			t.Fatalf("stream.Recv #%d = %q, want %q", i, got, want)
		}
	}
	if err := sink.Finish(); err != nil {
		t.Fatalf("sink.Finish: %v", err)
	}
	if _, err := stream.Recv(); !errors.Is(err, ErrStreamDone) {
		t.Fatalf("Recv after Finish = %v, want ErrStreamDone", err)
	}
}
