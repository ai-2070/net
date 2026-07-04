# Worldview

Before the machinery, the belief system. These pages explain *why* Net exists —
the world agents are entering, when Net is the right tool and when it isn't, and
how it relates to the things you already use (MCP, REST) — before you learn how
any of it works.

The one line to start from:

> **Net is a discovery mesh for agentic capability** — agents find live
> capabilities across a trusted mesh, invoke them safely, observe what happened,
> and recover when work fails.

Underneath that flagship use case, Net is a latency-first encrypted mesh:
capability discovery, typed RPC, durable logs, folded state, and artifact
transfer on one substrate. The agentic story is the fastest way in; the substrate
is why it holds up. If you want the mechanism first, jump to
[What is Net?](/docs/start/what-is-net).

## Read in this order

1. **[The Agentic Mesh](/docs/worldview/agentic-mesh)** — the worldview: work is
   distributed across tools, APIs, agents, GPUs, and humans, and agents need to
   discover and coordinate it live.
2. **[When to Use Net (and When Not To)](/docs/worldview/right-and-wrong-use-cases)** —
   explicit right and wrong use cases. Net is infrastructure with discipline, not
   "use us for everything."
3. **[MCP vs Net](/docs/worldview/mcp-vs-net)** — MCP made tools callable; Net
   makes capabilities discoverable. How the two compose (they do).
4. **[REST vs Net](/docs/worldview/rest-vs-net)** — where request/response and
   webhooks fit: the legacy edge, not the core.

Then, in the guides:
**[Submitted Is Not Completed](/docs/guides/submitted-is-not-completed)** — why
`200 OK` is not "work done," and what a system that tells the truth about work
looks like. The clearest way in if you've never thought about an event bus.
