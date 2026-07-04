# Implementation Plan: Hermes Native Integration (v2 — grounded in hermes-agent code)

**Follow-up to:** `MCP_BRIDGE_PLAN.md`. Bridge plan ships the wedge; this plan makes Hermes (github.com/NousResearch/hermes-agent) the native-citizen showcase.

**Repo ground truth this plan is built on** (verified against main, July 2026):

| Fact | Where | Consequence |
|---|---|---|
| Hermes has a full MCP client: stdio + remote, OAuth, sampling, elicitation, dynamic tool register/deregister with diffing | `tools/mcp_tool.py` (`_register_server_tools`, deregister-diff on refresh) | **Zero-code integration exists today** via `mcp_servers` config → `net mcp serve`. Also: the pin-promotion registration pattern is already written — mirror it. |
| Hermes has progressive tool disclosure built in: `tool_search` / `tool_describe` / `tool_call` bridge tools, threshold-gated, stateless catalog rebuilt per assembly, core tools never defer | `tools/tool_search.py` | Don't build a parallel meta-tool surface for *registered* tools. Net's search tool covers the *mesh index* (unregistered capabilities); Hermes's tool_search covers registered/pinned ones. Two levels, distinct jobs. |
| Tool registry: `ToolEntry(name, toolset, schema, handler, check_fn, requires_env, is_async, description, emoji, max_result_size_chars, dynamic_schema_overrides)`; `check_fn` has TTL cache + transient-failure grace | `tools/registry.py` | Pinned capability = one `ToolEntry` in toolset `net`. `check_fn` = daemon liveness (TTL/grace semantics fit mesh flakiness perfectly). `dynamic_schema_overrides` = live descriptor updates — "always up-to-date types" maps onto an existing field. |
| Plugin system: `plugin.yaml` manifests (`kind`, `provides_tools`, `hooks`, `platforms`); plugin override policy prevents silent built-in overrides | `plugins/*/plugin.yaml`, `tools/registry.py` override policy | Integration ships as `plugins/net/` — first-party plugin, no core patches. |
| Cross-platform approval machinery: interactive CLI + gateway approval contexts, `permissions_list_open` / `permissions_respond` exposed over Hermes's own MCP server | `tools/approval.py`, `mcp_serve.py` | Pin approval renders through Hermes's existing approval UX (approve from Telegram/Discord/CLI) — but resolves against the **Net daemon's** pin store. Hermes surface, daemon state. |
| Subagent delegation exists (`delegate_task`, spawn depth/concurrency limits) | `tools/delegate_tool.py`, `tools/async_delegation.py` | Delegation chain extends: root → machine → hermes gateway → subagent. Per-subagent attribution is a natural Phase 3 extension. |
| `net-mesh-sdk` 0.30.0 on PyPI (Python binding over the Rust core) | PyPI | `plugins/net/` depends on it; no FFI work in Hermes. |
| Schema sanitizer for third-party tool schemas | `tools/schema_sanitizer.py` | Run mesh descriptors through it before registry entry, same as MCP tools. |

**Naming collision to avoid:** Hermes already has `tool_search`/`tool_describe`/`tool_call`. The Net plugin's mesh-index tools must be unambiguous to the model: `net_search_capabilities` ("searches the Net mesh across your machines — NOT local tools"), `net_describe_capability`, `net_invoke_capability`. Descriptions must state the local/mesh distinction explicitly or models will pick the wrong search.

---

## Doctrine (H-rules, unchanged)

1. **H1 — Client, not node.** Hermes talks to the local running Net daemon via `net-mesh-sdk`. No embedded node.
2. **H2 — One consent engine, daemon-side.** Pin store, consent state, arg validation, credentialed blocklist, audit: daemon-owned. Hermes approval UX and CLI are views over it. Approved anywhere = approved everywhere.
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

Phase 1 requires the daemon-side consent/pin/validation engine (bridge plan Phases 2–3) exposed via `net-mesh-sdk`:
`capability.search/describe/invoke`, `pins.list/request/state`, consent resolved inside `invoke`, audit events, and a pin-change subscription.
**Gate test:** a pin approved via `net mcp pin approve` is immediately visible to a Python SDK client. Can't write that test → the engine isn't daemon-side → refactor first.

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
  daemon.py              # net-mesh-sdk client wrapper + liveness probe (feeds check_fn)
  tools.py               # the five ToolEntry definitions, toolset "net"
  pins.py                # pin-change subscription -> dynamic registration (Phase 2)
  folds.py               # stream fold consumers (Phase 6)
```

- [ ] `plugin.yaml` + registration through the standard plugin loader; respects plugin override policy (Net tools never shadow built-ins)
- [ ] The five tools registered as `ToolEntry`s, toolset `net`, `check_fn` = daemon liveness via SDK ping (TTL cache + grace absorbs mesh flaps — if daemon down past grace, Net tools cleanly vanish from the tools array instead of erroring mid-turn)
- [ ] `net_invoke_capability` calls daemon `capability.invoke`; `validation_error` returned verbatim to the model (self-repair); `requires_approval` returns the pin instruction string
- [ ] `net_request_pin` creates a pending request daemon-side and returns a **structured response** the model can relay: `{status: "pending_approval", request_id, approval_channels: ["cli","telegram",...], message: "Approve with: net mcp pin approve <id>"}` — never approves
- [ ] **Meta-tools are always-load** (exempt from `tool_search` deferral) while the plugin is enabled — five small, high-leverage tools; a search-to-find-search double indirection kills the flow. Pinned/promoted tools remain deferrable if the set grows large
- [ ] Mesh descriptors pass through `schema_sanitizer` before any model-visible surface
- [ ] Tool descriptions explicitly disambiguate mesh search from Hermes's local `tool_search`

**Acceptance:** the compressed milestone — machine B `net wrap`s GitHub; Hermes on machine A (plugin enabled, no MCP path involved) searches, describes, hits `requires_approval`, user approves, invokes. Plugin sees `ToolDescriptor`s only.

---

## Phase 2 — Pin promotion via dynamic registration

Mirror `tools/mcp_tool.py`'s server-tools pattern — it already solves this exact problem:

- [ ] `pins.py` subscribes to daemon pin-store changes; on approval, registers a real `ToolEntry` (real schema, risk tags in description, provider info); on unpin/revoke, deregisters — diff-based like `_register_server_tools`, no nuke-and-repave
- [ ] **Pinned tool names are allocated by the daemon and stable** (persist across sessions, retired on unpin). Hermes never invents names. Daemon handles collisions deterministically (two GitHub accounts, same tool on multiple nodes → preferred alias from pin request, else provider-suffixed). One source of truth for what a tool is called across Hermes, the shim, and OpenClaw
- [ ] Every pinned `ToolEntry` gets `check_fn` = daemon liveness, same TTL/grace as the meta-tools — daemon gone past grace means pinned tools cleanly vanish, no stale calls into a void
- [ ] Structured results/logs carry **audit refs**: invocation id, provider node, capability id, delegation chain id — not necessarily user-visible, always log-present
- [ ] `dynamic_schema_overrides` wired to the live descriptor: if the remote capability's announced type changes, the model sees the new schema at next `get_definitions()` — the "always up-to-date types" claim, demonstrated inside Hermes
- [ ] Pinned tools flow through Hermes's normal pipeline: `handle_function_call`, guardrails, hooks, truncation, and `tool_search` deferral if the pinned set grows large — zero special-casing
- [ ] **Approval UX:** pin requests surface through Hermes's existing approval flow (interactive CLI prompt; gateway `permissions_list_open`/`permissions_respond` for Telegram/Discord approval). Resolution writes to the **daemon** pin store. Test: approve a pin from Telegram, verify `net mcp pin list` (CLI) and Claude Code (shim) both see it.

**Acceptance:** pinned remote capability appears as a first-class typed Hermes tool; arg accuracy ≈ local-tool baseline; pin approved in Hermes visible in the shim within seconds and vice versa; descriptor update on the remote side propagates to the model-visible schema without restart.

---

## Phase 3 — Delegated agent identity

- [ ] Chain: `user root → machine → hermes-gateway agent identity`, requested from the daemon at plugin init
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
- [ ] Published tools are ordinary mesh capabilities: invocable from CLI, shim, or another Hermes, subject to the caller's daemon consent engine

**Acceptance:** Hermes B publishes one read-only tool; CLI and Hermes A invoke it; a `terminal` publish attempt without the flag is rejected; audit shows full chains both sides.

---

## Phase 5 — A2A as agent capabilities

- [ ] Announce `agent.hermes.message` / `agent.hermes.task` / `agent.hermes.status`; A2A *begins* as capability invocation
- [ ] Task lifecycle day one: `requested, accepted, completed, failed, cancelled` — **cancel maps to nRPC end-to-end cancellation and demonstrably stops remote work**, wired into Hermes's interrupt machinery (`tools/interrupt.py`)
- [ ] Inbound A2A renders as a Hermes conversation/session (it already multiplexes platforms — the mesh is one more channel surface); event chain visible
- [ ] A2A calls subject to the same daemon consent engine — an agent capability is a capability

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

- [ ] One daemon/gateway policy engine mediates tools, A2A, artifacts, streams, publication visibility
- [ ] Hermes consumes decisions; its approval UX renders daemon prompts; it never grows its own parallel policy layer
- [ ] Fixture policies tested: same-root read-only allowed; `shell` denied unless approved; A2A between named machines; artifact roots; confirmation for `desktop_control`

**Acceptance:** one policy config gates a tool call AND an A2A message; policy change alters both without touching plugin code.

---

## Failure semantics (registry stability)

- **No mid-turn tool mutation.** Hermes never changes the model-visible tool list mid-turn (prompt-cache and schema-stability invariants). Pin/schema/revocation changes update daemon state immediately; the registry reflects them at the next tools-assembly boundary.
- **Revocation fails closed anyway.** Invocation handlers check daemon state at call time — a revoked pin returns structured `capability_revoked` even if the model still holds the stale schema this turn.
- **Daemon disappearance:** meta-tools and pinned tools share the same check_fn TTL/grace — transient flap absorbed, sustained outage removes them cleanly at next assembly; in-flight calls return the canonical daemon-unavailable error.
- **Schema drift:** `dynamic_schema_overrides` pulls the live descriptor at assembly time; a call against a just-changed remote schema gets daemon-side validation against the *current* descriptor, and the validation error names the drift.

## Non-goals
Untrusted discovery, attestations, spend limits, billing, paid tasks (later ladder); the OpenClaw plugin (bridge plan, optional); Hermes as a horizontal product; patches to Hermes core outside `plugins/net/`.

## Metrics
Phase 0.5 config-to-first-invoke time; pinned-tool arg accuracy vs local baseline; pin approval cross-client propagation latency; % invocations with full delegation chain; fold query latency; cancel round-trip; migration success rate.

## Open risks

| Risk | Mitigation |
|---|---|
| Daemon engine partially shim-side | Cross-plan gate test before Phase 1 |
| Model confuses `tool_search` (local) with `net_search_capabilities` (mesh) | Explicit disambiguating descriptions; measure misroutes in Phase 1 |
| Meta-tools always-load costs 5 tool defs in every prompt | Trivial vs the double-indirection failure mode; revisit only if measured context pressure demands it |
| net-mesh-sdk Python API gaps (pin subscription, delegation context) | SDK gap = blocker filed against Net repo, not worked around with private hooks (H6) |
| Hermes upstream churn (1.3M LOC, fast-moving) | Everything in `plugins/net/`; only stable surfaces used (registry, plugin API, approval hooks); pin tested Hermes versions in CI |
| Pin approval UX drift between Hermes/CLI/shim | Single daemon store + the cross-client propagation test in Phase 2 |
