# Python — Watch the Event Stream

Invoking gets you one result; watching gets you the ongoing facts — what lets an
agent recover from a partial failure instead of trusting a single return value
([Submitted Is Not Completed](/docs/guides/submitted-is-not-completed)).

## Subscribe to typed events

`subscribe_typed` takes your event type (a `dataclass` or a Pydantic `BaseModel`)
and yields decoded instances:

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

Pydantic models work the same way — pass the model class as the first argument.

Subscriptions are **hot**: you see events emitted *after* you subscribe (plus
whatever is still in the ring buffer), not the whole history. There's no
replay-from-the-beginning on the bus — that's a durability decision (RedEX / an
adapter), covered in [Durable Logs](/docs/guides/durable-logs).

`subscribe(limit=…)` yields the raw events; `subscribe_typed(T, …)` decodes each
into `T`. Both take an optional `timeout` so the loop ends instead of blocking
forever.

## Location transparency

The same subscribe code works whether the publisher is in-process or several hops
away on the mesh. The concepts are in [Channels](/docs/concepts/channels) and
[Events and Causality](/docs/concepts/events-and-causality).
