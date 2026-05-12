package net

import (
	"context"
	"fmt"
	"testing"
	"time"
)

const testOrigin = uint64(0xABCDEF01)

// ---------------------------------------------------------------------------
// Redex + RedexFile
// ---------------------------------------------------------------------------

func TestRedexFileAppendReadRange(t *testing.T) {
	r := NewRedex("")
	defer r.Free()

	f, err := r.OpenFile("go/test/basic", nil)
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	defer f.Close()

	seq, err := f.Append([]byte("hello"))
	if err != nil {
		t.Fatalf("append: %v", err)
	}
	if seq != 0 {
		t.Fatalf("first seq should be 0, got %d", seq)
	}
	if f.Len() != 1 {
		t.Fatalf("Len should be 1, got %d", f.Len())
	}

	events, err := f.ReadRange(0, 10)
	if err != nil {
		t.Fatalf("read_range: %v", err)
	}
	if len(events) != 1 {
		t.Fatalf("expected 1 event, got %d", len(events))
	}
	if string(events[0].Payload) != "hello" {
		t.Fatalf("payload mismatch: %q", events[0].Payload)
	}
}

func TestRedexFileTail(t *testing.T) {
	r := NewRedex("")
	defer r.Free()

	f, err := r.OpenFile("go/test/tail", nil)
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	defer f.Close()

	_, err = f.Append([]byte("early"))
	if err != nil {
		t.Fatalf("append: %v", err)
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	events, errs, err := f.Tail(ctx, 0)
	if err != nil {
		t.Fatalf("tail: %v", err)
	}

	select {
	case ev := <-events:
		if string(ev.Payload) != "early" {
			t.Fatalf("backfill payload mismatch: %q", ev.Payload)
		}
	case <-time.After(1 * time.Second):
		t.Fatalf("timeout waiting for backfill")
	}

	if _, err := f.Append([]byte("live")); err != nil {
		t.Fatalf("append live: %v", err)
	}

	select {
	case ev := <-events:
		if string(ev.Payload) != "live" {
			t.Fatalf("live payload mismatch: %q", ev.Payload)
		}
	case err := <-errs:
		t.Fatalf("unexpected err from tail: %v", err)
	case <-time.After(1 * time.Second):
		t.Fatalf("timeout waiting for live")
	}

	cancel()
	// Drain to confirm the goroutine exits.
	drainTimeout := time.After(1 * time.Second)
	for {
		select {
		case _, ok := <-events:
			if !ok {
				return
			}
		case <-drainTimeout:
			t.Fatalf("tail goroutine didn't exit after context cancel")
		}
	}
}

// ---------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------

func TestTasksCRUD(t *testing.T) {
	r := NewRedex("")
	defer r.Free()

	tasks, err := OpenTasks(r, testOrigin, false)
	if err != nil {
		t.Fatalf("open tasks: %v", err)
	}
	defer tasks.Close()

	if _, err := tasks.Create(1, "alpha", 100); err != nil {
		t.Fatalf("create: %v", err)
	}
	if _, err := tasks.Create(2, "beta", 200); err != nil {
		t.Fatalf("create: %v", err)
	}
	if _, err := tasks.Rename(1, "alpha-renamed", 300); err != nil {
		t.Fatalf("rename: %v", err)
	}
	seq, err := tasks.Complete(2, 400)
	if err != nil {
		t.Fatalf("complete: %v", err)
	}
	if err := tasks.WaitForSeq(seq, 2*time.Second); err != nil {
		t.Fatalf("wait_for_seq: %v", err)
	}

	list, err := tasks.List(nil)
	if err != nil {
		t.Fatalf("list: %v", err)
	}
	if len(list) != 2 {
		t.Fatalf("expected 2 tasks, got %d", len(list))
	}

	byID := map[uint64]Task{}
	for _, t := range list {
		byID[t.ID] = t
	}
	if byID[1].Title != "alpha-renamed" {
		t.Fatalf("task 1 title mismatch: %q", byID[1].Title)
	}
	if byID[2].Status != "completed" {
		t.Fatalf("task 2 should be completed, got %q", byID[2].Status)
	}
}

func TestTasksSnapshotAndWatchDeliversPostCallUpdates(t *testing.T) {
	r := NewRedex("")
	defer r.Free()
	tasks, err := OpenTasks(r, testOrigin, false)
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	defer tasks.Close()

	seq, err := tasks.Create(1, "seed", 100)
	if err != nil {
		t.Fatalf("create: %v", err)
	}
	if err := tasks.WaitForSeq(seq, 2*time.Second); err != nil {
		t.Fatalf("wait_for_seq: %v", err)
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	snapshot, updates, errs, err := tasks.SnapshotAndWatch(ctx, nil)
	if err != nil {
		t.Fatalf("snapshot_and_watch: %v", err)
	}
	if len(snapshot) != 1 {
		t.Fatalf("snapshot should be 1, got %d", len(snapshot))
	}

	seq2, err := tasks.Create(2, "post", 200)
	if err != nil {
		t.Fatalf("create post: %v", err)
	}
	if err := tasks.WaitForSeq(seq2, 2*time.Second); err != nil {
		t.Fatalf("wait_for_seq 2: %v", err)
	}

	select {
	case batch := <-updates:
		if len(batch) != 2 {
			t.Fatalf("expected 2 tasks in delta, got %d", len(batch))
		}
	case err := <-errs:
		t.Fatalf("unexpected err: %v", err)
	case <-time.After(2 * time.Second):
		t.Fatalf("timeout waiting for delta")
	}
}

// Regression test mirroring the Rust / TS / Python regression suites.
// Drives a concurrent mutation to race the snapshot / stream reads
// inside SnapshotAndWatch. Trials where the mutation landed before
// the snapshot read are skipped (no further delta to deliver). Under
// the pre-fix `skip(1)` the race trials would hang because the
// watcher's internal `last` already equalled the post-mutation state;
// here the timeout would fail the test.
func TestRegressionTasksSnapshotAndWatchForwardsDivergentInitial(t *testing.T) {
	for trial := 0; trial < 20; trial++ {
		r := NewRedex("")
		tasks, err := OpenTasks(r, testOrigin, false)
		if err != nil {
			t.Fatalf("trial %d: open: %v", trial, err)
		}

		seq, err := tasks.Create(1, "seed", 100)
		if err != nil {
			t.Fatalf("trial %d: create: %v", trial, err)
		}
		if err := tasks.WaitForSeq(seq, 2*time.Second); err != nil {
			t.Fatalf("trial %d: wait: %v", trial, err)
		}

		mutated := make(chan error, 1)
		go func() {
			s, err := tasks.Create(2, "race", 200)
			if err != nil {
				mutated <- fmt.Errorf("create: %w", err)
				return
			}
			if err := tasks.WaitForSeq(s, 2*time.Second); err != nil {
				mutated <- fmt.Errorf("wait_for_seq: %w", err)
				return
			}
			mutated <- nil
		}()

		ctx, cancel := context.WithCancel(context.Background())
		snapshot, updates, errs, err := tasks.SnapshotAndWatch(ctx, nil)
		if err != nil {
			cancel()
			t.Fatalf("trial %d: snapshot_and_watch: %v", trial, err)
		}
		if mErr := <-mutated; mErr != nil {
			// Fail fast with the real cause rather than waiting for a
			// misleading delta-timeout below.
			cancel()
			t.Fatalf("trial %d: mutator failed: %v", trial, mErr)
		}

		if len(snapshot) == 2 {
			cancel()
			tasks.Close()
			r.Free()
			continue
		}
		if len(snapshot) != 1 {
			cancel()
			t.Fatalf("trial %d: snapshot should be [seed], got %d", trial, len(snapshot))
		}

		select {
		case batch := <-updates:
			if len(batch) != 2 {
				cancel()
				t.Fatalf("trial %d: expected 2 in delta, got %d", trial, len(batch))
			}
		case err := <-errs:
			cancel()
			t.Fatalf("trial %d: watch err: %v", trial, err)
		case <-time.After(1 * time.Second):
			cancel()
			t.Fatalf("trial %d: timeout waiting for delta", trial)
		}
		cancel()
		tasks.Close()
		r.Free()
	}
}

// ---------------------------------------------------------------------------
// Memories
// ---------------------------------------------------------------------------

func TestMemoriesCRUD(t *testing.T) {
	r := NewRedex("")
	defer r.Free()
	mem, err := OpenMemories(r, testOrigin, false)
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	defer mem.Close()

	if _, err := mem.Store(1, "hello", []string{"work"}, "alice", 100); err != nil {
		t.Fatalf("store: %v", err)
	}
	seq, err := mem.Pin(1, 110)
	if err != nil {
		t.Fatalf("pin: %v", err)
	}
	if err := mem.WaitForSeq(seq, 2*time.Second); err != nil {
		t.Fatalf("wait: %v", err)
	}

	list, err := mem.List(nil)
	if err != nil {
		t.Fatalf("list: %v", err)
	}
	if len(list) != 1 {
		t.Fatalf("expected 1 memory, got %d", len(list))
	}
	if !list[0].Pinned {
		t.Fatalf("memory should be pinned")
	}
}

func TestRegressionMemoriesSnapshotAndWatchForwardsDivergentInitial(t *testing.T) {
	for trial := 0; trial < 20; trial++ {
		r := NewRedex("")
		mem, err := OpenMemories(r, testOrigin, false)
		if err != nil {
			t.Fatalf("trial %d: open: %v", trial, err)
		}

		seq, err := mem.Store(1, "seed", []string{"t"}, "alice", 100)
		if err != nil {
			t.Fatalf("trial %d: store: %v", trial, err)
		}
		if err := mem.WaitForSeq(seq, 2*time.Second); err != nil {
			t.Fatalf("trial %d: wait: %v", trial, err)
		}

		mutated := make(chan error, 1)
		go func() {
			s, err := mem.Store(2, "race", []string{"t"}, "alice", 200)
			if err != nil {
				mutated <- fmt.Errorf("store: %w", err)
				return
			}
			if err := mem.WaitForSeq(s, 2*time.Second); err != nil {
				mutated <- fmt.Errorf("wait_for_seq: %w", err)
				return
			}
			mutated <- nil
		}()

		ctx, cancel := context.WithCancel(context.Background())
		snapshot, updates, errs, err := mem.SnapshotAndWatch(ctx, nil)
		if err != nil {
			cancel()
			t.Fatalf("trial %d: snapshot_and_watch: %v", trial, err)
		}
		if mErr := <-mutated; mErr != nil {
			cancel()
			t.Fatalf("trial %d: mutator failed: %v", trial, mErr)
		}

		if len(snapshot) == 2 {
			cancel()
			mem.Close()
			r.Free()
			continue
		}
		if len(snapshot) != 1 {
			cancel()
			t.Fatalf("trial %d: snapshot should be [seed], got %d", trial, len(snapshot))
		}

		select {
		case batch := <-updates:
			if len(batch) != 2 {
				cancel()
				t.Fatalf("trial %d: expected 2 in delta, got %d", trial, len(batch))
			}
		case err := <-errs:
			cancel()
			t.Fatalf("trial %d: watch err: %v", trial, err)
		case <-time.After(1 * time.Second):
			cancel()
			t.Fatalf("trial %d: timeout waiting for delta", trial)
		}
		cancel()
		mem.Close()
		r.Free()
	}
}
