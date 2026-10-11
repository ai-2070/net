// Code review 2026-10-11: RegisterFactoryFunc checked for an existing
// factory and swapped its own in as two separate steps, so concurrent
// registrations of one kind could both pass the check and both swap. The
// loser's factory then sat in the map — where a migration landing in the
// window would be built with it — until Rust rejected the loser and the
// rollback restored the winner's.

package net

import (
	"fmt"
	"sync"
	"sync/atomic"
	"testing"
)

func TestRegisterFactoryFunc_ConcurrentSameKindNeverExposesTheLoser(t *testing.T) {
	m := newLocalMesh(t)
	defer m.Shutdown()
	rt, err := NewDaemonRuntime(m)
	if err != nil {
		t.Fatalf("NewDaemonRuntime: %v", err)
	}
	defer rt.Close()

	const rounds, racers = 40, 8
	for round := 0; round < rounds; round++ {
		kind := fmt.Sprintf("race-kind-%d", round)

		var stop atomic.Bool
		seen := make(map[string]bool)
		monitorDone := make(chan struct{})
		go func() {
			defer close(monitorDone)
			for !stop.Load() {
				if f := lookupFactoryFunc(rt.runtimeID, kind); f != nil {
					seen[f().(flavouredDaemon).flavour] = true
				}
			}
		}()

		start := make(chan struct{})
		results := make([]error, racers)
		var wg sync.WaitGroup
		for g := 0; g < racers; g++ {
			wg.Add(1)
			go func(g int) {
				defer wg.Done()
				flavour := fmt.Sprint(g)
				<-start
				results[g] = rt.RegisterFactoryFunc(kind, func() MeshDaemon {
					return flavouredDaemon{flavour: flavour}
				})
			}(g)
		}
		close(start)
		wg.Wait()
		stop.Store(true)
		<-monitorDone

		winner := ""
		for g, err := range results {
			if err == nil {
				if winner != "" {
					t.Fatalf("round %d: two registrations of %q succeeded", round, kind)
				}
				winner = fmt.Sprint(g)
			}
		}
		if winner == "" {
			t.Fatalf("round %d: no registration of %q succeeded: %v", round, kind, results)
		}
		if got := lookupFactoryFunc(rt.runtimeID, kind)().(flavouredDaemon).flavour; got != winner {
			t.Fatalf("round %d: final factory is %q, want the winner %q", round, got, winner)
		}
		for flavour := range seen {
			if flavour != winner {
				t.Fatalf("round %d: the trampoline saw loser %q's factory (winner %q)", round, flavour, winner)
			}
		}
	}
}
