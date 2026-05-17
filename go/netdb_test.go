package net

import (
	"errors"
	"testing"
	"time"
)

// TestNetDbOpenAccessorsAndClose covers the happy path: open both
// adapters, fetch each via the NetDb accessors, mutate through them,
// close.
func TestNetDbOpenAccessorsAndClose(t *testing.T) {
	r := NewRedex("")
	defer r.Free()

	db, err := OpenNetDb(r, NetDbConfig{
		OriginHash:   testOrigin,
		WithTasks:    true,
		WithMemories: true,
	})
	if err != nil {
		t.Fatalf("OpenNetDb: %v", err)
	}
	defer db.Free()

	tasks, err := db.Tasks()
	if err != nil {
		t.Fatalf("Tasks(): %v", err)
	}
	defer tasks.free()
	if _, err := tasks.Create(1, "first", 1_000_000); err != nil {
		t.Fatalf("tasks.Create: %v", err)
	}

	memories, err := db.Memories()
	if err != nil {
		t.Fatalf("Memories(): %v", err)
	}
	defer memories.free()

	if err := db.Close(); err != nil {
		t.Fatalf("NetDb.Close: %v", err)
	}
	// Idempotent close.
	if err := db.Close(); err != nil {
		t.Fatalf("NetDb.Close (second): %v", err)
	}
}

// TestNetDbUnenabledAccessorErrors confirms that asking for a model
// that wasn't enabled at open time returns the typed ErrNetDb.
func TestNetDbUnenabledAccessorErrors(t *testing.T) {
	r := NewRedex("")
	defer r.Free()

	db, err := OpenNetDb(r, NetDbConfig{
		OriginHash:   testOrigin,
		WithTasks:    true,
		WithMemories: false,
	})
	if err != nil {
		t.Fatalf("OpenNetDb: %v", err)
	}
	defer db.Free()

	// Tasks succeeds.
	tasks, err := db.Tasks()
	if err != nil {
		t.Fatalf("Tasks() should succeed: %v", err)
	}
	tasks.free()

	// Memories fails with ErrNetDb.
	if _, err := db.Memories(); !errors.Is(err, ErrNetDb) {
		t.Fatalf("Memories() should error with ErrNetDb; got %v", err)
	}
}

// TestNetDbSnapshotRoundtrip captures a bundle, frees the source DB,
// restores into a fresh DB, and confirms the original task survives.
func TestNetDbSnapshotRoundtrip(t *testing.T) {
	r := NewRedex("")
	defer r.Free()

	db, err := OpenNetDb(r, NetDbConfig{
		OriginHash:   testOrigin,
		WithTasks:    true,
		WithMemories: true,
	})
	if err != nil {
		t.Fatalf("OpenNetDb: %v", err)
	}

	tasks, err := db.Tasks()
	if err != nil {
		t.Fatalf("Tasks: %v", err)
	}
	seq, err := tasks.Create(42, "snapshot-me", 100)
	if err != nil {
		t.Fatalf("Create: %v", err)
	}
	if err := tasks.WaitForSeq(seq, 2*time.Second); err != nil {
		t.Fatalf("WaitForSeq: %v", err)
	}
	tasks.free()

	bundle, err := db.Snapshot()
	if err != nil {
		t.Fatalf("Snapshot: %v", err)
	}
	if len(bundle) == 0 {
		t.Fatalf("snapshot bundle should not be empty")
	}
	_ = db.Close()
	db.Free()

	restored, err := OpenNetDbFromSnapshot(r, NetDbConfig{
		OriginHash:   testOrigin,
		WithTasks:    true,
		WithMemories: true,
	}, bundle)
	if err != nil {
		t.Fatalf("OpenNetDbFromSnapshot: %v", err)
	}
	defer restored.Free()

	tasks2, err := restored.Tasks()
	if err != nil {
		t.Fatalf("restored.Tasks: %v", err)
	}
	defer tasks2.free()

	list, err := tasks2.List(nil)
	if err != nil {
		t.Fatalf("restored.List: %v", err)
	}
	found := false
	for _, task := range list {
		if task.ID == 42 && task.Title == "snapshot-me" {
			found = true
			break
		}
	}
	if !found {
		t.Fatalf("expected restored list to contain id=42; got %+v", list)
	}
}

// TestNetDbEmptySnapshotOpensFromScratch confirms `OpenNetDbFromSnapshot`
// with a nil/empty bundle is equivalent to `OpenNetDb`.
func TestNetDbEmptySnapshotOpensFromScratch(t *testing.T) {
	r := NewRedex("")
	defer r.Free()

	db, err := OpenNetDbFromSnapshot(r, NetDbConfig{
		OriginHash: testOrigin,
		WithTasks:  true,
	}, nil)
	if err != nil {
		t.Fatalf("OpenNetDbFromSnapshot(nil): %v", err)
	}
	defer db.Free()

	// Should be openable + queryable.
	tasks, err := db.Tasks()
	if err != nil {
		t.Fatalf("Tasks: %v", err)
	}
	defer tasks.free()
}

// TestNetDbAdapterSurvivesDbFree confirms an adapter handle returned
// from `Tasks()` is independent — freeing the NetDb does NOT close
// the underlying adapter.
func TestNetDbAdapterSurvivesDbFree(t *testing.T) {
	r := NewRedex("")
	defer r.Free()

	db, err := OpenNetDb(r, NetDbConfig{
		OriginHash: testOrigin,
		WithTasks:  true,
	})
	if err != nil {
		t.Fatalf("OpenNetDb: %v", err)
	}

	tasks, err := db.Tasks()
	if err != nil {
		t.Fatalf("Tasks: %v", err)
	}
	defer tasks.free()

	// Free the parent NetDb. The adapter handle should still work.
	db.Free()

	if _, err := tasks.Create(1, "post-free", 100); err != nil {
		t.Fatalf("tasks.Create after NetDb.Free should still succeed; got %v", err)
	}
}
