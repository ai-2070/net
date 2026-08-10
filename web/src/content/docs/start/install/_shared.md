---
title: Install
description: Net ships as one release across five surfaces — a Rust crate, native Node and Python bindings, a Go module, and a C ABI.
---
# Install

Net ships as **one release across five surfaces**: a Rust crate, native Node and
Python bindings built from that same crate, a Go module over its C ABI, and the C
ABI itself. They are not independent ports. A version number means the same code
underneath, whichever package you install.

## What you are installing

Two layers, and knowing which one you have saves an afternoon:

- **The core binding** — the bus, the mesh transport, the storage stack. In Rust
  that is the crate; in Node and Python it is the native addon.
- **The ergonomic SDK** — typed channels, node builders, RPC helpers. A thin layer
  over the core, published separately.

Some surfaces reach features only through the core layer, never the SDK. That is
not a bug to work around and it is the most common way to be wrong about Net in
Node and Python: if an import fails, check which layer the symbol lives on before
concluding the feature is missing.

## Versions move together

Every package publishes at the same version from the same commit. The current
published release is **0.34**. Pin the same version across layers — a core at one
version and an SDK at another is a combination nobody built or tested.

## What an install gives you

Whichever surface you start on:

- the event bus — publish, poll, filters, shards;
- mesh transport with NAT traversal — peer discovery, encrypted sessions,
  identity-bound routing;
- the storage stack — RedEX logs, CortEX folds, NetDB queries, Dataforts blobs;
- daemon authoring through MeshOS;
- typed RPC through nRPC.

You do not have to use any of the higher layers to use the bus. They are there
when you need them.

## One thing to expect on your first run

**The default transport is memory, and memory discards events after counting
them.** A first program that publishes and then tries to read the event back will
not fail — it will hang, waiting for something that is never coming. Verifying
that the bus *accepted* an event is the right first check, and every language's
instructions below end with exactly that.

Receiving events back needs an adapter that retains them (Redis, JetStream) or the
mesh transport between two nodes. That is a separate decision, not a default.

## Deck, the operator TUI

A separate binary that nothing else depends on. Install it from whichever
ecosystem is convenient — it is the same tool either way:

```sh
cargo install net-deck
npm i -g @net-mesh/deck
pip install net-deck
```

See the [Deck reference](/docs/reference/deck) for the tabs and the signed admin
surface.
