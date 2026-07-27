---
title: Tool Federation
description: Agents publishing tools that other agents discover and invoke at runtime across trust boundaries, with credentials never leaving the node that holds them.
---

# Tool Federation

Every piece of this ships and is documented separately. This page names the
pattern they add up to, because the pattern is the point and it's easy to miss
when you meet the parts one at a time.

**Tool federation:** an agent publishes what it can do; other agents — on other
machines, in other organizations, written in other languages — discover those
tools at runtime and invoke them, without anyone hand-wiring a list and without
credentials leaving the node that holds them.

## What makes it federation rather than a registry

A registry is a place you publish to and look up from. Federation has no such
place. Four properties do the work:

**Discovery is runtime, not configuration.** A node announces its tools as
capabilities; peers see them appear and disappear. `watch_tools` pushes on
change — no polling, no service catalogue to keep current, and a tool that
stops being served stops being discoverable. See
[Discover and Invoke](/docs/guides/discover-and-invoke).

**Authority is local to the provider.** The node that owns a secret runs the
work that needs it. A caller never receives the credential — it receives the
result. That inverts the usual integration shape, where the caller collects
every API key it might need.

**Visibility is scoped.** A tool can be public, owner-scoped, or visible only
to organizations holding a grant — and in the last case it is *absent* from
what others see, not refused. See
[Private Capabilities](/docs/guides/private-capabilities).

**Schemas travel with the tool.** A descriptor carries its input and output
types, so a caller that discovered a tool a moment ago can type-check against
it. Nothing is agreed in advance.

## The pieces

| Piece | What it contributes | Where |
|---|---|---|
| Capability announcement | Publishing what this node can do, with tags and characteristics | [Capabilities](/docs/concepts/capabilities) |
| `serve_tool` / `call_tool` / `list_tools` / `watch_tools` | The tool surface itself | [Discover and Invoke](/docs/guides/discover-and-invoke) |
| nRPC | The typed transport underneath, with deadlines, streaming and cancellation | [Typed RPC](/docs/guides/nrpc) |
| Organizations | Who may discover and invoke, enforced provider-side | [Private Capabilities](/docs/guides/private-capabilities) |
| MCP bridge | Wrapping existing MCP servers in, and exposing the mesh out to MCP hosts | [Wrap MCP](/docs/guides/wrap-mcp-server), [Expose as MCP](/docs/guides/expose-net-as-mcp) |
| A2A | Handing a *long job* to another agent rather than calling a tool | [Agent-to-Agent](/docs/guides/agent-to-agent) |
| Agent identity | Proving which principal an agent acts for | [Agent Identity](/docs/concepts/agent-identity) |

## Where the MCP bridge fits

MCP is the on-ramp, not the destination. `net wrap` takes an existing stdio MCP
server and publishes its tools as owner-scoped mesh capabilities, so work you've
already done becomes federated without a rewrite. `net mcp serve` goes the other
way, fronting the mesh to a local MCP host like Claude Code.

Bridged tools carry `compat_tier: "mcp_bridge"` and are request/response only —
no streams, migration or artifacts. Native capabilities have the richer surface.
That tier is visible in discovery precisely so a caller can tell which it got.

## When you don't need this

If your tools are fixed, local and known at build time, MCP alone is simpler and
you should use it. Federation earns its complexity when the set of available
tools changes at runtime, spans trust boundaries, or involves credentials that
must not travel. The honest version of that trade is in
[When to Use Net](/docs/worldview/right-and-wrong-use-cases).

## See also

- [The Agentic Mesh](/docs/worldview/agentic-mesh) — why this shape
- [MCP vs Net](/docs/worldview/mcp-vs-net)
- [Build a Recoverable Capability](/docs/agent-briefs/build-a-recoverable-capability)
