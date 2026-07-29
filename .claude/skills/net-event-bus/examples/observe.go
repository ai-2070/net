// What the drop counters actually tell you (Go).
//
// Run: go run observe.go
//
// THE POINT OF THIS FILE, AND IT IS NOT WHAT YOU EXPECT.
//
// The usual advice — "backpressure is silent, so watch EventsDropped" — is
// wrong for the one mode where it matters most, and that mode is the default.
//
// Both counters sit at the producer boundary: they record what the bus
// accepted or refused from you, not what survived to an adapter.
//
//   - DropOldest (the default) evicts to make room, so IngestRaw always
//     succeeds and EventsDropped never moves. Events are lost and nothing
//     in-process says so.
//   - DropNewest and FailProducer refuse the producer instead, so the ingest
//     call and EventsDropped both see it.
//
// The exact counts vary between runs and between bindings — the drain worker
// races the producer. The shape is what matters, not the numbers.
//
// TWO GO NAMING TRAPS, both compile errors rather than bugs:
//
//   - The struct does NOT use the C header's snake_case names. It is
//     EventsIngested / EventsDropped. The JSON tags carry snake_case; the Go
//     fields do not.
//   - The batch counter is spelled BatchesDispathed — missing the second 'c'.
//     That typo is the actual field name, so stats.BatchesDispatched does not
//     compile.
package main

import (
	"fmt"
	"log"

	"github.com/ai-2070/net/go"
)

func measure(mode string) {
	// RingBufferCapacity must be a power of two and >= 1024 — both enforced at
	// construction, and neither is caught by the compiler.
	bus, err := net.New(&net.Config{
		NumShards:          1,
		RingBufferCapacity: 1024,
		BackpressureMode:   mode,
	})
	if err != nil {
		log.Fatal(err)
	}
	defer bus.Shutdown()

	refused := 0
	for i := 0; i < 20000; i++ {
		if err := bus.IngestRaw(`{"seq":1}`); err != nil {
			refused++
		}
	}

	// Stats returns (*Stats, error) in Go — the other bindings return the
	// struct directly.
	stats, err := bus.Stats()
	if err != nil {
		log.Fatal(err)
	}
	fmt.Printf("%-14s ingested=%-6d dropped=%-6d batches=%-6d ingest_errors=%d\n",
		mode, stats.EventsIngested, stats.EventsDropped,
		stats.BatchesDispathed, // sic — see the header comment
		refused)
}

func main() {
	fmt.Println("20,000 events into a 1024-slot buffer, nothing consuming:")
	fmt.Println()
	// Either casing parses: "DropOldest" and "drop_oldest" are the same thing.
	// An unrecognised value is rejected outright rather than defaulting.
	for _, mode := range []string{"DropOldest", "DropNewest", "FailProducer"} {
		measure(mode)
	}
	fmt.Println()
	fmt.Println("DropOldest loses events silently — and it is the default.")
	fmt.Println("The other two refuse the producer, so ingest and the counter agree.")
}
