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
                         # (net wrap / net mcp serve / net mcp pin / net forwarding)
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

### `net forwarding` (credential forwarding policy)

Manages the **caller-side** policy for opt-in credential/header forwarding —
deny-by-default (see [Credential forwarding](#credential-forwarding-opt-in-deny-by-default)).
It manages **policy only**: it records *that* a secret ref may go to a provider,
never the secret value (that lives in a separate keychain backend).

```
net forwarding enable | disable            # global kill switch (default: off)
net forwarding allow <ref> --header <H> [--provider <ID>...] [--any-provider] \
    [--capability <GLOB>...] [--purpose <TEXT>] [--force]
net forwarding rm <ref>                     # remove a secret ref's policy
net forwarding audit                        # value-free listing of every grant
net forwarding set-value <ref>              # store the secret VALUE (stdin → OS keychain)
```

| Flag (`allow`) | Meaning |
|---|---|
| `<ref>` | secret ref name — a lowercase slug label (`github-token`), never the value |
| `--header <H>` | the wire header the value is injected as (e.g. `Authorization`) |
| `--provider <ID>` | a provider (node id or `org:<name>`) allowed to receive it (repeatable) |
| `--any-provider` | allow any provider — **refused for a secret** (a credential to any destination is an exfiltration hole) |
| `--capability <GLOB>` | a capability-id glob the secret may accompany (e.g. `github.*`); empty matches nothing |
| `--purpose <TEXT>` | audit-legibility label, no enforcement |
| `--force` | required to configure a `Cookie` / `Set-Cookie` header (ambient authority at its worst) |

Every verb takes `--store <PATH>` to point at a non-default policy file (default
`<local data>/net-mesh/forwarding.json`, owner-only, written under a cross-process
lock). Policy (`allow`) and value (`set-value`) are separate steps: a ref can
have a policy but no value yet (forwarding stays off), and `audit` lists every
grant without ever touching a secret. `set-value` reads the secret from **stdin**
(never argv / shell history) into the OS keychain and needs a build with
`--features keychain`.

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

## Credential forwarding (opt-in, deny-by-default)

Net's default is **credential locality**: a secret lives on the machine that
owns the tool and never transits the mesh (this is why `net wrap --env` stays
local). Forwarding inverts that *only* for downstream services that understand
nothing but a bearer header — a **tagged concession**, not a headline feature.
The preference order never changes: provider-held credentials > Net
delegation / identity > forwarded credentials.

Every default here is hostile, and **both ends must opt in**:

- **Caller** — the global kill switch is off, and no secret is bound to any
  provider until `net forwarding allow` names specific providers and
  capabilities. A credential bound to "any" provider is refused.
- **Destination** — accepts no forwarded header until its accept-list names one
  explicitly; anything unlisted is stripped.

Deny wins on any mismatch, and a denial names *which* gate refused (global /
per-header / per-capability / per-identity) — never a header value.

### What's protected

Forwarded header **values** are authority metadata, never capability input —
they appear in no tool schema, argument, or result. On the wire they are sealed
to the destination node's key (anonymous X25519 sealed box + XChaCha20-Poly1305),
so a relay sees nothing; every non-secret envelope field — destination, caller
origin, capability, invocation id, expiry, single-use nonce, declared header
names — is bound as AEAD associated data, so a captured blob can't be redirected
to another destination, replayed against another invocation, or outlive its
short TTL. The secret wrapper type is unserializable and redacts itself in every
log / `Debug` / error path; values enter through the operator (`net forwarding
set-value`), never a model or a tool argument.

### Never for stdio

A wrapped **stdio** MCP server never forwards — per-call env mutation of a
shared child process is cross-caller contamination by construction. Forwarding
applies only to remote / HTTP-facing destinations, and the type system carries
the rule (there is no stdio injection target to construct).

### Honest labeling

A capability whose accept-list includes a credential header (`Authorization`,
`Cookie`, `x-api-key`, and other bearer-credential names) carries the
`accepts_forwarded_credentials` risk tag, so a caller sees the credential
surface in `net_describe_capability` before anything is sent. Security-sensitive
headers can never ride the non-secret "plain header" path, and session cookies
require an explicit `--force` acknowledgement.

> **Status.** The caller-side policy surface (`net forwarding`) and the
> OS-keychain value store ship now, deny-by-default; the forwarded-context object
> and its sealing are in place as primitives. Wiring the seal-and-inject step
> into the live wrap → invoke path — and distributing destination forwarding keys
> — is the remaining integration. Until then forwarding is configured and audited
> but not yet carried end-to-end, so nothing leaves the machine that hasn't been
> explicitly allowed *and* accepted.

## Related

- Guides: [Wrap an MCP Server](/docs/guides/wrap-mcp-server),
  [Expose Net as MCP](/docs/guides/expose-net-as-mcp).
- Worldview: [MCP vs Net](/docs/worldview/mcp-vs-net).
- [CLI Reference](/docs/reference/cli) for `net-mesh transfer` / `typegen` and exit
  codes.
