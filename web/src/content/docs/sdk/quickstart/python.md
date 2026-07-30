## Build it — Python

```bash
pip install net-mesh-sdk      # the ergonomic SDK — imports as net_sdk
pip install net-mesh          # the native binding — imports as net
```

Install both. Two packages, two import names, and none of the four strings match:
`net-mesh-sdk` → `net_sdk`, `net-mesh` → `net`. A `ModuleNotFoundError` here almost
always means the right package is installed under the other name. **Python 3.10 or
newer.**

Parts of this spine are reachable only on `net`, never on `net_sdk`. That is not a
gap to work around — it is where the surface lives.

### A bus node

```python
from net_sdk import NetNode

with NetNode(shards=4) as node:
    node.emit({"sensor": "lidar", "range_m": 12.5})
    node.emit_raw('{"sensor": "radar", "range_m": 45.0}')
    node.emit_batch([{"a": 1}, {"a": 2}, {"a": 3}])
```

The context manager does the shutdown and drain. Leaving it out is how a Python
program exits with a drain worker still holding events.

Transports are constructor arguments: `NetNode(shards=4)` is memory,
`NetNode(shards=4, redis_url="redis://localhost:6379")` and `jetstream_url=…` use
those backends behind the same `emit` / `subscribe` code.

### A mesh node

```python
from net_sdk import MeshNode

node = MeshNode(bind_addr="127.0.0.1:9000", psk="42" * 32)
```

Python takes the PSK as a **64-character hex string**, not raw bytes. `"42" * 32`
is the hex form of the same 32 `0x42` bytes Rust and TypeScript pass as an array —
if you are pairing a Python node with one of those, this is the line where the two
representations have to agree.

`MeshNode` is a plain object with no context manager. Call `shutdown()` yourself.

### The handshake

```python
HOST_ADDR = "127.0.0.1:9001"
host = MeshNode(bind_addr=HOST_ADDR, psk="42" * 32)
agent = MeshNode(bind_addr="127.0.0.1:9000", psk="42" * 32)

host.accept(agent.node_id)
agent.connect(HOST_ADDR, host.public_key, host.node_id)
host.start()
agent.start()
```

`node_id` and `public_key` are **properties, not methods** — no parentheses. It is
a small thing that produces a confusing error, because `host.public_key` without
the call still evaluates to something and gets passed along.

### Verify it worked

```python
from net_sdk import NetNode

with NetNode(shards=1) as node:
    node.emit({"msg": "hello, mesh"})

    stats = node.stats()
    assert stats.events_ingested == 1, "the bus did not accept the event"
    print(f"accepted: ingested={stats.events_ingested}")
```

Expect one line, `accepted: ingested=1`. The counter is read at the producer
boundary: accepted by this node, not received by anyone.

Next: [Announce a capability](/docs/sdk/python/announce).
