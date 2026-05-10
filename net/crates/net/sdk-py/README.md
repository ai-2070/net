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
    p.numeric_at_least(tag_key("hardware", "memory_mb"), 65536),
    p.metadata_equals("intent", "ml-training"),
)

# Local evaluation (no mesh round-trip).
matched = evaluate_predicate(pred, caps.tags, caps.metadata)

# Wire form for nRPC `cyberdeck-where:` headers — pair with the
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
