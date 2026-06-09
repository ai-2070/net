# Capabilities

Capabilities are how nodes in a Net mesh describe what they can do. A node announces a capability set — a collection of tags and metadata — and other nodes can query, filter, and route against that set. Capabilities are the substrate underneath placement decisions, channel authorization, capability-aware subscriptions, the targeting layer of nRPC, and the discovery surface for AI tools.

The idea is small. The system that grows out of it is unusually flexible: any property of a node that's worth describing — its hardware, its installed software, its physical location, its operating role, its current load, the tools it exposes — can be expressed as a capability and used as input to a decision somewhere else in the mesh.

## The shape of a capability set

A capability set is two things: an opaque set of tags and an ordered map of metadata key-value pairs. The wire format is fully opaque to Net — the SDK ships with `views` for the conventional axes (`hardware`, `software`, `devices`, `dataforts`, `tier`, and so on), but those views are lazy projections, not the source of truth. If your deployment wants a custom axis, you add it; nothing in Net's core has to know about it.

A typical capability advertisement for a fleet node might look something like this when projected through the standard views:

```
hardware.gpu          = "rtx-4090"
hardware.vram_gb      = 24
hardware.cpu_cores    = 32
software.cuda         = "12.3"
software.net          = "0.27.0"
devices.lidar         = "ouster-os1"
location.region       = "us-west-2"
location.zone         = "us-west-2a"
tier                  = "production"
role                  = "inference"
heat.queries_per_min  = 4200
```

Some of these are stable for the lifetime of the node (hardware, installed software). Some change at human pace (location, tier, role). Some change at workload pace (heat counters, current load). The capability subsystem handles all three by giving you change detection, validation, and predicate-friendly access — without forcing you to pick a different mechanism for each rate.

## Predicates

The real work happens in capability predicates. A predicate is a small expression that returns true or false against a capability set. Predicates compose with `and`, `or`, and `not`, and the primitives cover the four kinds of comparison you actually need in practice:

- **Existence.** Does this capability tag exist at all? `exists(hardware.gpu)`.
- **Equality.** Is the metadata value exactly this? `equals(role, "inference")`.
- **Numeric and semver comparison.** Is this version at least this? `software.cuda >= "12.0"`. Is this counter under this threshold? `heat.queries_per_min < 5000`.
- **String matching.** Does this metadata value match a glob or regex? Useful for matching hierarchical labels. Regex is gated behind a Cargo feature; deployments that need it opt in.

Predicates are serializable. The same predicate that you build at a Rust call site can travel as a `net-where:` header on an nRPC request, can be evaluated against a remote node's capability set without round-tripping back to the originator, and can be used as a filter on a channel subscription, a placement decision, or a tool-discovery walk.

## Discovery and propagation

Every node's capability set is disseminated as a signed announcement on a dedicated channel. The receiving node verifies the signature against the sender's ed25519 identity, then applies the announcement to a local `CapabilityFold` — a typed reduction that keeps one entry per `(class, node)` and uses generation numbers to resolve concurrent updates. Announcements carry a TTL; entries past their TTL are evicted by the fold's expiry task. The result is that every node holds an eventually-consistent view of every other reachable node's capabilities, indexed for fast query.

The fold is the discovery substrate for everything that asks "which nodes can do X." `list_tools`, `find_migration_targets`, the placement scheduler's candidate sweep, nRPC's capability-targeted call routing, channel publisher authorization checks — all of them are predicate evaluations over the local fold. There's no separate service-discovery system; there's no central registry to query; the fold *is* the registry, replicated by gossip and queried in memory.

Two properties fall out of this design that are worth holding onto.

**Query is fast and free in the hot path.** Predicate evaluation against the fold is single-digit nanoseconds for simple presence checks and microseconds for indexed multi-field predicates over tens of thousands of nodes — fast enough to call inside scheduling loops without thinking about it. The bulk-query path is index-driven on the conventional axes (`hardware`, `software`, `devices`, `dataforts`); custom axes use the same index machinery via the per-fold `Index` type.

**Aggregation scales the model up.** When a mesh grows large enough that every node holding every other node's capability set becomes wasteful, aggregator daemons in a parent subnet subscribe to a source subnet's detail channels, summarize what they see, and publish summaries upward at a coarser granularity. The substrate provides the framework; deployments decide where to place aggregators and what summarization granularity to use.

## Where capabilities show up

Capabilities are an input to almost every decision in Net beyond the bus itself.

**Channel authorization.** Channels can require that publishers and subscribers match a capability filter — "you must have `hardware.gpu`," or "you must be on `tier.production` and have `software.cuda >= 12`." The check happens at subscription time, the result is cached in the auth guard, and the per-packet path stays at single-digit nanoseconds.

**Placement.** When a daemon needs to run somewhere, the placement layer scores candidate nodes against the daemon's capability requirements and picks the best match. Placement filters can be hard (the daemon must have a GPU; no GPU, no placement) or soft (prefer GPUs, fall back to CPU); they can be composed across multiple axes, and they can be extended with custom scoring logic registered through a process-global callback registry.

**Targeted RPC.** An nRPC call can carry a `net-where:` predicate alongside its method and arguments. The receiver of the request evaluates the predicate against its own capability set; mismatched receivers reject the call without invoking the handler. This lets you write "call this method on any node in `region.us-west-2` with `software.net >= 0.20`" without standing up a separate service-discovery layer.

**Tool discovery.** Any typed nRPC service tagged as an AI tool advertises an `ai-tool:<id>` capability tag through the same fold. `list_tools(matcher)` walks the fold in-memory, applies the matcher, and returns the descriptors. Watch-style streaming is push-driven off the fold's change signal — when a tool joins or leaves the mesh, subscribers see the change without polling.

**Subscriptions.** Subscribers can advertise capability requirements as part of the subscribe call. Publishers that don't match are routed away from that subscriber — useful for fan-out where not every subscriber wants every event.

## Reserved axes

A handful of capability prefixes have specific meaning to the system itself:

- **`causal:`** capabilities describe a node's position in the causal graph — useful for routing causal events efficiently. Blob transfer discovery rides this prefix: a node holding a chunk advertises `causal:<blake3-hex>` and requesters consult the fold to find peers.
- **`fork-of:`** marks a node as a fork or replica of another entity.
- **`heat:`** is the namespace for data-gravity counters. Dataforts and CortEX use these to bias placement and caching.
- **`scope:`** marks the visibility scope of a capability for cross-subnet visibility decisions.
- **`ai-tool:`** marks a node as serving the named LLM-callable tool; populated automatically by `serve_tool`.
- **`subprotocol:`** lists the wire subprotocols the node handles; populated automatically by the substrate from the registry.

Everything else is yours to define. The substrate doesn't impose a schema; it gives you the tools to enforce one when you want to.

## What you'll actually write

In application code you'll see capabilities in three shapes. You'll *declare* them — at process startup, building a `CapabilitySet` that describes the local node. You'll *update* them — when a counter changes, when a role transitions, when a piece of hardware becomes unavailable. And you'll *query against* them — when you write a placement filter, a channel authorization, a targeted RPC predicate, or a tool-discovery walk.

The right model to hold is that capabilities are the typed description of the mesh's topology. Identity says who; channels say where; capabilities say *what kind of node you're talking to*. Once you have all three, most of the questions you'd otherwise solve with service discovery, configuration management, or hand-rolled metadata systems become predicates over a capability set.
