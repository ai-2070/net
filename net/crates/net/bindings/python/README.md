# Net Python

High-performance, schema-agnostic event bus for AI runtime workloads.

## Installation

```bash
pip install ai2070-net
```

The package publishes as `ai2070-net` on PyPI but imports as `from net import ...` (the in-source module name is preserved). For the higher-level Pythonic surface (generators, typed channels, dataclass/Pydantic support), install [`ai2070-net-sdk`](../../sdk-py/) instead — it depends on this package.

## Quick Start

```python
from net import Net

# Create event bus (defaults to CPU core count shards)
bus = Net()

# Ingest events - fast path with raw JSON strings (23M+ ops/sec)
bus.ingest_raw('{"token": "hello", "index": 0}')

# Or use dict for convenience (4M+ ops/sec)
bus.ingest({"token": "world", "index": 1})

# Batch ingestion for maximum throughput
events = [f'{{"token": "tok_{i}"}}' for i in range(10000)]
count = bus.ingest_raw_batch(events)

# Poll events
response = bus.poll(limit=100)
for event in response:
    print(event.raw)
    # Or parse to dict
    data = event.parse()

# Check stats
stats = bus.stats()
print(f"Ingested: {stats.events_ingested}, Dropped: {stats.events_dropped}")

# Shutdown
bus.shutdown()
```

## Context Manager

```python
with Net(num_shards=4) as bus:
    bus.ingest_raw('{"data": "value"}')
# Automatically shuts down
```

## Configuration

```python
bus = Net(
    num_shards=8,                    # Number of parallel shards
    ring_buffer_capacity=1_048_576,  # Events per shard (must be power of 2)
    backpressure_mode="drop_oldest", # What to do when full
)
```

## Net Encrypted UDP Transport

Net provides encrypted point-to-point UDP transport for high-performance scenarios:

```python
from net import Net, generate_net_keypair
import os

# Generate keypair for responder
keypair = generate_net_keypair()
psk = os.urandom(32).hex()

# Responder side
responder = Net(
    num_shards=2,
    net_bind_addr='127.0.0.1:9001',
    net_peer_addr='127.0.0.1:9000',
    net_psk=psk,
    net_role='responder',
    net_secret_key=keypair.secret_key,
    net_public_key=keypair.public_key,
    net_reliability='light',  # 'none', 'light', or 'full'
)

# Initiator side (knows responder's public key)
initiator = Net(
    num_shards=2,
    net_bind_addr='127.0.0.1:9000',
    net_peer_addr='127.0.0.1:9001',
    net_psk=psk,
    net_role='initiator',
    net_peer_public_key=keypair.public_key,
)

# Use as normal
initiator.ingest_raw('{"event": "data"}')
```

### Backpressure Modes

- `"drop_newest"` - Reject new events when buffer is full
- `"drop_oldest"` - Evict oldest events to make room
- `"fail_producer"` - Raise an error

### NAT traversal (optimization, not correctness)

Two NATed peers already reach each other through the mesh's routed-handshake path. NAT traversal opens a shorter direct path when the NAT shape allows it; it's never required for connectivity. Every method below is safe to call regardless of NAT type — a failed punch or a `traversal: *` `RuntimeError` is not a connectivity failure, traffic keeps riding the relay. The whole surface is a no-op when the native module was built without `--features nat-traversal`: every call raises `RuntimeError("traversal: unsupported")`.

```python
from net import NetMesh

mesh = NetMesh(bind_addr="0.0.0.0:9000", psk="00" * 32)

mesh.reclassify_nat()

klass  = mesh.nat_type()            # "open" | "cone" | "symmetric" | "unknown"
reflex = mesh.reflex_addr()         # "203.0.113.5:9001" or None

observed = mesh.probe_reflex(peer_node_id)   # "ip:port"

# Attempt a direct connection via the pair-type matrix.
# `coordinator` mediates the punch when the matrix picks one.
# Always returns — inspect stats to learn which path won.
mesh.connect_direct(peer_node_id, peer_pubkey_hex, coordinator_node_id)

# Cumulative counters — all int, monotonic.
s = mesh.traversal_stats()
s.punches_attempted   # coordinator mediated a PunchRequest + Introduce
s.punches_succeeded   # ack arrived AND direct handshake landed
s.relay_fallbacks     # landed on the routed path after skip/fail
```

Operators with a known-public address skip the classifier sweep entirely. The override pins `"open"` + the supplied address on every capability announcement; call `announce_capabilities()` after to propagate (the setter resets the rate-limit floor so the next announce is guaranteed to broadcast).

```python
mesh.set_reflex_override('203.0.113.5:9001')
mesh.announce_capabilities(caps)
# later:
mesh.clear_reflex_override()
mesh.announce_capabilities(caps)
```

Traversal failures surface as `RuntimeError` with a stable `traversal: <kind>[: <detail>]` message prefix. The `<kind>` discriminator is one of `reflex-timeout` | `peer-not-reachable` | `transport` | `rendezvous-no-relay` | `rendezvous-rejected` | `punch-failed` | `port-map-unavailable` | `unsupported`. Match on the prefix for machine-readable branching:

```python
try:
    mesh.connect_direct(peer_node_id, peer_pubkey_hex, coord_id)
except RuntimeError as e:
    msg = str(e)
    if msg.startswith("traversal: unsupported"):
        ...   # native module built without --features nat-traversal
    elif msg.startswith("traversal: peer-not-reachable"):
        ...
```

`"unsupported"` is the signal that the bindings are linked unconditionally and the native module doesn't have the feature — callers can branch cleanly without probing for symbol presence.

## Channels (distributed pub/sub)

Named pub/sub over the encrypted mesh. Publishers register channels
with access policy; subscribers ask to join via a membership
subprotocol; `publish` fans payloads out to every current subscriber.

```python
from net import NetMesh, ChannelAuthError, ChannelError

pub = NetMesh('127.0.0.1:9001', '42' * 32)
try:
    pub.register_channel(
        'sensors/temp',
        visibility='global',      # or 'subnet-local' | 'parent-visible' | 'exported'
        reliable=True,
        priority=2,
        max_rate_pps=1000,
    )

    # Subscriber side (after handshake with pub):
    # sub.subscribe_channel(pub.node_id, 'sensors/temp')

    # Fan a payload out to all subscribers.
    report = pub.publish(
        'sensors/temp',
        b'{"celsius": 22.5}',
        reliability='reliable',
        on_failure='best_effort',
        max_inflight=32,
    )
    print(f"{report['delivered']}/{report['attempted']} subscribers received")
finally:
    pub.shutdown()

# Typed errors for ACL outcomes:
# try: sub.subscribe_channel(peer_id, 'restricted')
# except ChannelAuthError: ...   # publisher denied
# except ChannelError: ...       # unknown channel / other rejection
```

Channel names always cross the binding as strings (not the u16 hash)
to avoid ACL bypass via collision. The Python binding does not yet
expose a dedicated per-channel receive API; that is a follow-up.

## CortEX & NetDb (event-sourced state)

Typed, event-sourced state on top of RedEX — tasks and memories with
filterable queries and sync watch iterators. Includes the
`snapshot_and_watch` primitive whose race fix landed on v2, so you
can safely "paint what's there now, then react to changes" without
losing updates that race during construction.

```python
from net import NetDb, CortexError

db = NetDb.open(origin_hash=0xABCDEF01, with_tasks=True, with_memories=True)
tasks = db.tasks

try:
    seq = tasks.create(1, 'write docs', 100)
    tasks.wait_for_seq(seq)   # block until the fold has applied
except CortexError as e:
    # adapter-level failure (RedEX I/O, fold halted, etc.)
    ...

# Snapshot + watch, one atomic call — no race.
snap, it = tasks.snapshot_and_watch_tasks(status='pending')
print('initial:', len(snap), 'pending tasks')
for batch in it:
    print('update:', len(batch), 'pending tasks')
    if len(batch) == 0:
        it.close()    # idempotent; ends the iterator
        break

db.close()
```

### Standalone adapters

If you only need one model, skip the `NetDb` facade:

```python
from net import Redex, TasksAdapter

redex = Redex(persistent_dir='/var/lib/net/redex')
tasks = TasksAdapter.open(redex, origin_hash=0xABCDEF01, persistent=True)
```

`MemoriesAdapter` exposes the same shape with `store` / `retag` /
`pin` / `unpin` / `delete` / `list_memories` / `watch_memories` /
`snapshot_and_watch_memories`.

### Raw RedEX file (no CortEX fold)

For domain-agnostic persistent logs — your own event schema, no
fold, no typed adapter — open a `RedexFile` directly from a `Redex`.
The tail is a sync Python iterator; call `close()` or let
`StopIteration` fire when the file closes.

```python
from net import Redex, RedexError

redex = Redex(persistent_dir='/var/lib/net/events')
file = redex.open_file(
    'analytics/clicks',
    persistent=True,
    fsync_interval_ms=100,           # or fsync_every_n=1000
    retention_max_events=1_000_000,
)

# Append (or batch-append).
seq = file.append(b'{"url": "/home"}')
# `append_batch` returns the first-seq int of the batch, or `None`
# for an empty input. The `None` return is the explicit "I
# appended nothing" signal — pre-`bugfixes-8` it returned `0`,
# which collided with the legitimate "first event of a non-empty
# batch landed at seq 0" return.
first = file.append_batch([b'{"a": 1}', b'{"a": 2}'])

# Tail — backfills the retained range, then streams live appends.
try:
    for event in file.tail(from_seq=0):
        print(event.seq, bytes(event.payload))
        if should_stop:
            break           # idempotent; ends the iterator via close()
except RedexError as e:
    ...

file.close()
```

Errors from the RedEX surface raise `RedexError` (invalid channel
name, bad config, append / tail / sync / close failures).

### Why `snapshot_and_watch_*`?

Calling `list_tasks()` then `watch_tasks()` takes two independent
state reads. A mutation landing between them would be silently lost
under the old `skip(1)` implementation. The atomic primitive returns
the snapshot and an iterator seeded so that any divergent initial
emission is forwarded through instead of dropped — see
[`docs/STORAGE_AND_CORTEX.md`](../../docs/STORAGE_AND_CORTEX.md).

## Redis Streams consumer-side dedup helper

The Net Redis adapter writes a stable `dedup_id` field on every
XADD entry: `{producer_nonce:hex}:{shard_id}:{sequence_start}:{i}`.
Combined with the bus's persistent producer-nonce path
(`producer_nonce_path` on `EventBusConfig`), the id is
stable across both within-process retries AND cross-process
restart — the `MULTI/EXEC`-timeout race becomes
filterable at consume time.

`RedisStreamDedup` is the consumer-side helper, exposed on the
`net` PyO3 module:

```python
from net import RedisStreamDedup
import redis

# ~10k events/sec * 1 min dedup window → ~600,000.
dedup = RedisStreamDedup(capacity=600_000)

r = redis.Redis(host="localhost", port=6379)
cursor = "0"
while True:
    # XRANGE bounds are INCLUSIVE on both ends. After the first
    # page we must use the exclusive form `(<id>` so we don't
    # re-read the entry the cursor points at — a vanilla
    # `min=cursor` loop spins forever once the cursor reaches the
    # tail and the same entry is returned every iteration.
    start = cursor if cursor == "0" else f"({cursor}"
    entries = r.xrange("net:shard:0", min=start, max="+", count=100)
    for entry_id, fields in entries:
        dedup_id = fields.get(b"dedup_id", b"").decode()
        if not dedup_id:
            # No dedup_id → older entry or non-Net producer; skip
            # dedup and process as-is.
            process(entry_id, fields)
            continue
        if not dedup.is_duplicate(dedup_id):
            process(entry_id, fields)
        cursor = entry_id.decode()
    if not entries:
        break
```

Surface:

```python
dedup = RedisStreamDedup()                # default capacity 4096
dedup = RedisStreamDedup(capacity=N)      # explicit; 0 → 1
dedup.is_duplicate(id: str) -> bool       # test-and-insert
dedup.len                                 # property — tracked-id count
dedup.capacity                            # property — configured cap
dedup.is_empty                            # property
dedup.clear()                             # reset (e.g. on consumer-group rebalance)
```

The helper is transport-agnostic — bring your own `redis-py` /
`aioredis` / equivalent client; it just answers the dedup
question against an in-memory LRU. Concurrency: each handle
wraps a Rust `Mutex<RedisStreamDedup>`, so concurrent calls from
multiple Python threads are safe but serialize. Production-shape
is one helper per consumer thread.

## Security Surface (Stage A–E)

The mesh layer surfaces the same identity / capabilities / subnets /
channel-auth story that the Rust SDK and the TypeScript / Node SDKs
ship. Full staging and rationale:
[`docs/SDK_SECURITY_SURFACE_PLAN.md`](../../docs/SDK_SECURITY_SURFACE_PLAN.md).
Python-binding parity details:
[`docs/SDK_PYTHON_PARITY_PLAN.md`](../../docs/SDK_PYTHON_PARITY_PLAN.md).

### Identity + permission tokens

Every node has an ed25519 identity; permission tokens are ed25519-
signed delegations that authorize a subject to `publish` /
`subscribe` / `delegate` / `admin` on a channel, optionally with
further delegation depth.

```python
from net import Identity, parse_token, verify_token, delegate_token

alice = Identity.generate()
bob = Identity.generate()

# Alice issues Bob a subscribe+delegate token good for 5 min, with
# one re-delegation hop remaining. `ttl_seconds=0` raises
# `TokenError` — a zero TTL would mint a born-expired token that
# every receiver would reject as `Expired`, leaving the issuer to
# diagnose the misuse from receiver-side log lines.
token = alice.issue_token(
    subject=bob.entity_id,
    scope=["subscribe", "delegate"],
    channel="sensors/temp",
    ttl_seconds=300,
    delegation_depth=1,
)
assert verify_token(token) is True

# Bob re-delegates to Carol; depth drops to 0 (leaf).
carol = Identity.generate()
child = delegate_token(bob, token, carol.entity_id, ["subscribe"])
assert parse_token(child)["delegation_depth"] == 0
```

### Capability announcements + peer discovery

Announce hardware / software / model / tool / tag fingerprints, then
query the local capability index with a filter.

```python
mesh.announce_capabilities({
    "hardware": {
        "cpu_cores": 16,
        "memory_mb": 65536,
        "gpu": {"vendor": "nvidia", "model": "h100", "vram_mb": 81920},
    },
    "models": [{"model_id": "llama-3.1-70b", "family": "llama",
                "context_length": 128_000}],
    "tags": ["gpu", "prod"],
})

gpu_peers = mesh.find_nodes({
    "require_gpu": True,
    "gpu_vendor": "nvidia",
    "min_vram_mb": 40_000,
})
```

#### Scoped discovery (reserved `scope:*` tags)

A provider can narrow *who its query result reaches* by tagging
its `CapabilitySet` with reserved `scope:*` tags. Queries call
`mesh.find_nodes_scoped(filter, scope)` to filter candidates. The
wire format and forwarders are untouched — enforcement is purely
query-side.

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

Accepted `scope` dict shapes: `{"kind": "any"}` (default),
`{"kind": "global_only"}`, `{"kind": "same_subnet"}`,
`{"kind": "tenant", "tenant": "<id>"}`,
`{"kind": "tenants", "tenants": [...]}`,
`{"kind": "region", "region": "<name>"}`,
`{"kind": "regions", "regions": [...]}`. Reserved announcement
tags: `scope:subnet-local` (visible only under `same_subnet`),
`scope:tenant:<id>`, `scope:region:<name>` — strictest scope
wins. Untagged peers resolve to `Global` and stay visible under
permissive queries. Full design:
[`docs/SCOPED_CAPABILITIES_PLAN.md`](../../docs/SCOPED_CAPABILITIES_PLAN.md).

Capability propagation is multi-hop, bounded by
`MAX_CAPABILITY_HOPS = 16` with `(origin, version)` dedup on every
forwarder. `capability_gc_interval_ms` controls both the index TTL
sweep and the dedup cache eviction. See
[`docs/MULTIHOP_CAPABILITY_PLAN.md`](../../docs/MULTIHOP_CAPABILITY_PLAN.md).

### Subnets

Nodes can bind to a hierarchical `SubnetId` (1–4 levels, each 0–255)
directly, or derive one from announced tags via a `SubnetPolicy`.

```python
# Explicit subnet.
mesh = NetMesh("127.0.0.1:9000", PSK, subnet=[3, 7, 2])

# Or derive from tags.
mesh = NetMesh(
    "127.0.0.1:9001", PSK,
    subnet_policy={
        "rules": [
            {"tag_prefix": "region:", "level": 0,
             "values": {"eu": 1, "us": 2, "apac": 3}},
            {"tag_prefix": "zone:", "level": 1,
             "values": {"a": 1, "b": 2, "c": 3}},
        ]
    },
)
```

### Channel authentication

Publishers set `publish_caps` / `subscribe_caps` / `require_token` on
`register_channel`. Subscribers present a `PermissionToken` via the
optional `token=bytes` kwarg on `subscribe_channel`.

```python
mesh.register_channel(
    "gpu/jobs",
    subscribe_caps={"require_gpu": True, "min_vram_mb": 16_000},
    require_token=True,
)

# Subscriber side, with a token issued by the publisher:
mesh.subscribe_channel(publisher_node_id, "gpu/jobs", token=token_bytes)
```

Denied subscribes raise `ChannelAuthError` (a subclass of
`ChannelError`); malformed tokens raise `TokenError` whose message
has the form `"token: <kind>"` (`invalid_signature`, `expired`,
`delegation_exhausted`, …). Successful subscribes populate an
`AuthGuard` bloom filter on the publisher so every subsequent
publish admits the subscriber in constant time. An expiry sweep
(default 30 s) evicts subscribers whose tokens age out; a per-
peer auth-failure rate limiter throttles bad-token storms. Cross-
SDK behaviour is fixed by the Rust integration suite; see
[`tests/channel_auth.rs`](../../tests/channel_auth.rs) and
[`tests/channel_auth_hardening.rs`](../../tests/channel_auth_hardening.rs).

## nRPC (request / response over the mesh)

nRPC is the request/response convention layer riding on top of
the pub/sub mesh + CortEX folds. Built with the `cortex` feature
(maturin's default picks it up). The native module exposes two
layers:

- **Raw bytes** — `net.MeshRpc` (pyclass): `serve(service, fn) ->
  ServeHandle`, `call(target, service, bytes) -> bytes`,
  `call_service(service, bytes) -> bytes`,
  `call_streaming(target, service, bytes) -> RpcStream`,
  `find_service_nodes(service) -> list[int]`. Synchronous calls
  release the GIL across `runtime.block_on(...)` so other Python
  threads run; handler callbacks dispatch under
  `tokio::task::spawn_blocking` so GIL acquisition doesn't starve
  the runtime.
- **Typed wrapper** — `net.mesh_rpc.TypedMeshRpc`: JSON
  encode/decode at the binding boundary so user code works with
  plain Python objects (dicts, lists, dataclasses via custom
  encoders). Resilience helpers (`RetryPolicy`, `HedgePolicy`,
  `CircuitBreaker`) plus the typed exception classes
  (`RpcNoRouteError`, `RpcTimeoutError`, `RpcServerError`,
  `RpcTransportError`, `RpcCodecError`, `BreakerOpenError`) live
  here too.

```python
from net import NetMesh
from net.mesh_rpc import (
    NRPC_TYPED_BAD_REQUEST,
    RetryPolicy,
    RpcServerError,
    TypedMeshRpc,
)

server = NetMesh("127.0.0.1:9001", "42" * 32)
client = NetMesh("127.0.0.1:9000", "42" * 32)
# (handshake omitted — see "Net Encrypted UDP Transport")

# Server side: register a typed handler. ServeHandle is a context
# manager — `with` ensures unregister on exit.
server_rpc = TypedMeshRpc.from_mesh(server)
def echo_sum(req: dict) -> dict:
    return {"echo": req["text"], "sum": sum(req["numbers"])}

with server_rpc.serve("echo_sum", echo_sum):
    client_rpc = TypedMeshRpc.from_mesh(client)
    reply = client_rpc.call(
        server.node_id(), "echo_sum",
        {"text": "hi", "numbers": [1, 2, 3]},
        opts={"deadline_ms": 200},
    )
    # reply == {"echo": "hi", "sum": 6}

# Streaming responses iterate decoded chunks until EOF or terminal error.
stream = client_rpc.call_streaming(
    target_node_id, "tail", {"tail": "events"},
    opts={"deadline_ms": 5000, "stream_window": 8},
)
for chunk in stream:
    process(chunk)
# stream.close() emits CANCEL to the server (best-effort).
# stream.grant(n) issues an explicit credit publish for batched cadence.

# Resilience helpers: retry / hedge / circuit breaker.
policy = RetryPolicy(max_attempts=4, initial_backoff_ms=50, max_backoff_ms=1000)
client_rpc.call_with_retry(target_node_id, "echo", {"hi": 1}, policy)
```

### Status codes + error model

Every caller-side failure is a typed exception whose message
carries the canonical `nrpc:<kind>: <detail>` prefix. The set of
kinds is fixed by the cross-binding contract: `no_route`,
`timeout`, `server_error`, `transport`, `codec_encode`,
`codec_decode`, `breaker_open`. The `classify_error(e)` helper
in `net.mesh_rpc` extracts the kind string from a caught
exception's message — useful for fallback paths where
discriminating without `isinstance` is awkward (e.g. when the
native module wasn't built and every typed alias collapses to
plain `Exception`):

```python
from net.mesh_rpc import classify_error

try:
    rpc.call(target, "echo", body, opts={"deadline_ms": 200})
except Exception as e:
    kind = classify_error(e)
    if kind == "no_route":
        ...      # capability index didn't surface a target
    elif kind == "timeout":
        ...      # caller-side deadline elapsed
    elif kind == "server_error":
        ...      # str(e) carries `status=0xNNNN message=...`
```

| Constant                       | Hex      | Meaning                                          |
| ------------------------------ | -------- | ------------------------------------------------ |
| `NRPC_TYPED_BAD_REQUEST`       | `0x8000` | Typed handler couldn't decode the request body.  |
| `NRPC_TYPED_HANDLER_ERROR`     | `0x8001` | Typed handler ran but returned an exception.     |

Cross-binding contract spec — including the canonical
`cross_lang_echo_sum` service used by every binding's wire-format
compat test — lives in
[`../../README.md#nrpc`](../../README.md#nrpc). The Python
binding's own compat suite is at
[`tests/test_cross_lang_compat.py`](tests/test_cross_lang_compat.py).

## Compute (daemons + migration)

Run `MeshDaemon`s directly from Python. `DaemonRuntime` owns the
factory table, the per-daemon hosts, and the
`Registering → Ready → ShuttingDown` lifecycle gate that decides
when inbound migrations may land. Daemons are any Python object
whose `process(event)` returns a list of `bytes`/`bytearray`
payloads — the runtime wraps each output in a causal link and
forwards it.

Build the native module with the `compute` feature (maturin picks
it up on the default build) and import from `net`. Full design
notes:
[`docs/SDK_COMPUTE_SURFACE_PLAN.md`](../../docs/SDK_COMPUTE_SURFACE_PLAN.md).

```python
from net import DaemonRuntime, NetMesh, Identity, CausalEvent


class EchoDaemon:
    """Stateless echo — ships every event's payload straight back."""

    name = "echo"

    def process(self, event: CausalEvent) -> list[bytes]:
        return [bytes(event.payload)]

    # Optional: snapshot() / restore(state) for migration-capable daemons.


mesh = NetMesh("127.0.0.1:9000", "42" * 32)
rt = DaemonRuntime(mesh)

# 1. Register factories BEFORE flipping the runtime to Ready.
rt.register_factory("echo", lambda: EchoDaemon())

# 2. Ready the runtime — after this point spawn / migration accept.
rt.start()

# 3. Spawn a daemon; Identity pins the ed25519 keypair so
#    origin_hash / entity_id stay stable across migrations.
identity = Identity.generate()
handle = rt.spawn("echo", identity)
print(f"origin = 0x{handle.origin_hash:08x}")

# 4. Manually feed an event for testing; real delivery happens
#    via the mesh's causal chain.
event = CausalEvent(handle.origin_hash, sequence=1, payload=b"hello")
outputs = rt.deliver(handle.origin_hash, event)

# 5. Clean shutdown.
rt.stop(handle.origin_hash)
rt.shutdown()
```

`process` must be synchronous — the core's contract is sync, and
the PyO3 bridge re-attaches the GIL for the duration of each call.
Raising inside `process` surfaces as `DaemonError` on the caller.

### Migration

`start_migration(origin_hash, source_node, target_node)`
orchestrates the six-phase cutover (`Snapshot → Transfer →
Restore → Replay → Cutover → Complete`). The source seals the
daemon's ed25519 seed into the outbound snapshot using the
target's X25519 static pubkey; the target rebuilds the daemon via
the factory registered under the same `kind`, replays any events
that arrived during transfer, then activates.

```python
from net import MigrationError, migration_error_kind

try:
    mig = rt.start_migration(handle.origin_hash, src_node_id, dst_node_id)
    # mig.phase  — "snapshot" | "transfer" | "restore" | ...
    # mig.source_node / mig.target_node
    mig.wait()                      # blocks to completion
except MigrationError as e:
    kind = migration_error_kind(e)
    if kind == "not-ready":               ...  # target start() didn't run
    elif kind == "factory-not-found":     ...  # target missing this kind
    elif kind == "compute-not-supported": ...  # target has no DaemonRuntime
    elif kind == "state-failed":          ...  # snapshot / restore threw
    elif kind == "identity-transport-failed": ...  # seal / unseal failed
    # ...see SDK_COMPUTE_SURFACE_PLAN.md for the full enum
```

`start_migration_with(origin, src, dst, opts)` exposes
options such as `seal_seed=False` for test scenarios. On the
*target* node, call
`rt.register_migration_target_identity(kind, identity)` before
any migration of that kind lands; without it the runtime rejects
sealed-seed envelopes with
`migration_error_kind == "identity-transport-failed"`.

### Surface at a glance

| Method | Description |
|---|---|
| `DaemonRuntime(mesh)` | Construct against an existing `NetMesh` |
| `rt.register_factory(kind, fn)` | Install a factory (before `start()`) |
| `rt.start() / rt.shutdown()` | Flip the lifecycle gate |
| `rt.spawn(kind, identity, config=None)` | Spawn a local daemon |
| `rt.spawn_from_snapshot(kind, identity, bytes, config=None)` | Rehydrate |
| `rt.stop(origin)` | Stop a local daemon |
| `rt.snapshot(origin)` | Capture bytes for persistence / migration |
| `rt.deliver(origin, event)` | Feed an event (returns `list[bytes]`) |
| `rt.start_migration(origin, src, dst)` | Orchestrate a live migration |
| `rt.register_migration_target_identity(kind, id)` | Pin unseal keypair on target for `kind` |
| `handle.origin_hash` / `entity_id` / `stats()` | Per-daemon identity + stats |
| `DaemonError` / `MigrationError` | Typed exceptions; `migration_error_kind(e)` parses `e.kind` |

## Compute Groups (Replica / Fork / Standby)

HA / scaling overlays on top of `DaemonRuntime`. Build the native
module with the `groups` feature (implies `compute`) to expose
`ReplicaGroup`, `ForkGroup`, `StandbyGroup`, and the `GroupError`
exception class.

- **`ReplicaGroup`** — N interchangeable copies of a daemon.
  Deterministic identity from `group_seed + index`, so a replacement
  respawned on another node has a stable `origin_hash`. Load-balances
  inbound events across healthy members; auto-replaces on node failure.
- **`ForkGroup`** — N independent daemons forked from a common parent
  at `fork_seq`. Unique keypairs, shared ancestry via a verifiable
  `ForkRecord`.
- **`StandbyGroup`** — active-passive replication. One member
  processes events; standbys hold snapshots and catch up via
  `sync_standbys()`. On active failure the most-synced standby
  promotes and replays the events buffered since the last sync.

```python
from net import (
    DaemonRuntime, ForkGroup, GroupError, ReplicaGroup, StandbyGroup,
    group_error_kind,
)

rt = DaemonRuntime(mesh)
rt.register_factory("counter", lambda: CounterDaemon())

# --- ReplicaGroup ----------------------------------------------------
replicas = ReplicaGroup.spawn(
    rt, "counter",
    replica_count=3,
    group_seed=bytes([0x11] * 32),
    lb_strategy="consistent-hash",   # or "round-robin" / "least-load"
                                     #    / "least-connections" / "random"
)
origin = replicas.route_event({"routing_key": "user:42"})
rt.deliver(origin, event)
replicas.scale_to(5)                 # grow
replicas.on_node_failure(failed_node_id)   # respawn elsewhere

# --- ForkGroup -------------------------------------------------------
forks = ForkGroup.fork(
    rt, "counter",
    parent_origin=0xABCDEF01,
    fork_seq=42,
    fork_count=3,
    lb_strategy="round-robin",
)
assert forks.verify_lineage()
for record in forks.fork_records():
    print(record["forked_origin"], record["fork_seq"])

# --- StandbyGroup ----------------------------------------------------
hot = StandbyGroup.spawn(
    rt, "counter",
    member_count=3,                  # 1 active + 2 standbys
    group_seed=bytes([0x77] * 32),
)
rt.deliver(hot.active_origin, event)
hot.sync_standbys()                  # periodic catchup
# On active-node failure:
# new_origin = hot.on_node_failure(failed_node_id)  # auto-promotes
```

### Typed errors

Failures raise `GroupError` (a subclass of `DaemonError`). Use
`group_error_kind(e)` to parse the discriminator from the Rust side's
`daemon: group: <kind>[: detail]` message prefix:

```python
from net import GroupError, group_error_kind

try:
    ReplicaGroup.spawn(rt, "never-registered",
                       replica_count=2, group_seed=bytes(32))
except GroupError as e:
    kind = group_error_kind(e)
    if kind == "not-ready":               ...  # runtime.start() didn't run
    elif kind == "factory-not-found":     ...  # kind wasn't registered
    elif kind == "no-healthy-member":     ...  # routed on an all-down group
    elif kind == "invalid-config":        ...  # e.g. replica_count == 0
    elif kind in ("placement-failed",
                  "registry-failed",
                  "daemon"):              ...  # core failure — read e args
```

Full staging, wire formats, and rationale:
[`docs/SDK_GROUPS_SURFACE_PLAN.md`](../../docs/SDK_GROUPS_SURFACE_PLAN.md).
Core semantics live in the main
[`README.md#daemons`](../../README.md#daemons).

## Performance Tips

1. **Use `ingest_raw()` for maximum throughput** - Pass pre-serialized JSON strings
2. **Use `ingest_raw_batch()` for bulk operations** - Reduces per-call overhead
3. **Increase `ring_buffer_capacity`** - Larger buffers handle bursts better
4. **Match `num_shards` to CPU cores** - Default is optimal for most cases

## License

Apache-2.0
