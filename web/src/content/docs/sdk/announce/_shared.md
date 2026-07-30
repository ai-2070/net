---
title: Announce
description: A capability is a typed unit of work a node can do. You announce it, every peer folds the announcement into its local index, and anyone can then discover and invoke it.
capability: Capability announce
boundary: /docs/sdk/c/headers-and-linking
boundaryLabel: C — the capability surface in net.go.h
---
# Announce a Capability

A capability is a typed unit of work a node can do. You announce it; every peer
folds the announcement into its local index; anyone who can see it can then
discover and invoke it.

## Two things you can announce

**Tags** describe what a node *is* — `gpu`, `region:eu-west`, a model it hosts, an
accelerator it has. Nothing is callable; you are populating a placement index so
somebody can find a machine that fits.

**Tools** describe what a node can *do* — a named, schema'd operation an agent can
invoke. A tool is the thing an LLM's tool-call resolves to.

Most agent code wants tools. Most placement code wants tags. They travel in the
same capability set and are announced by the same call.

## Serving a tool and announcing a tool are different acts

This is the shape that trips up every binding except one, so it is worth stating
before any code:

1. **Serving** registers a handler on an RPC surface, so that a call addressed to
   that tool name reaches your function.
2. **Announcing** puts the tool's descriptor into your capability set, so that
   peers folding your announcement learn the tool exists.

Do the first without the second and you have a working tool nobody can find:
`discover` returns nothing, `invoke` fails to route, and neither error mentions
announcing. **In Rust the two are fused** — registering a tool inserts it into the
node's tool registry and the next announce carries it. **In TypeScript, Python and
Go they are not**, and each fragment below shows the explicit merge step its
binding needs.

## The RPC handle

The tool API does not hang off the node. It hangs off a *typed RPC surface*
constructed over the node — serve, call, list and watch are all operations on that
surface.

Rust is the exception, and it is the reason this is worth spelling out: in Rust the
mesh node *is* that surface, so the tool methods are node methods. Documentation
written from Rust first tends to project that shape onto the other three, where it
is wrong. Take the constructor from your own fragment.

## Announcement mechanics

**Announcements expire.** The default TTL is five minutes. Re-announce before it
elapses or peers garbage-collect your entry and you quietly stop being
discoverable.

**Re-announcing is cheap.** The mesh diffs against your last announced set, so a
steady-state re-announce costs tens of bytes rather than a full rebroadcast. There
is no reason to hand-roll change detection.

**Announcements travel multi-hop**, bounded by a hop count, so a peer several hops
away can fold your capability without ever having connected to you directly.

**Announcing to nobody succeeds.** If the node has no connected, started peers, the
announce call returns cleanly and reaches no one. This is the single most common
false alarm on this page: the capability filter is fine, the mesh is empty. See
[the handshake](/docs/sdk/quickstart).

## Policy: announced is not invocable

Announcing makes a capability **discoverable**. It does not make it open.

Visibility and invocability are separate decisions, and the boundary is enforced
**on invoke, not on announce**. A credentialed capability can be visible to the
whole mesh while only its owner can call it. Nothing about the announce path grants
anyone the right to do anything — see [Invoke](/docs/sdk/invoke) for where the
check actually happens, and [Errors](/docs/sdk/errors) for what a caller sees when
it fails.

The full tag and axis model — hardware, software, model, tool, resource-limit
projections — is in [Capabilities](/docs/concepts/capabilities) and
[Capability Schema](/docs/reference/capability-schema).
