---
title: Net and MCP
description: "MCP describes tools a host can call. Net makes capabilities discoverable across machines and authority boundaries."
---

# Net and MCP

MCP gives agent hosts a standard way to describe and call tools. Net can carry
those tools beyond one configured host by publishing them as capabilities on a
trusted mesh.

> **MCP makes tools callable. Net makes capabilities discoverable.**

The two layers compose:

|                              | MCP                               | Net                                                 |
| ---------------------------- | --------------------------------- | --------------------------------------------------- |
| Organizing object            | tool exposed by a server          | capability offered by a provider                    |
| Normal scope                 | a host and its configured servers | nodes across machines, runtimes, and organizations  |
| Discovery                    | server configuration              | live capability announcements                       |
| Authority                    | host and server policy            | visibility and invocation authority at the provider |
| Work beyond request/response | server-specific                   | streams, durable state, tasks, and artifacts        |

Use MCP directly when a host already knows which local or remote servers it should
call. Add Net when providers change at runtime, capabilities span several machines,
or credentials and policy must remain with the provider.

## Publish an MCP server on Net

`net-mesh wrap` starts an existing stdio MCP server and announces its tools as Net
capabilities:

```sh
net-mesh wrap github -- npx -y @modelcontextprotocol/server-github
```

Wrapped tools are owner-only by default. The wrapping node keeps the server's
credentials and executes the tool locally; callers receive only the permitted
result.

A wrapped tool carries `compat_tier: "mcp_bridge"`. It retains MCP's
request/response shape, so it does not gain native Net streams, migration, or
artifact semantics merely by crossing the bridge.

## Expose Net to an MCP host

The bridge also runs in the other direction:

```sh
net-mesh mcp serve
```

This presents the mesh to an MCP host through a small set of meta-tools for search,
description, and invocation. The host can discover a capability without learning
the Net API. Credentialed or unknown capabilities remain search/describe-only
until approved through the pin and consent flow.

## Choose the smallest layer that fits

Use MCP alone when tools are fixed, local, and known to the host. Use MCP with Net
when those tools need live discovery, provider-held credentials, organization
boundaries, or distributed execution state.

Implementation guides:

- [Wrap an MCP server](/docs/guides/wrap-mcp-server)
- [Expose Net as MCP](/docs/guides/expose-net-as-mcp)
- [MCP bridge reference](/docs/reference/mcp-bridge)
