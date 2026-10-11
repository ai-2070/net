// Code review 2026-10-11: the streaming constructors started their ctx
// watcher only after the blocking native constructor returned, so a ctx
// cancelled while construction awaited the peer's initial-frame ACK could
// not abort it — the constructor sat out the whole call deadline.
//
// Fixed in two halves: the binding arms the reserved token for the
// construction span (watchStreamConstruction / installOrgCancelWatcher), and
// core selects the reply-subscription and initial-publish awaits against it
// (`until_cancelled` in mesh_rpc.rs), so a cancel ends construction instead
// of waiting out the reply-channel membership-ack timeout (~5 s against a
// silent peer).

package net

import (
	"context"
	"errors"
	"testing"
	"time"
)

func TestMeshRpc_CtxCancelAbortsABlockedStreamingConstruction(t *testing.T) {
	ctors := map[string]func(ctx context.Context, r *MeshRpc, target uint64) error{
		"CallStreaming": func(ctx context.Context, r *MeshRpc, target uint64) error {
			s, err := r.CallStreaming(ctx, target, "audit.dead.stream", []byte("x"), StreamOptions{})
			if s != nil {
				s.Close()
			}
			return err
		},
		"CallClientStream": func(ctx context.Context, r *MeshRpc, target uint64) error {
			c, err := r.CallClientStream(ctx, target, "audit.dead.client", ClientStreamOptions{})
			if c != nil {
				c.Close()
			}
			return err
		},
		"CallDuplex": func(ctx context.Context, r *MeshRpc, target uint64) error {
			d, err := r.CallDuplex(ctx, target, "audit.dead.duplex", DuplexOptions{})
			if d != nil {
				d.Close()
			}
			return err
		},
	}
	for name, ctor := range ctors {
		t.Run(name, func(t *testing.T) {
			a, b, cleanup := meshHandshakePair(t)
			defer cleanup()
			rpcA, err := NewMeshRpc(a)
			if err != nil {
				t.Fatalf("NewMeshRpc(a): %v", err)
			}
			defer rpcA.Close()
			target := b.NodeID()
			// The peer goes silent: the session stays in A's table, but the
			// initial frame is never ACKed.
			_ = b.Shutdown()

			ctx, cancel := context.WithCancel(context.Background()) // no deadline
			defer cancel()
			done := make(chan error, 1)
			start := time.Now()
			go func() { done <- ctor(ctx, rpcA, target) }()
			time.Sleep(300 * time.Millisecond)
			cancel()
			select {
			case err := <-done:
				if !errors.Is(err, context.Canceled) {
					t.Fatalf("%s returned %v after %v, want context.Canceled", name, err, time.Since(start))
				}
			case <-time.After(2 * time.Second):
				t.Fatalf("%s did not return within 2s of ctx cancel", name)
			}
		})
	}
}
