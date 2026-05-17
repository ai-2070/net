// NetDb binding tests — exercise the surface declared in netdb.go.
//
// # Run prerequisites
//
// This binding tier (`bindings/go/net/`) ships without a `go.mod` —
// it's the reference implementation for downstream Go consumers. To
// run these tests, wire the directory into your own go.mod (see
// `redex.go` for the cdylib build prerequisite + CGO_LDFLAGS notes).
//
// # Coverage summary
//
//   - TestNetDbOpenAccessorsAndClose — happy path: open both adapters,
//     mutate through each, idempotent close.
//   - TestNetDbUnenabledAccessorErrors — accessor returns ErrNetDb when
//     the model wasn't enabled at open time.
//   - TestNetDbSnapshotRoundtrip — Snapshot() + OpenNetDbFromSnapshot()
//     preserves a task across DB rebuild.
//   - TestNetDbEmptySnapshotOpensFromScratch — nil bundle is equivalent
//     to OpenNetDb.
//   - TestNetDbAdapterSurvivesDbFree — confirms the documented
//     Arc-clone semantics: freeing the NetDb leaves child adapter
//     handles functional.

package net

import (
	"errors"
	"testing"
	"time"
)

const testOriginNetDb uint64 = 0xC0DE_FEED_BEEF_DEAD

// TestNetDbOpenAccessorsAndClose covers the happy path: open both
// adapters via the NetDb accessors, mutate through each, close.
func TestNetDbOpenAccessorsAndClose(t *testing.T) {
	r := NewRedex()
	defer r.Close()

	db, err := OpenNetDb(r, NetDbConfig{
		OriginHash:   testOriginNetDb,
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
	defer tasks.Close()
	if _, err := tasks.Create(1, "first", 1_000_000); err != nil {
		t.Fatalf("tasks.Create: %v", err)
	}

	memories, err := db.Memories()
	if err != nil {
		t.Fatalf("Memories(): %v", err)
	}
	defer memories.Close()

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
	r := NewRedex()
	defer r.Close()

	db, err := OpenNetDb(r, NetDbConfig{
		OriginHash:   testOriginNetDb,
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
	_ = tasks.Close()

	// Memories fails with ErrNetDb (model disabled).
	if _, err := db.Memories(); !errors.Is(err, ErrNetDb) {
		t.Fatalf("Memories() should error with ErrNetDb; got %v", err)
	}
}

// TestNetDbSnapshotRoundtrip captures a bundle, frees the source DB,
// restores into a fresh DB, and confirms the original task survives.
func TestNetDbSnapshotRoundtrip(t *testing.T) {
	r := NewRedex()
	defer r.Close()

	db, err := OpenNetDb(r, NetDbConfig{
		OriginHash:   testOriginNetDb,
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
	token, err := tasks.Create(42, "snapshot-me", 100)
	if err != nil {
		t.Fatalf("Create: %v", err)
	}
	if err := tasks.WaitForSeq(token.Seq, 2*time.Second); err != nil {
		t.Fatalf("WaitForSeq: %v", err)
	}
	_ = tasks.Close()

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
		OriginHash:   testOriginNetDb,
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
	defer tasks2.Close()

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
	r := NewRedex()
	defer r.Close()

	db, err := OpenNetDbFromSnapshot(r, NetDbConfig{
		OriginHash: testOriginNetDb,
		WithTasks:  true,
	}, nil)
	if err != nil {
		t.Fatalf("OpenNetDbFromSnapshot(nil): %v", err)
	}
	defer db.Free()

	tasks, err := db.Tasks()
	if err != nil {
		t.Fatalf("Tasks: %v", err)
	}
	defer tasks.Close()
}

// TestNetDbAdapterSurvivesDbFree confirms an adapter handle returned
// from `Tasks()` is independent — freeing the NetDb does NOT close
// the underlying adapter. Matches the substrate contract documented
// at `net_cortex.h:135-138`.
func TestNetDbAdapterSurvivesDbFree(t *testing.T) {
	r := NewRedex()
	defer r.Close()

	db, err := OpenNetDb(r, NetDbConfig{
		OriginHash: testOriginNetDb,
		WithTasks:  true,
	})
	if err != nil {
		t.Fatalf("OpenNetDb: %v", err)
	}

	tasks, err := db.Tasks()
	if err != nil {
		t.Fatalf("Tasks: %v", err)
	}
	defer tasks.Close()

	// Free the parent NetDb. The adapter handle should still work.
	db.Free()

	if _, err := tasks.Create(1, "post-free", 100); err != nil {
		t.Fatalf("tasks.Create after NetDb.Free should still succeed; got %v", err)
	}
}

// TestNetDbNilRedex confirms OpenNetDb rejects a nil redex without
// segfaulting into the FFI.
func TestNetDbNilRedex(t *testing.T) {
	if _, err := OpenNetDb(nil, NetDbConfig{WithTasks: true}); !errors.Is(err, ErrNetDb) {
		t.Fatalf("OpenNetDb(nil) should error with ErrNetDb; got %v", err)
	}
	if _, err := OpenNetDbFromSnapshot(nil, NetDbConfig{WithTasks: true}, nil); !errors.Is(err, ErrNetDb) {
		t.Fatalf("OpenNetDbFromSnapshot(nil) should error with ErrNetDb; got %v", err)
	}
}
