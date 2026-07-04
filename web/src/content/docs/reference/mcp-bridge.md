# MCP Bridge

Reference for the Net ↔ MCP edge adapter (`net-mesh-mcp`). The bridge lives
entirely in an edge crate — the Net core and protocol crates have **zero** MCP
awareness, and the adapter rides the public `net-mesh-sdk` surface only (the same
rule the Redis / JetStream adapters follow). If MCP churns, the adapter churns; the
mesh does not.

For the how-to, see [Wrap an MCP Server](/docs/guides/wrap-mcp-server) and
[Expose Net as MCP](/docs/guides/expose-net-as-mcp). This page is the surface.

## Install

```bash
cargo install net-cli    # the `net-cli` crate builds the `net-mesh` binary
                         # (net wrap / net mcp serve / net mcp pin)
```

(The operator CLI binary `net-mesh` is produced by the separate `net-cli` crate,
not by the `net-mesh` library crate. Build from source with
`cargo build --release -p net-cli` → `target/release/net-mesh`.)

## Commands

### `net wrap` (supply side)

```
net wrap <name> [OPTIONS] -- <stdio-mcp-server-command...>
```

| Flag | Meaning |
|---|---|
| `<name>` | short label for the wrapped server (not a tool id) |
| `--identity <file>` | operator identity (`seed_hex`); owner-only scoping keys on it |
| `--env KEY=VALUE` | env for the child server (repeatable); **stays local, never transits the mesh** |
| `--allow <origin>` | admit an extra caller origin beyond the owner (repeatable) |
| `--credentialed` | force credential status up (always allowed) |
| `--no-credentials --force` | force credential status down (requires `--force`) |
| `--substitutable` | declare the tools interchangeable across providers (enables failover) |
| `--node-addr / --node-pubkey / --psk-hex` | the mesh peer to join |

Builds a node, **joins the mesh via a peer** (it does not bootstrap the first
node), reads `tools/list`, and announces each tool as a capability with
`compat_tier: "mcp_bridge"`, `visibility: owner_only`. Long-running; emits
`wrapped` → `tools_changed` → `server_exited` lifecycle events.

### `net mcp serve` (demand side)

```
net mcp serve [--identity <file>] [--allow-capability PROVIDER/CAP] [--pin-store <path>] \
  --node-addr <peer> --node-pubkey <hex> --psk-hex <psk>
```

Runs a stdio MCP **server** exposing the mesh to a local MCP host as meta-tools.
Run it under the same identity as the wrap side to invoke your own owner-only
tools. `--allow-capability` pre-approves a spicy capability at startup.

### `net mcp pin` (consent)

```
net mcp pin approve <provider/capability>    # out-of-band human consent
net mcp pin reject  <provider/capability>
net mcp pin list
```

The shim and the pin verbs share one per-user pin store
(`<local data>/net-mesh/mcp-pins.json` by default), so an approval in one terminal
is honored by a running `net mcp serve` in another.

## Meta-tools (what an MCP host sees via `net mcp serve`)

| Meta-tool | Purpose |
|---|---|
| `net_search_capabilities` | find capabilities by query → summaries (id, compat tier, status) |
| `net_describe_capability` | full detail (input/output schema, risk, provider) — never implies invoke |
| `net_invoke_capability` | invoke a capability by `provider/capability` id, typed args → typed result |
| `net_list_pinned_capabilities` | list capabilities the host may invoke |
| `net_request_pin` | request approval the model can't grant itself |

## Compat tier

Wrapped tools carry `compat_tier: "mcp_bridge"` and are **request/response only —
no streams, migration, or artifacts**. Native capabilities are richer. The bridge
is the funnel, not the destination.

## Authority model (confused-deputy defense)

```
MCP host        → talks only to the local shim
Shim            → talks only to the local net daemon
Daemon          → acts under the local user's identity
Remote wrapper  → enforces caller identity/scope (remote authorization)
Credentialed / external / unknown capabilities
                → require local shim consent or an approved pin
Display / search → never grants invocation
Pin approval    → local client consent, NOT remote authorization; wrapper policy always wins
```

Connecting an MCP host to `net mcp serve` grants it **no** ambient authority over
the mesh. Owner-only is the default for wrapped tools; widening is explicit and
per-origin.

## Related

- Guides: [Wrap an MCP Server](/docs/guides/wrap-mcp-server),
  [Expose Net as MCP](/docs/guides/expose-net-as-mcp).
- Worldview: [MCP vs Net](/docs/worldview/mcp-vs-net).
- [CLI Reference](/docs/reference/cli) for `net-mesh transfer` / `typegen` and exit
  codes.
