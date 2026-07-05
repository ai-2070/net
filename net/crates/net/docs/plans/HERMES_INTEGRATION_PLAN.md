# Implementation Plan: Hermes Native Integration (v2 — grounded in hermes-agent code)

**Follow-up to:** `net-mcp-bridge-implementation-plan.md`. Bridge plan ships the wedge; this plan makes Hermes (github.com/NousResearch/hermes-agent) the native-citizen showcase.

**Repo ground truth this plan is built on** (verified against main, July 2026):

| Fact | Where | Consequence |
|---|---|---|
| Hermes has a full MCP client: stdio + remote, OAuth, sampling, elicitation, dynamic tool register/deregister with diffing | `tools/mcp_tool.py` (`_register_server_tools`, deregister-diff on refresh) | **Zero-code integration exists today** via `mcp_servers` config → `net mcp serve`. Also: the pin-promotion registration pattern is already written — mirror it. |
| Hermes has progressive tool disclosure built in: `tool_search` / `tool_describe` / `tool_call` bridge tools, threshold-gated, stateless catalog rebuilt per assembly, core tools never defer | `tools/tool_search.py` | Don't build a parallel meta-tool surface for *registered* tools. Net's search tool covers the *mesh index* (unregistered capabilities); Hermes's tool_search covers registered/pinned ones. Two levels, distinct jobs. |
| Tool registry: `ToolEntry(name, toolset, schema, handler, check_fn, requires_env, is_async, description, emoji, max_result_size_chars, dynamic_schema_overrides)`; `check_fn` has TTL cache + transient-failure grace | `tools/registry.py` | Pinned capability = one `ToolEntry` in toolset `net`. `check_fn` = mesh reachability (TTL/grace semantics fit mesh flakiness perfectly). `dynamic_schema_overrides` = live descriptor updates — "always up-to-date types" maps onto an existing field. |
| Plugin system: `plugin.yaml` manifests (`kind`, `provides_tools`, `hooks`, `platforms`); plugin override policy prevents silent built-in overrides | `plugins/*/plugin.yaml`, `tools/registry.py` override policy | Integration ships as `plugins/net/` — first-party plugin, no core patches. |
| Cross-platform approval machinery: interactive CLI + gateway approval contexts, `permissions_list_open` / `permissions_respond` exposed over Hermes's own MCP server | `tools/approval.py`, `mcp_serve.py` | Pin approval renders through Hermes's existing approval UX (approve from Telegram/Discord/CLI) — but resolves against the **shared SDK** pin store. Hermes surface, shared state. |
| Subagent delegation exists (`delegate_task`, spawn depth/concurrency limits) | `tools/delegate_tool.py`, `tools/async_delegation.py` | Delegation chain extends: root → machine → hermes gateway → subagent. Per-subagent attribution is a natural Phase 3 extension. |
| `net-mesh-sdk` 0.30.0 on PyPI (Python binding over the Rust core) | PyPI | `plugins/net/` depends on it; no FFI work in Hermes. |
| Schema sanitizer for third-party tool schemas | `tools/schema_sanitizer.py` | Run mesh descriptors through it before registry entry, same as MCP tools. |

**Naming collision to avoid:** Hermes already has `tool_search`/`tool_describe`/`tool_call`. The Net plugin's mesh-index tools must be unambiguous to the model: `net_search_capabilities` ("searches the Net mesh across your machines — NOT local tools"), `net_describe_capability`, `net_invoke_capability`. Descriptions must state the local/mesh distinction explicitly or models will pick the wrong search.

---

## Doctrine (H-rules)

**The invariant: adapters attach; nodes participate. The MCP bridge translates protocol surfaces; Hermes holds identity as a Net participant.**

1. **H1 — Embedded first-class node.** Hermes embeds the Net node in-process via `net-mesh-sdk` (pyo3 over the Rust core): own keypair, directly addressable, joins the mesh itself. Marginal memory/CPU buys direct addressability — which A2A, streams, and migration all require. No daemon exists; if per-machine node count ever hurts, a shared daemon returns later as an optimization *behind the same SDK API*, invisible to Hermes. **The SDK API must never expose whether the local participant is an embedded node or a shared daemon** — that opacity is what keeps the refactor possible.
2. **H2 — One consent engine, one implementation.** Pin store, consent state, arg validation, credentialed blocklist, audit: implemented once in the Rust SDK, shared across every process on the machine (locked per-user store). Hermes approval UX, the shim, and the CLI are views over the same state. Approved anywhere = approved everywhere. **The shared store is policy/consent state, not shared node identity** — each first-class app/agent node keeps its own identity and delegation; nobody flattens keypairs into the store.
3. **H3 — Delegation before publication.** No side-effectful tool publication until per-agent identity exists.
4. **H4 — Explicit, tagged publication.** Selected tools only, owner-only default, risk-tagged.
5. **H5 — Streams feed state; models query folds.** Raw chunks never enter context.
6. **H6 — Public SDK + public plugin API only.** The integration is `plugins/net/` + `net-mesh-sdk` — an **official plugin, not a core dependency**, disabled unless enabled. **No Net-specific Hermes core patches**: if the plugin API lacks a primitive, upstream a general-purpose public plugin/registry/approval hook — never a private Net shortcut, in either codebase.
7. **H7 — No payments, no untrusted networks** in this plan.
8. **H8 — No key material, ever.** Hermes (and its subagents) never reads, receives, or relays private keys — identity or settlement. Keys live in the Rust core or external signers; agents request typed operations only. No tool result, config surface, or A2A message may carry key bytes.

### Anti-goals
No MCP awareness inside `plugins/net/` (it sees `ToolDescriptor`s); no full mesh dump into the registry; no auto-publication of Hermes's toolsets; no silent pin approval; no Hermes-side permission model; no payments.

---

## Cross-plan dependency

Phase 1 requires the consent/pin/validation engine exposed via `net-mesh-sdk` (graduated out of the bridge crate per the SDK plan):
`capability.search/describe/invoke`, `pins.list/request/state`, consent resolved inside `invoke`, audit events, and a pin-change subscription.
**Gate test:** a pin approved via `net mcp pin approve` is immediately visible to a Python SDK client, under concurrent access. Can't write that test → the engine isn't actually shared → refactor first.

---

## Phase 0.5 — Zero-code path (NEW: ship this week)

No Hermes code changes. Hermes's existing MCP client consumes the bridge shim:

```yaml
# ~/.hermes/config.yaml
mcp_servers:
  net:
    command: net
    args: ["mcp", "serve"]
```

- [ ] **Compatibility gate:** verify Hermes's MCP client against the shim (stdio, 2026-07-28 stateless shape). If it can't consume the stateless shim cleanly, fix Hermes's client or add a version adapter in the shim — **never downgrade the shim to old session semantics**
- [ ] Test Hermes restart behavior and MCP reload path (if supported): promoted pin appears without a full restart
- [ ] Test pin approval from a gateway platform (Telegram/Discord) with the gateway running
- [ ] Verify tool-list refresh: pin approved → shim emits listChanged → Hermes's `_register_server_tools` diff picks up the promoted tool without restart
- [ ] Verify Hermes's `tool_search` deferral handles the Net meta-tools correctly (they're non-core → deferrable; confirm the bridge-tool-within-bridge-tool path doesn't confuse the model; if it does, mark Net meta-tools always-load in config)
- [ ] Docs: "Use Net with Hermes today" page — config snippet + the two-machine quickstart

**Acceptance:** stock Hermes, config-only, invokes a wrapped GitHub capability on another machine. This is the fastest demo in either plan and it validates the bridge against a real production MCP client.

**Why keep going past this:** this is the MCP-compatibility tier. Everything below is what native integration adds.

---

## Phase 1 — `plugins/net/`: native client

```
plugins/net/
  plugin.yaml            # kind: standalone, provides_tools: [net_search_capabilities,
                         #   net_describe_capability, net_invoke_capability,
                         #   net_list_pinned_capabilities, net_request_pin]
  __init__.py
  node.py                # embedded Net node lifecycle (join on plugin init, clean shutdown) + mesh-reachability probe (feeds check_fn)
  tools.py               # the five ToolEntry definitions, toolset "net"
  pins.py                # pin-change subscription -> dynamic registration (Phase 2)
  folds.py               # stream fold consumers (Phase 6)
```

- [ ] `plugin.yaml` + registration through the standard plugin loader; respects plugin override policy (Net tools never shadow built-ins)
- [ ] The five tools registered as `ToolEntry`s, toolset `net`, `check_fn` = **local node initialized + SDK usable** — never "remote peers visible." A healthy-but-isolated node keeps its tools; remote absence surfaces as empty search results or per-call mesh-unreachable errors, not tool flicker. Only local node/SDK failure past grace removes tools from the array
- [ ] `net_invoke_capability` calls SDK `capability.invoke` (validation + consent applied inside the SDK, never re-derived in Python); `validation_error` returned verbatim to the model (self-repair); `requires_approval` returns the pin instruction string
- [ ] `net_request_pin` creates a pending request in the shared pin store and returns a **structured response** the model can relay: `{status: "pending_approval", request_id, approval_channels: ["cli","telegram",...], message: "Approve with: net mcp pin approve <id>"}` — never approves
- [ ] **Meta-tools are always-load** (exempt from `tool_search` deferral) while the plugin is enabled — five small, high-leverage tools; a search-to-find-search double indirection kills the flow. Pinned/promoted tools remain deferrable if the set grows large
- [ ] Mesh descriptors pass through `schema_sanitizer` before any model-visible surface
- [ ] Tool descriptions explicitly disambiguate mesh search from Hermes's local `tool_search`

**Acceptance:** the compressed milestone — machine B `net wrap`s GitHub; Hermes on machine A (plugin enabled, no MCP path involved) searches, describes, hits `requires_approval`, user approves, invokes. Plugin sees `ToolDescriptor`s only.

---

## Phase 2 — Pin promotion via dynamic registration

Mirror `tools/mcp_tool.py`'s server-tools pattern — it already solves this exact problem:

- [ ] `pins.py` subscribes to shared pin-store changes via the SDK; on approval, registers a real `ToolEntry` (real schema, risk tags in description, provider info); on unpin/revoke, deregisters — diff-based like `_register_server_tools`, no nuke-and-repave
- [ ] **Pinned tool names are allocated by the shared pin store (SDK-side) and stable** (persist across sessions, retired on unpin). Hermes never invents names. The SDK handles collisions deterministically (two GitHub accounts, same tool on multiple nodes → preferred alias from pin request, else provider-suffixed). One source of truth for what a tool is called across Hermes, the shim, and OpenClaw
- [ ] Every pinned `ToolEntry` gets the same check_fn semantics as the meta-tools (local node/SDK health, not peer visibility) — local failure past grace means pinned tools cleanly vanish, no stale calls into a void
- [ ] Structured results/logs carry **audit refs**: invocation id, provider node, capability id, delegation chain id — not necessarily user-visible, always log-present
- [ ] `dynamic_schema_overrides` wired to the live descriptor: schema changes flow through live — the "always up-to-date types" claim, demonstrated inside Hermes. (Pin records carry descriptor_hash for deferred cross-ownership hardening; no v1 enforcement)
- [ ] Pinned tools flow through Hermes's normal pipeline: `handle_function_call`, guardrails, hooks, truncation, and `tool_search` deferral if the pinned set grows large — zero special-casing
- [ ] **Approval UX:** pin requests surface through Hermes's existing approval flow (interactive CLI prompt; gateway `permissions_list_open`/`permissions_respond` for Telegram/Discord approval). Resolution writes to the **shared** pin store. Test: approve a pin from Telegram, verify `net mcp pin list` (CLI) and Claude Code (shim) both see it.

**Acceptance:** pinned remote capability appears as a first-class typed Hermes tool; arg accuracy ≈ local-tool baseline; pin approved in Hermes visible in the shim within seconds and vice versa; descriptor update on the remote side propagates to the model-visible schema without restart.

---

## Phase 3 — Delegated agent identity

- [ ] Chain: `user root → machine → hermes-gateway agent identity`, derived from the user root identity via the SDK at node init
- [ ] Every invocation carries the chain; remote wrapper audit shows *which* Hermes
- [ ] Extension: `delegate_task` subagents get child delegations (`… → hermes-gateway → subagent-N`), so a spawned subagent's mesh calls are attributable and individually revocable — this is a Net-native answer to subagent audit that no MCP setup has
- [ ] `net identity delegations list/revoke` honored: revoking the gateway delegation kills all its subagents' access
- [ ] Delegation acquisition failure at startup → Net tools **unavailable** (check_fn false), never silently degraded to machine identity; expiry/renewal handled by the SDK with renewal-failure surfacing the same way

**Acceptance:** remote logs distinguish gateway vs subagent invocations; revoking one machine's Hermes doesn't touch the other machine's.
**Gate for Phase 4:** complete.

---

## Phase 4 — Hermes publishes selected local tools

- [ ] Explicit publication: `hermes net publish <tool>` or config allowlist — never automatic, never a whole toolset by default
- [ ] Publication sensitivity tiers (enforced, not advisory):

| Tier | Examples | Publish behavior |
|---|---|---|
| harmless | `time.now`, sanitized `sys.info` | explicit publish |
| scoped read | read-only file roots, repo status | explicit roots only |
| **sensitive read** | `session_search` (private conversation history, memory, accidentally-mentioned secrets), browser read state | explicit + warning; owner-only, never bundled |
| side-effectful | write/patch, `terminal_tool`, `cronjob_tools` | dangerous flag + approval |
| desktop/network control | `computer_use_tool`, browser actions | **not publishable until the shared policy engine (Phase 9) exists** — a mesh-published desktop is a remote-control primitive |
- [ ] Risk tags derived from toolset membership + Hermes's own approval classification (it already knows which tools are dangerous — reuse that judgment, don't re-tag by hand)
- [ ] Enforced gate: `side_effectful`-tagged tools cannot publish pre-delegation or without the explicit dangerous flag
- [ ] Published tools are ordinary mesh capabilities: invocable from CLI, shim, or another Hermes, subject to caller-side consent **and provider-side policy** — the provider retains authority; caller consent alone is never sufficient

**Acceptance:** Hermes B publishes one read-only tool; CLI and Hermes A invoke it; a `terminal` publish attempt without the flag is rejected; audit shows full chains both sides.

---

## Phase 5 — A2A as agent capabilities

- [ ] Announce `agent.hermes.message` / `agent.hermes.task` / `agent.hermes.status`; A2A *begins* as capability invocation
- [ ] Task lifecycle day one: `requested, accepted, completed, failed, cancelled` — **cancel maps to nRPC end-to-end cancellation and demonstrably stops remote work**, wired into Hermes's interrupt machinery (`tools/interrupt.py`)
- [ ] Inbound A2A renders as a Hermes conversation/session (it already multiplexes platforms — the mesh is one more channel surface); event chain visible
- [ ] A2A calls subject to the same shared consent engine — an agent capability is a capability

**Acceptance:** Hermes A (laptop) tasks Hermes B (desktop); full traceable chain both sides. Run 2: cancel mid-task, B's work stops (verify via B's session), both chains show `cancelled`.

---

## Phase 6 — Native streaming + fold consumption

- [ ] Streaming capabilities via native channels (log tail, long-running command output, monitoring feeds)
- [ ] **Fold pipeline mandatory:** stream → `folds.py` consumer → local fold state → model queries the fold or receives triggered events. Raw chunks never enter context. (Hermes precedent: it already streams tool output to *display* while summarizing for context — same philosophy, enforce it here.)
- [ ] Trigger support: fold condition emits an event Hermes surfaces as a proactive message (it has the gateway delivery machinery for cron — reuse the delivery path)
- [ ] Backpressure verified at transport and consumer; slow consumer = flat memory; interrupt closes cleanly

**Acceptance:** laptop Hermes invokes `desktop.logs.tail(...)`; error-index fold; model answers "errors in last 5 min?" from the fold; memory flat under a deliberately slow consumer; clean teardown on interrupt.

---

## Phase 7 — Artifacts via Dataforts

- [ ] Artifact refs (content hash) instead of inlined payloads past a size threshold; wired into A2A `context_refs`
- [ ] Integration point: Hermes's `tool_result_storage` / file tools consume pulled artifacts as local files
- [ ] Exercised: generated file, screenshot, patch

**Acceptance:** cross-machine artifact handoff, BLAKE3-verified; 50MB result transits as a ref, not through the task channel.

---

## Phase 8 — Mikoshi migration showcase

- [ ] Hermes task state checkpointable as a fold over its event chain (bridge to `hermes_state` session persistence where sane — don't rewrite Hermes's session model, checkpoint enough to resume)
- [ ] Scripted: task on laptop → checkpoint/artifacts move → continues on desktop → same agent identity, same causal chain; third-node observer sees one uninterrupted stream
- [ ] **Prerequisite:** the identity succession rule (two-nodes-one-identity edge case) must be specified before this phase — migration will hit it

**Acceptance:** 10/10 scripted runs; recorded hero artifact alongside the bridge plan's failover demo.

---

## Phase 9 — Shared permission model

- [ ] One shared policy engine (SDK-side locally, enforced mesh-side by providers) mediates tools, A2A, artifacts, streams, publication visibility
- [ ] Hermes consumes decisions; its approval UX renders the shared engine's prompts; it never grows its own parallel policy layer
- [ ] Fixture policies tested: same-root read-only allowed; `shell` denied unless approved; A2A between named machines; artifact roots; confirmation for `desktop_control`

**Acceptance:** one policy config gates a tool call AND an A2A message; policy change alters both without touching plugin code.

---

## Failure semantics (registry stability)

- **No mid-turn tool mutation.** Hermes never changes the model-visible tool list mid-turn (prompt-cache and schema-stability invariants). Pin/schema/revocation changes update shared state immediately; the registry reflects them at the next tools-assembly boundary.
- **Revocation fails closed anyway.** Invocation handlers check shared consent state at call time — a revoked pin returns structured `capability_revoked` even if the model still holds the stale schema this turn.
- **Node/mesh loss:** meta-tools and pinned tools share the same check_fn TTL/grace — transient flap absorbed, sustained loss removes them cleanly at next assembly; in-flight calls return the canonical mesh-unreachable error.
- **Schema drift:** `dynamic_schema_overrides` pulls the live descriptor at assembly time; a call against a just-changed remote schema gets SDK-side validation against the *current* descriptor, and the validation error names the drift.


## Distribution: stable mirror at `github.com/hermes-pro/net`

Hermes installs plugins by cloning GitHub shorthand: `hermes plugins install hermes-pro/net`. That makes the mirror repo the release artifact, with rules:

- **Mirror is a build output, not a dev repo.** Source of truth is the main tree; CI syncs release-tagged builds one-way. No direct PRs (redirect to source), no force-push, ever.
- **HEAD of main = latest stable**, because clone-install takes HEAD. Pre-releases live on tags/branches only. Breaking HEAD breaks every new install that hour.
- Repo layout is exactly what the Hermes plugin loader expects at root (plugin manifest + package), with `net-mesh-sdk` **pinned** in requirements per release — the plugin never floats the SDK.
- Each release states provenance in the README/tag: built from `<source>@<sha>`. Signed tags.
- `hermes plugins update` re-pulls — same HEAD discipline covers updates.

Install line (docs + homepage): `hermes plugins install hermes-pro/net`

## Non-goals
Untrusted discovery, attestations, spend limits, billing, paid tasks (later ladder); the OpenClaw plugin (bridge plan, optional); Hermes as a horizontal product; patches to Hermes core outside `plugins/net/`.

## Metrics
Phase 0.5 config-to-first-invoke time; pinned-tool arg accuracy vs local baseline; pin approval cross-client propagation latency; % invocations with full delegation chain; fold query latency; cancel round-trip; migration success rate.

## Open risks

| Risk | Mitigation |
|---|---|
| Consent engine partially shim-private instead of SDK-shared | Cross-plan gate test before Phase 1 |
| Model confuses `tool_search` (local) with `net_search_capabilities` (mesh) | Explicit disambiguating descriptions; measure misroutes in Phase 1 |
| Meta-tools always-load costs 5 tool defs in every prompt | Trivial vs the double-indirection failure mode; revisit only if measured context pressure demands it |
| net-mesh-sdk Python API gaps (pin subscription, delegation context) | SDK gap = blocker filed against Net repo, not worked around with private hooks (H6) |
| Hermes upstream churn (1.3M LOC, fast-moving) | Everything in `plugins/net/`; only stable surfaces used (registry, plugin API, approval hooks); CI matrix pins **commit SHAs, not tags** — Hermes installs pull master, so a tag identifies a lower bound, not a code state. Plus a rolling `master HEAD` CI row, since that's what real users run |
| Pin approval UX drift between Hermes/CLI/shim | Single shared store + the cross-client propagation test in Phase 2 |
