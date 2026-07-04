# Documentation Strategy — Agentic-Mesh Legibility + Developer Wedge

**Status:** IN PROGRESS → substantially DONE (2026-07-04). Phase 0 (audit),
Phase 1 (worldview + positioning), Phase 2 (developer-wedge guides), Phase 3
(five-language SDK spine — see `DOCS_SDK_SPINE_PLAN.md`), Phase 4 (agent briefs),
and Phase 5 (reference: `reference/mcp-bridge.md`) have all landed as separate
commits, each with `cd web && npm run build` green. Remaining polish: Phase 3b
per-binding deep-verification against `sdk-ts`/`sdk-py`/`go`/`net.h` beyond the
grounded pass already done, and any further reference reframing. Authored on
branch `docs-5`
(off the near-production MCP branch — the MCP bridge crate `net-mesh-mcp`,
`net wrap` / `net mcp serve` / `net mcp pin`, tests, and CI are present; see
`MCP_BRIDGE_PLAN.md`). This plan governs the public docs under
`web/src/content/docs/` and the two cross-artifact reconciliations they touch
(root `README.md`, the Next.js homepage hero).

**Goal:** Rebuild the docs so Net is *legible as a worldview* and *usable as an
implementation surface* — leading with the shipped wedge (agent capability
federation), keeping the latency-first mesh substrate as the identity
underneath, and adding an agent-operable implementation layer. Not "here is our
protocol"; instead: here is the world agents are entering, here is when Net is
right or wrong, here is the fastest way in (MCP + native SDK), here is how to
build in five languages, here are agent-readable briefs.

**Scope of this plan (decided):** ONE master plan. Phase 1 (Legibility) is
specified to executable depth; Phases 2–5 are sketched and spin off their own
sub-plans (`DOCS_SDK_SPINE_PLAN.md`, `DOCS_AGENT_BRIEFS_PLAN.md`) when reached.

---

## Positioning decision (frozen)

**Net leads with the agentic use case, then broadens to the other domains where
the protocol is strong.** The substrate identity is *not* discarded.

- **Lead line (funnel):** *Net is a discovery mesh for agentic capability —
  agents find live capabilities across a trusted mesh, invoke them safely,
  observe what happened, and recover when work fails.*
- **Substrate line (identity, one layer down):** *Underneath, Net is a
  latency-first encrypted mesh: capability discovery, typed RPC, durable logs,
  folded state, and artifacts on one substrate — the same substrate that also
  runs vehicular, industrial, robotics, and edge workloads.*
- **Why layered, not a rename:** the latency/real-time-mesh story is Net's
  defensible moat; "agentic capability discovery" alone is a crowded, fashionable
  category. Lead with the immediate use case for legibility; keep the moat as the
  reason-to-believe. This is the same "wedge now, substrate underneath" framing
  already frozen in `MCP_BRIDGE_PLAN.md` ("Net's wedge is agent capability
  federation … payments, billing, identity … grow from that substrate later").

**Reconciliation obligation:** the root `README.md` currently opens as *"Network
Event Transport — a latency-first encrypted mesh protocol"* and the homepage hero
must not contradict the docs' agentic lead. Phase 1 includes an explicit
reconcile task so all three artifacts (README, homepage, `docs/`) tell one
layered story. **Do not let the docs quietly re-position the company while the
README says something else.**

---

## Doctrine (non-negotiable constraints)

1. **Worldview before machinery.** The first pages sell the belief system. A
   reader understands *why* Net exists before they learn how nRPC works. Do not
   open a worldview page with RedEX/CortEX/nRPC.
2. **Every concept page answers one question:** *what problem does this solve for
   an agent trying to get work done?* If a page can't answer it, it's reference,
   not concept.
3. **Honesty is pinned to shipped behavior (see Phase 0 — audit complete,
   `docs/misc/DOCS_CLAIMS_AUDIT.md`).** No worldview claim ships until the code
   cashes it. The audit resolved the honesty items as follows:
   - **Multi-hop discovery is SHIPPED** (`MULTIHOP_CAPABILITY_PLAN.md` — hop-count
     16, TTL dedup, tests). The earlier "deferred" caveat came from a stale skill
     doc; it is reversed. Worldview copy **may** claim discovery across the mesh,
     and should state the **hop-count-16 bound** rather than imply infinite reach.
   - **MCP-bridged tools are `compat_tier: "mcp_bridge"` — request/response
     only, no streams / migration / artifacts** (confirmed). The rich surface
     (live state, failures, retries, artifacts, streams) is *native* capabilities.
     The bridge is the funnel, not the destination; say so on every page.
   - **Real command names (confirmed):** onboarding has **no `net up`** (use SDK
     `MeshBuilder` / bootstrap peer); discovery is **`net cap query <tags>`** and
     **`net cap nodes`** (there is no `net cap search`); capability invocation has
     **no `cap` verb** — use SDK nRPC `call_typed` or the `net mcp serve`
     `net_invoke_capability` meta-tool. Wrap/serve: `net wrap … -- <cmd>`,
     `net mcp serve`, `net mcp pin approve|reject|list`.
4. **Additive IA, never destructive.** Add `worldview/`, `sdk/`, `agent-briefs/`;
   reframe landings and nav. **Keep `concepts/` and `reference/` intact** — they
   are load-bearing (three reference pages were just ported into the
   `net-claude-skill`). No page deletions; renames only via `docs.order.ts`
   labels + slug redirects, never by moving files out from under inbound links.
5. **Agent-operable.** `agent-briefs/` pages are executable by a coding agent:
   Goal / Files / Commands / Expected output / Test plan / Acceptance / Pitfalls.
   The docs are for Reader 3 (an agent), not only Reader 1–2 (humans).
6. **One conceptual spine, five bindings — asymmetry stated, not hidden.** The
   SDK section follows a single 10-step skeleton across Rust / TS / Python / Go /
   C. Where Go and C diverge (poll-based, no named channels / typed firehose),
   the page says so rather than faking parity.

---

## Reader journeys (the docs serve three readers)

| Reader | Needs | Served by |
|---|---|---|
| **1. Founder / investor / partner** | What is this? Why now? Why not MCP/REST? Where does it fit? | `worldview/` + the "submitted is not completed" demo |
| **2. Developer** | How do I run it? Wrap an MCP server? Announce/discover/invoke? Debug it? | `start/` + `guides/` + `sdk/` |
| **3. Agent / Claude Code** | Exact tasks, files to edit, commands, expected output, verification | `agent-briefs/` + the published skill (`github.com/ai-2070/net-claude-skill`) |

Reader 3 is the unusual, category-defining one. The `net-claude-skill` repo is
the agent's on-ramp; `agent-briefs/` are per-feature task docs that complement it
(the skill is standing reference; a brief is a one-shot buildable task).

---

## Information architecture (additive)

Current sections (keep all): `start`, `concepts`, `guides`, `reference`,
`tutorials`, `releases`. **New** sections: `worldview`, `sdk`, `agent-briefs`.

Proposed sidebar order in `web/src/docs.order.ts` → `sections`:

```
worldview, start, guides, concepts, sdk, agent-briefs, reference, tutorials, releases
```

Rationale: legibility first (`worldview`), then the fast on-ramp (`start`),
task recipes (`guides`), the mental model (`concepts`), language bindings (`sdk`),
agent tasks (`agent-briefs`), lookup (`reference`), then `tutorials` / `releases`.

### New/changed pages (source asset each draws on)

**`worldview/`** (Phase 1)
- `README.md` — section landing: the belief system in ~200 words + links.
- `agentic-mesh.md` — "Net is a discovery mesh for agentic capability." The
  worldview spine. Draws on `MCP_BRIDGE_PLAN.md` wedge statement + `AGENT_TOOLS.md`.
- `submitted-is-not-completed.md` — the flagship explainer. **Reuses the frozen
  `event-semantics.md` doctrine + payment ladder** from the skill (`200 OK` is
  not work done; facts, not acknowledgements). Static side-by-side markdown now;
  interactive `.mdx` version deferred (MDX is supported by the renderer).
- `right-and-wrong-use-cases.md` — explicit "Use Net when …" / **"Do NOT use Net
  when …"**. Draws on the current `start/what-is-net.md` § "When to use" + Kyra's lists.
- `mcp-vs-net.md` — "MCP made tools callable; Net makes capabilities
  discoverable." Real commands only (`net wrap`, `net mcp serve`). States the
  `mcp_bridge` compat tier. Not "MCP is bad," not "Net replaces MCP."
- `rest-vs-net.md` — REST/webhooks as the dirty edge for legacy/HTTP-only
  systems; "do not model Net internally as REST." (Gate on Phase 0 confirming a
  REST/webhook edge actually exists to document; otherwise ship as a short
  positioning note, not an integration guide.)

**`start/`** (Phase 1 touch, Phase 2 expand)
- `what-is-net.md` — reframe opening to the layered positioning (agentic lead +
  substrate line); keep the existing mechanism content below the fold.
- `quickstart.md` — Phase 2: add the native `net up` → `net cap search` →
  `net cap invoke` path as the lead on-ramp.

**`guides/`** (Phase 2)
- `wrap-mcp-server.md` — `net wrap <name> -- <cmd>`; owner-only default; consent.
- `expose-net-as-mcp.md` — `net mcp serve`; search/describe/invoke meta-tools;
  `net mcp pin approve` consent flow.
- `discover-and-invoke.md` — native `net cap search` / `net cap invoke` + SDK.
- `recover-failed-workflow.md` — nRPC retry/hedge/circuit-breaker + task lifecycle.
- (existing guides stay.)

**`sdk/<lang>/`** for `rust`, `typescript`, `python`, `go`, `c` (Phase 3) — one
skeleton each: `quickstart`, `announce`, `discover`, `invoke`, `watch`,
`artifacts`, `errors`. Rides the **existing per-language gating** (`languages`
config in `docs.order.ts`, taxonomy in `web/src/lib/docs-language.ts`).

**`agent-briefs/`** (Phase 4) — e.g. `wrap-an-mcp-server`,
`implement-describe-service`, `capability-search`, `order-recovery-demo`,
`sdk-example`. Executable-by-agent format.

### Nav / homepage wiring (mechanics, not Starlight)

The site is **Next.js**, not Astro/Starlight. Docs are a file tree under
`web/src/content/docs/` auto-discovered by `web/src/lib/docs.ts`; ordering,
labels, hide, and language gating live in `web/src/docs.order.ts`. A folder's
`README.md` is its landing page. Adding a section = create the folder + a
`README.md`, then add it to `sections`, `folders`, and `labels` in
`docs.order.ts`. The homepage hero is a separate Next.js page (under
`web/src/app/`), reconciled in Phase 1.

---

## Phase plan

### Phase 0 — Pre-flight claims audit (blocking gate) — ✅ DONE (2026-07-04)

Output: `docs/misc/DOCS_CLAIMS_AUDIT.md`. Result: all seven lifecycle claims
(discover → describe → invoke → observe → recover → artifacts → policy) are
shipped. Only copy corrections needed — command names (no `net up`; `net cap
query` not `search`; invoke via SDK/meta-tool) and one caveat that flipped in our
favor (multi-hop is shipped). No worldview claim dropped. The verification notes
below record what was checked.

Verify (against code, not memory):
- **discover:** `net cap search` / `find_nodes` / capability index — and the
  **multi-hop caveat** (constraint #3). Record the exact discovery scope.
- **describe:** `tool.metadata.fetch` / describe meta-tool / `ToolDescriptor`
  input+output schema (`adapters/mcp/src/serve/meta_tools.rs`, `wrap/descriptor.rs`).
- **invoke:** `net cap invoke` / nRPC `call_typed`; MCP `wrap/invoke.rs`
  translation incl. at-most-once-on-timeout (commit F1) + overridable deadline (F5).
- **observe:** bus subscribe + RedEX replay.
- **recover:** nRPC `RetryPolicy` / `HedgePolicy` / `CircuitBreaker`; task lifecycle.
- **artifacts:** Dataforts `fetch_blob` / `store_dir` / `fetch_dir`.
- **policy / local authority:** MCP owner-only default, consent/pin, credential
  locality (`MCP_BRIDGE_PLAN.md` doctrine #3–4; `wrap/credentials.rs`,
  `serve/consent.rs`).
- **compat tier:** confirm `mcp_bridge` = request/response only.

**Acceptance:** every worldview claim is either backed by a named primitive or
downgraded/removed. No page in later phases asserts an unbacked capability.

**Exit gate:** if a headline claim (e.g. "discover across the mesh") is materially
narrower than the copy implies, fix the copy (or file the build task) before
Phase 1 ships that page.

### Phase 1 — Legibility (executable; ~2–3 days)

Deliverables: the six `worldview/` pages above, the `start/what-is-net.md`
reframe, the README + homepage-hero reconciliation, and the `docs.order.ts`
wiring (new `worldview` section first, `folders.worldview` order, `labels`).

**Acceptance criteria (all must hold):**
- `worldview/` appears first in the sidebar; every page opens with the reader's
  problem, not a Net primitive; `right-and-wrong-use-cases.md` contains an
  explicit "Do NOT use Net when …" list.
- `submitted-is-not-completed.md` reuses the `event-semantics.md` doctrine and
  passes a fact-check: no claim exceeds shipped behavior; the payment ladder is
  the worked example.
- `mcp-vs-net.md` references only real commands (`net wrap`, `net mcp serve`,
  `net mcp pin`) and states the `mcp_bridge` request/response tier.
- README, homepage hero, and `docs/` tell one layered story (agentic lead +
  latency substrate); no artifact contradicts another.
- **No existing `concepts/` or `reference/` page removed or moved.**
- `cd web && npm run build` passes; all internal doc links resolve; new pages
  render in the sidebar with correct labels.

### Phase 2 — Developer wedge (~3–4 days; depends on Phase 0)

The four `guides/` pages + the `start/quickstart.md` native path. MCP is now a
**first-class documentable path** (the bridge is built on this branch), alongside
the native SDK on-ramp. Both directions documented: `net wrap` (supply) and
`net mcp serve` (demand). Spin off details as needed.

**Acceptance:** a developer following `wrap-mcp-server.md` end-to-end wraps a
real stdio MCP server (`npx -y @modelcontextprotocol/server-github`) and invokes
it from another node; every command in the guide is copy-runnable; owner-only +
consent behavior is shown, not hand-waved.

### Phase 3 — SDK skeletons (sub-plan `DOCS_SDK_SPINE_PLAN.md`)

Five languages, one 10-step spine, per-language gating via `docs.order.ts`
`languages`. Go/C asymmetry stated. Best executed as a generator/agent-brief
rather than 35 hand-written pages.

### Phase 4 — Agent implementation briefs (sub-plan `DOCS_AGENT_BRIEFS_PLAN.md`)

Executable-by-agent task docs; cross-link the published `net-claude-skill`.

### Phase 5 — Reference completion / light reframe

Keep every existing `reference/` page; add any gaps surfaced by Phase 0
(e.g. an MCP-bridge reference, capability-announcement schema). Reframe landings
toward the agentic vocabulary **without** dropping content.

---

## Kyra's proposal → this plan (reconciliation)

| Kyra idea | Decision | Why |
|---|---|---|
| Sell worldview first | **Adopt** | Real gap; docs currently open on a mechanism. |
| Right/wrong use cases | **Adopt** | Trust-building; currently soft + buried. |
| "Submitted is not completed" flagship | **Adopt, reuse skill doctrine** | Asset already written + validated (`event-semantics.md`). |
| MCP bridge as first dev path | **Adopt — now first-class** | Bridge is built on this branch (not vaporware). |
| REST as dirty edge | **Adopt, gated** | Ship only if a REST/webhook edge exists to document (Phase 0). |
| 5-language SDK on one spine | **Adopt** | Rides existing language-gating; Go/C asymmetry stated. |
| Agent briefs | **Adopt** | Category-defining Reader 3. |
| Rename `concepts/` into agentic vocab | **Modify → additive reframe** | Don't churn working, linked pages; reframe landings only. |
| Proposed `reference/` (drops adapter-trait/filter-dsl/replication-config/subprotocol-ids) | **Reject the drops** | Load-bearing; recently ported into the skill. |
| `payments-later.md`, `agent-to-agent.md` | **Defer as stubs** | Speculative; must not imply shipped features. |
| Astro/Starlight sidebar assumption | **Correct → Next.js** | Nav is `docs.order.ts`, not Starlight. |

---

## Risks & non-goals

- **Over-narrowing the brand.** Mitigated by the frozen layered positioning +
  the README/homepage reconciliation task. If the substrate story disappears from
  the top of the funnel, that's a regression, not a simplification.
- **Writing checks the code can't cash.** Mitigated by the Phase 0 audit gate and
  the two pinned caveats (multi-hop discovery deferred; `mcp_bridge` tier).
- **Scope blowout (35 SDK pages + briefs).** Mitigated by phasing + sub-plans +
  generating SDK pages rather than hand-writing them.
- **Non-goals:** building the MCP bridge (done on this branch; see
  `MCP_BRIDGE_PLAN.md`), payments/billing docs, an interactive demo harness
  (deferred), and any change to `concepts/` / `reference/` content beyond
  additive reframing.

---

## Immediate next step

On approval, execute **Phase 0** (the claims audit → `DOCS_CLAIMS_AUDIT.md`),
then **Phase 1** (the six `worldview/` pages + reframe + reconciliation +
`docs.order.ts` wiring), and stop for review before Phase 2.
