---
title: Redis Streams Deduplication
description: Why the Redis adapter can write an entry twice, how the persistent producer nonce closes the restart hole, and the consumer-side dedup helper in every binding.
boundary: /docs/sdk/c/memory-and-threading#dedup-handles
boundaryLabel: "Dedup handles in C — memory and threading"
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

## What this does not cover

The helper filters producer-retry duplicates. It is not an exactly-once
guarantee: an LRU that has evicted an id will let a late retry through, which
is why capacity has to cover your window. Delivery semantics generally are in
[Submitted Is Not Completed](/docs/guides/submitted-is-not-completed).
