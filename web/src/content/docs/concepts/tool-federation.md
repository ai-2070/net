---
title: Tool federation
description: "Publish tools as capabilities so applications on other machines can discover and invoke them without receiving the provider's credentials."
---
# Tool federation

Tool federation lets one participant publish a tool as a capability and lets
applications on other machines discover and invoke it at runtime. The provider
keeps its credentials and executes the operation locally.

```text title="Federated tool call"
provider announces a typed tool
→ caller discovers it by capability
→ authority is evaluated
→ provider executes locally
→ caller receives the permitted result
```

## What federation adds

**Live discovery.** Providers announce tools through the capability fold. A watch
reports when tools appear, change, or disappear, so callers do not maintain a
static server list.

**Provider-local authority.** The provider decides who may invoke the tool and
keeps any API keys or device credentials required to execute it.

**Scoped visibility.** A tool may be public, visible only to its owner
organization, or encrypted for organizations holding a discovery grant. Callers
outside the audience do not receive the descriptor.

**Portable schemas.** Tool descriptors carry input and output schemas. A caller
can inspect or generate bindings for a tool it discovered at runtime.

## The pieces

| Piece | Role | Documentation |
|---|---|---|
| Capability announcements | publish what a provider offers | [Capabilities](/docs/concepts/capabilities) |
| `serve_tool`, `call_tool`, `list_tools`, `watch_tools` | serve, discover, and invoke tools | [Discover and invoke](/docs/guides/discover-and-invoke) |
| nRPC | typed request/response and streaming transport | [nRPC](/docs/guides/nrpc) |
| Organization authority | control discovery and invocation | [Private capabilities](/docs/guides/private-capabilities) |
| MCP bridge | publish MCP tools on Net or expose Net to an MCP host | [Net and MCP](/docs/worldview/mcp-vs-net) |
| A2A | delegate a longer job to another agent | [Agent-to-agent](/docs/guides/agent-to-agent) |
| Agent identity | identify the principal an agent represents | [Agent identity](/docs/concepts/agent-identity) |

## MCP tools and native capabilities

`net-mesh wrap` publishes tools from an existing stdio MCP server. `net-mesh mcp
serve` exposes mesh capabilities to a local MCP host.

Bridged tools retain request/response semantics and carry
`compat_tier: "mcp_bridge"`. Native Net capabilities can additionally define
streams, artifact movement, migration, and Net task state. Choose native semantics
when the operation needs them; the bridge is sufficient for ordinary MCP calls.

## When federation is unnecessary

Use MCP directly when tools are fixed, local, and known to one host. Federation is
useful when providers change at runtime, capabilities span machines or
organizations, or credentials must remain on the provider.

See also:

- [The Agentic Mesh](/docs/worldview/agentic-mesh)
- [Build a recoverable capability](/docs/agent-briefs/build-a-recoverable-capability)
