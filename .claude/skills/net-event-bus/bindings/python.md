# Python binding

Read `../apis.md` first for the four surfaces and the cross-SDK rules. This page
is only what is Python-specific.

## Packages and import

```bash
pip install net-mesh-sdk
```

```python
from net_sdk import NetNode
```

**Publishes as `net-mesh-sdk`, imports as `net_sdk`.** There is no package
called `net-sdk`; `pip install net-sdk` does not work.

Underneath sits `net-mesh`, the PyO3 binding, which **imports as `net`**. A few
surfaces live only there and `bindings/coverage.md` marks them `core-only` —
`RedisStreamDedup`, the whole payments surface, compute/groups. `node.bus`
exposes the native module as the escape hatch.

## Construction and lifecycle

```python
from dataclasses import dataclass
from net_sdk import NetNode

@dataclass
class TempReading:
    sensor_id: str
    celsius: float

# A transport that STORES. The default (memory) selects the Noop
# adapter, which counts batches and discards them — publish succeeds
# and `subscribe()` then blocks forever with nothing to yield. Use
# memory for ingestion, batching, backpressure, counters and lifecycle;
# use redis_url= / jetstream_url= / mesh_* the moment a consumer has to
# receive something.
with NetNode(shards=4, redis_url='redis://127.0.0.1:6379') as node:
    temps = node.channel('sensors/temperature', TempReading)
    temps.publish(TempReading(sensor_id='A1', celsius=22.5))

    for r in temps.subscribe():           # sync generator
        print(f'{r.sensor_id}: {r.celsius}°C')
```

That is a **tagged EventBus topic** — one node, many logical streams over its
own bus. It is not distributed pub/sub: for two nodes to exchange events by
channel name, use the `net.NetMesh` channel methods (`core-only` in Python —
`net_sdk.MeshNode` does not wrap them). See `concepts.md` § Channel.

**`NetNode(...)` is synchronous** — no `await`, no factory. Use the context
manager for automatic shutdown.

## The runtime model — a blocking FFI call behind an async surface

`subscribe()` returns an `EventStream` supporting **both** `for ... in` and
`async for` (`net/crates/net/sdk-py/src/net_sdk/stream.py`). Pick one mode per
stream instance; interleaving both on the same instance is undefined.

**The `async for` path still calls a blocking FFI poll on every step.** It is
`async` in shape, not in behaviour — it will stall the event loop under load. In
an asyncio application where loop responsiveness matters, prefer the sync
iterator inside `asyncio.to_thread(...)`. See `runtime.md` § Python.

This is the Python-specific trap that survives being ported from the TypeScript
docs, where `for await...of` really is non-blocking.

## The buffer-capacity rule no compiler enforces

`ring_buffer_capacity` must be a **power of two and at least 1024**. It is
validated in the shared core config at construction, so every binding raises the
same way — and no compile or type check catches it. The default is 1,048,576
(1M events per shard), which is also why a "demonstrate backpressure" snippet
that emits a few thousand events into a default node drops nothing at all.

Spelt `buffer_capacity=1024` in this binding.

## Names and shapes

- `node.channel('name', Model)` — the tagged-topic surface. The model may be a
  `@dataclass` (including `slots=True`), a Pydantic model (anything with
  `model_dump()`), or a plain class (anything with `__dict__` or `__slots__`).
  `name` is validated against the canonical channel grammar and raises
  `ChannelNameError` — lowercase only, no `//`, no leading/trailing `/`, no
  `.`/`..` segments, ≤ 255 bytes.
- `node.channel('name', parse=fn)` — third argument for types whose
  constructor is not `Model(**payload)`. `fn` takes payload-only JSON (the
  `_channel` routing tag is already stripped) and returns the event.
- Discovery is `find_nodes` / `find_nodes_scoped` / `find_service_nodes`,
  returning a **list**, plus `find_best_node` / `find_best_node_scoped`, which
  apply the requirement's weights and return one `int | None`. `None` is no
  match; `0` is a real node id, so test `is None`.
- The predicate DSL is exposed as functions and a builder: `p`, `tag_key`,
  `evaluate_predicate`, `evaluate_predicate_with_trace`, `predicate_debug_report`.

## Errors

The binding raises. `RpcTimeoutError` and `RpcError` must be imported from `net`
directly, not from `net_sdk` — and without the nRPC feature compiled in, every
`Rpc*Error` aliases down to `Exception`, so an `except RpcError` clause silently
becomes `except Exception`. `error-codes.md` has the full hierarchy.

## Shutdown

The context manager handles it. Without `with`, call the shutdown method
explicitly — process exit is not enough.

## Gaps

`bindings/coverage.md` is authoritative. Compute/groups and Redis dedup are
`core-only`; import them from `net`.

## Where to look when this page is not enough

- **Authoritative source:** `net/crates/net/sdk-py/src/net_sdk/` — `node.py`,
  `channel.py`, `stream.py` — over `net/crates/net/bindings/python/`, where the
  PyO3 surface is declared in Rust.
- **Checked examples:** `../examples/hello.py` and `../examples/observe.py` — the
  second reads `events_ingested` / `events_dropped`, the two stats fields this
  binding surfaces. Type-checked with mypy in CI; nothing proves they run.

## Never infer from another binding

- Construction is **synchronous** here and **async** in Node.
  `await NetNode.create(...)` is not Python.
- `async for` here performs blocking FFI work. The TypeScript async iterator
  does not.
- Rust has no `channel()` — do not port a channel example to Rust.
- The predicate functions above are Python and TypeScript shapes. Rust uses the
  `pred!` macro and has none of these free functions.
