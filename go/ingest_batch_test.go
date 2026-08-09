// A batch element that cannot be serialized must be reported.
//
// IngestBatch skipped values whose `json.Marshal` failed and ingested
// the rest, returning only the accepted count — so a caller could not
// tell a serialization omission from a ring-buffer drop. Those need
// different responses: a drop is backpressure and may be retried, a
// marshal failure is a payload bug that will fail identically forever.
// Rust, TypeScript and Python all serialize the whole batch first, so
// a bad element aborts rather than silently deleting itself.
//
// Pure Go — the marshal step runs before any cgo call.

package net

import (
	"encoding/json"
	"strings"
	"testing"
)

// unserializable has no JSON representation: channels cannot be marshalled.
type unserializable struct {
	Ch chan int
}

func TestIngestBatchChecked_ReportsTheFailingIndex(t *testing.T) {
	events := []interface{}{
		map[string]string{"ok": "1"},
		map[string]string{"ok": "2"},
		unserializable{Ch: make(chan int)},
		map[string]string{"ok": "4"},
	}

	var bs *Net // never reached: marshal fails before any cgo call
	n, err := bs.IngestBatchChecked(events)

	if err == nil {
		t.Fatal("a batch containing an unserializable value must return an error")
	}
	if n != 0 {
		t.Fatalf("nothing may be ingested when the batch is rejected, got %d", n)
	}
	// The index is the actionable part — without it the caller has to
	// bisect a batch of thousands to find the offending payload.
	if !strings.Contains(err.Error(), "index 2") {
		t.Fatalf("error must name the failing index, got %v", err)
	}
}

func TestIngestBatchChecked_FailsOnTheFirstBadElement(t *testing.T) {
	// Two bad elements: the error must name the first, so a caller
	// fixing them one at a time makes progress deterministically.
	events := []interface{}{
		unserializable{Ch: make(chan int)},
		unserializable{Ch: make(chan int)},
	}

	var bs *Net
	_, err := bs.IngestBatchChecked(events)
	if err == nil {
		t.Fatal("expected an error")
	}
	if !strings.Contains(err.Error(), "index 0") {
		t.Fatalf("want the first bad index, got %v", err)
	}
}

// The unchecked path keeps its documented skip-and-continue contract.
//
// It briefly delegated to IngestBatchChecked and discarded the error,
// so one bad element dropped the entire batch and returned 0 with
// nothing to inspect — strictly worse than the skip it documents, and
// a silent behaviour change for every existing caller. The two
// functions answer different questions and must not share a body.
//
// Asserted on the marshal stage alone: a nil receiver panics inside
// IngestRawBatch, so reaching cgo is itself the evidence that the
// batch was not abandoned at index 2.
func TestIngestBatch_SkipsUnserializableAndKeepsTheRest(t *testing.T) {
	events := []interface{}{
		map[string]string{"ok": "1"},
		map[string]string{"ok": "2"},
		unserializable{Ch: make(chan int)},
		map[string]string{"ok": "4"},
	}

	marshalled := 0
	for _, e := range events {
		if _, err := json.Marshal(e); err == nil {
			marshalled++
		}
	}
	if marshalled != 3 {
		t.Fatalf("test premise: want 3 serializable events, got %d", marshalled)
	}

	defer func() {
		// A panic from the nil receiver means the marshal loop ran to
		// completion and handed a non-empty slice to IngestRawBatch.
		// An abort at index 2 would have returned before that.
		if recover() == nil {
			t.Fatal("expected IngestRawBatch to be reached on the nil receiver")
		}
	}()
	var bs *Net
	bs.IngestBatch(events)
}

func TestIngestBatchChecked_WrapsTheMarshalCause(t *testing.T) {
	var bs *Net
	_, err := bs.IngestBatchChecked([]interface{}{unserializable{Ch: make(chan int)}})
	if err == nil {
		t.Fatal("expected an error")
	}
	// %w, not %v — a caller should be able to errors.As the
	// json.UnsupportedTypeError underneath.
	if !strings.Contains(err.Error(), "json") && !strings.Contains(err.Error(), "chan") {
		t.Fatalf("error should carry the marshal cause, got %v", err)
	}
}
