package net

import (
	"errors"
	"testing"
)

// TestMeshDbReaderRunnerHappyPath exercises the minimal end-to-end:
// build a reader, append events, build a runner, execute a query,
// drain results.
func TestMeshDbReaderRunnerHappyPath(t *testing.T) {
	r := NewMeshDbReader()
	defer r.Free()

	// Seed three events on origin 0xABCD.
	for i, payload := range []string{"alpha", "beta", "gamma"} {
		if err := r.Append(0xABCD, uint64(i), []byte(payload)); err != nil {
			t.Fatalf("append %d: %v", i, err)
		}
	}

	runner, err := NewMeshDbRunner(r)
	if err != nil {
		t.Fatalf("NewMeshDbRunner: %v", err)
	}
	defer runner.Free()

	// QueryAt — single row.
	q, err := QueryAt(0xABCD, 1)
	if err != nil {
		t.Fatalf("QueryAt: %v", err)
	}
	defer q.Free()

	it, err := runner.Execute(q)
	if err != nil {
		t.Fatalf("Execute: %v", err)
	}
	defer it.Free()

	rows, err := it.Drain()
	if err != nil {
		t.Fatalf("Drain: %v", err)
	}
	if len(rows) != 1 {
		t.Fatalf("expected 1 row, got %d", len(rows))
	}
	if rows[0].Origin != 0xABCD || rows[0].Seq != 1 {
		t.Fatalf("row mismatch: %+v", rows[0])
	}
	if string(rows[0].Payload) != "beta" {
		t.Fatalf("payload mismatch: %q", rows[0].Payload)
	}
}

func TestMeshDbQueryBetweenRejectsEmptyRange(t *testing.T) {
	_, err := QueryBetween(0, 5, 5)
	if !errors.Is(err, ErrMeshDbInvalidArg) {
		t.Fatalf("expected ErrMeshDbInvalidArg, got %v", err)
	}
}

func TestMeshDbReaderFreedRejectsAppend(t *testing.T) {
	r := NewMeshDbReader()
	r.Free()
	if err := r.Append(1, 0, []byte("x")); err == nil {
		t.Fatalf("expected error after Free")
	}
}

// TestMeshDbRunnerSurvivesReaderFree confirms the substrate Arc clone
// keeps the runner valid after its source reader has been freed.
func TestMeshDbRunnerSurvivesReaderFree(t *testing.T) {
	r := NewMeshDbReader()
	if err := r.Append(1, 0, []byte("x")); err != nil {
		t.Fatalf("append: %v", err)
	}

	runner, err := NewMeshDbRunner(r)
	if err != nil {
		t.Fatalf("runner: %v", err)
	}
	defer runner.Free()

	r.Free()  // source reader gone

	q, err := QueryLatest(1)
	if err != nil {
		t.Fatalf("QueryLatest: %v", err)
	}
	defer q.Free()

	it, err := runner.Execute(q)
	if err != nil {
		t.Fatalf("Execute after reader.Free: %v", err)
	}
	defer it.Free()
	rows, err := it.Drain()
	if err != nil {
		t.Fatalf("Drain: %v", err)
	}
	if len(rows) != 1 || string(rows[0].Payload) != "x" {
		t.Fatalf("expected one row 'x', got %+v", rows)
	}
}
