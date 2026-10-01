# Net Python SDK

**A latency-first encrypted mesh where services and agents announce what they
can do, discover each other at runtime, and invoke work over typed RPC.**

[![PyPI](https://img.shields.io/pypi/v/net-mesh-sdk.svg)](https://pypi.org/project/net-mesh-sdk/)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](#license)

There is no broker. Every node is a peer on a flat, encrypted topology. A node
publishes the capabilities it has — a GPU, a model, a tool, a licensed seat —
and other nodes find it by *what it can do*, not by hostname. Credentials never
leave the node that holds them: the machine with the secret runs the work.

```python
# Find something that can do the job, then do it — no registry, no config.
# `rpc` is the TypedMeshRpc handle built in "The loop" below.
resp = call_tool(rpc, "summarize", {"text": text})
```

## Why this instead of a queue

Kafka, NATS and Redis Streams move bytes between a fixed producer and a fixed
consumer through a broker you operate. That's a different problem. Net is for
when **the set of participants isn't known in advance** and **work has state
worth observing**:

| You need | Net gives you |
|---|---|
| Tools that appear and vanish at runtime | Capability announce + discovery, event-driven — no polling, no service registry |
| Work spread across machines or organizations | One flat encrypted mesh; multi-hop discovery bounded at 16 hops |
| Credentials that must not travel | The node holding the secret executes; callers never see it |
| More than "did it return 200" | Durable logs, folded state, artifacts, streams, and replayable recovery |
| GPUs matched by capability, not hostname | Hardware is a discoverable characteristic, with atomic gang-claim under contention |

**Don't** reach for it when one API call solves the problem, when a single
server and database are enough, or when you have a fixed producer, a fixed
consumer and a broker you're happy operating. The honest version of that list
is in [When to Use Net](https://ai2070.net/docs/worldview/right-and-wrong-use-cases).

## Install

```bash
pip install net-mesh-sdk
```

Publishes as `net-mesh-sdk`, imports as `net_sdk`. The native binding
`net-mesh` comes in transitively; PyPI wheels ship every feature enabled.

## The loop: announce → discover → invoke

**Announce** a tool, and it becomes discoverable across the mesh:

```python
from net_sdk import MeshNode, add_tool_capabilities_to_announce, serve_tool

node = MeshNode(bind_addr="127.0.0.1:0", psk="42" * 32)   # 32-byte hex PSK
rpc = node.rpc()   # a TypedMeshRpc; the tool surface takes it. Build once, reuse.

def web_search(req):
    return {"results": [f"first hit for '{req['query']}'"]}

handle = serve_tool(
    rpc,
    {"name": "web_search", "description": "Search the web.", "tags": ["web", "research"]},
    web_search,
)
# handle stays alive while the tool is served; close it to withdraw.
# Announce the capability (the ai-tool:<tool_id> tag + tools[] entry) so
# peers can discover it — registering the handler alone is not enough.
caps = add_tool_capabilities_to_announce({}, [handle.descriptor])
node.announce_capabilities(caps)
```

**Discover** — react to the mesh changing rather than polling it:

```python
for t in node.list_tools():                 # baseline snapshot
    print(t.tool_id, "v" + str(t.version), "tags=", t.tags)

async for change in node.watch_tools():     # pushed on fold mutation
    print(change)   # added, removed, or publisher count changed
```

**Invoke** — `call_tool` finds a provider for the name and calls it:

```python
from net_sdk import call_tool, call_tool_async, call_tool_streaming
from net.mesh_rpc import AsyncTypedMeshRpc

resp = call_tool(rpc, "web_search", {"query": "how does the capability fold work"})

arpc = AsyncTypedMeshRpc.from_mesh(node._native)
resp = await call_tool_async(arpc, "web_search", {"query": "…"})

for chunk in call_tool_streaming(rpc, "tail", {"tail": "events"}):
    handle_chunk(chunk)
```

For services rather than tools, nRPC gives you the same shape with deadlines,
streaming and cancellation — [Typed RPC](https://ai2070.net/docs/guides/nrpc).

## Mesh channels

A publisher registers a channel; peers subscribe; `publish` fans a payload out
to the roster and reports what reached whom. (`NetNode.channel` is a different
thing: a channel on the local bus.)

```python
from net_sdk import ChannelAuthError, MeshNode

publisher = MeshNode("127.0.0.1:0", "42" * 32)
subscriber = MeshNode("127.0.0.1:0", "42" * 32)
# ...connect them (subscriber.connect(...) / publisher.accept(...)) and start
# both, as in the quickstart.

publisher.register_channel(
    "sensors/temp",
    visibility="global",                      # or "subnet-local", "parent-visible", "exported"
    reliable=True,
    subscribe_caps={"require_tags": ["reader"]},  # who may subscribe
)
# The publisher checks the subscriber's announced capabilities, so announce
# the tag first (and let the announcement reach the publisher).
subscriber.announce_capabilities({"tags": ["reader"]})
subscriber.subscribe_channel(publisher.node_id, "sensors/temp")  # ChannelAuthError if refused

report = publisher.publish("sensors/temp", b'{"c": 22.5}', reliability="reliable")
print(report["attempted"], report["delivered"], report["errors"])

for event in subscriber.recv(64):             # every shard; events carry no sender
    print(event.raw)
```

`recv` can't tell you who sent an event. When you authorize by peer, read the
stream through `open_stream_inbox(stream_id)` instead: each item carries the
peer whose session authenticated it.

## Compute and groups

Daemons run on a node, snapshot their state, and migrate between nodes. Groups
replicate them.

```python
from net import Identity
from net_sdk.compute import CausalEvent, DaemonRuntime
from net_sdk.groups import ReplicaGroup

class Counter:                                # the MeshDaemon protocol
    def __init__(self): self.n = 0
    def process(self, event): self.n += 1; return [self.n.to_bytes(4, "little")]
    def snapshot(self): return self.n.to_bytes(4, "little")
    def restore(self, state): self.n = int.from_bytes(state, "little")

rt = DaemonRuntime(node)                       # takes the SDK node directly
rt.register_factory("counter", Counter)
rt.start()

ident = Identity.generate()
daemon = rt.spawn("counter", ident)
rt.deliver(daemon.origin_hash, CausalEvent(ident.origin_hash, 1, b""))

group = ReplicaGroup.spawn(rt, "counter", 3, b"\x01" * 32, "consistent-hash")
target = group.route_event({"routing_key": "user-42"})
```

A Python daemon's `process` runs under the GIL: right for control-plane work,
not for a hot loop.

## Async

`AsyncMeshNode` is the asyncio twin, built over the same native node:

```python
from net_sdk import AsyncMeshNode

node = AsyncMeshNode("127.0.0.1:0", "42" * 32)
await node.connect(peer_addr, peer_public_key, peer_node_id)
node.start()
await node.subscribe_channel(peer_node_id, "sensors/temp")
async for event in node.events():             # cancel the task to stop
    print(event.raw)
```

`AsyncMeshNode.from_node(node)` wraps an existing `MeshNode`, and `node.sync`
reaches the sync API.

## The bus

`NetNode` is the other node type: a sharded, in-process event bus with explicit
backpressure.

```python
from net_sdk import NetNode

# Context manager handles shutdown/drain for you. `redis_url` because the
# default (memory) transport selects the Noop adapter: it counts batches and
# discards them, so `subscribe` would return nothing here.
with NetNode(shards=4, redis_url="redis://127.0.0.1:6379") as node:
    node.emit({"sensor": "lidar", "range_m": 12.5})
    node.emit_raw('{"sensor": "radar", "range_m": 45.0}')
    node.emit_batch([{"a": 1}, {"a": 2}, {"a": 3}])

    for event in node.subscribe(limit=10, timeout=5.0):
        print("event", event)

    stats = node.stats()
    print(stats.events_ingested, "ingested,", stats.events_dropped, "dropped")
```

`emit` returns once the event is **accepted into the local ring buffer** — not
that anyone processed it. Under backpressure it drops, and
`stats().events_dropped` is how you find out. That distinction is the whole
philosophy: [Submitted Is Not Completed](https://ai2070.net/docs/guides/submitted-is-not-completed).

Transports are constructor arguments — `redis_url=`, `jetstream_url=`, or
`mesh_*`, with the same `emit` / `subscribe` code. The default, memory, is the
one that **does not deliver**: it counts events and discards them, so it suits
ingestion, batching, backpressure, counters and lifecycle, and a `subscribe`
loop on it waits forever rather than erroring.

## Claude Code Skill

Net looks like Kafka or NATS from the outside, and the model underneath is
different enough that an agent working from surface familiarity will write
integration code that runs and is quietly wrong. Install the skills first:

```bash
npx skills add ai-2070/net-claude-skill -g
```

Drop `-g` to install into the current project only. To update to the latest
version:

```bash
npx skills update -g
```

Restart Claude Code and run `/skills` — **net-event-bus** and **net-payments**
should be listed. They load automatically when a request matches:

> *"Wire up a Net publisher and subscriber over the mesh in Python."*

`net-event-bus` covers pub/sub, nRPC, the MCP bridge, organization capability
auth, the gang-claim scheduler, and RedEX / CortEX / Dataforts.
`net-payments` covers x402 pricing, quotes, settlement and spend policy. Full
install options in [Claude Skills](https://ai2070.net/docs/start/claude-skills).

### Give the agent the source too

[`opensrc`](https://github.com/vercel-labs/opensrc) is a small tool that fetches a package's real source into a local cache for exactly this purpose:

```bash
npx -y opensrc@latest path ai-2070/net
```

## Two packages, and what lives where

`net_sdk` is the pure-Python wrapper; every method dispatches into the `net`
PyO3 binding. Most things are re-exported on `net_sdk`, but a couple of
surfaces live only on `net`:

```python
from net import RedisStreamDedup            # consumer-side dedup helper
from net import RpcTimeoutError, RpcError   # the nRPC error family
```

Which classes exist at all depends on the Cargo features the binding was built
with. Wheels ship everything; this only bites on source builds via
`maturin develop`.

## What's in the box

| Surface | Guide |
|---|---|
| Event bus — shards, typed streams, backpressure, Redis / JetStream | [Event bus](https://ai2070.net/docs/guides/event-bus) |
| Mesh streams — direct peer-to-peer, windowed | [Mesh streams](https://ai2070.net/docs/guides/mesh-streams) |
| Capabilities — announce and discover | [Discover and invoke](https://ai2070.net/docs/guides/discover-and-invoke) |
| nRPC — typed request/response, streaming, cancellation | [Typed RPC](https://ai2070.net/docs/guides/nrpc) |
| Distributed mesh channels — roster fan-out with capability auth (`MeshNode.register_channel` / `subscribe_channel` / `publish`) | [Channels](https://ai2070.net/docs/concepts/channels) |
| RedEX / CortEX / NetDB — logs, folds, queries | [Durable logs](https://ai2070.net/docs/guides/durable-logs), [Folds](https://ai2070.net/docs/guides/cortex-folds), [NetDB](https://ai2070.net/docs/guides/netdb-queries) |
| MeshDB — federated queries | [MeshDB](https://ai2070.net/docs/guides/netdb-queries#federated-queries-meshdb) |
| Dataforts — blobs, greedy cache, data gravity (`net_sdk.blob`; `MeshNode.store_dir` / `fetch_dir`) | [Blob storage](https://ai2070.net/docs/guides/dataforts) |
| Compute + Groups — daemons, migration, replica/fork/standby (`net_sdk.compute`, `net_sdk.groups`) | [Daemons](https://ai2070.net/docs/guides/daemons-and-placement), [Continuity](https://ai2070.net/docs/guides/continuity-and-migration) |
| Deck — the operator surface | [Deck](https://ai2070.net/docs/reference/deck) |
| MCP bridge — wrap an MCP server, or serve the mesh as MCP | [Wrap MCP](https://ai2070.net/docs/guides/wrap-mcp-server), [Expose as MCP](https://ai2070.net/docs/guides/expose-net-as-mcp) |
| Organizations — capabilities only your org can discover | [Private capabilities](https://ai2070.net/docs/guides/private-capabilities) |
| Security — identity, delegable tokens, subnets (`net_sdk.identity`, `net_sdk.subnets`) | [Identity](https://ai2070.net/docs/concepts/identity), [Security model](https://ai2070.net/docs/concepts/security-model) |
| Errors — the full exception hierarchy | [Errors](https://ai2070.net/docs/sdk/python/errors) |
| Redis Streams dedup | [Deduplication](https://ai2070.net/docs/reference/redis-dedup) |

## Where to go next

1. [Quickstart](https://ai2070.net/docs/sdk/python/quickstart) — install the SDK and connect two nodes.
2. [Discover and invoke](https://ai2070.net/docs/guides/discover-and-invoke) — announce a tool and call it by capability.
3. [Production deployment](https://ai2070.net/docs/guides/production-deployment) — run the mesh beyond a single host.
4. [Errors](https://ai2070.net/docs/sdk/python/errors) — the exception hierarchy, then [net-mesh-sdk on PyPI](https://pypi.org/project/net-mesh-sdk/).

[GitHub](https://github.com/ai-2070/net)

## License

MIT OR Apache-2.0
