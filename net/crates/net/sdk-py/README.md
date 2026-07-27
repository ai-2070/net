# Net Python SDK

Ergonomic Python SDK for the Net mesh — a latency-first encrypted mesh where
services and agents announce capabilities, discover each other, and invoke work
over typed RPC.

Wraps the `net` PyO3 bindings with generators, typed events, typed channels,
and a Pythonic API.

**Docs: <https://ai2070.net/docs/sdk/python/quickstart>** ·
[Concepts](https://ai2070.net/docs/concepts/architecture)

## Install

```bash
pip install net-mesh-sdk
```

The package publishes as `net-mesh-sdk` and imports as `net_sdk`. The native
binding `net-mesh` comes in transitively.

## Quickstart

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
acceptance, not delivery. Under backpressure events can drop; check
`stats().events_dropped`. See
[Submitted Is Not Completed](https://ai2070.net/docs/guides/submitted-is-not-completed).

Transports are constructor arguments — `NetNode(shards=4)` is memory;
`NetNode(shards=4, redis_url="redis://localhost:6379")` and `jetstream_url=…`
use those backends with the same `emit` / `subscribe` code.

## The mesh node

For the agentic surface — capabilities, tools, nRPC — create a `MeshNode`:

```python
from net_sdk import MeshNode

node = MeshNode(bind_addr="127.0.0.1:0", psk="42" * 32)   # 32-byte hex string
```

Note the PSK is a **hex string** here, not raw bytes as in Rust. From there the
loop is
[Announce](https://ai2070.net/docs/sdk/python/announce) →
[Discover](https://ai2070.net/docs/sdk/python/discover) →
[Invoke](https://ai2070.net/docs/sdk/python/invoke) →
[Watch](https://ai2070.net/docs/sdk/python/watch).

## Two packages, and what lives where

`net_sdk` is the pure-Python wrapper; every method dispatches into the `net`
PyO3 binding. Most things you want are re-exported on `net_sdk`, but a few
surfaces are only on `net` — notably `RedisStreamDedup` and the `Rpc*Error`
classes. Import those directly:

```python
from net import RedisStreamDedup
```

Which classes are reachable at all depends on the Cargo features the binding
was built with. **PyPI wheels ship every feature enabled**; that only matters
for source builds via `maturin develop`, where a disabled feature's classes are
simply absent.

## What's here

| Surface | Guide |
|---|---|
| Event bus — shards, typed streams, backpressure, Redis / JetStream | [Event bus](https://ai2070.net/docs/guides/event-bus) |
| Mesh streams — direct peer-to-peer, windowed | [Mesh streams](https://ai2070.net/docs/guides/mesh-streams) |
| Capabilities — announce and discover | [Discover and invoke](https://ai2070.net/docs/guides/discover-and-invoke) |
| nRPC — typed request/response and streaming | [Typed RPC](https://ai2070.net/docs/guides/nrpc) |
| Channels — hierarchical pub/sub with capability auth | [Channels](https://ai2070.net/docs/concepts/channels) |
| RedEX / CortEX / NetDB — logs, folds, queries | [Durable logs](https://ai2070.net/docs/guides/durable-logs), [Folds](https://ai2070.net/docs/guides/cortex-folds), [NetDB](https://ai2070.net/docs/guides/netdb-queries) |
| MeshDB — federated queries | [NetDB](https://ai2070.net/docs/guides/netdb-queries#federated-queries-meshdb) |
| Dataforts — blobs, greedy cache, data gravity | [Blob storage](https://ai2070.net/docs/guides/dataforts) |
| Compute + Groups — daemons, migration, replica/fork/standby | [Daemons](https://ai2070.net/docs/guides/daemons-and-placement), [Continuity](https://ai2070.net/docs/guides/continuity-and-migration) |
| Deck — the operator surface | [Deck](https://ai2070.net/docs/reference/deck) |
| Security — identity, delegable tokens, subnets | [Identity](https://ai2070.net/docs/concepts/identity), [Security model](https://ai2070.net/docs/concepts/security-model) |
| Errors — the full exception hierarchy | [Errors](https://ai2070.net/docs/sdk/python/errors) |
| Redis Streams dedup | [Deduplication](https://ai2070.net/docs/reference/redis-dedup) |

## License

MIT OR Apache-2.0
