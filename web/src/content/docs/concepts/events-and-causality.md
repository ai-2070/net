# Events and Causality

An event in Net is the unit of communication on a channel. It carries a payload (opaque JSON bytes), an identity (who produced it), and a causal link (where it sits in the chain of things its producer has done and observed). Everything Net does with state — durable logs, folded views, daemon migration, partition recovery — is expressed in terms of events and the causal links that chain them together.

The causal-ordering model is the part of Net that most reliably surprises people coming from other systems. It's worth a careful read.

## What a causal link is

Every event produced by an entity carries a 32-byte structure called a causal link. The link names the producer (via its origin hash), names the producer's view of what it had observed when it produced the event (a compressed sketch called a horizon), names a monotonic sequence number within the producer's own timeline, and hashes the event back to its parent.

Two facts follow from this structure that drive everything else.

First, **two events from the same producer are totally ordered**. The sequence number says which came first. If you see them out of order, you can detect that — and the parent-hash chain lets you verify that nothing was inserted or rewritten between them.

Second, **two events from different producers are ordered only if their causal cones overlap**. If producer A's event was made before A observed any of producer B's events, the two events are concurrent: there is no fact of the matter about which came first. Net doesn't pretend there is one. Software that needs a total order across unrelated producers needs a different primitive (a consensus log, or a centralized timestamp service); Net optimizes for the much more common case where the producers that need to agree are the ones whose events have already mixed.

## Why no global clock

Distributed systems built around a global clock — whether a real one or a logical one like a Lamport timestamp — pay a coordination cost on every event. Net's design point is that you mostly don't need that cost. The events that need to be ordered are the events that have observed each other, and *those* events carry enough information in their causal links to order themselves without going through any central authority.

The corollary is that Net gets faster when you ask it to do less. A channel whose subscribers don't need to know about events from another channel pays nothing for that other channel's traffic. A producer whose events are causally independent of another producer's events doesn't synchronize with that producer. Coordination is opt-in, and the bill is itemized.

## Horizons

The horizon field in a causal link is a compressed sketch of what the producer had observed — from every other producer — at the moment it built the event. It's used for two things.

First, **subscribers can decide whether they've seen enough context to consume an event.** If your daemon's local horizon doesn't include some of the events the incoming event's horizon references, you know there's causal history you haven't seen yet; you can wait, or you can request the missing events, or you can proceed with the understanding that your view is incomplete.

Second, **the horizon makes partition healing tractable.** When two halves of a partitioned mesh reconnect, every reachable producer can compare horizons and figure out exactly which events the other side hasn't seen. There's no full-log diff, no Merkle tree exchange — just a horizon swap and a targeted replay.

The horizon is encoded as an 8-byte (64-bit) bloom sketch, so it can ride alongside the rest of the causal link. Sketches have false positives (you might think you've observed something you haven't) but never false negatives (you'll never miss something you actually need). For the partition-healing and out-of-order-detection use cases, that's the right trade-off.

## Entity logs

Each entity's stream of events lives in an *entity log*, an append-only structure that validates the causal chain on every append. The validator confirms that each new event's `parent_hash` matches the hash of the previous event's link plus payload — if it doesn't, the append is rejected, and the chain has been tampered with or corrupted.

The chain hash isn't a security primitive on its own. Tamper resistance comes from Net's AEAD encryption on the wire and from the producer's signature on permission-bound events. The chain hash is a structural primitive: it tells you the chain hasn't been *accidentally* damaged or reordered, and it lets you do efficient prefix lookups (any prefix of a valid chain is itself a valid chain).

## State snapshots

Replaying every event from genesis is fine for some workloads (audit, analytics, rebuild-from-scratch recovery) and unworkable for others (a long-lived daemon coming back online after an outage). For those, Net provides snapshots: a captured point-in-time state plus the head causal link plus the entity's horizon, signed by the producer.

A snapshot is a checkpoint. Resume from it and the entity can continue producing events whose causal chain reaches back through the snapshot to its origin, without ever materializing the prefix. Snapshots are how daemons migrate cleanly between nodes, how partitions reconcile without replaying gigabytes of history, and how a new replica catches up to the live tail without taking the producer offline.

## What this gives you in practice

In application code you almost never reach into a causal link directly. The system reads the link to order events, the durable-log layer uses it to deduplicate, the partition-recovery code uses it to drive replay — and your code just gets events delivered in an order that respects causality. The cases where you'll see the link explicitly are debugging, audit, and the small set of operations (snapshot, fork, replicate) where you're choosing to operate on the chain rather than on the events.

The mental model to hold onto is this: in Net, time is a graph, not a number. Two events are ordered if one caused the other; otherwise they're concurrent, and your code should be ready for that. Every other property of the system — wire-speed routing, partition recovery, daemon migration, replicated state — falls out of taking that idea seriously.
