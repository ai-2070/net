package net

// Tests for CortEX read-your-writes tokens (go/write_token.go). Plan:
// docs/internal/plans/GO_BINDING_CONSOLIDATION_AND_BLOBS_PLAN.md, slice S4.

import (
	"context"
	"errors"
	"testing"
	"time"
)

func openTasksFor(t *testing.T, r *Redex, origin uint64) *TasksAdapter {
	t.Helper()
	tasks, err := OpenTasks(r, origin, false)
	if err != nil {
		t.Fatalf("OpenTasks: %v", err)
	}
	t.Cleanup(func() { tasks.Close() })
	return tasks
}

func TestWriteTokenTasksReadYourWrites(t *testing.T) {
	r := NewRedex("")
	defer r.Free()
	tasks := openTasksFor(t, r, testOrigin)

	if got := tasks.OriginHash(); got != testOrigin {
		t.Fatalf("OriginHash = %#x, want %#x", got, testOrigin)
	}
	seq, err := tasks.Create(7, "read me back", 1)
	if err != nil {
		t.Fatalf("Create: %v", err)
	}
	tok := tasks.Token(seq)
	if tok != (WriteToken{OriginHash: testOrigin, Seq: seq}) {
		t.Fatalf("Token(%d) = %+v", seq, tok)
	}
	if err := tasks.WaitForToken(tok, time.Second); err != nil {
		t.Fatalf("WaitForToken: %v", err)
	}
	list, err := tasks.List(nil)
	if err != nil {
		t.Fatalf("List: %v", err)
	}
	if len(list) != 1 || list[0].ID != 7 {
		t.Fatalf("List right after WaitForToken = %+v, want task 7", list)
	}
}

func TestWriteTokenMemoriesReadYourWrites(t *testing.T) {
	r := NewRedex("")
	defer r.Free()
	mem, err := OpenMemories(r, testOrigin, false)
	if err != nil {
		t.Fatalf("OpenMemories: %v", err)
	}
	defer mem.Close()

	seq, err := mem.Store(3, "remember this", []string{"t"}, "go-test", 1)
	if err != nil {
		t.Fatalf("Store: %v", err)
	}
	if err := mem.WaitForToken(mem.Token(seq), time.Second); err != nil {
		t.Fatalf("WaitForToken: %v", err)
	}
	list, err := mem.List(nil)
	if err != nil {
		t.Fatalf("List: %v", err)
	}
	if len(list) != 1 || list[0].ID != 3 {
		t.Fatalf("List right after WaitForToken = %+v, want memory 3", list)
	}
	other := WriteToken{OriginHash: testOrigin + 1, Seq: seq}
	if err := mem.WaitForToken(other, 100*time.Millisecond); !errors.Is(err, ErrWrongOrigin) {
		t.Fatalf("memories token from another origin: want ErrWrongOrigin, got %v", err)
	}
}

// The native check is on the origin, not the adapter object.
func TestWriteTokenOriginNotIdentity(t *testing.T) {
	r := NewRedex("")
	defer r.Free()
	writer := openTasksFor(t, r, testOrigin)
	seq, err := writer.Create(1, "x", 1)
	if err != nil {
		t.Fatalf("Create: %v", err)
	}

	// A different origin is refused, whatever the sequence.
	foreign := WriteToken{OriginHash: testOrigin ^ 0xFF, Seq: seq}
	if err := writer.WaitForToken(foreign, 100*time.Millisecond); !errors.Is(err, ErrWrongOrigin) {
		t.Fatalf("token from a different origin: want ErrWrongOrigin, got %v", err)
	}

	// A second adapter with the SAME origin accepts the writer's token —
	// but only once its own fold has applied that sequence. A matching
	// origin alone is not visibility, so wait on its fold first.
	reader := openTasksFor(t, r, testOrigin)
	if err := reader.WaitForSeq(seq, time.Second); err != nil {
		t.Fatalf("reader WaitForSeq: %v", err)
	}
	if err := reader.WaitForToken(writer.Token(seq), 0); err != nil {
		t.Fatalf("same-origin token on a second adapter that applied it: %v", err)
	}
	// And a same-origin token for a write that never happened is not
	// applied.
	if err := reader.WaitForToken(writer.Token(seq+1000), 0); !errors.Is(err, ErrTokenTimeout) {
		t.Fatalf("same-origin token for an unapplied seq: want ErrTokenTimeout, got %v", err)
	}
}

// Zero polls once — the deliberate contrast with WaitForSeq, where zero
// waits indefinitely.
func TestWriteTokenZeroTimeoutPolls(t *testing.T) {
	r := NewRedex("")
	defer r.Free()
	tasks := openTasksFor(t, r, testOrigin)

	start := time.Now()
	err := tasks.WaitForToken(tasks.Token(1_000_000), 0)
	elapsed := time.Since(start)
	if !errors.Is(err, ErrTokenTimeout) || !errors.Is(err, ErrStreamTimeout) {
		t.Fatalf("zero timeout on an unapplied seq: want ErrTokenTimeout (and ErrStreamTimeout), got %v", err)
	}
	// Loose on purpose: a preempted CI runner can stall a synchronous poll
	// well past 50 ms. A real wait would still be caught by the positive
	// timeout check below.
	if elapsed > time.Second {
		t.Fatalf("zero timeout took %v; it should poll, not wait", elapsed)
	}
	// A positive timeout really waits.
	start = time.Now()
	if err := tasks.WaitForToken(tasks.Token(1_000_000), 120*time.Millisecond); !errors.Is(err, ErrTokenTimeout) {
		t.Fatalf("120ms timeout on an unapplied seq: want ErrTokenTimeout, got %v", err)
	}
	if waited := time.Since(start); waited < 100*time.Millisecond {
		t.Fatalf("a 120ms timeout returned after only %v", waited)
	}
}

func TestWriteTokenContext(t *testing.T) {
	r := NewRedex("")
	defer r.Free()
	tasks := openTasksFor(t, r, testOrigin)
	seq, err := tasks.Create(1, "applied", 1)
	if err != nil {
		t.Fatalf("Create: %v", err)
	}
	applied := tasks.Token(seq)
	if err := tasks.WaitForToken(applied, time.Second); err != nil {
		t.Fatalf("WaitForToken: %v", err)
	}

	t.Run("success", func(t *testing.T) {
		ctx, cancel := context.WithTimeout(context.Background(), time.Second)
		defer cancel()
		if err := tasks.WaitForTokenContext(ctx, applied); err != nil {
			t.Fatalf("WaitForTokenContext on an applied token: %v", err)
		}
	})
	t.Run("pre-cancelled reports cancellation even when applied", func(t *testing.T) {
		ctx, cancel := context.WithCancel(context.Background())
		cancel()
		if err := tasks.WaitForTokenContext(ctx, applied); !errors.Is(err, context.Canceled) {
			t.Fatalf("pre-cancelled context: want context.Canceled, got %v", err)
		}
	})
	t.Run("deadline", func(t *testing.T) {
		ctx, cancel := context.WithTimeout(context.Background(), 120*time.Millisecond)
		defer cancel()
		// In a goroutine, so a wait that ignores its context fails this
		// subtest by name instead of hanging the binary.
		done := make(chan error, 1)
		go func() { done <- tasks.WaitForTokenContext(ctx, tasks.Token(1_000_000)) }()
		select {
		case err := <-done:
			if !errors.Is(err, context.DeadlineExceeded) {
				t.Fatalf("deadline on an unapplied token: want context.DeadlineExceeded, got %v", err)
			}
		case <-time.After(2 * time.Second):
			t.Fatal("WaitForTokenContext ignored its deadline")
		}
	})
	t.Run("cancel mid-wait is noticed within a slice", func(t *testing.T) {
		ctx, cancel := context.WithCancel(context.Background())
		done := make(chan error, 1)
		go func() { done <- tasks.WaitForTokenContext(ctx, tasks.Token(1_000_000)) }()
		time.Sleep(150 * time.Millisecond)
		cancelled := time.Now()
		cancel()
		select {
		case err := <-done:
			if !errors.Is(err, context.Canceled) {
				t.Fatalf("cancelled mid-wait: want context.Canceled, got %v", err)
			}
			// One 50ms slice, plus scheduler slack on a loaded box.
			if lag := time.Since(cancelled); lag > 200*time.Millisecond {
				t.Fatalf("cancellation noticed after %v; slices are %v", lag, tokenWaitSlice)
			}
		case <-time.After(2 * time.Second):
			t.Fatal("WaitForTokenContext ignored cancellation")
		}
	})
}

// Adapters from a NetDb inherit NetDbConfig.OriginHash, so their tokens
// name their writes.
func TestWriteTokenNetDbAdaptersInheritOrigin(t *testing.T) {
	r := NewRedex("")
	defer r.Free()
	const origin = uint64(0x5EED)
	db, err := OpenNetDb(r, NetDbConfig{OriginHash: origin, WithTasks: true, WithMemories: true})
	if err != nil {
		t.Fatalf("OpenNetDb: %v", err)
	}
	defer db.Close()

	tasks, err := db.Tasks()
	if err != nil {
		t.Fatalf("db.Tasks: %v", err)
	}
	defer tasks.Close()
	mem, err := db.Memories()
	if err != nil {
		t.Fatalf("db.Memories: %v", err)
	}
	defer mem.Close()
	if tasks.OriginHash() != origin || mem.OriginHash() != origin {
		t.Fatalf("NetDb adapters' origins = %#x / %#x, want %#x", tasks.OriginHash(), mem.OriginHash(), origin)
	}
	seq, err := tasks.Create(9, "via netdb", 1)
	if err != nil {
		t.Fatalf("Create: %v", err)
	}
	if err := tasks.WaitForToken(tasks.Token(seq), time.Second); err != nil {
		t.Fatalf("WaitForToken on a NetDb tasks adapter: %v", err)
	}
}

func TestWriteTokenAfterClose(t *testing.T) {
	r := NewRedex("")
	defer r.Free()
	tasks, err := OpenTasks(r, testOrigin, false)
	if err != nil {
		t.Fatalf("OpenTasks: %v", err)
	}
	seq, err := tasks.Create(1, "x", 1)
	if err != nil {
		t.Fatalf("Create: %v", err)
	}
	tasks.Close()
	if err := tasks.WaitForToken(tasks.Token(seq), time.Second); !errors.Is(err, ErrShuttingDown) {
		t.Fatalf("WaitForToken after Close: want ErrShuttingDown, got %v", err)
	}
	if err := tasks.WaitForTokenContext(context.Background(), tasks.Token(seq)); !errors.Is(err, ErrShuttingDown) {
		t.Fatalf("WaitForTokenContext after Close: want ErrShuttingDown, got %v", err)
	}
}

// A context cancelled while the native wait is in progress is reported as
// cancelled, even if that wait then succeeds (cubic review, PR #1165).
func TestWriteTokenContextCancelledDuringASuccessfulWait(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	calls := 0
	err := waitForTokenContext(ctx, func(uint32) error {
		calls++
		cancel() // lands mid-wait
		return nil
	})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("waitForTokenContext = %v, want context.Canceled", err)
	}
	if calls != 1 {
		t.Fatalf("native wait called %d times, want 1", calls)
	}
	// Uncancelled, a successful wait is a success.
	if err := waitForTokenContext(context.Background(), func(uint32) error { return nil }); err != nil {
		t.Fatalf("waitForTokenContext without cancellation = %v, want nil", err)
	}
}
