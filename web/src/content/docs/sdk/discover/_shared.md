---
title: Discover
description: Query the mesh by what you need rather than by who has it — two surfaces, one for placement and one for tools.
capability: Capability discovery
boundary: /docs/sdk/c/headers-and-linking
boundaryLabel: C — the capability surface in net.go.h
---
# Discover Capabilities

Query the mesh by *what you need*, not by who has it. There is no registry to ask
and no address to know: every node folds the announcements it hears into a local
index, and discovery is a read of that index.

## Two surfaces, two questions

**Filter nodes** answers *which machines fit* — GPUs with enough VRAM, a region, a
model, a tag you invented. It returns node ids. This is placement.

**List tools** answers *what can I call* — the named, schema'd operations peers
have announced. It returns descriptors. This is the agent surface, and it is what
lowers into an LLM's tool array.

The same announcement can feed both.

## Discovery is a local read

Both surfaces read the node's own folded index. Nothing goes on the wire when you
call them, which has three consequences worth holding onto:

- **They are fast and synchronous** in the bindings whose types allow it. A
  discovery call is not a network round trip.
- **They can be empty and correct.** An announcement that has not arrived yet is
  indistinguishable from one that was never made.
- **They are a snapshot.** The answer was true when the index last changed, not
  necessarily now.

## Folding takes a moment — do not poll for it

An announcement propagates asynchronously, and multi-hop: a match can come from a
node several hops away that you have never connected to. So the index right after
a peer announces is often still empty.

The wrong fix is a polling loop. Every binding exposes a **watch**: take a `list`
baseline, then subscribe, and changes are pushed the moment the fold mutates. It is
event-driven off the fold's change signal, so an idle mesh costs zero periodic
work.

Each binding's watch takes an optional interval. **It is a staleness ceiling, not
a poll rate** — a safety-net re-diff at least that often. Leave it unset for pure
event-driven behaviour. Reading it as "how often to check" produces a timer nobody
needed.

## Discovery is advisory

Finding a node that *can* do something gets you no claim on it. There is no
exclusivity in a discovery result, no reservation, and no promise the node will
still be there when you call.

If you need to atomically claim a contended exclusive resource — a GPU island, an
accelerator slot, a licensed seat — that is the gang-claim scheduler, not
`find_nodes`. Using discovery as a booking system is how two callers end up
believing they own the same device.

## Seeing is not calling

A capability you can discover is not a capability you can invoke. Visibility and
invocability are separate, and the check happens on
[invoke](/docs/sdk/invoke) — see [Errors](/docs/sdk/errors) for what a refusal
looks like when it arrives.

Richer predicates (numeric, semver, AND/OR/NOT), the scoping model, and the CLI
equivalent (`net-mesh cap query --tag …`) are in
[Capabilities](/docs/concepts/capabilities).
