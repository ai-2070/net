# The Storage Stack

The storage layer turns the ephemeral event bus into something you can build a system on. It's three layers stacked on top of each other — RedEX, CortEX, and NetDB — and the layers are deliberately small enough that you can use one without the others when that's what fits your problem.

The layering follows the same logic as the rest of Net. Each layer adds exactly one capability, and the capability above is expressed in terms of the layer below.

```
┌─────────────────────────────────────────┐
│  NetDB    query plane, federated views  │
├─────────────────────────────────────────┤
│  CortEX   folds, materialized state     │
├─────────────────────────────────────────┤
│  RedEX    durable append-only logs      │
└─────────────────────────────────────────┘
```

You can build a useful Net system using only RedEX. You can build a more useful one with CortEX. You bring in NetDB when you have enough state across enough channels and enough nodes that a single-layer query model gets clumsy.

## RedEX

RedEX is the durable layer. It turns a channel into a named append-only log that persists every event in causal order, indexed by sequence within the log and by cursor for resumption.

The data structure is unromantic: events are appended in arrival order, fsynced in batches, and indexed by an in-memory sequence map that's rebuilt on startup from the on-disk log. There's no compaction primitive at this layer, no schema, no secondary indices — just append, read by cursor, and a tail subscription that delivers new events as they land.

What makes RedEX useful is what it doesn't try to be. It's not a database, so it has no query optimizer, no transaction manager, and no schema migration story to worry about. It's not Kafka, so there's no broker process to operate and no partition rebalancing to plan around. It's a single Rust type (`RedexFile`) backed by a single on-disk file (plus an index), and the API surface is `append`, `read_from`, and `subscribe`. The rest of the storage stack — folds, queries, blobs, replication — composes against that surface.

A RedEX log is owned by a single producer entity. Replication is handled by a separate subprotocol that streams the log's tail to designated replica nodes; consumers on a replica see the same events in the same order with at most a small delay behind the primary. Replication is per-channel and opt-in: you mark a channel as replicated, configure the placement strategy, and the mesh handles the rest.

## CortEX

CortEX runs reductions over RedEX logs. A *fold* in CortEX is a small piece of code (you supply) that consumes events from a log and produces a piece of state (you also supply). The CortEX runtime drives the fold: it pulls events from the log, hands them to your fold function one at a time, persists the resulting state, and exposes it to query.

The model is event sourcing made concrete. The log is the source of truth; the fold is your interpretation of it; the state is whatever your interpretation produced. If you change the fold, CortEX can replay the log from genesis and rebuild the state — there's no separate migration step, because the state was always derivable from events.

CortEX is also where *reactive* state lives. A fold can have subscribers; whenever the fold's state changes, subscribers receive the update. This is the substrate for materialized views: a fold that maintains a per-user counter, a fold that maintains a leaderboard, a fold that maintains a routing table. Subscribers are pulling from a tail just like any other channel consumer, so a CortEX-backed view scales the same way a regular subscriber roster scales.

The two named domain models that ship in CortEX — tasks and memories — are themselves folds. Tasks model long-running workloads with explicit lifecycle (created, running, completed, failed); memories model durable observations that a daemon needs across restarts. Both are useful out of the box, and both serve as worked examples for writing your own folds.

## NetDB

NetDB is the query layer. Once you have enough folds across enough nodes that "go look at this one fold" stops being the right operation, NetDB gives you a single query surface that federates across folds, across channels, and across nodes.

The query language is a small AST — predicate-based selection, time-travel cursors, lineage joins, cross-chain aggregates — designed to compile down to fold reads and capability-routed RPCs without requiring a central planner. A NetDB query is a portable structure; it travels to where the data is, executes there, and returns results. There's no central coordinator, no global query plan, and no synchronous fan-out.

NetDB also has a time dimension. Because every fold's state is reproducible from its log, you can ask a NetDB query "what was this state as of this causal moment" and the runtime will materialize the historical view from the log. Lineage joins use the same primitive: "find every event whose ancestry includes this event" is a graph traversal over causal links, and CortEX has them already.

NetDB is opt-in. It's the layer you reach for when "talk to the right node and read the right fold" stops scaling — typically when you have queries that need to combine data from multiple nodes, or when you need historical or lineage queries that span more than one fold.

## Dataforts

Sitting alongside the three-layer stack is Dataforts: content-addressed blob storage with a greedy-LRU cache and gravity-based placement. Dataforts is for the payloads that don't belong in events themselves — large model weights, training datasets, video segments, anything where you want deduplication and locality but don't need fine-grained causal ordering.

A blob in Dataforts is referenced by its content hash. Producers `put` a blob and get back a `BlobRef`; consumers `get` a `BlobRef` and the runtime fetches the bytes from the nearest node that has them, populating the local cache as it goes. The greedy-LRU policy decides what to evict when the cache is full; the data-gravity counters bias caching and placement toward the nodes that read each blob most often.

Dataforts uses RedEX underneath to track blob metadata and the heat counters that drive placement. Once you have a `BlobRef`, you can pass it through events, store it in folds, and join against it in NetDB queries — blobs and events compose, with the bus carrying the small payloads and Dataforts carrying the large ones.

## Picking your layer

The right way to think about this stack is from the top down. Ask what query you need to satisfy. If the query is "give me the current state of this thing on this node" — that's a fold; you want CortEX. If the query is "what events did this node see in this time window" — that's a log read; you want RedEX. If the query is "show me this aggregate across these nodes" — that's NetDB.

For blobs, ask whether you care about every byte being durable in causal order (events with the bytes inline, RedEX) or whether you care about content-addressed deduplication, locality, and large payloads (BlobRefs in events, bytes in Dataforts). The two cases are usually obvious; the boundary lives somewhere around tens of kilobytes.

You don't have to commit to a layer when you build a channel. The same channel can be ephemeral on day one, marked durable on day two, given a fold on day three, and indexed by NetDB on day four — each change is additive, and none of them invalidate the code you wrote at the layer below.
