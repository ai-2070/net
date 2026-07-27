---
title: Redis Streams Deduplication
description: Why the Redis adapter can write an entry twice, how the persistent producer nonce closes the restart hole, and the consumer-side dedup helper in all five languages.
---

# Redis Streams Deduplication

The Redis Streams adapter can, in one specific failure mode, write the same
event to the stream twice. This page explains when, what the bus does about it,
and the helper you use on the consumer side to filter the rest.

If you are writing your own adapter rather than consuming from Redis, the
producer-side contract is in [Adapter Trait](/docs/reference/adapter-trait).

## Where duplicates come from

The adapter writes a batch inside `MULTI`/`EXEC`. If that round-trip times out,
the adapter cannot tell whether Redis committed the transaction. It retries.
When the original did commit, the retry produces a second set of stream entries
with **distinct server-generated `*` ids but identical content** — so a consumer
keying on the stream id sees two different events.

To make that detectable, every `XADD` carries a stable **`dedup_id`** field.
It is derived from `(producer_nonce, shard, sequence_start, i)` and is
identical across retries of the same logical event. Consumers filter on it.

JetStream needs none of this: the same identifier rides in `Nats-Msg-Id` and
the server dedupes natively.

## The restart hole, and `producer_nonce_path`

The producer nonce is what makes `dedup_id` stable, and by default it is fresh
per process. That leaves a gap: a producer that crashes mid-batch and restarts
gets a **new** nonce, so its retransmits look like new events, and the half of
the partial batch that was already accepted is persisted twice.

Persist the nonce to close it:

```rust
let cfg = EventBusConfig::builder()
    .num_shards(4)
    .redis(RedisAdapterConfig::new("redis://localhost:6379"))
    .producer_nonce_path("/var/lib/myapp/producer.nonce")
    .build()?;
```

The bus loads the `u64` at that path, creating it on first run. Point it at
durable storage that survives the process — not `/tmp`, and not a container
layer that is discarded on restart.

## The consumer-side helper

`RedisStreamDedup` answers one question — *have I seen this `dedup_id`?* —
against a bounded in-memory LRU. It is transport-agnostic: read entries with
whatever Redis client you already use, pull `dedup_id` out of each entry's
field map, and ask.

**Sizing.** Capacity is a count of distinct ids, and it needs to cover your
dedup window: a consumer at ~10k events/sec wanting a one-minute window picks
~600,000. The default is 4096, which is only right for low-rate streams.

Use **one helper per consumer thread** — the helpers are not shared-thread-safe
— and call `clear()` after a consumer-group rebalance to reset the window
without discarding the instance.

### Rust

```rust
use net_sdk::RedisStreamDedup;

let mut dedup = RedisStreamDedup::with_capacity(600_000);

for entry in stream {
    let id = entry.fields["dedup_id"].as_str();
    if !dedup.is_duplicate(id) {
        process(entry);
    }
}
```

`with_capacity(n)`, `new()` (default capacity), `is_duplicate(&str) -> bool`
(test-and-insert), `len()`, `capacity()`, `is_empty()`, `clear()`. Re-exported
as `net_sdk::RedisStreamDedup`; the canonical implementation is
`net::adapter::RedisStreamDedup`.

### TypeScript

```typescript
import { RedisStreamDedup } from '@net-mesh/sdk';

const dedup = new RedisStreamDedup(600_000);

for (const entry of entries) {
  const dedupId = entry.fields.dedup_id;
  if (dedupId && dedup.isDuplicate(dedupId)) continue;
  process(entry);
}
```

`new RedisStreamDedup(capacity?)` — omitted defaults to 4096, `0` is clamped
to 1. Then `isDuplicate(dedupId): boolean`, and the getters `len`, `capacity`,
`isEmpty`, plus `clear()`.

### Python

The helper lives on the underlying `net` PyO3 module. `net_sdk`'s `NetNode`
wrapper does not re-export it, so import it directly:

```python
from net import RedisStreamDedup

dedup = RedisStreamDedup(capacity=600_000)

for entry_id, fields in r.xrange("net:shard:0", "0", "+"):
    if not dedup.is_duplicate(fields[b"dedup_id"].decode()):
        process(entry_id, fields)
```

`RedisStreamDedup(capacity=None)`, `is_duplicate(dedup_id) -> bool`, `len()`,
`capacity()`. The hot path releases the GIL.

### Go

```go
d := net.NewRedisStreamDedup(600_000)
defer d.Close()

for _, entry := range entries {
    if d.IsDuplicate(entry["dedup_id"]) {
        continue
    }
    process(entry)
}
```

`NewRedisStreamDedup(capacity uint)` — `0` selects the default. Then
`IsDuplicate(string) bool`, `IsDuplicateChecked(string) (bool, error)` (which
reports a closed handle or an embedded NUL rather than swallowing it), `Len()`,
`Capacity()`, `IsEmpty()`, `Clear()`, and `Close()`. A finalizer calls `Close`,
but close it explicitly.

### C

```c
net_redis_dedup_t *dedup = net_redis_dedup_new(600000);   /* 0 → default 4096 */

if (net_redis_dedup_is_duplicate(dedup, dedup_id) != 1) {
    process(entry);
}

net_redis_dedup_free(dedup);
```

`net_redis_dedup_new(size_t capacity)` never returns NULL. `is_duplicate`
returns `1` for a duplicate the caller should skip, `0` for first sight. Also
`net_redis_dedup_len`, `net_redis_dedup_capacity`, and `net_redis_dedup_free`.

## What this does not cover

The helper filters producer-retry duplicates. It is not an exactly-once
guarantee: an LRU that has evicted an id will let a late retry through, which
is why capacity has to cover your window. Delivery semantics generally are in
[Submitted Is Not Completed](/docs/guides/submitted-is-not-completed).
