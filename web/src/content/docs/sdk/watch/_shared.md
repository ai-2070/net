---
title: Watch
description: Invoking gets you one result. Watching gets you the ongoing facts — the events the work emits as it happens.
capability: Channels — pub/sub with capability auth
boundary: /docs/sdk/c/memory-and-threading
boundaryLabel: C — polling and the cursor trap
---
# Watch the Event Stream

Invoking gets you one result. Watching gets you the ongoing facts: the events the
work emits while it happens. This is the observe half of the agent loop, and it is
what lets you recover from a partial failure instead of trusting a single return
value — see [Submitted Is Not Completed](/docs/guides/submitted-is-not-completed).

## Subscriptions are hot

You see events emitted **after** you subscribe, plus whatever is still in the ring
buffer. Not the whole history.

There is no replay-from-the-beginning on the bus, and that is a design decision
rather than a missing feature. Durability is a separate layer: a RedEX log or a
retaining adapter, covered in [Durable Logs](/docs/guides/durable-logs). A
consumer that needs to see everything since a known point needs one of those, not
a longer timeout.

## Two consumption models, split by binding

This is the sharpest asymmetry on the whole spine, and it is not cosmetic:

- **Rust, TypeScript and Python give you a stream.** You subscribe and iterate;
  the runtime delivers.
- **Go and C give you a cursor.** You call `Poll(limit, cursor)`, get a batch and a
  next cursor, and call again.

Neither is a wrapper over the other at the API level, and code does not port
between them by changing syntax. A Go consumer is a loop you drive; a Python
consumer is a loop that drives you. Plan the shape of your consumer around the
binding you are on rather than around the one the example was written in.

## The bus is location-transparent

The same consumption code works whether the publisher is in-process or several
hops away on the mesh. Between mesh nodes a subscriber joins a named channel by
the publisher's node id and the publisher fans out to its roster; the reading side
does not change.

What does change is what "still in the buffer" means. In-process, the ring buffer
is the whole story. Across the mesh, an event has to arrive before it can be
buffered, so a consumer that starts late misses more than it would locally.

## Watching capabilities is a different thing

This page is about the event stream — the data flowing through a node. Watching
the **set of available tools** change is on [Discover](/docs/sdk/discover), and it
uses a different surface with a different cadence model. The two are easy to
confuse by name and share nothing in implementation.

The concepts are in [Channels](/docs/concepts/channels) and
[Events and Causality](/docs/concepts/events-and-causality).
