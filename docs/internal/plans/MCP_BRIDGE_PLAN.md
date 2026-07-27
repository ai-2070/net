# Implementation Plan: Net MCP Bridge + Federation Wedge (v2)

**Goal:** Ship the agent capability federation wedge — `net up` / `net wrap` / `net mcp serve` — so any existing MCP host can use tools across a user's trusted mesh in under 5 minutes, with credential locality and paranoid defaults, without the Net core ever learning MCP exists.

**Wedge statement (frozen):** Net's wedge is agent capability federation. An agent can find a capability somewhere else on the mesh and use it. Payments, billing, identity services, and the enterprise control plane grow from that substrate later. Not in scope for this plan.

**Status (2026-07-04):** the Phase 1–4 core landed on the `mcp-3` branch (`net-mesh-mcp` crate). A code review and 13 fixes are recorded in [`CODE_REVIEW_2026_07_04_MCP_BRIDGE.md`](../misc/CODE_REVIEW_2026_07_04_MCP_BRIDGE.md); the security/design refinements from that pass — most notably that **cross-provider collapse/failover is opt-in and off by default** (Phase 4) and that **invoke is at-most-once for credentialed tools** — are folded into the phases below.

---

## Doctrine (non-negotiable constraints)

1. **Core purity.** `net-core` / protocol crates have zero MCP awareness. All MCP code lives in the edge adapter package (`@net-mesh/mcp`, `net-mesh-mcp` crate). If MCP churns, the adapter churns; the mesh does not. Same pattern as the Redis/JetStream adapters.
   **The adapter rides on `net-mesh-sdk` only** — the same public Rust surface the Python/TS bindings wrap — never on core crates directly. Shim and wrap are SDK clients like Hermes and OpenClaw; if the adapter needs a primitive the SDK lacks, the SDK grows a public primitive (same rule as H6). This makes the bridge the first SDK conformance consumer, and forces the **provider-side SDK surface** (announce / serve / withdraw capabilities) into existence at Phase 1 instead of being invented under deadline when Hermes/OpenClaw publish tools. Packaging is unchanged: the CLI links the SDK crate and still ships as one binary.
2. **Compat tier is explicit.** Bridged MCP tools are request/response only — no streams, no migration, no artifacts. Descriptors carry `compat_tier: "mcp_bridge"`. Native capabilities remain richer. The bridge is the funnel, not the destination.
3. **Owner-only by default.** Wrapped credentialed tools are visible and invocable only by the same root identity unless explicitly widened. Display/search never implies invocation permission.
4. **No ambient full-mesh authority.** No MCP host receives blanket authority over the mesh by connecting to `net mcp serve`. Authority is always mediated by shim consent, pinned status, wrapper owner/policy, and local daemon identity.
5. **Public SDK API only.** Everything Hermes or the OpenClaw plugin uses must be public SDK surface. No private hooks.
6. **Target MCP spec 2026-07-28 (stateless).** Do not build against 2025-11-25 session semantics. Stdio-first dodges most of the churn anyway.

### Authority model (the confused-deputy defense, explicit)

```
MCP host        → can only talk to the local shim
Shim            → can only talk to the local net daemon
Daemon          → acts under the local user's identity
Remote wrapper  → enforces caller identity/scope (remote authorization)
Credentialed / external / unknown-status capabilities
                → require local shim consent or an approved pin
Display/search  → never grants invocation
Pin approval    → local client consent for this shim/user profile,
                  NOT remote authorization; wrapper policy always wins
```

---

## Package layout

```
net/crates/net/adapters/mcp/        # Rust adapter crate: net-mesh-mcp
  src/
    wrap/                           # sidecar: MCP server -> mesh capability
      stdio.rs                      # spawn + JSON-RPC over stdio
      descriptor.rs                 # tools/list -> ToolDescriptor lowering
      invoke.rs                     # nRPC -> MCP tools/call translation
      policy.rs                     # visibility/invocation scope enforcement
      credentials.rs                # credential-status classification
    serve/                          # shim: mesh -> MCP server
      shim.rs                       # stdio MCP server implementation (2026-07-28)
      meta_tools.rs                 # search/describe/invoke surface
      pinned.rs                     # promotion to first-class typed tools
      validation.rs                 # pre-flight arg validation vs descriptor schema
      consent.rs                    # shim-side allowlist + pin approval flow
    daemon_client.rs                # thin client to running net daemon (no embedded node)
    spec/                           # version adapters, isolated for future churn
cli/
  commands/wrap.rs                  # net wrap <name> [flags] -- <command...>
  commands/mcp_serve.rs             # net mcp serve [--expose pinned|--direct --limit N]
  commands/mcp_pin.rs               # net mcp pin approve/reject/list
bindings/{node,python,go}/mcp/      # thin wrappers over the Rust adapter (later phases)
```

---

## Phase 0a — Native smoke (no MCP anywhere)

Isolate the failure boundary: prove Net onboarding + cross-machine invocation before any MCP code exists.

- [ ] `net up` onboarding path hardened: two machines, same root identity, joined in <90 seconds each (measured and timed)
- [ ] Machine B announces one native dummy capability (`demo.echo`)
- [ ] Machine A: `net cap search echo` and `net cap invoke <cap_id> --args '{...}'` work over the mesh
- [ ] Owner-only visibility verified: a third node with a different root identity cannot see or invoke the capability

**Acceptance:** scripted demo — two fresh machines to first cross-machine invocation, wall-clock under 5 minutes, recorded.

**Exit gate:** if `net up` onboarding can't hit the time budget, fix that before anything else. The wedge dies here if this is slow. A Phase 0a failure is a Net problem; a Phase 0b failure is a wrapping problem — never confuse the two.

## Phase 0b — Hardcoded MCP smoke

- [ ] Machine B wraps one stdio MCP tool via a hardcoded prototype wrapper (no CLI polish)
- [ ] Machine A searches/invokes it through the same CLI path as 0a

**Acceptance:** same demo as 0a but through the MCP translation path; any regression relative to 0a is attributable to wrapping alone.

---

## Phase 1 — `net wrap` (supply side, stdio only)

- [ ] `net wrap github -- npx -y @modelcontextprotocol/server-github` spawns the stdio server, speaks MCP JSON-RPC
- [ ] Reads `tools/list`, lowers each tool to a `ToolDescriptor` with:
  - `compat_tier: "mcp_bridge"`
  - `visibility: "owner_only"`, `invocation_scope: "same_root_identity"` (announcement metadata — honest even before full permission system)
  - `substitutability` field: default `provider_local` (NOT substitutable); `provider_equivalent` only when explicitly flagged
  - a **channel-safe `tool_id`**: the MCP name verbatim when it is already a valid channel id, else sanitized (lowercased, out-of-charset chars → `_`) and hash-suffixed with the original name; the original name is kept for invocation. So an uppercase / camelCase / spaced tool (`createIssue`) is bridged under a stable safe id rather than dropped — only an empty name is skipped (review F10)
- [ ] **Credential-status classification (conservative):**
  - env additions passed or inherited secrets configured → `credentialed`
  - common secret patterns detected → `credentialed`
  - known external API server → `external_api`
  - otherwise → `credential_status: "unknown"` — treated exactly like `credentialed` for consent purposes
  - explicit flags: `--credentialed` (upward override always allowed), `--no-credentials` (downward override requires `--force`)
  - **Detection failure must never become permission bypass** — unknown is spicy until proven boring
- [ ] Incoming nRPC call → validate caller identity/scope → translate to MCP `tools/call` → return result
- [ ] Credentials never transit: env vars/tokens stay in the wrapper process on the owning machine
- [ ] Lifecycle: wrapper crash → capability announcement withdrawn within heartbeat; restart re-announces
- [ ] Explicit widening flags implemented and tested: `--allow peer:<node_id>`, `--allow org:<org_id>`; `--public` deferred (not in v0)
- [ ] Tool list refresh: re-read `tools/list` on `listChanged` notification or periodic poll; descriptor updates propagate ("always up-to-date types" must hold for bridged tools too)

**Acceptance:** wrapped fixture server on machine B, invoked from machine A via CLI; token-leak regression test passes (below, using the fixture's sentinel token); different-identity node denied at the wrapper with a **structured rejection** (signed if signed rejection events already exist as a primitive; otherwise logged locally and signed rejections deferred — do not let this block the wedge).

**Token-leak regression test (permanent CI fixture):** set a fake token with a distinctive sentinel value, invoke the wrapped tool cross-machine, then grep daemon logs, mesh packet captures, traces, and error output for the sentinel. Assert absent or redacted everywhere.

**Non-goals:** remote/HTTP MCP servers, OAuth flows, non-stdio transports. All deferred.

---

## Phase 2 — `net mcp serve` (demand side, meta-tools default)

- [ ] Stdio MCP server shim targeting the 2026-07-28 stateless spec shape
- [ ] **Shim is a thin client to the running `net` daemon** — never an embedded node. Multiple hosts (Claude Code + Cursor on one machine) = N shims, one daemon, one identity. Clear error if no daemon is running.
- [ ] Default surface = meta-tools only:
  - `net_search_capabilities(query)` — returns grouped results (v0 = node-namespaced)
  - `net_describe_capability(cap_id)` — full schema + risk/credential status + provider info
  - `net_invoke_capability(cap_id, args)` — with pre-flight validation
  - `net_list_pinned_capabilities()`
  - `net_request_pin(cap_id)` — creates a **pending pin request**; does not grant anything (see Phase 3)
- [ ] **Pre-flight arg validation:** validate `args` against the descriptor's input schema BEFORE routing; on failure return a crisp, field-naming validation error the model can self-repair from. Track validation-failure rate as a core metric.
- [ ] **Shim-side consent:** capabilities with status `credentialed`, `external_api`, or `unknown` are NOT invocable through `net_invoke_capability` until allowlisted in shim config or pinned with user approval. Search/describe still show them, marked `requires_approval`.
- [ ] `--direct --limit N` flag for small-mesh demo mode (direct tool exposure); explicitly not the default
- [ ] **Failure strings are product.** Ship these exact behaviors:
  - `No Net daemon is running. Start one with: net up`
  - `No remote capabilities found. Run 'net wrap ...' on another machine.`
  - `Capability requires local approval. Approve with: net mcp pin approve <id>`
  - `Denied by remote wrapper: caller root identity does not match owner scope.`
- [ ] One-line host config documented and tested:

```json
{ "mcpServers": { "net": { "command": "net", "args": ["mcp", "serve"] } } }
```

- [ ] **MCP host matrix** maintained from day one (host behavior varies):

| Host | stdio MCP | tools/list changed | schema fidelity | approval behavior | truncation behavior |
|---|---|---|---|---|---|
| Claude Code | | | | | |
| Cursor | | | | | |
| (third host) | | | | | |

- [ ] **User-facing docs state the approval boundary explicitly:** approving `net_invoke_capability` in your MCP host lets the model *request* invocations; the shim still blocks credentialed/external/unknown capabilities unless you allowlist or pin them.

**Acceptance:** Claude Code on machine A searches, describes, and invokes wrapped fixture tools on machine B through the shim — including the erroring, slow, and schema-changing tools; the fixture's fake-credentialed capability is blocked until approved; two hosts on one machine share the daemon cleanly; host matrix populated for all three hosts. (GitHub run = recorded demo, not the gate.)

---

## Phase 3 — Pinning as promotion (the load-bearing feature)

Pinning is the reliability mechanism and the consent mechanism, not a convenience. Two rules govern everything here:

- **Pin approval is local client consent** for this shim/user profile — it is never remote authorization. Wrapper owner/policy always wins.
- **The model must not be able to approve its own future tool access.** Consent happens outside the model loop.

Tasks:

- [ ] Pin flow: `net_request_pin(cap_id)` (model-callable) creates a pending request and returns instructions; the user confirms out-of-band via `net mcp pin approve <id>` (CLI) or a local prompt. Only then is the pin active.
- [ ] `net mcp serve --allow-model-pinning` exists for people who want host-prompt-mediated pinning; **off by default**
- [ ] Pinned capability = promoted to a **first-class MCP tool** in the shim's tool list, with its real name and full JSON schema (restores per-call schema accuracy + individual host approval prompt)
- [ ] An approved pin satisfies shim-side consent for that capability, for that user profile, on that machine — nothing wider
- [ ] Pin state persisted per user (daemon-side), shared across shims/hosts on the same machine
- [ ] `tools/list` change notification emitted on pin approval/removal so hosts refresh
- [ ] Canonical usage pattern documented: **search → describe → invoke (validated) → request pin → user approves → first-class typed tool**
- [ ] Optional: auto-suggest a pin request after N successful invokes of the same capability

**Acceptance:** pinned fixture tool appears as a native typed tool in Claude Code with correct schema — including after a commanded schema change; invoke error rate on pinned tools ≈ direct-tool baseline (measure both); a model attempting to activate a pin without user approval fails with the pending-approval message.

---

## Phase 4 — Duplicate grouping + failover routing

- [ ] **Canonical identity is structured, never a string:** `{capability: "github.create_issue", provider: <node_id/pubkey>}`. Node *aliases* are mutable display labels — they never enter identifiers, pins, or scripts. Display form uses `/` for the node qualifier (`homelab/github.create_issue`) since `.` is ambiguous inside capability names and `@` is taken by versioning. Model-facing pinned names are daemon-allocated, host-charset-safe (`[a-zA-Z0-9_-]`, ≤64): `github_create_issue`, provider-suffixed only when disambiguation demands it.
  - v0 refinement: the pinned name is a **pure function of the capability id** (always hash-suffixed), so it is independent of which other pins are approved — an out-of-band approve/reject can never remap a name a host cached onto a different capability (review F9). The provider node id is **canonicalized at parse** (decimal, whitespace-trimmed, `0x`-hex accepted) so identity and routing agree on one spelling; it is still carried as a string rather than a typed `u64` (the deeper form, deferred — review F11).
- [ ] v0 (ships with Phase 2): search results grouped by capability, providers listed:

```
github.create_issue
  providers: homelab, laptop
```

- [ ] v1: `descriptor_hash = hash(tool_name + input_schema + output_schema + semantics_version + compat_tier + credential_context)`
- [ ] **`credential_context` is privacy-safe and never raw-token-derived.** It is an opaque local equivalence class: `hash(provider_id + account_id_if_known + local_salt)`. If the account id is unknown, do not collapse. Public announcements never carry anything that could correlate private accounts across meshes — collapse happens only within owner-local context.
- [ ] Collapse into one logical capability only when `substitutability: "provider_equivalent"`; filesystem-class tools stay provider-local forever. The equivalence key (`descriptor_fingerprint`) folds `substitutability` + `credential_status` alongside the schema, so a credentialed / provider-local primary can never fingerprint-match a collapsible candidate (review F1/F2 hardening, commit `dab94971c`).
- [ ] **Collapse + failover are OPT-IN, off by default** (`MeshGateway::trust_equivalent_providers` / `net mcp serve --trust-equivalent-providers`). Equivalence today is proven only from *wire-declared* attributes a peer controls (substitutability, credential_status, public schema), with **no proof the peer shares the primary's owner/root identity** — that verification is deferred to the permission system. Until it lands, the safe default keeps every provider provider-local: each is discovered, pinned, and invoked on its own node id, so a hostile co-tenant that forged a matching contract can neither become a group's representative nor intercept a failover (review F2). Operators whose mesh peers are all their own opt in explicitly.
- [ ] **Invoke is at-most-once for credentialed tools.** A timeout does not prove non-execution, so only a duplicate-safe (uncredentialed) tool retries a timed-out call on the same provider — the same class that is eligible to collapse/fail over. A credentialed / stateful tool surfaces the timeout rather than re-running it, so a side effect (issue, charge) is never silently duplicated (review F1; `InvokeSafety` enum).
- [ ] Routing for collapsed capabilities (when opted in): owner/policy filter → health → proximity → load → pinned preference
- [ ] **The failover demo (hero artifact):** same tool wrapped on three machines, kill one mid-session, next invoke succeeds via another provider, agent never notices. Script it, record it, put it on the site. This is the demo no MCP gateway can do. (Runs with `--trust-equivalent-providers`, and only for uncredentialed substitutable tools where duplicate execution is harmless.)

**Acceptance:** failover demo passes 10/10 scripted runs; cross-account collapse is impossible by construction (test with two distinct tokens for the same provider); collapse/failover is inert unless explicitly enabled.

---

## Phase 5 — Native showcases (Hermes + OpenClaw plugin)

Everything here uses public SDK API only. **The OpenClaw items are optional accelerants — the core wedge must never depend on that ecosystem.**

- [ ] Hermes connects natively: delegated keypair from user root, reads capability index directly, lowers descriptors via existing OpenAI/Anthropic/Gemini translators, nRPC + channels for calls
- [ ] Hermes announces itself as a capability (`agent.hermes` + tool shape). **A2A begins as capability invocation against agent capabilities;** if durable conversation/task semantics require additional event types later, they layer on the same substrate — no commitment that a separate A2A envelope never exists
- [ ] OpenClaw plugin (optional track): embeds SDK, exposes mesh capabilities as OpenClaw skills, publishes selected skills to the mesh (owner-only default). Ship to ClawHub; port to at least one community fork (NanoClaw or IronClaw) in the same release cycle so the extension serves the whole OpenClaw-family ecosystem
- [ ] Showcase demos (things the bridge tier structurally cannot do):
  - [ ] Streaming tool: continuous feed (camera/log tail) consumed by an agent with real backpressure
  - [ ] Artifact movement: agent on laptop produces file, agent on desktop pulls via Dataforts by hash
  - [ ] Migration: Hermes moves laptop → desktop mid-task via Mikoshi, conversation continues
- [ ] Demo scripts published as the "why graduate from the bridge" page

**Acceptance:** each showcase runs from a scripted, reproducible setup; the tier story is documented with the upgrade path.

### Tier language (official)



| Tier | Meaning |
|---|---|
| MCP compatibility | use Net from existing MCP hosts (`net mcp serve`) |
| Mesh capabilities | publish existing tools into Net (`net wrap`, pinning) |
| Native Net | streams, artifacts, A2A, migration, policy (SDK) |

---

## Cross-cutting workstreams

**Docs / quickstart (ships with Phase 2).** README leads with exactly three conceptual steps, in this order — never the shim alone (empty index = instant bounce). Show expected output so people see it worked:

```bash
# 1. both machines
net up

# 2. machine B
$ net wrap time -- uvx mcp-server-time
wrapped 1 tool:
  time.get_current_time
visibility: owner_only
scope: same_root_identity

# 3. machine A
$ net cap search time
time.get_current_time
  provider: machine-b
  compat: mcp_bridge
  requires_approval: false

$ net mcp serve   # + one-line MCP host config
```

**Conformance fixture (`net-mcp-fixture`) — acceptance runs against this, never against GitHub.** A purpose-built stdio MCP server with injectable behaviors: deterministic tool set, slow tool, erroring tool, schema-change-on-command (exercises live descriptor updates), `listChanged` on demand, large-payload tool, fake-credentialed tool with a sentinel token (feeds the token-leak test). Hermetic, no network, runs in CI. GitHub/filesystem servers are **recorded demos** — demos want recognizable brands, tests want determinism; one artifact doing both jobs was the bug. `server-everything` is a host-matrix row, not the fixture: you must own the fixture to make it misbehave on command.

**Security review (before Phase 2 ships).** Threat-model both confused deputies: supply side (mesh peer spends the homelab token → owner-only default + wrapper-side identity check) and demand side (single host approval of meta-tool = skeleton key → shim consent + out-of-band pin approval). Verify the authority model block holds end to end. Token-leak regression test in CI.

**Spec tracking.** MCP 2026-07-28 finalizes July 28. Pin the RC now; one owner watches the changelog through final; all spec-version logic isolated in `adapters/mcp/spec/`.

**Metrics.** Time-to-first-cross-machine-invoke (target <5 min); meta-tool invoke validation-failure rate vs pinned-tool rate; wrapper uptime/failover success; pin-request → approval conversion; adoption: installs → wraps → serves → pins.

---

## SDK matrix & binding conventions (cross-cutting, applies to all plans)

One Rust core (`net-mesh-sdk`), thin idiomatic wrappers. Not five SDKs — one SDK, five faces. Bindings follow the **existing construction** in the repo, not an invented one:

| Language | Binding path | Concurrency shape |
|---|---|---|
| Rust | the crate itself; source of truth incl. resilience policy defaults (retry/hedge/circuit-breaker) | async (tokio) + blocking helpers |
| TypeScript | napi (existing) | Promise-native only — that's the idiom |
| Python | pyo3 native class + typed pure-Python layer (dataclasses, `.pyi`, `py.typed`) — existing pattern | **dual sync + async, both first-class**, wrapping the same native class |
| Go | cgo over the C ABI (existing) | blocking + `context.Context` cancellation — goroutines are the async story, no separate variant |
| C | the ABI itself (already shipped under Go); documented as public SDK when embedded demand shows | callback-based |

Rules:

- **Idiomatic in shape, identical in concepts.** Same nouns and lifecycles everywhere; a Go dev and a TS dev reading each other's Net code recognize everything. Sync/async duality exists only where the language culture demands it (Python: yes; TS/Go: no).
- **Known gap, scheduled not assumed:** the current Python binding is sync-complete; native `async def` handler support requires pyo3-asyncio/tokio-bridge work its own docstring names as a follow-up. **Hermes Phase 1 depends on the async surface** — this lands before or with the Hermes plugin, not after.
- **Every new surface ships to the whole matrix or is explicitly staged.** Pins, consent, provider-side (announce/serve/withdraw), payments: each lands in Rust + the bindings with named consumers first (TS/Python), Go/C staged by demand. In Python, "ships" means both halves — an async-only or sync-only new API breaks the construction.
- **Resilience policies mirror Rust defaults in every binding** (the existing mesh_rpc pattern). Cancellation exists in every surface — the `Cancellable` pattern is already there; new APIs keep it.
- **Bindings marshal; they never implement.** Logic lives in the Rust core or the daemon. Five divergent verification behaviors is how payment systems get robbed.
- **Conformance per binding:** golden vectors for every signed object, and the key invariant as a negative test *per language* — no binding surface can accept, return, serialize, or log private key bytes. Five languages, five columns in the test matrix.

## Explicit non-goals (this plan)

- Payments, billing events, attestations, KYB, settlement — later ladder rungs
- Remote/HTTP MCP server wrapping, OAuth passthrough
- Public mesh exposure (`--public`), cross-org federation
- Marketplace/registry website surface
- MCP Apps / Tasks extension support in the shim (evaluate after final spec)
- Signed rejection events (if not already a primitive — structured rejection suffices for v0)

## Open risks

| Risk | Mitigation |
|---|---|
| `net up` onboarding slower than 90s/machine | Phase 0a exit gate; fix before building on top |
| Meta-tool arg accuracy still poor despite validation | Pin-promotion path exists; measure early, bias demos toward pinned tools |
| Credential-status heuristics misclassify | Unknown treated as credentialed; downward override requires `--force` |
| Users read "Net = MCP router" | Tier language + native showcases; the bridge is never the headline of the site |
| MCP final spec shifts from RC | Isolated spec adapters; stdio-first limits exposure |
| Upstream plugin API churn (fast-moving codebase) | Optional track, thin plugin, fork ports same cycle |
| Host truncates even the small meta-tool set | Host matrix from day one; tested against 3 hosts in Phase 2 |
| Adapter quietly grows core-internal dependencies | CI dependency check: `net-mesh-mcp` may depend on `net-mesh-sdk` only; violations fail the build |
| Collapse/failover trusts wire-declared equivalence (a peer can forge a matching contract; no proof it shares the owner/root identity) | Off by default; opt-in `--trust-equivalent-providers`. Full fix = verify a shared owner/root identity, deferred with the permission system (review F2) |
| Retrying a credentialed invoke on a timeout duplicates a real side effect | At-most-once for credentialed / stateful tools — only uncredentialed (duplicate-safe) tools retry a timeout; retry-safety is an `InvokeSafety` flag (review F1) |
| A wrapped stdio server floods stdout, or is dropped mid-reply, hanging the wrapper | Bounded line reader (32 MiB cap, over-length lines dropped); server-initiated replies written off the read path so both pipes can't wedge (review F3/F7) |

---

## Appendix A — Hermes Native Integration

Moved to its own plan: **`hermes-native-integration-plan.md`**. Summary of what lives there: H-rules (client-not-node, one daemon-side consent engine, delegation before publication, folds before model context, public SDK only), staged phases from native client through pin promotion, delegation, publication, A2A with day-one cancellation, fold-based streaming, Dataforts artifacts, Mikoshi migration, and the shared permission model.

**Cross-plan dependency:** the Hermes plan's Phase 1 requires the consent/pin/validation engine from this plan's Phases 2–3 to live **daemon-side**, exposed via public SDK. If any of it shipped shim-side, refactor before starting Hermes work.
