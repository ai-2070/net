---
title: Announce
description: Announce typed tools and provider properties so callers with visibility can discover them through the mesh.
capability: Capability announce
boundary: /docs/sdk/c/headers-and-linking
boundaryLabel: C — the capability surface in net.go.h
---

# Announce a Capability

A capability is a typed unit of work a node can do. An announcement places its
descriptor and provider properties into the distributed fold. Callers with the
required discovery authority can then find it. Invocation is a separate decision.

## Two things you can announce

**Tags** describe what a node _is_ — `gpu`, `region:eu-west`, a model it hosts, an
accelerator it has. Nothing is callable; you are populating a placement index so
somebody can find a machine that fits.

**Tools** describe what a node can _do_ — a named, schema'd operation an agent can
invoke. A tool is the thing an LLM's tool-call resolves to.

Most agent code wants tools. Most placement code wants tags. They travel in the
same capability set and are announced by the same call.

## Serving a tool and announcing a tool are different acts

Most bindings expose these as separate operations:

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

The tool API does not hang off the node. It hangs off a _typed RPC surface_
constructed over the node — serve, call, list and watch are all operations on that
surface.

Rust is the exception, and it is the reason this is worth spelling out: in Rust the
mesh node _is_ that surface, so the tool methods are node methods. Documentation
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
announce call returns cleanly and reaches no one. If discovery remains empty,
confirm the node has joined a peer before changing the capability filter. See
[the handshake](/docs/sdk/quickstart).

## Discovery and invocation have separate authority

Announcing does not make a capability visible to every participant or open to
invocation.

Visibility and invocability are separate decisions. Organization-scoped discovery
can hide or encrypt a descriptor for callers outside its audience. The provider
then makes the final admission decision when a caller invokes it. See
[Invoke](/docs/sdk/invoke) and [Errors](/docs/sdk/errors).

The full tag and axis model — hardware, software, model, tool, resource-limit
projections — is in [Capabilities](/docs/concepts/capabilities) and
[Capability Schema](/docs/reference/capability-schema).
