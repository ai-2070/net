// Observe a drop (Go).
//
// Run: go run observe.go
//
// What it proves: Stats() reports drops, and the Go field names are Go's, not
// the C header's.
//
// WHY THIS EXISTS. Backpressure is silent by default — IngestRaw returns nil
// while events are being dropped. The only evidence is EventsDropped.
//
// TWO NAMING TRAPS, both of which are compile errors rather than bugs:
//
//   - The Go struct does NOT use the C header's snake_case names. It is
//     EventsIngested / EventsDropped, not events_ingested / events_dropped.
//     The JSON tags carry the snake_case form; the Go fields do not.
//   - The batch counter is spelled BatchesDispathed — missing the second 'c'.
//     That typo is the actual field name in the shipped module, so
//     `stats.BatchesDispatched` does not compile.
package main

import (
	"fmt"
	"log"

	"github.com/ai-2070/net/go"
)

func main() {
	bus, err := net.New(&net.Config{NumShards: 1})
	if err != nil {
		log.Fatal(err)
	}
	defer bus.Shutdown()

	// Push far more than the buffer holds, with nothing polling.
	for i := 0; i < 1000; i++ {
		// Returns nil even while events are being dropped.
		if err := bus.IngestRaw(`{"seq":1}`); err != nil {
			log.Fatal(err)
		}
	}

	// Stats returns (*Stats, error) — unlike the other bindings, which return
	// the struct directly.
	stats, err := bus.Stats()
	if err != nil {
		log.Fatal(err)
	}

	fmt.Printf("ingested=%d dropped=%d batches=%d\n",
		stats.EventsIngested,
		stats.EventsDropped,
		stats.BatchesDispathed, // sic — see the header comment
	)

	if stats.EventsDropped > 0 {
		fmt.Println("drops happened and nothing raised an error — alert on this counter")
	}
}
