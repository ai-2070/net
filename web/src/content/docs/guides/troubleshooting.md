---
title: Troubleshooting
description: Symptoms that look like bugs and are usually the system working as designed — plus the ones that are genuinely bugs, and how to tell them apart.
---
# Troubleshooting

Symptoms that look like bugs and are usually the system working as designed —
plus the ones that are genuinely bugs, and how to tell them apart.

## `poll()` returns nothing, but `ingest()` succeeded

**Most common by far.** `poll()` reads **through the adapter**, not out of the
local ring buffer. `EventBusConfig::default()` uses the no-op adapter, which
accepts batches and discards them. There is nothing to read back.

```rust
let stats = bus.stats();
// events_ingested = 2, events_dispatched = 2, and poll() still returns []
```

That's not a failure — the events were ingested and dispatched, into a sink that
throws them away. The core crate's readable adapters are Redis, JetStream, and
the mesh. Pick one; see the [Quickstart](/docs/start/quickstart#reading-events-back).

## Events vanish on exit

If you drop the bus without `shutdown().await`, anything still in a ring buffer
is lost. The `Drop` impl prints a warning and `EventBusStats::shutdown_was_lossy`
is set, so this is detectable rather than silent — check that flag in tests and
treat it as a bug in your shutdown path, not a routine event.

A panic that short-circuits your shutdown path does the same thing.

## A re-announced capability doesn't show up

`min_announce_interval` defaults to **10 seconds**. Announcements inside that
window coalesce: the local index and `local_announcement` are updated (so
self-queries and late-joiner session pushes see the newest capabilities), and
one trailing-edge flush re-broadcasts at the end of the window.

The catch: **that trailing-edge flush requires `MeshNode::start_arc`.** With a
bare `start()`, an in-window re-announce is dropped rather than deferred. If
you're testing announce behaviour and a second announcement seems to disappear,
this is almost always why — assert against the host's self-index or an RPC
rather than a peer's view, or lower the interval with
`with_min_announce_interval`.

## Discovery returns nothing on a fresh node

Capability and island folds are **local views that converge**. A node sees its
own announcements immediately; peer-hosted entries appear only once their
announcements propagate. An empty result seconds after startup usually means
"not converged yet", not "nothing exists".

On a genuinely isolated node, only self-hosted entries will ever match.

## Regex filters match nothing

The `regex` feature is **off by default**, and it **fails closed**. A peer can
still send you a regex pattern, but a node built without the feature treats it
as matching nothing — no error, no warning. If you rely on regex predicates,
enable the feature explicitly. See [Install](/docs/start/install#feature-flags).

## A capability call fails with `NoRoute`

No node in the local capability index advertises that service. Either the
provider hasn't announced yet (see convergence above), its announcement hasn't
reached this node, or the service name doesn't match exactly — names are plain
strings compared exactly, with no normalization.

If several nodes serve it and calls fail intermittently, check the proximity
graph's health view: `call_service_typed` skips candidates reported unhealthy,
and with all candidates unhealthy you get `NoRoute` rather than a timeout.

## Handshake never completes

The Net adapter is a Noise `NKpsk0` session between two **named** peers, and it
is not symmetric:

- both sides need the **same PSK**;
- the **initiator** must already hold the responder's static **public** key;
- the **responder** holds the keypair.

An initiator with the wrong `peer_static_pubkey`, or a mismatched PSK, fails the
handshake rather than falling back to anything. Check
`handshake_retries` / `handshake_timeout` before assuming a network problem.

For peers behind NAT, see [NAT and traversal](/docs/guides/nat-and-traversal) —
and note that `port-mapping` (UPnP-IGD / NAT-PMP) is off by default because it
modifies router state.

## Ingest succeeds but events are dropped

Check `stats().events_dropped`. When a shard's ring buffer fills, the
backpressure mode decides what happens, and three of the four modes drop
silently by design:

- `DropOldest` (default) — evicts the oldest buffered event;
- `DropNewest` — discards the incoming event;
- `Sample { rate }` — keeps one in `rate`;
- `FailProducer` — the only mode that tells the caller.

If you need to know about drops at the call site, you have to ask for
`FailProducer`.

## Throughput doesn't improve when I shard the spend policy

It won't. Contention benchmarks put the atomic accounting unit at the
`(day, network, asset)` counter row, but logically independent traffic currently
benchmarks identically to maximum same-counter contention — the coupling is the
global file lock, not accounting authority. Don't design around per-capability
parallelism until the store backend changes. See
[Spend policy](/docs/payments/spend-policy-and-approvals).

## Doc examples don't compile

The Quickstart and Payments snippets are compiled in CI as
[`docs_quickstart.rs`](https://github.com/ai-2070/net/blob/master/net/crates/net/examples/docs_quickstart.rs)
and
[`docs_payments.rs`](https://github.com/ai-2070/net/blob/master/net/crates/net/payments/examples/docs_payments.rs).
If a snippet on those pages doesn't build against your version, check your crate
version first — and then please file it, because it means the example and the
page have drifted.

## Getting more signal

- `bus.stats()` — ingested, dropped, dispatched, batches, lossy-shutdown flag.
  Atomic and lock-free; reading them is free.
- The `has_more`, `failed_shards`, and `stalled_shards` fields on a
  `ConsumeResponse` surface adapter-health conditions worth alerting on.
- [Deck](/docs/reference/deck) — live cluster view including a failures tab and
  an nRPC call tail.
