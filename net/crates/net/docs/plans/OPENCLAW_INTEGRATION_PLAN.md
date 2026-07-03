# Implementation Plan: OpenClaw Integration (grounded in openclaw/openclaw code)

**Companion to:** `net-mcp-bridge-implementation-plan.md` (prerequisite) and `hermes-native-integration-plan.md` (the deeper native showcase). OpenClaw is the **distribution play**: biggest agent install base, and its architecture has a hole exactly Net-shaped. This plan is deliberately lighter than the Hermes plan — OpenClaw gets tourist + resident + selected native features, not the full Mikoshi showcase.

**Repo ground truth** (verified against main, July 2026):

| Fact | Where | Consequence |
|---|---|---|
| Gateway-centric hub-and-spoke: one Gateway per host, WS control plane on loopback `:18789`; **nodes are peripherals** (iOS/Android/macOS, `role: "node"`, `node.invoke` for `camera.*`/`canvas.*`/`system.*`), explicitly "not gateways" | `docs/network.md`, `docs/nodes/index.md` | **There is no gateway-to-gateway federation.** Cross-machine access = SSH tunnel or Tailscale, documented as such. That's the pitch: *your OpenClaw on the VPS uses tools on your Mac's OpenClaw, no tunnel, no VPN, no exposed port.* Net federates the plane OpenClaw doesn't have. |
| Built-in MCP client: `bundle-mcp` session-scoped runtime on the official `@modelcontextprotocol/sdk`, stdio/http/sse transports, config-driven server catalogs | `src/agents/agent-bundle-mcp-runtime.ts`, `bundle-mcp-config.ts` | **Zero-code path exists**: add `net mcp serve` as a stdio server in bundle-mcp config. Ship it as a docs page the week the shim exists. |
| Rich plugin API: `registerTool` (static or **per-context factory** returning a varying tool list), `registerCommand`, `registerService`, `registerGatewayMethod`, `registerChannel`, `registerHook`, node-invoke policy hooks, exec-approvals runtime, security-audit collector | `src/plugins/types.ts`, `src/plugins/tool-types.ts`, `packages/plugin-sdk` | Everything fits in one extension, no core patches. Tool factories are evaluated at tools-array assembly — **pin promotion is pull-based**: factory queries the daemon pin store and returns the pinned set. No register/deregister bookkeeping at all (cleaner than the Hermes diff pattern; ironically the "stateless catalog" lesson Hermes cites came from OpenClaw). |
| `registerChannel` registers a native messaging channel (peer of Telegram/Discord/WhatsApp) | `src/plugins/types.ts` | **A2A becomes a chat surface.** An agent on another machine appears as a conversation in OpenClaw — messages route through the gateway like any DM, with the existing pairing/allowlist machinery. This is the OpenClaw-specific killer feature; Hermes doesn't have an equivalent this clean. |
| Extensions are npm packages: `openclaw.plugin.json` + `openclaw` stanza in package.json (`extensions`, `install.npmSpec`, `minHostVersion`); ClawHub is the distribution channel | `extensions/*/package.json`, `docs/clawhub` | Ship as `@net-mesh/openclaw` on npm + ClawHub. `@net-mesh/sdk` and `@net-mesh/core` already exist on npm (Node binding over the Rust core) — dependency solved. |
| Device pairing + exec approvals + security-audit collector hooks exist and are mature (QR pairing, token rotation, per-command allowlists in `src/node-host/exec-policy.ts`) | `extensions/device-pair`, `packages/plugin-sdk/src/exec-approvals-runtime.ts` | Pin approval renders through OpenClaw's existing approval UX; decisions write to the **Net daemon** store (H2 holds). Register a security-audit collector so `openclaw security audit` inspects Net exposure. |
| `extensions/file-transfer` exists | `extensions/file-transfer/` | Artifact pulls (Dataforts) can hand off to the existing file-transfer surface instead of inventing delivery UX. |
| `extensions/migrate-hermes` exists | — | They actively onboard Hermes users. The ecosystems are adjacent; one Net mesh serving both agents is a real user story (see Phase 5). |

**Naming note:** `packages/net-policy` already exists in-tree (outbound network policy, unrelated). The extension must be unambiguous: package `@net-mesh/openclaw`, plugin id `net-mesh`, CLI namespace `openclaw net-mesh …` — never bare `net` inside OpenClaw.

---

## Doctrine (same H-rules, OpenClaw dialect)

1. **Client, not node.** The extension talks to the local running Net daemon via `@net-mesh/sdk`. No embedded mesh node inside the gateway process.
2. **One consent engine, daemon-side.** Pin store, consent, validation, audit live in the Net daemon. OpenClaw's approval UX, Hermes's approval UX, the CLI, and the shim are all views over the same store. Approved anywhere = approved everywhere — including across *agents*: a pin approved in Hermes is live in OpenClaw on the same machine.
3. **Delegation before publication.** No side-effectful OpenClaw tools published to the mesh before per-agent identity.
4. **Explicit, tagged publication.** Never auto-publish; node capabilities (camera/canvas/screen) are a **later, separately-gated tier** — they're physical-world sensors, spicier than any API tool.
5. **Streams feed state; models query folds.**
6. **Public APIs only, both sides:** public plugin SDK, public `@net-mesh/sdk`. Gaps become issues, not private hooks.
7. **No key material, ever.** The extension, the model, and anything arriving over the A2A channel never reads, receives, or relays private keys. Keys live in the Rust core or external signers; agents request typed operations only.
8. **Loopback stays loopback.** The Net extension must never require binding the gateway WS beyond `127.0.0.1`. Cross-machine traffic is the daemon's job — that's the whole point, and it's the security pitch: federation without widening the gateway's attack surface.

---

## Cross-plan dependency

Same gate as the Hermes plan: daemon-side consent/pin/validation engine exposed via SDK, plus pin-change subscription. Additional gate test here: **pin approved via Hermes's approval flow is visible to the OpenClaw factory on next tools-assembly** (cross-agent consistency, same daemon).

---

## Phase 0.5 — Zero-code path (ship the week the shim exists)

No extension. Bundle-mcp config only:

```jsonc
// OpenClaw MCP server catalog
{ "servers": { "net": { "type": "stdio", "command": "net", "args": ["mcp", "serve"] } } }
```

- [ ] Verify bundle-mcp against the shim (2026-07-28 stateless shape) — tools listed, meta-tools callable, `requires_approval` errors render sanely in session
- [ ] Verify catalog refresh picks up pin promotions (listChanged handling in the session-scoped runtime; if refresh is session-start-only, document "new pins appear next session" and file the gap)
- [ ] Approval-required / unknown capabilities render as **recoverable, instructive errors** in session, not generic tool failures
- [ ] Docs page: "Use Net with OpenClaw today" — config + two-machine quickstart + the explicit topology diagram (`OpenClaw(VPS) → local daemon → mesh → daemon(Mac) → wrapped tools` — OpenClaw itself never connects remotely, and the gateway never leaves loopback)

**Acceptance:** stock OpenClaw, config-only, invokes a wrapped capability on another machine.

---

## Phase 1 — `@net-mesh/openclaw` extension: native client

```
extensions-external/net-mesh/          # ships from the Net repo, not the OpenClaw tree
  package.json                         # openclaw stanza: install.npmSpec @net-mesh/openclaw, minHostVersion
  openclaw.plugin.json
  src/
    index.ts                           # plugin entry: register everything below
    daemon.ts                          # @net-mesh/sdk client + liveness (registerService lifecycle)
    meta-tools.ts                      # the five tools (static registration)
    pins.ts                            # tool FACTORY: pinned caps -> AnyAgentTool[] per assembly (Phase 2)
    channel.ts                         # Net A2A channel (Phase 5)
    cli.ts                             # registerCommand: openclaw net-mesh status|search|pin ...
    audit.ts                           # security-audit collector: what's published, what's pinned, exposure summary
```

- [ ] `registerService` owns daemon client lifecycle (connect on gateway start, clean release on `oneShotCliRun`)
- [ ] Five meta-tools registered statically and **always visible** — never buried behind any tool discovery/deferral mechanism. The meta-tools are the doorway; don't hide the doorway. Pinned tools (factory-returned) follow normal tool-scaling behavior. Descriptions explicitly say "searches the Net mesh across your machines, not this gateway's local tools"
- [ ] `net_invoke_capability`: daemon-side validation errors returned verbatim (model self-repair); `requires_approval` returns the approval instruction
- [ ] `net_request_pin` creates a pending request daemon-side and returns the structured pending response (`{status: "pending_approval", request_id, approval_channels, message}`) — the model relays it, **never approves it**. Approval flows through operator UI / pairing-style prompt → daemon store. Non-negotiable
- [ ] Liveness: daemon down → meta-tools return the canonical failure string (`No Net daemon is running. Start one with: net up`), factory returns `null` so pinned tools cleanly vanish rather than erroring mid-turn
- [ ] `registerCommand`: `openclaw net-mesh status/search/describe/pin list` for humans
- [ ] Security-audit collector registered from day one: published capabilities, active pins, widened visibilities

**Acceptance:** compressed milestone — machine B `net wrap`s the fixture server; OpenClaw on machine A (extension installed, bundle-mcp path not involved) searches, describes, hits approval on the fake-credentialed tool, user approves, invokes. GitHub version is the recorded demo, not the gate.

---

## Phase 2 — Pin promotion via tool factory

- [ ] `pins.ts` registers ONE `OpenClawPluginToolFactory`: on each tools-assembly it reads the daemon pin store (short-TTL cached + invalidated by the pin-change subscription) and returns the approved set as first-class `AnyAgentTool`s — real names, real schemas, risk tags in descriptions
- [ ] **Pinned tool names are allocated by the daemon** (same rule as the Hermes plan): stable across sessions, retired on unpin, collisions resolved daemon-side. The extension never invents names — one namespace across OpenClaw, Hermes, shim, CLI
- [ ] Live types: factory pulls current descriptors, so a remote capability's schema change is model-visible next assembly, no restart — same "always up-to-date types" demo as Hermes, implemented in ~10 lines because the factory model is pull-based
- [ ] **Approval UX:** pin requests surface through OpenClaw's approval machinery (exec-approvals runtime pattern / device-pairing-style prompt in the operator UI); resolution writes to the daemon store
- [ ] Cross-agent test: approve in OpenClaw → visible in Hermes plugin + `net mcp pin list` within seconds; and the reverse
- [ ] **Hero demo (record it):** pin a GitHub capability in Hermes on machine A → OpenClaw on the same machine gets it as a first-class tool immediately → invoke from OpenClaw → unpin from CLI → gone from both. One daemon, one consent substrate, every agent.

**Acceptance:** pinned remote capability appears as a native typed OpenClaw tool; arg accuracy ≈ local baseline; cross-agent pin propagation verified both directions and recorded as the demo.

---

## Phase 3 — Delegated identity

- [ ] Chain: `user root → machine → openclaw-gateway agent identity`, requested at service init; all invocations carry it
- [ ] Remote wrapper audit distinguishes "OpenClaw gateway on VPS" from "Hermes on laptop" — per-agent, not per-machine
- [ ] Revoking the OpenClaw delegation kills its mesh access without touching Hermes or the CLI on the same box
- [ ] (Later, with Phase 4-node-tier) sub-delegations per paired node if node capabilities ever publish

**Acceptance:** audit logs name the agent; revocation is agent-scoped. **Gate for Phase 4.**

---

## Phase 4 — Publish selected gateway tools to the mesh

- [ ] Explicit publication via CLI/config: `openclaw net-mesh publish <tool>` — never automatic
- [ ] Every descriptor carries a mechanical side-effect classification: `side_effect: none | local_read | external_read | local_write | external_write | physical_sensor | desktop_control` — publish gates key off the taxonomy, not vibes
- [ ] Publication tiers (enforced):

| Tier | Examples | Gate |
|---|---|---|
| harmless | time/status, sanitized system info | explicit publish |
| read-only local | configured read roots, static artifacts | explicit scope |
| external read | **web search / API reads — these are `external_api`, often credentialed: they spend quota and leak queries. Publishable, owner-only, explicit — never "harmless"** | credential/external warning |
| sensitive read | session/conversation state | strong warning, owner-only |
| side-effectful | browser automation, exec/shell, write paths | dangerous flag + approval |
| physical sensors / desktop | `camera.*`, `screen.*`, `device.*`, computer use | **deferred** — needs its own consent design (a mesh-published camera is a remote surveillance primitive); node-invoke policy hooks are the enforcement point when it comes |

- [ ] Canvas render: classify deliberately before publishing — only Tier 1 if provably deterministic, no session-state access, no external side effects; otherwise it lands where the taxonomy puts it
- [ ] Published tools carry risk tags derived from OpenClaw's own tool classification; audit collector reports them

**Acceptance:** one read-only tool published from OpenClaw, invoked from Hermes and CLI on another machine; a shell publish without the flag is rejected; `openclaw security audit` shows the exposure.

---

## Phase 5 — A2A as a channel (the OpenClaw-native move)

This is OpenClaw's native showcase: `registerChannel` makes remote agents feel like **conversations**. Division of labor, not rivalry:

| Agent | Native showcase |
|---|---|
| Hermes | deep substrate: delegation, subagents, folds, artifacts, Mikoshi |
| OpenClaw | distribution + channel UX: remote agents as conversations |

- [ ] `channel.ts` registers a `net-mesh` channel: inbound `agent.*.message` invocations from the mesh arrive as messages in a conversation keyed by the remote agent's identity; outbound replies invoke the remote agent's capability
- [ ] The user experience: your Hermes on the laptop appears in OpenClaw like a Telegram contact. You (or your OpenClaw agent) message it; it does work; replies land in the thread. Cross-agent, cross-machine, E2E encrypted, no platform in the middle
- [ ] **Pairing before model context, strictly:** unpaired inbound messages are held in a pending inbox / pairing request — they never enter the agent loop. Feeding untrusted mesh messages to the model before pairing is prompt-injection by DM. Operator sees a notification; the model sees nothing until approval
- [ ] Message vs task vs status are distinct in UX and policy: `agent.openclaw.message` = conversational, reply-able; `agent.openclaw.task` = bounded work item with lifecycle — **a message never silently becomes a task**; task requests from non-allowlisted identities require approval even when paired; `agent.openclaw.status` = low-risk presence probe
- [ ] OpenClaw announces `agent.openclaw.message/task/status` on the mesh (owner-only default); task lifecycle `requested/accepted/completed/failed/cancelled` with **day-one cancellation** wired to session abort — cancel must stop remote work, not just flip local UI. `timed_out` documented as the v1 lifecycle extension
- [ ] Fold triggers (Phase 6) deliver through this channel — "the desktop's error fold fired" arrives as a message, riding delivery machinery that already reaches the user's phone

**Acceptance:** Hermes (laptop) ⇄ OpenClaw (desktop) exchange messages and a bounded task through the mesh, rendered as a normal conversation in OpenClaw; cancel mid-task stops remote work; unknown-agent message requires pairing approval first.

---

## Phase 6 — Streams + folds (scoped)

- [ ] Consume native streaming capabilities into folds (daemon/SDK-side fold, extension queries it) — raw chunks never enter session context
- [ ] Fold triggers → `net-mesh` channel messages (Phase 5 delivery path)
- [ ] Skip the full streaming showcase here — that's Hermes's job; OpenClaw ships the *consumption* pattern and the trigger UX

**Acceptance:** remote log-tail folds; trigger condition fires; user gets the alert as a channel message on whatever platform their OpenClaw delivers to.

---

## Deliberately out of scope for OpenClaw

Mikoshi migration (OpenClaw session state is gateway-owned; not a natural fit — Hermes carries that demo); node-tier capability publication (needs its own consent design); artifacts beyond handing Dataforts pulls to `file-transfer` (nice-to-have, tracked not planned); payments/attestations (later ladder).

## Distribution workstream

- [ ] `@net-mesh/openclaw` on npm + ClawHub listing (quickstart, security posture front and center: loopback-only gateway, credential locality, no tunnels)
- [ ] **Fork ports same release cycle** (NanoClaw, IronClaw): the plugin API surface used must be checked against forks; keep to the stable core of the API. The OpenAI-owned channel is never the only channel.
- [ ] The "one mesh, two agents" demo as the flagship recording: Hermes and OpenClaw on different machines sharing tools and messaging each other over the same daemon pair

## Metrics
Config-to-first-invoke (Phase 0.5); pinned-tool accuracy vs baseline; cross-agent pin propagation latency; channel A2A round-trip; fork compatibility matrix; ClawHub installs → active daemons funnel.

## Open risks

| Risk | Mitigation |
|---|---|
| Plugin API churn post-OpenAI-acquisition (4.8M LOC, fast-moving) | Everything in one external extension; only stable plugin-sdk surfaces; `minHostVersion` pinned; fork ports as insurance |
| bundle-mcp session-scoped catalogs don't refresh on pin (Phase 0.5) | Documented "next session" behavior + upstream issue; native extension (Phase 2) is pull-based and immune |
| Tool factory called per-assembly → daemon query on hot path | Short-TTL cache + subscription invalidation; measure assembly latency budget |
| Name confusion with in-tree `net-policy` and bare "net" | `net-mesh` plugin id / package / CLI namespace everywhere |
| Channel A2A lets unknown mesh agents reach the model | Pairing-approval gate identical to unknown-DM posture; owner-only announcement default |
| Camera/node publication demanded by users before consent design exists | Explicitly deferred tier with a stated reason; audit collector makes any accidental exposure visible |
