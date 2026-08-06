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

with NetNode(shards=4) as node:
    # Other transports: redis_url=, jetstream_url=, or mesh_* kwargs
    temps = node.channel('sensors/temperature', TempReading)
    temps.publish(TempReading(sensor_id='A1', celsius=22.5))

    for r in temps.subscribe():           # sync generator
        print(f'{r.sensor_id}: {r.celsius}°C')
```

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

- `node.channel('name', Model)` — the named-channel surface. The model may be a
  `@dataclass`, a Pydantic model (anything with `model_dump()`), or a plain
  class (anything with `__dict__`).
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
