---
title: Events and Causality
description: "How producer-local sequence, parent hashes, and compact observation horizons describe event relationships without a global total order."
---

# Events and Causality

An event carries an application payload, the identity of its producer, and a causal
link. The link records the event's position in the producer's own history and a
compact summary of what that producer had observed.

This metadata lets Net distinguish three cases:

- one event follows another from the same producer;
- one producer created an event after observing another producer's event;
- the relationship between two events is unknown or concurrent.

It does not impose one total order across unrelated producers.

## Causal links

Each event from an entity carries a 32-byte causal link containing:

- the producer's origin hash;
- a monotonic sequence number in that producer's timeline;
- a hash linking the event to its parent;
- a compact observation summary called a horizon.

Events from one producer are totally ordered by sequence and parent linkage. If an
event arrives with a gap or an unexpected parent hash, a consumer can detect that
its local chain is incomplete or inconsistent.

Across producers, an order exists only when the observations establish one. If A
creates an event after observing B's event, B precedes A in that causal history. If
neither producer observed the other, the events are concurrent for this model.
Applications that require a total order across unrelated writers need a consensus
log, sequencer, or another ordering service.

## Horizons are compact summaries

A horizon is an 8-byte Bloom-style sketch of the producer's observations. It is
small enough to accompany each event, but it is probabilistic: false positives are
possible.

Use the horizon to decide that more context may be required or to narrow a recovery
request. Do not treat it as exact proof that every dependency is present. Sequence
numbers, parent hashes, retained logs, and explicit replay ranges provide the
precise validation needed to close a gap.

Partition recovery therefore depends on more than exchanging horizons. A required
source or replica must still be reachable, the relevant history must still be
retained, and replay must complete successfully.

## Entity logs

An entity log is an append-only history for one producer. On append, the validator
checks the expected sequence and parent linkage. A mismatch identifies a missing,
reordered, corrupted, or conflicting chain segment.

The parent hash is a structural integrity mechanism, not complete authorization by
itself. Session encryption, signatures, permission tokens, and provider policy
supply the relevant security boundaries.

## Snapshots

A snapshot records materialized state together with the entity's current causal
head and horizon. A consumer can restore that state and then replay retained events
created after the snapshot.

Snapshots reduce replay work. They do not guarantee recovery on their own: the
snapshot must be available and valid, required later events must be retained, and a
reachable source must serve them. Migration and replication add their own routing,
placement, and cutover protocols around this primitive.

## Application behavior

Most application code consumes events rather than inspecting causal links directly.
Causal details become visible when diagnosing a gap, validating a log, restoring a
snapshot, or implementing migration and replication.

The practical rule is:

```text
same producer        → sequence and parent linkage define order
observed dependency  → causal metadata records the relationship
no known relationship → treat the events as concurrent
```

See [Durable logs](/docs/guides/durable-logs) for retained history and
[Continuity and migration](/docs/guides/continuity-and-migration) for the protocols
that use these records.
