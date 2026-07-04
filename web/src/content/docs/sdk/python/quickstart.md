# Python — Quickstart

```bash
pip install net-mesh-sdk
```

## A node that emits events

```python
from net_sdk import NetNode

# Context manager handles shutdown/drain for you.
with NetNode(shards=4) as node:
    node.emit({"sensor": "lidar", "range_m": 12.5})
    node.emit_raw('{"sensor": "radar", "range_m": 45.0}')
    node.emit_batch([{"a": 1}, {"a": 2}, {"a": 3}])

    for event in node.subscribe(limit=10, timeout=5.0):
        print("event", event)

    stats = node.stats()
    print(stats.events_ingested, "ingested,", stats.events_dropped, "dropped")
```

`emit` returns once the event is accepted into the local ring buffer —
confirmation of acceptance, not delivery (see
[Submitted Is Not Completed](/docs/guides/submitted-is-not-completed)). Under
backpressure events can drop; check `stats().events_dropped`.

Transports are constructor arguments — `NetNode(shards=4)` is memory;
`NetNode(shards=4, redis_url="redis://localhost:6379")` and `jetstream_url=…` use
those backends with the same `emit`/`subscribe` code.

## The mesh node

For the agentic surface — tools and nRPC — create a `MeshNode`:

```python
from net_sdk import MeshNode

node = MeshNode(bind_addr="127.0.0.1:0", psk="42" * 32)   # psk is a 32-byte hex string
```

Note the PSK here is a **hex string** (not raw bytes as in Rust). From here the
loop is [Announce](/docs/sdk/python/announce) →
[Discover](/docs/sdk/python/discover) → [Invoke](/docs/sdk/python/invoke).

## Next

[Announce](/docs/sdk/python/announce). Same step in
[Rust](/docs/sdk/rust/announce) / [TypeScript](/docs/sdk/typescript/announce).
