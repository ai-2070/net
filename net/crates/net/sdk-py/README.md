# Net Python SDK

Ergonomic Python SDK for the Net mesh network.

Wraps the `net` PyO3 bindings with generators, typed events, typed channels, and a Pythonic API.

## Install

```bash
pip install ai2070-net-sdk
```

The package publishes as `ai2070-net-sdk` on PyPI but imports as `from net_sdk import ...` (the in-source module name is preserved). The native binding `ai2070-net` is pulled in transitively as a dependency.

## Quick Start

```python
from net_sdk import NetNode

node = NetNode(shards=4)

# Emit events
node.emit({'token': 'hello', 'index': 0})
node.emit_raw('{"token": "world"}')

# Batch
count = node.emit_batch([{'a': 1}, {'a': 2}, {'a': 3}])

# Poll
response = node.poll(limit=100)
for event in response:
    print(event.raw)

# Stream (generator)
for event in node.subscribe(limit=100):
    print(event.raw)

node.shutdown()
```

## Context Manager

```python
with NetNode(shards=4) as node:
    node.emit({'hello': 'world'})
    for event in node.subscribe(limit=10, timeout=5.0):
        print(event.raw)
```

## Typed Streams

### Dataclass

```python
from dataclasses import dataclass

@dataclass
class TokenEvent:
    token: str
    index: int

for token in node.subscribe_typed(TokenEvent, limit=100):
    print(f'{token.index}: {token.token}')
```

### Pydantic

```python
from pydantic import BaseModel

class TemperatureReading(BaseModel):
    sensor_id: str
    celsius: float
    timestamp: float

for reading in node.subscribe_typed(TemperatureReading, limit=100):
    print(f'{reading.sensor_id}: {reading.celsius}°C')
```

## Typed Channels

```python
from net_sdk import TypedChannel

temps = node.channel('sensors/temperature', TemperatureReading)

# Publish
temps.publish(TemperatureReading(sensor_id='A1', celsius=22.5, timestamp=1700000000.0))

# Subscribe
for reading in temps.subscribe():
    print(f'{reading.sensor_id}: {reading.celsius}°C')
```

## Ingestion Methods

| Method | Input | Speed | Returns |
|--------|-------|-------|---------|
| `emit(obj)` | dict, dataclass, Pydantic | Fast | `Receipt` |
| `emit_raw(json)` | str | Fastest | `Receipt` |
| `emit_batch(objs)` | list | Bulk | `int` |
| `emit_raw_batch(jsons)` | list[str] | Bulk fastest | `int` |
| `fire(json)` | str | Fire-and-forget | None |

## Transports

```python
# In-memory (default)
node = NetNode(shards=4)

# Redis
node = NetNode(shards=4, redis_url='redis://localhost:6379')

# JetStream
node = NetNode(shards=4, jetstream_url='nats://localhost:4222')

# Encrypted mesh
node = NetNode(
    shards=4,
    mesh_bind='0.0.0.0:9000',
    mesh_peer='192.168.1.10:9001',
    mesh_psk='...',
    mesh_role='initiator',
    mesh_peer_public_key='...',
)

# Persistent producer nonce — required for cross-restart
# dedup against JetStream / Redis adapters. The bus loads (or
# creates on first run) a u64 nonce at this path and stamps it on
# every batch, so retries from a crashed-and-restarted producer
# are recognized by the backend's dedup window.
node = NetNode(
    shards=4,
    redis_url='redis://localhost:6379',
    producer_nonce_path='/var/lib/myapp/producer.nonce',
)
```

## Redis Streams consumer-side dedup helper

The Redis adapter writes a stable `dedup_id` field on every XADD
entry — see [`bindings/python/README.md`](../bindings/python/README.md#redis-streams-consumer-side-dedup-helper)
for the full contract. `RedisStreamDedup` is exposed on the
underlying `net` PyO3 module; `sdk-py`'s `NetNode` wrapper does
not yet re-export it (tracked in `SDK_PYTHON_PARITY_PLAN.md`).
Import directly:

```python
from net import RedisStreamDedup

dedup = RedisStreamDedup(capacity=64_000)

# Read entries from your Redis client of choice; pull the
# `dedup_id` field from each entry and test-and-insert.
for entry_id, fields in r.xrange("net:shard:0", "0", "+"):
    if not dedup.is_duplicate(fields[b"dedup_id"].decode()):
        process(entry_id, fields)
```

## NAT Traversal (optimization, not correctness)

Two NATed peers already reach each other through the mesh's routed-handshake path. NAT traversal opens a shorter direct path when the NAT shape allows it; it's never required for connectivity. The surface is exposed on the underlying `net` PyO3 module and is a no-op when the native package was built without `--features nat-traversal`.

```python
# Access via the underlying PyO3 handle on sdk-py's MeshNode.
# Ergonomic sdk-py wrappers are a planned follow-up; the PyO3
# methods mirror the Rust SDK surface directly. (The event-bus
# `NetNode` in `net_sdk.node` does NOT expose these — NAT
# traversal lives on `MeshNode` from `net_sdk.mesh`.)
from net_sdk import MeshNode
node = MeshNode(bind_addr="0.0.0.0:9000", psk="00" * 32)
native = node._native   # the PyO3 `NetMesh` handle

native.reclassify_nat()

klass  = native.nat_type()            # "open" | "cone" | "symmetric" | "unknown"
reflex = native.reflex_addr()         # "203.0.113.5:9001" | None

observed = native.probe_reflex(peer_node_id)   # "ip:port"

# Attempt a direct connection via the pair-type matrix.
# `coordinator` mediates the punch when the matrix picks one.
# Always returns — stats tell you which path won.
native.connect_direct(peer_node_id, peer_pubkey_hex, coordinator_node_id)

# Cumulative counters — all int, monotonic.
s = native.traversal_stats()
s.punches_attempted   # coordinator mediated a PunchRequest + Introduce
s.punches_succeeded   # ack arrived AND direct handshake landed
s.relay_fallbacks     # landed on the routed path after skip/fail
```

Operators with a known-public address skip the classifier sweep entirely. The override pins `"open"` + the supplied address on every capability announcement; call `announce_capabilities()` after to propagate (the setter resets the rate-limit floor so the next announce is guaranteed to broadcast).

```python
native.set_reflex_override('203.0.113.5:9001')
native.announce_capabilities(caps)
# later:
native.clear_reflex_override()
native.announce_capabilities(caps)
```

Traversal failures surface as `RuntimeError` with a stable `traversal: <kind>[: <detail>]` message prefix. The `<kind>` discriminator is one of `reflex-timeout` | `peer-not-reachable` | `transport` | `rendezvous-no-relay` | `rendezvous-rejected` | `punch-failed` | `port-map-unavailable` | `unsupported`. Match on the prefix for machine-readable branching:

```python
try:
    native.connect_direct(peer_node_id, peer_pubkey_hex, coord_id)
except RuntimeError as e:
    msg = str(e)
    if msg.startswith("traversal: unsupported"):
        ...   # native library built without --features nat-traversal
    elif msg.startswith("traversal: peer-not-reachable"):
        ...
```

A build without the `nat-traversal` feature raises `traversal: unsupported` for every NAT call — the routed path keeps working regardless.

## Mesh Streams (multi-peer + back-pressure)

For direct peer-to-peer messaging — open a stream to a specific peer
and catch back-pressure as a first-class exception:

```python
from net_sdk import MeshNode, BackpressureError, NotConnectedError

node = MeshNode(bind_addr='127.0.0.1:9000', psk='00' * 32)
# ... handshake (node.connect(...) / node.accept(...)) ...

stream = node.open_stream(
    peer_node_id=peer_id,
    stream_id=0x42,
    reliability='reliable',
    window_bytes=256,    # max in-flight packets before BackpressureError
)

# Three canonical daemon patterns:

# 1. Drop on pressure — best for telemetry / sampled streams.
try:
    node.send_on_stream(stream, [b'{}'])
except BackpressureError:
    metrics.inc('stream.backpressure_drops')
except NotConnectedError:
    # peer gone or stream closed — reopen if needed
    pass

# 2. Retry with exponential backoff (5 ms → 200 ms, up to max_retries).
node.send_with_retry(stream, [b'{}'], max_retries=8)

# 3. Block until the network lets up (bounded retry, ~13 min worst case).
# Releases the GIL for the duration, so other Python threads keep running.
node.send_blocking(stream, [b'{}'])

# Live stats — tx/rx seq, in-flight, window, backpressure count.
stats = node.stream_stats(peer_id, 0x42)
```

Both exceptions inherit from `Exception` and are re-exported from
`net_sdk`, so `try`/`except` works as expected. The transport never
retries or buffers on its own behalf — the helper methods are
opt-in policies, not defaults. See `docs/TRANSPORT.md` for the full
contract.

## Security (identity, tokens, capabilities, subnets)

The full security surface — ed25519 `Identity`, `PermissionToken`
issue / install / delegate, `CapabilityAnnouncement` broadcast +
`find_nodes`, `SubnetId` / `SubnetPolicy`, channel auth with
`publish_caps` / `subscribe_caps` / `require_token` — is shipped
on the underlying **`net`** PyO3 package, not this wrapper. Import
directly:

```python
from net import (
    Identity, TokenError, IdentityError,
    parse_token, verify_token, delegate_token, channel_hash,
)
from net import NetMesh  # adds announce_capabilities / find_nodes /
                        # entity_id / subscribe_channel(..., token=)
```

Quick example — issue a token and round-trip it through the mesh:

```python
import os
from net import Identity, NetMesh

seed = os.urandom(32)                     # persist via your own secret manager
identity = Identity.from_seed(seed)

# Mesh reuses the same keypair — `entity_id` is stable across restarts.
mesh = NetMesh(
    "127.0.0.1:9000",
    psk="42" * 32,
    identity_seed=seed,
)
assert mesh.entity_id == identity.entity_id

# Issue a SUBSCRIBE-scope token for a grantee.
grantee = Identity.generate()
token = identity.issue_token(
    subject=grantee.entity_id,
    scope=["subscribe"],
    channel="sensors/temp",
    ttl_seconds=300,    # `0` raises TokenError (zero TTL would mint a born-expired token)
)

# Publisher gates the channel on tokens; subscribers attach them.
mesh.register_channel("sensors/temp", require_token=True)
# subscriber_mesh.subscribe_channel(mesh.node_id, "sensors/temp", token=token)
```

`TokenError` messages have the form `"token: <kind>"` where `<kind>`
is one of `invalid_format | invalid_signature | expired |
not_yet_valid | delegation_exhausted | delegation_not_allowed |
not_authorized`. Parse with `str(e).removeprefix("token: ")` for
programmatic dispatch.

### Scoped capability discovery (reserved `scope:*` tags)

A provider can narrow *who its query result reaches* by tagging
its `CapabilitySet` with reserved `scope:*` tags (e.g.
`scope:tenant:oem-123`, `scope:region:eu-west`,
`scope:subnet-local`). Queries call `mesh.find_nodes_scoped(filter,
scope)` to filter candidates. The wire format and forwarders are
untouched — enforcement is purely query-side.

```python
# GPU pool advertised to one tenant only.
mesh.announce_capabilities({
    "tags": ["model:llama3-70b", "scope:tenant:oem-123"],
})

# Tenant-scoped query — returns this node + any Global (untagged) peers.
oem_nodes = mesh.find_nodes_scoped(
    {"require_tags": ["model:llama3-70b"]},
    {"kind": "tenant", "tenant": "oem-123"},
)
```

`scope` accepts the dict form `{"kind": "<kind>", ...}`:
`"any"` (default), `"global_only"`, `"same_subnet"`,
`{"kind": "tenant", "tenant": "<id>"}`,
`{"kind": "tenants", "tenants": [...]}`,
`{"kind": "region", "region": "<name>"}`,
`{"kind": "regions", "regions": [...]}`. Strictest scope wins —
`scope:subnet-local` dominates tenant/region tags on the same set.
Untagged peers resolve to `Global` and stay visible under
permissive queries (matches the v1 default). Full design:
[`docs/SCOPED_CAPABILITIES_PLAN.md`](../docs/SCOPED_CAPABILITIES_PLAN.md).

Full surface + runnable examples:
[`bindings/python/README.md`](../bindings/python/README.md#security-surface-stage-ae).
Cross-SDK contract + rationale:
[`docs/SDK_SECURITY_SURFACE_PLAN.md`](../docs/SDK_SECURITY_SURFACE_PLAN.md).

> **Note.** The `net_sdk` wrapper (generators / typed channels /
> Pydantic) doesn't yet re-export the security types — use `net`
> directly for the identity / capability / subnet / channel-auth
> paths. Follow-up work to proxy them through `net_sdk` is tracked
> in [`SDK_PYTHON_PARITY_PLAN.md`](../docs/SDK_PYTHON_PARITY_PLAN.md).

## nRPC (request / response over the mesh)

nRPC is the request/response convention layer riding on top of
the pub/sub mesh. It turns a directed channel pair
(`<service>.requests` / `<service>.replies.<caller_origin>`) into
a typed RPC surface with deadlines, queue-group fan-out, response
streaming, and end-to-end cancellation.

The typed surface ships in the native `net` PyO3 package at
`net.mesh_rpc` (synchronous calls; the binding releases the GIL
across `runtime.block_on(...)` so other Python threads can run,
and dispatches handler callbacks under
`tokio::task::spawn_blocking` so GIL acquisition doesn't starve
the runtime):

```python
from net import NetMesh
from net.mesh_rpc import (
    CircuitBreaker,
    HedgePolicy,
    NRPC_TYPED_BAD_REQUEST,
    RetryPolicy,
    RpcServerError,
    TypedMeshRpc,
)

server = NetMesh("127.0.0.1:9001", "42" * 32)
client = NetMesh("127.0.0.1:9000", "42" * 32)
# (handshake omitted — see Mesh Streams example)

# Server side: register a typed handler. The returned ServeHandle
# is a context manager — `with` ensures unregister on exit.
server_rpc = TypedMeshRpc.from_mesh(server)
def echo_sum(req: dict) -> dict:
    return {"echo": req["text"], "sum": sum(req["numbers"])}

with server_rpc.serve("echo_sum", echo_sum) as handle:
    # Client side: typed call with a 200ms deadline.
    client_rpc = TypedMeshRpc.from_mesh(client)
    try:
        reply = client_rpc.call(
            server.node_id(),
            "echo_sum",
            {"text": "hi", "numbers": [1, 2, 3]},
            opts={"deadline_ms": 200},
        )
        # reply == {"echo": "hi", "sum": 6}
    except RpcServerError as e:
        # The status is encoded in the error message; helper:
        from net.mesh_rpc import classify_error
        kind = classify_error(e)
        # ...dispatch on kind...
```

### Streaming responses

```python
stream = client_rpc.call_streaming(
    target_node_id, "tail", {"tail": "events"},
    opts={"deadline_ms": 5000, "stream_window": 8},  # optional flow control
)
for chunk in stream:               # decoded objects
    process(chunk)
# stream.close() emits CANCEL to the server (best-effort);
# in-flight chunks are silently discarded.
# stream.grant(n) issues an explicit credit publish for batched
# cadence (no-op on streams without flow control).
```

### Resilience helpers

```python
policy = RetryPolicy(
    max_attempts=4,
    initial_backoff_ms=50,
    max_backoff_ms=1000,
    jitter=0.2,
)
reply = client_rpc.call_with_retry(
    target_node_id, "echo", {"hello": "world"}, policy,
)

# HedgePolicy fans out parallel attempts on a delay;
# first success wins, losers cancelled.
hedge = HedgePolicy(max_parallel=3, hedge_delay_ms=50)
client_rpc.call_with_hedge_to(target_node_ids, "echo", {...}, hedge)

# CircuitBreaker — closed → open → half-open with a configurable
# failure predicate. Open breakers raise `BreakerOpenError`
# carrying the `nrpc:breaker_open:` prefix.
breaker = CircuitBreaker(failure_threshold=5, reset_after_ms=1000)
breaker.call(lambda: client_rpc.call(target_node_id, "echo", {}))
```

### Errors

Every caller-side failure is a typed exception with a stable
`nrpc:` prefix in the message. Subclasses (all in `net.mesh_rpc`):

| Exception                | Kind segment    | Trigger                                  |
| ------------------------ | --------------- | ---------------------------------------- |
| `RpcNoRouteError`        | `no_route`      | No session to target / capability gone   |
| `RpcTimeoutError`        | `timeout`       | Deadline elapsed before reply            |
| `RpcServerError`         | `server_error`  | Handler returned a non-OK status         |
| `RpcTransportError`      | `transport`     | Wire-level send / receive failure        |
| `RpcCodecError`          | `codec_encode` / `codec_decode` | Encode / decode failure |
| `BreakerOpenError`       | `breaker_open`  | Circuit breaker rejected the call        |

Two stable status constants exposed by `net.mesh_rpc`:

| Constant                       | Hex      | Meaning                                          |
| ------------------------------ | -------- | ------------------------------------------------ |
| `NRPC_TYPED_BAD_REQUEST`       | `0x8000` | Typed handler couldn't decode the request body.  |
| `NRPC_TYPED_HANDLER_ERROR`     | `0x8001` | Typed handler ran but returned an exception.     |

Cross-binding contract spec — including the canonical
`cross_lang_echo_sum` service used by every binding's wire-format
compat test — lives in [`../README.md#nrpc`](../README.md#nrpc).

> **Note.** The `net_sdk` wrapper doesn't yet re-export
> `TypedMeshRpc` — use the native `net` package directly. Same
> follow-up plan as the security surface
> ([`SDK_PYTHON_PARITY_PLAN.md`](../docs/SDK_PYTHON_PARITY_PLAN.md)).

## MeshDB (federated query layer)

MeshDB is the typed query layer above the RedEX / CortEX /
capability-index substrate. Build with `--features meshdb` on the
native binding (`maturin develop --features meshdb`); MeshDB
classes import from the **`net`** package. Architectural
overview: [`../README.md#meshdb`](../README.md#meshdb).

### Quick start

```python
from net import InMemoryChainReader, MeshQuery, MeshQueryRunner

reader = InMemoryChainReader()
reader.append(0xAB, 1, b"v1")
reader.append(0xAB, 2, b"v2")
reader.append(0xAB, 3, b"v3")

runner = MeshQueryRunner(reader)

# Atomic operator — yields the tip row.
rows = runner.execute(MeshQuery.latest(0xAB))
assert rows[0].seq == 3
assert rows[0].payload == b"v3"

# Composite pipeline via the fluent builder.
query = (
    MeshQuery.builder()
    .between(0xAB, 1, 4)
    .count()
    .build()
)
[agg_row] = runner.execute(query)
assert agg_row.decode_aggregate().value == 3.0
```

The runner is synchronous — `execute(query) -> list[ResultRow]`
drains the result stream against an internal Tokio runtime. The
GIL is released around the executor call so a background thread
can keep ticking.

### Operator surface

| Family | Factories / builder methods |
|---|---|
| Atomic | `MeshQuery.at(origin, seq)`, `MeshQuery.between(origin, start, end)`, `MeshQuery.latest(origin)`, `MeshQuery.lineage_emit(origin, entries, direction)` |
| Composite | `MeshQuery.filter(inner, predicate)`, `MeshQuery.window(inner, size)`, `MeshQuery.count(inner, group_by=None)`, `MeshQuery.sum/avg/min/max/percentile(inner, field, ...)`, `MeshQuery.distinct_count(inner, field)`, `MeshQuery.join(left, right, kind, key, strategy=None, watermark_secs=None)` |
| Fluent builder | `MeshQuery.builder().<at|between|latest>(...).<filter|window|count|...>(...).build()` — common-ops shortcut over the static factories |

`Predicate` ships static factories paralleling the wire format:
`Predicate.exists / equals / numeric_at_least / numeric_at_most /
numeric_in_range / string_prefix / string_matches /
semver_at_least` plus the boolean combinators `Predicate.and_ /
or_ / not_`. Field paths target row-intrinsic names (`"origin"` /
`"seq"`) or dotted JSON-payload paths (`"a.b.c"`).

### Sentinel row decoders

Atomic rows expose `.payload` directly. Composite rows carry
postcard-encoded sentinels — decode with the typed methods:

```python
[agg] = runner.execute(MeshQuery.count(MeshQuery.between(0xAB, 1, 4)))
result = agg.decode_aggregate()       # AggregateResult
assert result.kind == "count"
assert result.count == 3

[bucket] = runner.execute(MeshQuery.window(MeshQuery.between(0xAB, 1, 10), 5))
window = bucket.decode_window()       # WindowBoundary
print(window.start, window.end, len(window.rows))

[paired] = runner.execute(MeshQuery.join(left, right, "inner", "seq"))
joined = paired.decode_joined()       # JoinedRow
print(joined.left, joined.right)
```

`decode_*` returns `None` when the payload isn't a sentinel
envelope (i.e. for atomic-operator rows), so callers branch on
"did the decoder recognise this?" without a separate type query.

### Phase F result cache

Opt in at runner-construction time and tune per-call via
`ExecuteOptions`:

```python
from net import CachePolicy, ExecuteOptions, MeshQueryRunner

runner = MeshQueryRunner(reader, enable_cache=True)

# Default: TimeBound TTL = 5 s (mirrors the join watermark).
rows = runner.execute(query)

# Permanent — only safe when the result is immutable under
# substrate semantics.
opts = ExecuteOptions(cache_policy=CachePolicy.permanent())
rows = runner.execute(query, opts)

# Custom TTL.
opts = ExecuteOptions(cache_policy=CachePolicy.time_bound(30.0))
rows = runner.execute(query, opts)

# Bypass cache entirely (skip lookup + writeback).
rows = runner.execute(query, ExecuteOptions(bypass_cache=True))
```

### Lineage emit

The SDK doesn't walk the `fork-of:` graph itself — callers supply
pre-walked entries in walk order:

```python
from net import LineageEntry, MeshQuery, MeshQueryRunner

runner = MeshQueryRunner(reader)
query = MeshQuery.lineage_emit(
    0xAA,
    [
        LineageEntry(0xAA, 0, tip_seq=5),
        LineageEntry(0xBB, 1, tip_seq=3),
        LineageEntry(0xCC, 2),  # tip_seq omitted -> emits seq=0
    ],
    "back",
)
rows = runner.execute(query)
# Compose with .at / .between to fetch event bodies per chain.
```

### Errors

`MeshDbError` is the unified failure surface (planner / executor
/ invalid arguments) — every factory and runner method raises it
on error.

> **Note.** The `net_sdk` wrapper doesn't yet re-export the
> MeshDB surface — use the native `net` package directly.

## Compute (daemons + migration)

The full compute surface — `DaemonRuntime`, `MeshDaemon`
(duck-typed), `DaemonHandle`, `MigrationHandle`, plus the six-
phase migration orchestrator — ships on the native **`net`** PyO3
package (when built with the `compute` feature). Import directly:

```python
from net import (
    DaemonRuntime, NetMesh, Identity, CausalEvent,
    DaemonError, MigrationError, migration_error_kind,
)


class EchoDaemon:
    name = "echo"

    def process(self, event):
        return [bytes(event.payload)]


mesh = NetMesh("127.0.0.1:9000", "42" * 32)
rt = DaemonRuntime(mesh)
rt.register_factory("echo", lambda: EchoDaemon())
rt.start()

handle = rt.spawn("echo", Identity.generate())
# rt.deliver(handle.origin_hash, CausalEvent(handle.origin_hash, 1, b"hi"))
rt.stop(handle.origin_hash)
rt.shutdown()
```

Live migration (`rt.start_migration(origin, src, dst)`) returns a
`MigrationHandle` whose `wait()` drives the cutover to completion;
failures raise `MigrationError` with a stable `kind` parseable by
`migration_error_kind(e)`. Full surface + runnable examples:
[`bindings/python/README.md`](../bindings/python/README.md#compute-daemons--migration).
Cross-SDK contract + rationale:
[`docs/SDK_COMPUTE_SURFACE_PLAN.md`](../docs/SDK_COMPUTE_SURFACE_PLAN.md).

> **Note.** Like the security types, the `net_sdk` wrapper doesn't
> yet re-export `DaemonRuntime` / `MigrationHandle` — use the
> native `net` package directly. Proxying these through `net_sdk`
> is tracked in
> [`SDK_PYTHON_PARITY_PLAN.md`](../docs/SDK_PYTHON_PARITY_PLAN.md).

## Groups (replica / fork / standby)

HA / scaling overlays on top of `DaemonRuntime` — `ReplicaGroup`,
`ForkGroup`, `StandbyGroup` — ship on the native **`net`** PyO3
package (when built with the `groups` feature). Import directly:

```python
from net import (
    DaemonRuntime, NetMesh, Identity, CausalEvent,
    ReplicaGroup, ForkGroup, StandbyGroup,
    GroupError, group_error_kind,
)


class CounterDaemon:
    """Minimal stateful daemon — increments on every event."""

    name = "counter"

    def __init__(self):
        self._count = 0

    def process(self, event: CausalEvent) -> list[bytes]:
        self._count += 1
        return [self._count.to_bytes(4, "little")]


# Build a mesh + runtime, register the factory, flip to Ready.
mesh = NetMesh("127.0.0.1:9000", "42" * 32)
rt = DaemonRuntime(mesh)
rt.register_factory("counter", lambda: CounterDaemon())
rt.start()

# A sample event — `rt.deliver` expects a `CausalEvent` per the
# compute surface. The origin/sequence match the replica the
# group routes to.
event = CausalEvent(0x1234_5678, sequence=1, payload=b"tick")

# N interchangeable replicas with deterministic per-index identity.
replicas = ReplicaGroup.spawn(
    rt, "counter",
    replica_count=3,
    group_seed=bytes([0x11] * 32),
    lb_strategy="consistent-hash",   # or "round-robin" | "least-load" | ...
)
origin = replicas.route_event({"routing_key": "user:42"})
rt.deliver(origin, CausalEvent(origin, sequence=1, payload=b"tick"))
replicas.scale_to(5)

# N independent daemons forked from a common parent; verifiable lineage.
forks = ForkGroup.fork(
    rt, "counter",
    parent_origin=0xABCDEF01,
    fork_seq=42,
    fork_count=3,
    lb_strategy="round-robin",
)
assert forks.verify_lineage()

# Active-passive replication with replay on promotion.
hot = StandbyGroup.spawn(
    rt, "counter",
    member_count=3,                  # 1 active + 2 standbys
    group_seed=bytes([0x77] * 32),
)
active_origin = hot.active_origin
active_event = CausalEvent(active_origin, sequence=1, payload=b"tick")
rt.deliver(active_origin, active_event)
hot.sync_standbys()                   # periodic catchup

try:
    ReplicaGroup.spawn(
        rt, "never-registered",
        replica_count=2, group_seed=bytes(32),
        lb_strategy="round-robin",
    )
except GroupError as e:
    kind = group_error_kind(e)
    # kind ∈ { "not-ready", "factory-not-found", "no-healthy-member",
    #         "placement-failed", "registry-failed", "invalid-config",
    #         "daemon" }
```

Full surface + runnable examples:
[`bindings/python/README.md`](../bindings/python/README.md#compute-groups-replica--fork--standby).
Cross-SDK contract + rationale:
[`docs/SDK_GROUPS_SURFACE_PLAN.md`](../docs/SDK_GROUPS_SURFACE_PLAN.md).

> **Note.** `net_sdk` does not yet proxy the groups surface; use the
> native `net` package directly, the same way as the security types.

## Capability enhancements (typed taxonomy + predicates + validation)

The SDK exposes a caller-local enhancement layer on top of
`announce_capabilities` / `find_nodes`. The wire format is
byte-identical across all five bindings (Rust / TS / Python / Go / C) —
pinned by JSON fixtures under `tests/cross_lang_capability/`.

```python
from net_sdk import (
    # Typed taxonomy
    tag_from_user_string, RESERVED_PREFIXES,
    # Chain helpers
    empty_capabilities, require_tag, require_axis_value, with_metadata,
    # Predicates
    p, evaluate_predicate,
    predicate_to_rpc_header, predicate_from_rpc_header, RPC_WHERE_HEADER,
    tag_key,
    # Predicate trace + debug
    evaluate_predicate_with_trace,
    predicate_debug_report, redact_metadata_keys,
    # Validation
    validate_capabilities,
    # Diff
    diff_capabilities,
    # Placement filters
    standard_placement, placement_filter_from_fn,
)

# Build a capability set in the wire shape `{ tags, metadata }`.
caps = empty_capabilities()
caps = require_tag(caps, "hardware", "gpu")
caps = require_axis_value(caps, "software", "os", "linux")
caps = with_metadata(caps, "intent", "ml-training")

# Author a predicate.
pred = p.and_(
    p.exists(tag_key("hardware", "gpu")),
    p.numeric_at_least(tag_key("hardware", "memory_gb"), 64),
    p.metadata_equals("intent", "ml-training"),
)

# Local evaluation (no mesh round-trip).
matched = evaluate_predicate(pred, caps.tags, caps.metadata)

# Wire form for nRPC `net-where:` headers — pair with the
# header-bearing call variants so server-side filtering picks
# the right candidate without re-running the predicate per hop.
header_value = predicate_to_rpc_header(pred)
# Reverse direction: parse a peer-supplied header back to AST.
decoded = predicate_from_rpc_header(header_value)

# Validate against the canonical schema.
report = validate_capabilities(caps)
if not report.is_valid():
    print("schema errors:", report.errors)

# Detect what changed between two snapshots.
delta = diff_capabilities(prev_caps, caps)

# Single-evaluation trace — every clause's verdict + skipped
# children for short-circuit AND/OR.
result, trace = evaluate_predicate_with_trace(pred, tags, metadata)

# Profile a predicate across a corpus + render a per-clause report.
debug = predicate_debug_report(pred, contexts)
safe = redact_metadata_keys(debug, ["intent"])  # scrub before persisting
print(safe.render())

# Wrap a predicate as a placement-filter callback the substrate
# invokes per candidate. Pair with `standard_placement` to
# install a custom scoring axis driven by the Python predicate.
pf = placement_filter_from_fn(
    lambda cand: evaluate_predicate(pred, cand.tags, cand.metadata),
)
placement = standard_placement(custom_filter_id=pf.id)
```

The wire format is byte-identical across all five bindings (Rust /
TS / Python / Go / C) — pinned by JSON fixtures under
`tests/cross_lang_capability/`.

## Storage + cross-node RedEX replication

`net_sdk` is the event-bus wrapper; the storage surface (CortEX
adapters, NetDB, raw `Redex` / `RedexFile`) lives on the underlying
**`net`** PyO3 package, same as the security and groups surfaces.
RedEX channels can replicate across the mesh — opt in per channel
with `replication=True` on `Redex.open_file` after calling
`Redex.enable_replication(mesh)`:

```python
from net import NetMesh, Redex

mesh = NetMesh(bind_addr='127.0.0.1:0', psk='...')
redex = Redex(persistent_dir='/var/lib/net/events')
redex.enable_replication(mesh)

file = redex.open_file(
    'orders/audit',
    replication=True,
    replication_factor=3,
    replication_heartbeat_ms=500,
    replication_placement='standard',
)
file.append(b'event payload')
```

Failover uses a deterministic nearest-RTT election with NodeId
tie-break — no broadcast / no epoch. `Redex.replication_prometheus_text()`
renders the seven per-channel metric shapes for an HTTP scrape
endpoint. See the native `net` package's README for the full
config surface (`replication_pinned_nodes`, `replication_leader_pinned`,
`replication_on_under_capacity`, `replication_budget_fraction`) and
operator semantics.

## Dataforts (greedy cache, gravity, blob refs, read-your-writes)

Dataforts is the compositional data plane on top of RedEX / CortEX
/ capability-index / proximity-graph. The Python surface lives on
the underlying `net` PyO3 package (same as RedEX / CortEX): the
native module is built with the `dataforts` Cargo feature; wheel
artifacts published to PyPI ship with the feature on.

Four phases:

- **Phase 1 — Greedy-LRU caching.** Per-node speculative caching
  of in-scope chains observed via the tail-subscription path.
  Five-axis admission (scope + proximity + capability-preference
  + colocation + storage-cap) plus a bandwidth budget gate decide
  whether to admit each inbound event. Cold channels evict under
  cluster-cap pressure and withdraw their `causal:<hex>`
  advertisement. The runtime also observes `BlobRef`-shaped
  payloads and runs the `should_pull_blob` admission gate; on
  admit, the wired `BlobAdapter::prefetch` spawns a best-effort
  pull via the per-chunk replication runtime.
- **Phase 3 — `BlobRef` + blob adapters.** Two shapes:
  - **External-hook variant (v0.15):** a `b"\xB0\xB1\xB2\xB3"`
    magic + version + 32-byte BLAKE3 + size + URI reference
    whose bytes live in the caller's storage (S3 / Ceph / IPFS
    / local FS). Use `register_filesystem_blob_adapter` +
    `blob_publish` / `blob_resolve` from Python.
  - **Substrate-owned variant (v0.2):** the `MeshBlobAdapter`
    Python class wraps the in-tree Rust adapter — every chunk
    persists as a content-addressed `RedexFile` on the local
    Redex, and replication wires through the existing per-
    channel runtime. Methods: `store` / `fetch` / `fetch_range`
    / `exists` / `prometheus_text`. The full v0.2 surface
    (publish_with_blob, BlobRefcountTable, BlobMetrics,
    prefetch, migration controller) is still being incrementally
    exposed — operator scripts get the CRUD path today; the
    deeper integration points run from Rust until each Python
    wrapper lands.
- **Phase 4 — Data gravity.** Per-chain read-rate counters with
  exponential decay. Threshold-crossing emissions stamp
  `heat:<hex>=<rate>` onto the chain's capability announcement;
  greedy weights cache pulls by `heat × scope-match × proximity`.
  The v0.2 blob track adds parallel `BlobHeatRegistry` keyed on
  chunk hash + `heat:blob:<hex>=<rate>` tag emission +
  `drive_blob_migration_tick` consumer. These surfaces are
  exposed from Rust today; Python wrappers land in a follow-up
  cross-binding slice.
- **Phase 3.5 — Active blob overflow (v0.3 blob track).** Push-
  side complement of Phase 4's pull-driven migration. Disabled
  by default; opt in with `MeshBlobAdapter(..., overflow=True)`
  at construction or `mesh_blob.set_overflow_enabled(True)` at
  runtime. The kwarg also accepts a `dict` for typed tuning
  (`{"enabled": True, "high_water_ratio": 0.80, "scope": "zone"}`).
  Read-only properties: `overflow_enabled`, `overflow_active`,
  `overflow_config`. Operators dashboard via the
  `dataforts_blob_overflow_*` counter family in
  `mesh_blob.prometheus_text()`.
- **Phase 5 — Read-your-writes.** Every `tasks.create`,
  `memories.insert`, etc. returns a `WriteToken`. Pass it to
  `tasks.wait_for_token(token, deadline_ms=…)` and the call
  blocks until the local fold has *applied* that seq — tracking
  both `applied_through_seq` and `folded_through_seq` so a
  stalled fold raises `CortexError`, not a silent return.
  `deadline_ms=0` is a non-blocking poll (synchronous check
  without scheduling a wait).

```python
import hashlib
from net import (
    NetMesh, Redex, Tasks, BlobRef, RedexError, MeshBlobAdapter,
    register_filesystem_blob_adapter, blob_publish, blob_resolve,
)

mesh = NetMesh(bind_addr='0.0.0.0:7000', psk='...')
redex = Redex(persistent_dir='/var/lib/net/redex')

# Phase 1 — wire greedy into the mesh inbound dispatch.
redex.enable_greedy_dataforts(
    mesh,
    scopes=['region:us'],
    total_cap_bytes=1 << 30,         # 1 GiB cluster-cap
    per_channel_cap_bytes=64 << 20,
)

# Phase 4 — layer gravity on top.
redex.enable_gravity_for_greedy(
    mesh,
    emit_threshold_ratio=1.5,
    decay_half_life_secs=300,
)

# Phase 3 v0.15 — external-hook variant.
register_filesystem_blob_adapter('local', '/var/blobs')
ref = blob_publish('local', 'local://obj/payload', some_bytes)
back = blob_resolve(ref)

# Phase 3 v0.2 — substrate-owned variant.
mesh_blob = MeshBlobAdapter(redex, "mesh-app", persistent=True)
payload = b"the substrate carries the bytes"
import blake3  # `pip install blake3` for a parity hash
h = blake3.blake3(payload).digest()
br = BlobRef("mesh://demo", h, len(payload))
mesh_blob.store(br, payload)
back = mesh_blob.fetch(br)
assert back == payload
print(mesh_blob.prometheus_text())

# Phase 3.5 / v0.3 — opt this node into active blob overflow.
# Disabled by default; one boolean opts in. Operators tuning
# thresholds pass a dict instead — missing keys inherit defaults,
# unknown keys raise TypeError (typo defense).
mesh_blob = MeshBlobAdapter(
    redex,
    "mesh-overflow",
    persistent=True,
    overflow={"enabled": True, "high_water_ratio": 0.80, "scope": "zone"},
)
print(mesh_blob.overflow_enabled)   # True
print(mesh_blob.overflow_active)    # False (no tick has fired in-process yet)
print(mesh_blob.overflow_config)    # {'enabled': True, 'high_water_ratio': 0.80, ...}

# Runtime control — no need to rebuild the adapter:
mesh_blob.set_overflow_enabled(False)
mesh_blob.set_overflow_config({"enabled": True, "max_pushes_per_tick": 4})

# Phase 5 — read-your-writes.
tasks = Tasks.open(redex, origin_hash=mesh.origin_hash)
result = tasks.create(1, 'first', 100)
tasks.wait_for_token(result.token, deadline_ms=250)

# Diagnostics.
print(redex.greedy_cached_channel_count())
print(redex.greedy_prometheus_text())
```

A build without the `dataforts` Cargo feature raises `RedexError`
with `"enable_greedy_dataforts requires the 'dataforts' feature;
rebuild with --features dataforts"` so the failure mode is typed,
not a silent no-op.

The canonical channel hash is 32-bit (`channel_hash(name)` returns
an `int` in the u32 range). The per-packet wire `NetHeader`
`channel_hash` stays `u16` — fast-path filter hint, may
bucket-collide at scale; ACL / config / cache / RYW decisions key
on the canonical 32-bit hash via registry disambiguation. The
`PermissionToken` wire form is 161 bytes (the channel-hash
widening grew it from 159).

## API

| Method | Description |
|--------|-------------|
| `NetNode(shards=4)` | Create a new node |
| `emit(obj)` | Emit dict, dataclass, or Pydantic model |
| `emit_raw(json)` | Emit a JSON string (fastest) |
| `emit_batch(objs)` | Batch emit |
| `emit_raw_batch(jsons)` | Batch emit strings |
| `fire(json)` | Fire-and-forget |
| `poll(limit)` | One-shot poll |
| `poll_one()` | Poll a single event |
| `subscribe(limit, timeout)` | Generator stream |
| `subscribe_typed(model)` | Typed generator stream |
| `channel(name, model)` | Create a typed channel |
| `stats()` | Ingestion statistics |
| `shards()` | Number of active shards |
| `shutdown()` | Graceful shutdown |
| `bus` | Access underlying PyO3 binding |

## License

Apache-2.0
