## Watch it — Python

### Subscribe to typed events

```python
from dataclasses import dataclass
from net_sdk import NetNode

@dataclass
class TemperatureReading:
    sensor_id: str
    celsius: float

with NetNode(shards=4) as node:
    for reading in node.subscribe_typed(TemperatureReading, limit=100, timeout=5.0):
        if reading.celsius > 80:
            print(f"HOT: {reading.sensor_id} at {reading.celsius}C")
```

`subscribe_typed(T, …)` takes the type as its **first positional argument** — a
`dataclass` or a Pydantic `BaseModel` — and yields decoded instances.
`subscribe(limit=…)` yields the raw events.

Python's is a **synchronous** iterator, unlike the async iterables in TypeScript
and the `Stream` in Rust. It blocks the calling thread. That matters for where you
put it: a `for` loop over `subscribe_typed` in an asyncio program blocks the event
loop.

### Always pass a timeout

```python
for reading in node.subscribe_typed(TemperatureReading, limit=100, timeout=5.0):
    ...
```

Both `subscribe` and `subscribe_typed` take an optional `timeout`. Without one the
loop blocks indefinitely waiting for an event that may never come — which on the
default memory transport is the normal case, not the exceptional one.

### Verify it worked

```python
stats = node.stats()
print(f"consumed against {stats.events_ingested} ingested")
assert stats.events_ingested > 0, "nothing was ever accepted to watch"
```

If the loop yields nothing, check the transport before the subscription: memory
counts events and discards them, so emitting and then subscribing waits for
something already gone. See [Quickstart](/docs/sdk/python/quickstart).

Next: [Move artifacts](/docs/sdk/python/artifacts).
