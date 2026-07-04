# MCP Bridge — `net wrap` / `net mcp serve`

Read this when the user wants to **bridge Model Context Protocol (MCP) tools over the mesh** in either direction:

- **Supply side (`net wrap`)** — take an existing local stdio MCP server (a `npx`/`uvx` package, an internal tool) and expose its tools as **owner-scoped mesh capabilities** that other nodes can discover and invoke.
- **Demand side (`net mcp serve`)** — expose the mesh's capabilities to a **local MCP host** (Claude Code, Cursor, …) as a stdio MCP server, so the model can search, describe, invoke, and **pin** tools that live on other machines.

Triggers: "MCP", "wrap an MCP server", "bridge MCP over the mesh", "expose mesh tools to Claude Code / Cursor", "net wrap", "net mcp serve", "pin a capability", "net mcp pin", "capability federation".

This is the `net-mesh-mcp` adapter crate (`net/crates/net/adapters/mcp/`) + the `net wrap` / `net mcp` CLI. It **rides on the same primitives** the rest of this skill covers: capabilities (`capabilities.md`) for discovery, nRPC (`nrpc.md`) for the invoke/describe calls. If MCP is not involved, you don't need this file.

---

## Doctrine (the parts that will bite you if you assume otherwise)

1. **Credentials never transit the mesh.** A wrapped server's env vars / tokens stay in that server's child process on the owning machine. Only *tool arguments* and *results* cross the wire — never the secret. There is a permanent CI token-leak test asserting this.
2. **Owner-only by default.** A wrapped tool is invocable and describable only by the **same root identity** that wrapped it (an AEAD-verified `caller_origin`, not a self-claimed field). Widen explicitly with `--allow`. Display/search never implies invocation.
3. **The demand side never trusts a wire-declared credential status.** A capability that self-reports `credential_status: "none"` is **still gated** — `credentialed` / `external_api` / `unknown` / `none` all require a local allowlist entry or an approved pin before `net_invoke_capability` will run them. Consent is fail-closed: an empty policy gates *everything*.
4. **Pin approval is local client consent, not remote authorization.** It clears the *shim's* gate for this user profile on this machine; the remote wrapper's owner scope always wins on top. **The model cannot approve its own access** — `net_request_pin` only records a *pending* request; a human approves out-of-band via `net mcp pin approve`.
5. **Compat tier = request/response only.** Bridged tools carry `compat_tier: "mcp_bridge"`: no streaming, no artifacts, no migration, no sampling/elicitation/resources/prompts. The bridge is the funnel, not the destination — richer semantics are native Net (streams, Dataforts, Mikoshi).

---

## Supply side — `net wrap`

```
net wrap <name> [flags] -- <stdio MCP server command...>
```

Spawns the stdio server, speaks MCP JSON-RPC over its stdin/stdout, reads `tools/list`, lowers each tool to a mesh capability, and serves an nRPC handler per tool plus a `describe` service. Long-running: it emits a streaming `--output` (a `wrapped` event, then `tools_changed` / `server_exited` on lifecycle transitions) and reconciles the mesh on the server's `tools/list_changed`.

```bash
net wrap github --identity ~/.net/id.toml -- npx -y @modelcontextprotocol/server-github
```

Key flags:

| Flag | Effect |
|---|---|
| `--identity <PATH>` | Operator identity file (`seed_hex = "..."`). Owner-only scoping keys on it, so use a **stable** identity, not an ephemeral key — an ephemeral key would admit nobody. Defaults to the profile's `identity`. |
| `--env KEY=VALUE` | Env var for the wrapped server (repeatable). **This is where credentials go** — they live in the child, never on the wire. |
| `--credentialed` | Force `credential_status = credentialed` (upward override — always allowed). |
| `--no-credentials` + `--force` | Force `credential_status = none` (downward override — needs `--force`; unknown is spicy until proven boring). |
| `--substitutable` | Declare the tools `provider_equivalent` (Phase 4 collapse/failover eligibility). Default is `provider_local` — a filesystem-class tool stays provider-local forever. |
| `--allow <ORIGIN>` | Admit an extra caller origin (decimal or `0x`-hex) beyond same-root (repeatable). |

**Tool-name sanitization:** a tool whose MCP name isn't already a valid channel id (uppercase / camelCase / spaces / punctuation — `createIssue`) is **sanitized** into a stable channel-safe id and still bridged; the original name is kept for invocation. Only a tool with an *empty* name is skipped. So wrapping GitHub-style servers doesn't silently drop tools.

---

## Demand side — `net mcp serve`

```
net mcp serve [flags]
```

A stdio MCP server that fronts the mesh. Point a host at it with one line of config:

```json
{ "mcpServers": { "net": { "command": "net", "args": ["mcp", "serve"] } } }
```

It is a **thin client to the running `net` daemon**, never an embedded node — N hosts on one machine = N shims, one daemon, one identity. Clear error if no daemon: `No Net daemon is running. Start one with: net up`.

Default surface = five **meta-tools** (not the raw mesh tools — that keeps the host's tool list small and the schema per-call accurate):

| Meta-tool | Does |
|---|---|
| `net_search_capabilities(query)` | Substring search over id / name / description across the mesh; each row carries `cap_id`, `credential_status`, `providers`, and `requires_approval`. |
| `net_describe_capability(cap_id)` | Full input schema + credential status + provider info. |
| `net_invoke_capability(cap_id, arguments)` | Pre-flight validates `arguments` against the schema, checks consent, routes the nRPC `tools/call`, returns the result. |
| `net_list_pinned_capabilities()` | The approved pins. |
| `net_request_pin(cap_id)` | Records a **pending** pin request and returns the approval instructions. Grants nothing. |

Flags:

| Flag | Effect |
|---|---|
| `--identity <PATH>` | Run under the same identity as your `net wrap` side so owner-scoped tools admit you without an explicit `--allow`. |
| `--allow-capability <PROVIDER/CAP>` | Pre-approve a credentialed/external/unknown capability for invocation (repeatable) — a standing allowlist entry, an alternative to pinning. |
| `--pin-store <PATH>` | Pin-store file (defaults to the per-user store the `net mcp pin` verbs write). |
| `--trust-equivalent-providers` | **Opt in** to Phase-4 cross-provider collapse + failover. **Off by default** — see the security note below. |

Consent failure surfaces as: `Capability requires local approval. Approve with: net mcp pin approve <id>`. A remote owner-scope rejection surfaces as: `Denied by remote wrapper: caller root identity does not match owner scope.`

---

## Pinning — promotion to a first-class tool

Pinning is the reliability + consent mechanism, not a convenience.

```
net mcp pin approve <cap_id> [--pin-store <PATH>]
net mcp pin reject  <cap_id> [--pin-store <PATH>]
net mcp pin list           [--pin-store <PATH>]
```

Flow: the model calls `net_request_pin(cap_id)` → a **pending** record is written → a human runs `net mcp pin approve <cap_id>` out-of-band → the serve loop notices the change and emits `tools/list_changed`. An **approved** pin is then **promoted to a first-class typed MCP tool** in the host's tool list, with its real input schema (restores per-call schema accuracy + the host's own per-tool approval prompt), and it clears the shim's consent gate for that capability.

- The pin store is a per-user JSON file, **owner-only `0600`**, written atomically under a cross-process lock (a stale snapshot can't resurrect a revoked approval).
- Promoted tool names are a **pure function of the capability id** (always hash-suffixed), so approving/rejecting *other* pins never remaps a name a host cached onto a different capability.

---

## Phase 4 — collapse + failover (opt-in)

When the same tool is wrapped on several nodes, the demand side *can* collapse them into one logical capability and fail invoke/describe over between providers. **This is off by default** and enabled per-serve with `--trust-equivalent-providers` (or `MeshGateway::trust_equivalent_providers` in code).

Why opt-in: equivalence is proven only from **wire-declared** attributes a peer controls (`substitutability`, `credential_status`, public schema), with no proof the peer shares your owner/root identity — that verification is deferred. On a multi-identity mesh, leaving it on would let a co-tenant that forged a matching contract stand in for your provider. With it off, each provider is discovered, pinned, and invoked on its own node id. Enable only when every mesh peer is your own / trusted.

Collapse is additionally gated to `substitutability == provider_equivalent` **and** `credential_status == none` (cross-account collapse is impossible by construction — the fingerprint folds those fields).

---

## Semantics that differ from a plain tool call

- **Invoke is at-most-once for credentialed tools.** A timeout doesn't prove the tool didn't run, so only an *uncredentialed* (duplicate-safe) tool retries a timed-out call; a credentialed / stateful tool surfaces the timeout rather than re-running it, so a side effect (issue, charge) is never silently duplicated. Failover, when enabled, is likewise limited to the uncredentialed class.
- **Invoke deadline is generous (120s default) and overridable**; describe stays short (5s). A slow tool (web fetch, image gen) isn't killed at 5s.
- **Owner scope gates both describe and invoke** on the AEAD-verified caller origin — so an out-of-scope node sees nothing in search *and* can't invoke.
- **The pin store is reloaded per invoke**, so an out-of-band `net mcp pin approve` takes effect immediately (no restart, no stale-snapshot window).

---

## Cross-references

- `capabilities.md` — the discovery layer the bridge announces onto (`find_nodes` / capability tags). A bridged tool is a capability with `compat_tier: "mcp_bridge"`.
- `nrpc.md` — the request/response layer the `describe` and `invoke` calls ride on.
- `mesh.md` — PSK / identity bootstrap for the two nodes (`net wrap` host + `net mcp serve` client) to reach each other.
- `net/crates/net/docs/plans/MCP_BRIDGE_PLAN.md` — the design of record (phases, doctrine, open risks).
- Source of truth: `net/crates/net/adapters/mcp/src/` (`wrap/*` supply side, `serve/*` demand side, `spec/*` MCP wire types).
