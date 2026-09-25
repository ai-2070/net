---
title: Where Net fits
description: "What Net is for, how it relates to the systems you already use, and when another tool is the simpler choice."
---

# Where Net fits

Net connects capabilities across machines, runtimes, and organizations. An
application asks for work to be done; Net discovers providers that announce the
right capability, evaluates availability and authority, invokes one of them, and
keeps the related streams, state, and artifacts attached to the work.

That makes Net a substrate beneath applications, not a replacement for their user
experience or business model. A workspace, fleet console, agent runtime, or
industrial application can use Net while keeping its own workflows, approvals,
and interface.

The shortest description is:

> **Net addresses capabilities under identity and authority.**

Other systems organize distributed work around different objects. HTTP addresses
an endpoint or resource. MCP describes a tool that a configured host can call.
NATS addresses a subject. Zenoh addresses data through a key expression. Net's
unit is a capability offered by a provider, together with the authority and live
state needed to use it.

## Read in this order

1. **[The Agentic Mesh](/docs/worldview/agentic-mesh)** explains the problem from
   an application's point of view.
2. **[When to use Net](/docs/worldview/right-and-wrong-use-cases)** gives the fit
   boundary, including cases where HTTP, MCP, NATS, or a normal database is enough.
3. **[How Net relates to other systems](/docs/worldview/how-net-compares)** compares
   MCP, HTTP, NATS and Zenoh with Net by addressable object, boundary and trust
   model — one section per system, anchored (`#mcp-and-net`, `#http-and-net`,
   `#nats-and-net`, `#zenoh-and-net`).

For the implementation model, continue to [What is Net?](/docs/start/what-is-net).
For a concrete distinction between acceptance, execution, and verified outcome,
read [Submitted is not completed](/docs/guides/submitted-is-not-completed).
