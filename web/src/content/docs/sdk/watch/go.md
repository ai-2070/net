## Watch it — Go

Go is cursor-based. There is no subscribe iterator; you poll and page forward.

```go
cursor := ""
for {
    resp, err := bus.Poll(100, cursor)
    if err != nil {
        log.Fatal(err)
    }
    for _, ev := range resp.Events {
        var reading struct {
            SensorID string  `json:"sensor_id"`
            Celsius  float64 `json:"celsius"`
        }
        if err := json.Unmarshal(ev, &reading); err == nil && reading.Celsius > 80 {
            fmt.Printf("HOT: %s at %.1fC\n", reading.SensorID, reading.Celsius)
        }
    }
    if resp.NextID == "" {
        break        // caught up
    }
    cursor = resp.NextID
}
```

`Poll(limit, cursor)` returns a `*PollResponse` with `Events []json.RawMessage` and
a `NextID`. An empty string cursor starts from the earliest buffered event; an
empty `NextID` means you have caught up to the tail.

Events arrive as `json.RawMessage`, so you unmarshal each one yourself. There is no
typed subscribe to do it for you.

### For a live loop

```go
for {
    resp, err := bus.Poll(100, cursor)
    if err != nil {
        log.Fatal(err)
    }
    // ... handle resp.Events ...
    if resp.NextID != "" {
        cursor = resp.NextID
    }
    time.Sleep(200 * time.Millisecond)   // your cadence, your choice
}
```

**Keep the cursor when `NextID` comes back empty.** Overwriting it with the empty
string restarts from the earliest buffered event and replays everything you have
already handled. That is the one bug this loop shape reliably produces.

The polling cadence is yours to pick. Nothing in the binding chooses it for you,
and there is no push path to fall back to — this is the binding asymmetry described
above, not a gap waiting to be filled.

### Verify it worked

```go
stats, err := bus.Stats()
if err != nil {
    log.Fatal(err)
}
fmt.Printf("consumed against %d ingested\n", stats.EventsIngested)
if stats.EventsIngested == 0 {
    log.Fatal("nothing was ever accepted to watch")
}
```

If `Poll` returns nothing, check the transport before the loop: memory counts
events and discards them. See [Quickstart](/docs/sdk/go/quickstart).

Next: [Move artifacts](/docs/sdk/go/artifacts).
