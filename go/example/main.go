// Example usage of the Net Go bindings.
//
// What this example proves, and what it deliberately does not: `net.New(nil)`
// selects the default memory adapter, which COUNTS events and DISCARDS them.
// So every ingest below succeeds and `Poll` returns zero events — by design,
// not by failure. The observable success condition here is producer-side
// acceptance (`Stats().EventsIngested`), which the example asserts.
//
// Configure a real adapter — Redis, JetStream, or a mesh peer — before
// treating a non-empty `Poll` as the thing that worked. See
// https://ai2070.net/docs/sdk/go/watch.
package main

import (
	"encoding/json"
	"fmt"
	"log"
	"time"

	"github.com/ai-2070/net/go"
)

func main() {
	fmt.Printf("Net version: %s\n", net.Version())

	// Create event bus with default configuration
	bus, err := net.New(nil)
	if err != nil {
		log.Fatalf("Failed to create event bus: %v", err)
	}
	defer bus.Shutdown()

	fmt.Printf("Event bus created with %d shards\n", bus.NumShards())

	// Ingest some events using the fast raw path
	events := []string{
		`{"type": "token", "value": "hello", "index": 0}`,
		`{"type": "token", "value": "world", "index": 1}`,
		`{"type": "tool_call", "name": "search", "args": {"query": "AI"}}`,
	}

	for _, e := range events {
		if err := bus.IngestRaw(e); err != nil {
			log.Printf("Failed to ingest event: %v", err)
		}
	}
	fmt.Printf("Ingested %d events\n", len(events))

	// Ingest using Go structs
	type TokenEvent struct {
		Type  string `json:"type"`
		Value string `json:"value"`
		Index int    `json:"index"`
	}

	if err := bus.Ingest(TokenEvent{Type: "token", Value: "!", Index: 2}); err != nil {
		log.Printf("Failed to ingest struct event: %v", err)
	}

	// Batch ingest
	batchEvents := []string{
		`{"type": "token", "value": "batch1"}`,
		`{"type": "token", "value": "batch2"}`,
		`{"type": "token", "value": "batch3"}`,
	}
	ingested := bus.IngestRawBatch(batchEvents)
	fmt.Printf("Batch ingested %d events\n", ingested)

	// Give workers time to process
	time.Sleep(100 * time.Millisecond)

	// Statistics. This is the real success condition for this example: the
	// producer boundary accepted every event. Assert it rather than printing
	// it, so a regression that silently stops accepting fails the run instead
	// of scrolling past.
	const wantIngested = 7 // 3 raw + 1 struct + 3 batch
	stats, err := bus.Stats()
	if err != nil {
		log.Fatalf("Failed to get stats: %v", err)
	}
	fmt.Printf("Stats: ingested=%d, dropped=%d\n",
		stats.EventsIngested, stats.EventsDropped)
	if stats.EventsIngested != wantIngested {
		log.Fatalf("the bus did not accept every event: ingested=%d, want %d",
			stats.EventsIngested, wantIngested)
	}

	// Poll events.
	//
	// Expect ZERO here. The default memory adapter counts events and throws
	// them away, so there is nothing to read back. That is the configured
	// behavior, not a failure — but it is also why polling cannot be the
	// success condition for this program. Point the bus at a Redis,
	// JetStream, or mesh adapter and this same loop starts returning events.
	response, err := bus.Poll(100, "")
	if err != nil {
		log.Fatalf("Failed to poll: %v", err)
	}

	fmt.Printf("Polled %d events (has_more=%v)\n", response.Count, response.HasMore)
	if response.Count == 0 {
		fmt.Println("  (none — the default memory adapter discards events; " +
			"configure an adapter to read them back)")
	}
	for i, raw := range response.Events {
		var event map[string]interface{}
		if err := json.Unmarshal(raw, &event); err == nil {
			fmt.Printf("  Event %d: %v\n", i, event)
		}
	}

	// Pagination example
	if response.HasMore {
		nextResponse, err := bus.Poll(100, response.NextID)
		if err != nil {
			log.Printf("Failed to poll next page: %v", err)
		} else {
			fmt.Printf("Next page: %d events\n", nextResponse.Count)
		}
	}

	// Flush and shutdown
	if err := bus.Flush(); err != nil {
		log.Printf("Failed to flush: %v", err)
	}

	// Deliberately not "Done!". This program proved that the bus accepted 7
	// events; it did not prove that anything received them, and a closing
	// line that reads like success for the whole loop is how the empty poll
	// above gets mistaken for a working round trip.
	fmt.Printf("Accepted %d events at the producer boundary. "+
		"Nothing consumed them — that needs an adapter.\n", stats.EventsIngested)
}
