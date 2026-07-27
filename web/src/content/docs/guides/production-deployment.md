---
title: Running in Production
description: The defaults are tuned for tests and benchmarks, not for production.
---
# Running Net in Production

The defaults are tuned for tests and benchmarks, not for production. This guide
is the list of decisions you have to make before a deployment is real, and the
consequence of getting each one wrong.

## The defaults you must change

**The adapter.** `EventBusConfig::default()` uses the no-op adapter, which
accepts batches and discards them. Nothing persists, and `poll()` returns
nothing. Choose the mesh (`AdapterConfig::Net`), Redis, or JetStream
deliberately. This is the single most common production surprise — see
[Troubleshooting](/docs/guides/troubleshooting).

**Key material.** The Net adapter needs a PSK you generate and distribute, plus
a static keypair on the responder side whose public half the initiator already
holds. Nothing in the library invents these, rotates them, or notices a leak.
Treat the PSK as a mesh-membership secret. See
[Security model](/docs/concepts/security-model).

**Feature flags.** The default set is the full stack. Pare it down for smaller
builds, but know which choices are behavioural rather than cosmetic: `regex`
fails closed when absent, and `port-mapping` is off because it modifies router
state. See [Install](/docs/start/install#feature-flags).

## Capacity knobs that matter

**Shards** (`num_shards`) parallelize ingest across independent ring buffers.
The default tracks physical core count. Raise it if contention metrics climb
under sustained ingest; each shard costs memory and a worker.

**Batching** (`BatchConfig`) trades throughput against tail latency. Bigger
batches amortize the adapter call; smaller ones cut latency.
`BatchConfig::high_throughput()` and `low_latency()` are the two presets.

**Backpressure** decides what happens when a ring buffer fills, and three of the
four modes drop silently:

| Mode | On a full buffer | Producer learns? |
| --- | --- | --- |
| `DropOldest` (default) | evicts the oldest buffered event | no |
| `DropNewest` | discards the incoming event | no |
| `Sample { rate }` | keeps one in `rate` | no |
| `FailProducer` | returns an error | **yes** |

Pick based on whether your data is more valuable at the head or the tail of the
stream — and whether losing it silently is acceptable. If it isn't, you need
`FailProducer`; monitoring `events_dropped` after the fact is not the same thing.

## Shutdown is a contract, not a nicety

`shutdown().await` stops new ingests, waits for in-flight ones, flushes each
shard, waits for batch workers to dispatch, and tears down. **Drop the bus
without it and buffered events are lost.**

Wire it into your process's termination path, including the panic path. The
`Drop` impl warns and `EventBusStats::shutdown_was_lossy` records it, so a lossy
shutdown is detectable — assert on that flag in integration tests rather than
discovering it in production.

## What to monitor

`bus.stats()` counters are atomic and lock-free; reading them is free. At
minimum:

- **`events_dropped`** — the backpressure signal. Any sustained non-zero value
  means you're losing data.
- **`events_ingested` vs `events_dispatched`** — a widening gap means the
  adapter isn't keeping up.
- **`shutdown_was_lossy`** — should never be true.

On the consume side, `ConsumeResponse` carries `has_more`, `failed_shards`, and
`stalled_shards`. `failed_shards` and `stalled_shards` are adapter-health
conditions and belong on an alert.

[Deck](/docs/reference/deck) gives you the live cluster view — node health,
daemon supervision, migrations, an nRPC tail, and a failures tab.

## Scaling shards at runtime

A bus built with a `ScalingPolicy` (the default) can add and remove shards
while running:

```rust
bus.start_scaling_monitor();                   // automatic, within policy bounds
let added = bus.manual_scale_up(2).await?;     // or drive it yourself
let removed = bus.manual_scale_down(1).await?;
```

Adding is cheap. Removing is not instantaneous: the bus marks the shard
draining, waits for its worker to clear the buffer and dispatch, then
unregisters it — `manual_scale_down` returns only the ids that fully drained.

Note the mid-poll caveat: a scaling change that lands during a `poll()` is
invisible to that call and appears on the next one. Pagination via `next_id`
makes this self-healing, but if you need topology-stable semantics you must
serialize polls against scaling yourself.

## Data retention is the adapter's business

If your cursor points past events the adapter has trimmed (Redis
`max_stream_len`, JetStream `max_messages` / `max_age`), the next poll resumes
from the earliest event still retained. **There is no separate gap signal** —
you will not get an error telling you that you missed events. If detecting gaps
matters, track sequence continuity in your own payloads.

## Before you go live

- [ ] A real adapter is configured, and you've confirmed `poll()` returns data.
- [ ] PSK and static keys are generated, distributed, and stored outside the repo.
- [ ] Permission tokens have validity windows short enough that your revocation
      gap is acceptable — revocation takes effect at the next full check, not the
      next packet.
- [ ] Backpressure mode matches how much you care about silent loss.
- [ ] `shutdown()` is on every termination path, panics included.
- [ ] `events_dropped`, the ingested/dispatched gap, and `failed_shards` are
      alerting.
- [ ] Crate versions match across `net-mesh`, `net-mesh-sdk`, and any bindings —
      see [Versioning](/docs/reference/versioning).
