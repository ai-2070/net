# MCP vs Net

The Model Context Protocol (MCP) gave the ecosystem a huge win: a standard way to
make a tool **callable** by an agent. If you have an MCP server, any MCP host can
invoke its tools. That is real supply, and Net builds on it rather than competing
with it.

The distinction in one line:

> **MCP made tools callable. Net makes capabilities discoverable.**

Not "MCP is bad." Not "Net replaces MCP." MCP is the **call surface**; Net is
**discovery, presence, policy, events, artifacts, and coordination** around that
surface. They compose.

## What each layer owns

| | MCP | Net |
|---|---|---|
| **Job** | make a tool callable | make capabilities discoverable + coordinated |
| **Scope** | one host ↔ its wired-in servers | a mesh of nodes across machines / orgs |
| **Discovery** | you configure the servers | query the mesh by capability at runtime |
| **State** | request/response | events, streams, durable logs, recovery |
| **Identity/policy** | host-local | mesh identity, owner-only defaults, consent |
| **Artifacts** | — | content-addressed blob/dir transfer (native) |

MCP answers "how do I call this tool?" Net answers "which capabilities exist right
now, across my trusted mesh, and did the work actually happen?"

## The bridge is the fastest way in

Because MCP already has supply, the quickest path onto the mesh is to **wrap what
you already have**. The bridge runs in both directions.

**Existing MCP server → discoverable Net capability.** Wrap a stdio MCP server and
its tools become capabilities other nodes can discover and invoke:

```
net wrap github -- npx -y @modelcontextprotocol/server-github
```

By default the wrapped tools are **owner-only**: visible to the mesh, but
invocable only by the same root identity until you explicitly widen access.
Credentials stay on the wrapping node.

**Net capability → MCP host tool.** Run a shim that exposes the mesh to any MCP
host as a small set of meta-tools — `search`, `describe`, `invoke`:

```
net mcp serve
```

Now an ordinary MCP host (a desktop agent, an IDE) can search the mesh, describe a
capability's schema and risk, and invoke it — without knowing Net exists. Anything
credentialed or unknown is search/describe-only until a human approves it
out-of-band:

```
net mcp pin approve provider/capability
```

## The honest limit: bridged tools are request/response

A capability that came in through the MCP bridge carries
`compat_tier: "mcp_bridge"` and is **request/response only** — no streams, no
migration, no artifacts. That's a deliberate boundary: the bridge is the funnel,
not the destination. **Native** Net capabilities are richer — they can stream, fail
and recover as typed events, move artifacts, and migrate between nodes. So the
bridge is how you *get on the mesh fast*; native capabilities are how you get the
full surface once it's worth it.

## When MCP alone is enough

If your tools are hand-wired, local, and nothing needs to be discovered at
runtime, use MCP directly — you don't need a mesh
([When to Use Net](/docs/worldview/right-and-wrong-use-cases)). Reach for Net when
the tools live somewhere else, the set changes, credentials must stay put, or the
work has state you have to observe and recover.
