// Minimal sanity check for the Go binding.
//
// Run: go run hello.go
//
// What it proves: the module builds against the cdylib, a bus starts, a raw
// JSON event is accepted, and shutdown is clean.
//
// WHAT IT DELIBERATELY DOES NOT DO: read the event back.
//
// The default transport is memory, which selects the Noop adapter. Events flow
// producer -> ring buffer -> drain worker -> adapter, and the Noop adapter
// counts batches and discards them (adapter/noop.rs: "Just count, don't store";
// its poll_shard returns an empty result). So on memory transport Poll always
// returns zero events — a round-trip example here would spin forever.
//
// To actually receive events you need an adapter that retains them: Redis or
// JetStream, or the mesh transport between two nodes. See mesh.md.
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

	if err := bus.IngestRaw(`{"msg":"hello, mesh"}`); err != nil {
		log.Fatal(err)
	}

	// Stats returns (*Stats, error) in Go — the other bindings return the
	// struct directly. EventsIngested counts at the *producer* boundary.
	stats, err := bus.Stats()
	if err != nil {
		log.Fatal(err)
	}
	if stats.EventsIngested != 1 {
		log.Fatalf("the bus did not accept the event: ingested=%d", stats.EventsIngested)
	}

	fmt.Printf("accepted: ingested=%d dropped=%d\n", stats.EventsIngested, stats.EventsDropped)
}
