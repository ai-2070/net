---
title: Architecture
description: "How Net separates capability use, state and artifacts, identity and authority, and encrypted transport."
---

# Architecture

Applications usually enter Net through a capability: discover providers, select
one that is currently eligible, and invoke it. Four architectural concerns support
that path:

```text title="Application to wire"
capabilities and invocation
        ↓
state, streams, and artifacts
        ↓
identity and authority
        ↓
encrypted transport and routing
```

These are responsibility boundaries, not isolated systems. A transport failure can
still make a provider unavailable, and an authority change can still alter a
routing result. The separation identifies which layer owns each decision and which
evidence an application can rely on.

Most application code uses capabilities, nRPC, channels, and state. Transport and
packet-level identity become visible during deployment, diagnostics, performance
tuning, or adapter development.

## Capabilities and invocation

A capability names work a provider offers. Its descriptor can include a schema and
properties used for discovery. Applications query the local capability fold,
evaluate their requirements, and invoke a provider through nRPC or a higher-level
tool API.

Three decisions remain distinct:

1. whether a caller may learn that a capability exists;
2. whether a provider is currently available and qualifies for the request;
3. whether that provider admits this invocation.

The provider makes the final admission decision. Applications can call a selected
node directly or address a service name when another eligible provider may perform
the same work.

## State, streams, and artifacts

Events are the common coordination record. They carry producer identity and causal
metadata rather than depending on one global clock.

Higher layers use those records in different ways:

- **RedEX** retains append-only history for replay.
- **CortEX** folds event history into derived state.
- **NetDB** queries supported folded-state models.
- **Dataforts** stores and transfers content-addressed blobs and directories.
- **MeshOS** coordinates long-running stateful daemons and placement.

An application adopts only the pieces it needs. A simple capability may use nRPC
alone. Long-running work may add task events and artifacts. A stateful service may
add a RedEX log and a CortEX fold.

## Identity and authority

A node has a cryptographic identity independent of its current network address.
Sessions authenticate peers, and event metadata carries the origin required by
higher layers.

Authority is evaluated at several boundaries:

- transport and channel policy determine whether traffic can enter a path;
- signed permission tokens grant scoped channel actions;
- organization membership and grants control organization-scoped discovery and
  invocation;
- subnet authority credentials control attachment, routing, and export across
  protected subnet boundaries — topology places a node, but never authorizes
  it;
- provider policy remains final for the operation itself.

Capability predicates are useful for matching provider properties. They are not an
access boundary because providers assert those properties unless an external
attestation says otherwise.

## Transport and routing

The transport layer moves encrypted Net packets between nodes. The current wire
protocol uses UDP, a fixed 68-byte header, a Noise NKpsk0 session handshake, and
ChaCha20-Poly1305 frames. Relays route from authenticated header information without
reading the encrypted application payload.

Transport also owns session setup, congestion behavior, peer routing, NAT
classification and traversal, and direct or relayed paths. Operators configure
listen addresses, bootstrap peers, and any relay or port-mapping policy required by
the deployment.

Exact packet fields and compatibility rules belong in [Wire format](/docs/reference/wire-format).
Operational topology belongs in [NAT and traversal](/docs/guides/nat-and-traversal)
and [Production deployment](/docs/guides/production-deployment).

## What Net does not replace

Net sits beneath applications and can coexist with:

- MCP as a tool interface;
- HTTP at SaaS and web boundaries;
- NATS or another broker inside an operational domain;
- Zenoh for data-centric edge systems;
- databases and object stores that remain systems of record.

Adapters expose selected operations or data to the mesh. The surrounding system
keeps the responsibilities that belong to its layer.

## Where to read next

- [Capabilities](/docs/concepts/capabilities)
- [Identity](/docs/concepts/identity)
- [Events and causality](/docs/concepts/events-and-causality)
- [Storage stack](/docs/concepts/storage-stack)
- [Security model](/docs/concepts/security-model)
