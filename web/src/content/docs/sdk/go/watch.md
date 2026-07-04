# Go — Watch the Event Stream

The Go binding is **poll-based** — there is no async subscribe iterator. You watch
by polling with a cursor and paging forward. This is the binding asymmetry called
out in the [SDK index](/docs/sdk/go): Rust/TS/Python give you an async stream; Go
(and C) give you `Poll`.

```go
cursor := ""
for {
    resp, err := bus.Poll(100, cursor)
    if err != nil {
        log.Fatal(err)
    }
    for _, ev := range resp.Events {
        // ev is json.RawMessage — decode into your type
        var reading struct {
            SensorID string  `json:"sensor_id"`
            Celsius  float64 `json:"celsius"`
        }
        if err := json.Unmarshal(ev, &reading); err == nil && reading.Celsius > 80 {
            fmt.Printf("HOT: %s at %.1fC\n", reading.SensorID, reading.Celsius)
        }
    }
    if resp.NextID == "" {
        break            // caught up — or keep polling on an interval for a live loop
    }
    cursor = resp.NextID
}
```

Consumption is **hot**: you see events from where the cursor points forward, not
the whole history. Durable replay from an offset is a persistence decision (RedEX /
an adapter), covered in [Durable Logs](/docs/guides/durable-logs).

For a live loop, keep calling `Poll` on an interval; an empty `NextID` means you've
caught up to the tail.
