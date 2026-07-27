# Docs Claims Audit (Phase 0 of `DOCS_STRATEGY_PLAN.md`)

**Date:** 2026-07-04. **Branch:** `docs-5` (off the near-production MCP branch).

Purpose: confirm every claim the worldview docs will make is cashed by shipped
code, or downgrade/remove it. Each row maps a claim to the primitive
(file/command) that backs it, or to a correction. Verified against code on this
branch — **not** against the `net-claude-skill`, which can lag the branch (it
did, on multi-hop — see below).

Verdict codes: **✅ SHIPPED** (claim is safe) · **⚠️ CORRECT COPY** (real, but the
name/scope in the plan/Kyra draft is wrong) · **❌ NOT SHIPPED** (do not claim).

---

## Capability lifecycle claims

| Claim | Verdict | Backing primitive |
|---|---|---|
| Agents **discover** live capabilities across the mesh | ✅ SHIPPED | `net cap query <tags>` + SDK `find_nodes`; capability index folds signed announcements. **Multi-hop propagation is SHIPPED** — see correction #3. |
| Agents **describe** a capability (schema, risk, provider) before invoking | ✅ SHIPPED | `net mcp serve` meta-tool `net_describe_capability` (`adapters/mcp/src/serve/backend.rs`, `meta_tools.rs`); `ToolDescriptor` carries `input_schema`/`output_schema` + credential/risk status. Display never implies invocation (`serve/consent.rs`, doctrine #3). |
| Agents **invoke** a capability and get a typed result | ✅ SHIPPED | SDK nRPC `Mesh::call_typed` / `call_service` (`sdk/src/mesh_rpc.rs`); via a host, the `net_invoke_capability` meta-tool. MCP-wrap invoke is **at-most-once on timeout** (commit F1) with an overridable deadline (F5). |
| Agents **observe** what happened (events / replay) | ✅ SHIPPED | `subscribe_channel` (`sdk/src/mesh.rs`, `compute.rs`), `net.subscribe`; RedEX replay-from-cursor (`replay_subscriptions`). |
| Agents **recover** when work fails (retry/hedge/breaker) | ✅ SHIPPED | `call_with_retry` / `call_typed_with_retry`, hedge helpers, `CircuitBreaker` (`sdk/src/mesh_rpc_resilience.rs`); task-lifecycle layer. |
| Work can move **artifacts** (blobs/dirs) | ✅ SHIPPED (native only) | SDK `fetch_blob` / `fetch_dir` / `store_dir` (`sdk/src/transport.rs`); `net-mesh transfer` CLI. **Not** carried by MCP-bridged tools — see #2. |
| **Credentials stay local**; owner-only by default; consent-gated | ✅ SHIPPED | `OwnerScope::owner_only` (`adapters/mcp/src/wrap/invoke.rs`); credential-status classification (`wrap/credentials.rs`); pin/consent flow `net mcp pin approve\|reject\|list` (`serve/consent.rs`, `serve/pins.rs`). |
| Existing **MCP server → discoverable Net capability** | ✅ SHIPPED | `net wrap <name> -- <cmd>` (`cli/src/commands/wrap.rs`, `adapters/mcp/src/wrap/`). |
| **Net capability → MCP host tool** | ✅ SHIPPED | `net mcp serve` stdio MCP server exposing `net_*` meta-tools (`adapters/mcp/src/serve/shim.rs`). |

## Substrate claims (the "one layer down" story)

| Claim | Verdict | Backing |
|---|---|---|
| Latency-first encrypted mesh (Noise + ChaCha20-Poly1305, ed25519) | ✅ SHIPPED | Core transport; `concepts/architecture.md`, `reference/wire-format.md`. |
| Durable logs / folded state / federated queries | ✅ SHIPPED | RedEX / CortEX / NetDB (`guides/durable-logs.md`, `cortex-folds.md`, `netdb-queries.md`). |
| Typed RPC on the bus | ✅ SHIPPED | nRPC (`guides/nrpc.md`). |
| Runs vehicular / industrial / robotics / edge | ✅ SHIPPED (positioning) | Root `README.md` § Applications. Keep as breadth, below the agentic lead. |

---

## Corrections the worldview/guide copy MUST apply

**1. ❌ No `net up` onboarding command.** The `net-mesh` top-level commands are:
`identity, admin, ice, snapshot, audit, log, failures, cap, peer, daemon, netdb,
subnet, gateway, channel, aggregator, transfer, wrap, mcp, typegen, completion`
(`cli/src/main.rs`). `net up` appears in `MCP_BRIDGE_PLAN.md` Phase 0a as an
*aspirational* onboarding target; it is **not shipped**. Onboarding in docs must
use the real path — construct a node via the SDK (`MeshBuilder` / `NetNode`) with
a bootstrap peer, or the existing `identity` / `peer` CLI verbs — **not** a
`net up` one-liner. (If a one-liner is wanted, file it as a build task; do not
document it as if it exists.)

**2. ⚠️ `net cap search` and `net cap invoke` do not exist.** The `cap`
subcommands are **`show | query | nodes | announce`** (`cli/src/commands/cap.rs`).
- Discovery by tag = **`net cap query <tags>`** (not `search`).
- Listing the index = **`net cap nodes`**.
- Capability **invocation has no `cap` verb** — invoke via the **SDK (nRPC
  `call_typed`)** or the **`net mcp serve` `net_invoke_capability` meta-tool**.
Every `discover-and-invoke.md` / quickstart reference to `net cap search` /
`net cap invoke` must use these real names.

**3. ✅ Multi-hop discovery is SHIPPED — reverse the "deferred" caveat.**
`MULTIHOP_CAPABILITY_PLAN.md` is **SHIPPED** (stages M-1…M-7): hop-count = 16,
TTL-based dedup, origin rate limiting, route install from receipt, 6 unit + 5
integration tests (`tests/capability_multihop.rs`). Three edge-case integration
tests were deferred (hop-exhaustion at 17+ nodes, tamper-forward, split-horizon)
but the **feature ships**. → `DOCS_STRATEGY_PLAN.md` constraint #3's first bullet
is now wrong; worldview copy **may** claim discovery across the mesh (bounded by
hop-count 16), and should state that bound rather than implying infinite reach.

**4. ✅ Keep the `mcp_bridge` compat-tier caveat (confirmed).** Wrapped MCP tools
carry `compat_tier: "mcp_bridge"` and are **request/response only — no streams,
migration, or artifacts** (`adapters/mcp/src/bridge.rs`, `serve/backend.rs`;
`MCP_BRIDGE_PLAN.md` doctrine #2). Native capabilities are richer. Every page
that mentions the bridge must say the bridge is the funnel, not the destination.

---

## Open item for the REST page

`worldview/rest-vs-net.md` is **gated**: confirm a REST/webhook edge adapter
actually exists before writing an integration-flavored page. Grep found no
first-class REST adapter crate on this branch (unlike `adapters/mcp`,
`adapters/redis`, `adapters/jetstream`). → Ship `rest-vs-net.md` as a **short
positioning note** ("use REST/webhooks at the legacy edge; do not model Net
internally as REST"), not a how-to, unless/until a REST adapter lands.

## Net result

All seven lifecycle claims (discover → describe → invoke → observe → recover →
artifacts → policy) are shipped. The only copy corrections are **command names**
(#1, #2) and **one caveat that flips in our favor** (#3). No worldview claim needs
to be dropped; several need to be named correctly.
