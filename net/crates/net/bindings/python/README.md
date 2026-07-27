# Net Python

The PyO3 extension for the Net mesh — a latency-first encrypted mesh where
services and agents announce capabilities, discover each other, and invoke work
over typed RPC.

**Most people want [`net-mesh-sdk`](https://pypi.org/project/net-mesh-sdk/)
instead.** That's the Pythonic wrapper — context managers, generators, typed
channels, dataclass and Pydantic support — and it depends on this package. Use
`net-mesh` directly when you want the raw binding, or need one of the few
surfaces the wrapper doesn't re-export.

**Docs: <https://ai2070.net/docs/sdk/python/quickstart>** ·
[Concepts](https://ai2070.net/docs/concepts/architecture)

## Install

```bash
pip install net-mesh
```

Publishes as `net-mesh`, imports as `net`.

## Quickstart

```python
from net import Net

with Net(num_shards=4) as bus:
    bus.ingest_raw('{"sensor": "lidar", "range_m": 12.5}')
    bus.ingest({"sensor": "radar", "range_m": 45.0})

    # Cursor-paginated. None starts from the earliest buffered event.
    resp = bus.poll(100)
    for event in resp.events:
        print("event", event)
    # resp.next_id -> pass back as `cursor` to page forward

    stats = bus.stats()
    print(stats.events_ingested, "ingested,", stats.events_dropped, "dropped")
```

Ingest returns once the event is accepted into the local ring buffer —
acceptance, not delivery. Under backpressure events can drop; check
`events_dropped`. See
[Submitted Is Not Completed](https://ai2070.net/docs/guides/submitted-is-not-completed).

## What lives only here

The wrapper re-exports most of this module, with two notable exceptions you
must import from `net` directly:

```python
from net import RedisStreamDedup            # consumer-side dedup helper
from net import RpcTimeoutError, RpcError   # the nRPC error family
```

See [Deduplication](https://ai2070.net/docs/reference/redis-dedup) and
[Errors](https://ai2070.net/docs/sdk/python/errors) — the latter documents a
trap worth knowing: without the nRPC feature compiled in, every `Rpc*Error`
aliases down to `Exception`.

## Cargo features

**PyPI wheels ship every feature enabled**, so this only matters for source
builds via `maturin develop`, where a disabled feature's classes are simply
absent from the module.

```bash
maturin develop --features "netdb redex-disk meshdb meshos"
```

## Claude Code Skill

Net looks like Kafka or NATS from the outside, and the model underneath is
different enough that an agent working from surface familiarity will write
integration code that runs and is quietly wrong. Install the skills first:

```bash
git clone https://github.com/ai-2070/net-claude-skill.git /tmp/net-claude-skill
mkdir -p ~/.claude/skills
cp -R /tmp/net-claude-skill/net-event-bus /tmp/net-claude-skill/net-payments ~/.claude/skills/
```

Restart Claude Code and run `/skills` — **net-event-bus** and **net-payments**
should be listed. `net-event-bus` covers pub/sub, nRPC, the MCP bridge,
organization capability auth, the gang-claim scheduler, and RedEX / CortEX /
Dataforts. `net-payments` covers x402 pricing, quotes, settlement and spend
policy. Full install options — project-scoped, symlinked to stay current — in
[Claude Skills](https://ai2070.net/docs/start/claude-skills).

## What's in the box

| Surface | Guide |
|---|---|
| Event bus — shards, backpressure, Redis / JetStream | [Event bus](https://ai2070.net/docs/guides/event-bus) |
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

## License

MIT OR Apache-2.0
