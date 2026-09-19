// The event log you no longer run (Go).
//
// One node, one local append-only log. Eight records are appended, replayed
// from the start, then replayed again from a consumer checkpoint — and the
// second replay is three records, not eight, because the offset is the
// consumer's own bookkeeping and the log is not asked to remember it.
//
// Kafka needs a cluster for this. RedexFile is one file with a monotonic
// sequence, and the consumer's position lives in the consumer.
//
// Run: go run eventlog.go
//
// Expected final line: RESULT ok records=8 replayed=8 resumed=3
package main

import (
	"fmt"
	"log"

	mesh "github.com/ai-2070/net/go"
)

// Records appended to the log.
const records = 8

// The consumer's checkpoint: the next sequence it has NOT processed.
// Records 0..4 are done, so it resumes at 5.
const checkpoint = 5

func main() {
	redex := mesh.NewRedex("")
	defer redex.Free()

	logFile, err := redex.OpenFile("audit/events", nil)
	if err != nil {
		log.Fatalf("open file: %v", err)
	}
	defer func() { _ = logFile.Close() }()

	for index := range records {
		seq, err := logFile.Append([]byte(fmt.Sprintf("event:%d", index)))
		if err != nil {
			log.Fatalf("append: %v", err)
		}
		fmt.Printf("appended event %d at seq %d\n", index, seq)
	}

	// Full replay. Every record, in append order — the log does not need a
	// broker to hand them back.
	all, err := logFile.ReadRange(0, logFile.Len())
	if err != nil {
		log.Fatalf("read range: %v", err)
	}

	// The consumer's checkpoint is application state: it holds the next
	// sequence the consumer has not processed, and the consumer reads from
	// there. ReadRange is half-open — [start, end) — so the checkpoint is
	// the start bound verbatim: no +1, and no record replayed twice.
	// Nothing is committed on the log's side, which is why a second consumer
	// with a different checkpoint costs the log nothing.
	resumed, err := logFile.ReadRange(checkpoint, logFile.Len())
	if err != nil {
		log.Fatalf("read range: %v", err)
	}

	// Replay is a read, not a transformation: the same range yields the same
	// sequence, so a fold over it is reproducible.
	again, err := logFile.ReadRange(0, logFile.Len())
	if err != nil {
		log.Fatalf("read range: %v", err)
	}
	stable := len(again) == len(all)
	for i := 0; stable && i < len(all); i++ {
		if again[i].Seq != all[i].Seq {
			stable = false
		}
	}

	fmt.Printf("records in the log: %d\n", logFile.Len())
	fmt.Printf("replayed from 0:    %d\n", len(all))
	fmt.Printf("resumed from %d:     %d\n", checkpoint, len(resumed))
	fmt.Printf("replay is stable:   %v\n", stable)

	fmt.Printf("RESULT ok records=%d replayed=%d resumed=%d\n", logFile.Len(), len(all), len(resumed))
}
