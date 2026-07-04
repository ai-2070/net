# Wrap an MCP Server

The fastest way to put a capability on the mesh is to wrap a tool you already
have. `net wrap` spawns an existing stdio MCP server, reads its tool list,
announces each tool as a discoverable Net capability, and translates incoming
mesh calls back into MCP `tools/call` — all without the Net core ever learning
that MCP exists.

> **MCP made tools callable. `net wrap` makes them discoverable.** See
> [MCP vs Net](/docs/worldview/mcp-vs-net) for where this fits.

## Prerequisite: a mesh to join

`net wrap` builds a real mesh node under your identity and **joins the mesh via a
peer** — so it needs at least one reachable node to join. You provide that peer's
address, Noise public key, and pre-shared key:

```
--node-addr <ip:port>   --node-pubkey <hex>   --psk-hex <psk>
```

There is no one-command `net up` yet: the bootstrap peer is a running mesh node
you stand up via the SDK (`MeshBuilder`) or an existing daemon. If you just want
to see the wrap → discover → invoke loop end-to-end today, the self-contained
two-node path is the SDK harness in
[Discover and Invoke](/docs/guides/discover-and-invoke); this page is the operator
command for federating a tool onto a mesh that already exists.

## Wrap a server

```
net wrap github \
  --node-addr 203.0.113.7:4433 --node-pubkey 0x<peer-hex> --psk-hex <psk> \
  --identity ./operator.toml \
  --env GITHUB_TOKEN=ghp_xxx \
  -- npx -y @modelcontextprotocol/server-github
```

- `github` is a short **label** for this wrapped server (shown in output; not a
  tool id).
- Everything after `--` is the **stdio MCP server command** to spawn.
- `--identity` is your operator identity file (`seed_hex = "..."`). Owner-only
  scoping keys on it, so use a stable identity, not an ephemeral key.
- `--env KEY=VALUE` (repeatable) sets environment for the child server. **These
  stay in the child process on this machine and never transit the mesh** — this is
  how credentials stay local. The one deliberate exception, for remote/HTTP
  destinations that only speak bearer auth, is opt-in, deny-by-default
  [credential forwarding](/docs/reference/mcp-bridge#credential-forwarding-opt-in-deny-by-default);
  a wrapped stdio server like this one never forwards.

`net wrap` is long-running. It emits a `wrapped` event (the served + skipped
tools, the announced visibility/scope, and any widened origins), then a
`tools_changed` event whenever the server's tool set changes, and a
`server_exited` event when it stops — at which point the capabilities are
withdrawn from the mesh.

## Owner-only by default

Every wrapped tool is announced as **visible to the mesh but invocable only by the
same root identity** — the owner. Discovery is not authorization: other nodes can
*see* and *describe* the capability, but a call from outside the owner scope is
rejected at the wrapper, verified against the AEAD-authenticated caller origin.

To widen who may invoke, admit specific caller origins explicitly:

```
net wrap github --allow 0x<caller-origin> --allow 0x<other-origin> \
  --node-addr <peer> --node-pubkey <hex> --psk-hex <psk> \
  -- npx -y @modelcontextprotocol/server-github
```

`--allow` widens the *local* enforcement beyond the owner; the announced scope
label stays the owner identity, so consumers read the reported `allowed_origins`
to know who may actually invoke. There is no flag that grants blanket mesh-wide
invocation — widening is always an explicit, per-origin decision.

## Credential status

`net wrap` classifies each wrapped tool's credential exposure automatically. You
can override:

- `--credentialed` — force status to `credentialed` (upward; always allowed).
- `--no-credentials --force` — force status to `none` (downward; requires
  `--force` to confirm you really mean it).

Credential status flows into the descriptor a consumer sees, and into the consent
gate on the demand side ([Expose Net as MCP](/docs/guides/expose-net-as-mcp)):
anything credentialed is search/describe-only until a human approves it.

## The honest limit: `mcp_bridge` tier

Wrapped tools carry `compat_tier: "mcp_bridge"` and are **request/response only —
no streams, no migration, no artifacts**. The bridge is the funnel that gets
existing supply onto the mesh fast; the richer surface (streams, recovery as typed
events, artifact transfer, migration) belongs to **native** capabilities. Reach
for a native capability ([Discover and Invoke](/docs/guides/discover-and-invoke))
when a wrapped tool's request/response shape isn't enough.

## Embedding it (SDK)

The CLI wraps the SDK primitive `net_mcp::wrap::wrap_server`. To wrap a server
from inside your own process (so the wrapping node is one you also program
against), call it directly against a `Mesh` you built — the end-to-end path is
demonstrated in `adapters/mcp/tests/wrap_end_to_end.rs`.
