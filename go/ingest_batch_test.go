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
