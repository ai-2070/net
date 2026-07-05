# Implementation Plan: Hermes Native Integration (v2 — grounded in hermes-agent code)

**Follow-up to:** `net-mcp-bridge-implementation-plan.md`. The bridge plan covers MCP *compatibility*; this plan makes Hermes (github.com/NousResearch/hermes-agent) a first-class Net participant — the native same-root showcase.

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

## The four killer use cases (north star — every phase serves one of these)

Same root, your machines. The plugin exists for:

1. **Reach** — one agent, remote hands. Mac Hermes runs `pc/terminal.run`, `pc/computer_use`, `pc/files.read`; model routes by naming the machine (`pc/...`, `mac/...`) or by capability tags (`gpu:true`). One brain, many hands.
2. **Grab** — files move over Dataforts by content hash, either direction, integrity-checked. No SMB/scp/AirDrop.
3. **Parallel** — PC agent grinds a long job while the Mac agent keeps talking to you. The *only* same-root A2A case.
4. **Move** — the brain follows you: `hermes move pc` relocates the agent, memory intact.

Filter: plugin work that doesn't make one of these four faster is ring-jumping. MCP bridging, payments, strangers, attestation are later rings that serve these, never headline them. One sentence: **your agent, every machine you own — reach, grab, parallelize, move.**

## Doctrine (H-rules)

**The invariant: adapters attach; nodes participate. The MCP bridge translates protocol surfaces; Hermes holds identity as a Net participant.**

1. **H1 — Embedded first-class node.** Hermes embeds the Net node in-process via `net-mesh-sdk` (pyo3 over the Rust core): own keypair, directly addressable, joins the mesh itself. Marginal memory/CPU buys direct addressability — which A2A, streams, and migration all require. No daemon exists; if per-machine node count ever hurts, a shared daemon returns later as an optimization *behind the same SDK API*, invisible to Hermes. **The SDK API must never expose whether the local participant is an embedded node or a shared daemon** — that opacity is what keeps the refactor possible.
2. **H2 — One consent engine, one implementation.** Pin store, consent state, arg validation, credentialed blocklist, audit: implemented once in the Rust SDK, shared across every process on the machine (locked per-user store). Hermes approval UX and any other frontend (shim, admin CLI) are views over the same state. Approved anywhere = approved everywhere. **The shared store is policy/consent state, not shared node identity** — each first-class app/agent node keeps its own identity and delegation; nobody flattens keypairs into the store.
3. **H3 — Delegation before publication.** No side-effectful tool publication until per-agent identity exists.
4. **H4 — Explicit, tagged publication.** Selected tools only, owner-only default, risk-tagged.
5. **H5 — Streams feed state; models query folds.** Raw chunks never enter context.
6. **H6 — Public SDK + public plugin API only.** The integration is `plugins/net/` + `net-mesh-sdk` — an **official plugin, not a core dependency**, disabled unless enabled. **No Net-specific Hermes core patches**: if the plugin API lacks a primitive, upstream a general-purpose public plugin/registry/approval hook — never a private Net shortcut, in either codebase.
7. **H7 — No payments, no untrusted networks** in this plan.
8. **H8 — No key material, ever.** Hermes (and its subagents) never reads, receives, or relays private keys — identity or settlement. Keys live in the Rust core or external signers; agents request typed operations only. No tool result, config surface, or A2A message may carry key bytes.
9. **H9 — CLI is not a runtime surface.** `plugins/net/` never shells out to `net` or `hermes` for normal operation; native integration tests run with the `net` binary absent from PATH. CLI commands, where they exist, are human/admin/debug frontends over typed SDK APIs — never prerequisites for search, invoke, publish, pinning, consent, delegation, streams, artifacts, migration, or payments. Install/update commands (`hermes plugins install ...`) are human distribution UX, not runtime dependencies.

### Anti-goals
No MCP awareness inside `plugins/net/` (it sees `ToolDescriptor`s); no full mesh dump into the registry; no auto-publication of Hermes's toolsets; no silent pin approval; no Hermes-side permission model; no payments.

---

## Cross-plan dependency

Phase 1 requires the consent/pin/validation engine exposed via `net-mesh-sdk` (graduated out of the bridge crate per the SDK plan):
`capability.search/describe/invoke`, `pins.list/request/state`, consent resolved inside `invoke`, audit events, and a pin-change subscription.
**Gate test:** a pin approved through the SDK consent/pin API from one process is immediately visible to a Python SDK client in another process, under concurrent access. Can't write that test → the engine isn't actually shared → refactor first. CLI/shim visibility is optional compatibility coverage, not a Phase 1 gate.

> **Gate status (2026-07-05, branch `hermes-plan`).** The gate was audited against both codebases; two of the five primitives were missing from Python, so the "refactor first" ran as **Step 1** (5 commits):
> - ✅ **`capability.search/describe/invoke` with consent resolved inside** — the `describe → validate → consent/pins → invoke` composition was extracted into `net_mcp::serve::gated_invoke` (one implementation; the stdio shim delegates) and exposed as **`net_sdk.CapabilityGateway`** (`net.CapabilityGateway`) returning a structured `{status}` result. This speaks the `net wrap` bridge protocol, so it discovers/invokes exactly the wrapped capabilities Phase 1 targets.
> - ✅ **`pins.list/request/state`** — already shared (`net_sdk.PinStore`/`AsyncPinStore`, cross-process-locked, same file `net mcp pin` uses). The **SDK-to-SDK** propagation gate holds: a pin written by one SDK process is visible to an independent SDK process/plugin (`integrations/hermes/tests/test_plugin.py::test_request_pin_records_pending_and_list_reflects_it` drives plugin-SDK → operator `PinStore.approve` → plugin-SDK list against one shared store; `test_consent_pins.py::test_cli_approval_is_visible_from_python` additionally covers the CLI frontend as compatibility coverage). The per-user default path is now `net_sdk.default_pin_store_path()` (graduated from the CLI).
> - ✅ **pin-change subscription** — landed in Phase 2: `PinStore::watch` / `snapshot_and_watch` (SDK, feature `pin-watch`) is a real OS file watcher; Python `AsyncPinStore.snapshot_and_watch` / `AsyncPinWatcher` yield `PinChange{added, removed}` deltas. (This was the one open ❌ at Step 1; the reference shim still polls, which is fine for a compatibility frontend.)
> - **audit events** — deferred (Phase 2/3; no per-invocation audit crosses to Python yet).
>
> Both a sync `CapabilityGateway` and an awaitable `AsyncCapabilityGateway` (spawns on the mesh runtime, bridges the JoinHandle) are exposed, so `is_async` plugin handlers `await` the gateway directly. Net: the Phase-1 native path is unblocked; the only cross-plan primitive still outstanding is per-invocation audit.

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
- [ ] The five tools registered as `ToolEntry`s, toolset `net`, `check_fn` = **local node initialized + SDK usable** — never "remote peers visible." A healthy-but-isolated node keeps its tools; remote absence surfaces as empty search results or per-call mesh-unreachable errors, not tool flicker. Only local node/SDK failure past grace removes tools from the array. **Lid-close test (the realistic home topology is asymmetric — laptop sleeps, desktop stays on):** close the Mac's lid, reopen — node rejoins, remote tools reappear at next assembly, no stale-node weirdness. This is the first thing that happens to a real user and the first thing that breaks
- [ ] `net_invoke_capability` calls SDK `capability.invoke` (validation + consent applied inside the SDK, never re-derived in Python); `validation_error` returned verbatim to the model (self-repair); `requires_approval` returns the pin instruction string
- [ ] `net_request_pin` creates a pending request in the shared pin store and returns a **structured response** the model can relay: `{status: "pending_approval", request_id, approval_channels: ["telegram","desktop","cli_fallback"], message: "Approval required in Hermes or another trusted operator surface."}` — never approves; the CLI is a fallback channel, not the canonical approval UX
- [ ] **Meta-tools are always-load** (exempt from `tool_search` deferral) while the plugin is enabled — five small, high-leverage tools; a search-to-find-search double indirection kills the flow. Pinned/promoted tools remain deferrable if the set grows large
- [ ] Mesh descriptors pass through `schema_sanitizer` before any model-visible surface
- [ ] Tool descriptions explicitly disambiguate mesh search from Hermes's local `tool_search`

**Acceptance:** the compressed milestone — machine B exposes a GitHub capability through a wrapped provider fixture (launched as a headless service by a human — admin CLI is fine for that, runtime shell-outs are not); Hermes on machine A (plugin enabled, no MCP path involved) searches, describes, hits `requires_approval`, user approves via the SDK consent API, invokes. Plugin sees `ToolDescriptor`s only.

> **Phase 1 status (2026-07-05).** The plugin is built and unit-tested in `net/crates/net/integrations/hermes/` (`plugin.yaml`, `__init__.py` `register(ctx)`, `node.py`, `tools.py`, 8 pytest cases). **Done:** the five `net_*` tools register (toolset `net`, `is_async`, shared `check_fn` = local node/SDK health); `net_invoke_capability` returns `validation_error` / `requires_approval` verbatim from the SDK gate; `net_request_pin` returns the structured pending response (operator-surface-first per H9, CLI as a fallback channel) and shares the store with `net mcp pin` (proven by a plugin→pending→operator-approve→list test); descriptions disambiguate the mesh search from Hermes's local `tool_search`. **Deferred:** meta-tools-always-load (Hermes keys never-defer off a *name* in the core `_HERMES_CORE_TOOLS` list — needs a config mechanism / upstream public hook, not a core patch per H6); running mesh descriptors through `schema_sanitizer` (relevant once pin-promotion in Phase 2 registers descriptor schemas as real tools — the five meta-tool schemas are static and already sanitized by Hermes's normal assembly). **Live acceptance (SDK level, no CLI):** proven — `adapters/mcp/tests/serve_end_to_end.rs::gated_invoke_holds_until_the_pin_is_approved` wraps the fixture via `ServerPublisher` on a live two-node mesh, then drives the *exact* `gated_invoke` the Python `CapabilityGateway` wraps: unpinned → `RequiresApproval` (tool never reached), `PinStore::mutate` approve (as `net mcp pin approve`), approval visible → `Invoked` (echo round-trips). Both gates exercised — consumer consent + provider owner-scope. **Real Hermes loader:** validated. A CPython-3.13 `net-mesh` wheel was built (maturin `-i`) and the plugin was driven through Hermes's *real* `PluginContext` → `tools.registry` → `get_definitions` in the Hermes venv (`integrations/hermes/tests/real_hermes_loader_check.py`): the five tools register into the real registry under toolset `net`, survive Hermes's model-facing assembly (each `check_fn` builds the embedded node in Hermes's own interpreter, the schema sanitizer runs), and the `on_session_end` hook tears the node down. **Still not run here:** the *Python* consumer against a real provider **cross-process** — covered transitively (the Python gateway is a thin PyO3 wrapper over the SDK-level-proven gate); the **lid-close / sleep-wake rejoin test** newly required by this phase (node rejoins, remote tools reappear at next assembly, no stale-node weirdness) — not yet exercised (needs a real sleep/wake or a simulated node drop+rejoin harness); and a full running Hermes end-to-end against a live remote `net wrap` provider (needs two machines / a running gateway platform).

---

## Phase 2 — Pin promotion via dynamic registration

Mirror `tools/mcp_tool.py`'s server-tools pattern — it already solves this exact problem:

- [ ] `pins.py` subscribes to shared pin-store changes via the SDK; on approval, registers a real `ToolEntry` (real schema, risk tags in description, provider info); on unpin/revoke, deregisters — diff-based like `_register_server_tools`, no nuke-and-repave
- [ ] **Pinned tool names are allocated by the shared pin store (SDK-side) and stable** (persist across sessions, retired on unpin). Hermes never invents names. The SDK handles collisions deterministically (two GitHub accounts, same tool on multiple nodes → preferred alias from pin request, else provider-suffixed). One source of truth for what a tool is called across Hermes, the shim, and OpenClaw
- [ ] Every pinned `ToolEntry` gets the same check_fn semantics as the meta-tools (local node/SDK health, not peer visibility) — local failure past grace means pinned tools cleanly vanish, no stale calls into a void
- [ ] Structured results/logs carry **audit refs**: invocation id, provider node, capability id, delegation chain id — not necessarily user-visible, always log-present
- [ ] `dynamic_schema_overrides` wired to the live descriptor: schema changes flow through live — the "always up-to-date types" claim, demonstrated inside Hermes. (Pin records carry descriptor_hash for deferred cross-ownership hardening; no v1 enforcement)
- [ ] Pinned tools flow through Hermes's normal pipeline: `handle_function_call`, guardrails, hooks, truncation, and `tool_search` deferral if the pinned set grows large — zero special-casing
- [ ] **Approval UX:** pin requests surface through Hermes's existing approval flow (gateway `permissions_list_open`/`permissions_respond` for Telegram/Discord approval; terminal prompt as fallback). Resolution writes to the **shared** pin store. Test: approve a pin from Telegram, verify an independent SDK process and the Hermes plugin both see it; shim/CLI visibility is compatibility coverage when those frontends exist.

**Acceptance:** pinned remote capability appears as a first-class typed Hermes tool; arg accuracy ≈ local-tool baseline; pin approved in Hermes visible in the shim within seconds and vice versa; descriptor update on the remote side propagates to the model-visible schema without restart.

> **Phase 2 status (2026-07-05) — subscription-based, done.** The "no pin-change subscription" blocker is closed: `PinStore::watch` / `snapshot_and_watch` (SDK, feature `pin-watch`) is a real OS file watcher over the machine-shared store — it catches the atomic temp+rename AND a cross-process `net mcp pin approve`, debounced, emitting `PinChange{added, removed}` deltas (Rust test + Python `AsyncPinStore.snapshot_and_watch` / `AsyncPinWatcher`, tested). `integrations/hermes/pins.py` consumes it: promote the snapshot, then per delta describe the cap → register a real async `ToolEntry` (live schema, risk-tagged description, stable name) via `tools.registry` (as `mcp_tool.py` does), retire on removal — **diff-based, event-driven, no polling** (a cross-process approve promotes within ~1s). Consent still runs on every invoke, so a revoked pin fails closed. Promotion starts per-session (`on_session_start`). 3 pin tests + the full plugin suite green; the real-Hermes-loader check still passes. **Refinements deferred:** SDK-side canonical name allocation (names are deterministic client-side for now — the plan's "SDK allocates the name" is a later SDK slice); `dynamic_schema_overrides` wired to the live descriptor (schema captured at promote time); audit refs (Phase 3); a full running-Hermes acceptance with a live remote provider (needs the 2-machine infra).

---

## Phase 3 — Delegated agent identity

- [ ] Chain: `user root → machine → hermes-gateway agent identity`, derived from the user root identity via the SDK at node init
- [ ] Every invocation carries the chain; remote wrapper audit shows *which* Hermes
- [ ] Extension: `delegate_task` subagents get child delegations (`… → hermes-gateway → subagent-N`), so a spawned subagent's mesh calls are attributable and individually revocable — this is a Net-native answer to subagent audit that no MCP setup has
- [ ] SDK delegation list/revoke APIs honored (human CLI frontends may wrap them later): revoking the gateway delegation kills all its subagents' access
- [ ] Delegation acquisition failure at startup → Net tools **unavailable** (check_fn false), never silently degraded to machine identity; expiry/renewal handled by the SDK with renewal-failure surfacing the same way

**Acceptance:** remote logs distinguish gateway vs subagent invocations; revoking one machine's Hermes doesn't touch the other machine's.
**Gate for Phase 4:** complete.

> **Phase 3 status (2026-07-05) — Slice A (SDK + derivation) landed; Slice B (the wire) is the remaining headline.** Investigation found the whole delegation-chain machinery already exists in the core (`PermissionToken`/`TokenChain`/`RevocationRegistry`) — it gates *channel* subscribe/publish today — while a capability invoke carries **only** the tool arguments: the wrap provider's confused-deputy defense is an owner-scope check on the AEAD-verified `caller_origin`, and the `OwnerScope` doc comment literally marks the "same-root delegation chain" check as the deferred Phase-3 work. So Phase 3 splits:
>
> **Slice A — the SDK delegation surface + plugin derivation (done).** `net_sdk::delegation` adds the `root → machine → gateway → subagent` derivation *convention* over the core machinery: `DelegationChain::{derive_gateway, extend_to_subagent, verify, to/from_bytes}`, a shared `RevocationRegistry`, and a `derive_child_seed` blake3 KDF so machine/gateway identities are reproducible from the root with no extra persistence. Exposed to Python (`net.DelegationChain` / `RevocationRegistry` / `derive_child_identity`, re-exported through `net_sdk.delegation`) — **H8-clean: opaque `Identity` handles + public entity-ids only, seeds never cross into Python**. The plugin's `delegation.py` derives the chain at node init from `NET_MESH_IDENTITY_SEED` (now the *user root*) and `node.check_net_available` gates on it: a **revoked or expired** gateway delegation removes the tools (never invoke under an invalid chain), and derivation failure with a seed present is an acquisition failure → tools unavailable, never a silent degrade (no seed ⇒ un-delegated dev node, tools still load). Revocation semantics fall out of the core model exactly as the acceptance wants: bumping the **machine** issuer's floor kills its gateway chain *and its subagents* (the gateway link is in every subagent's chain) while another machine's chain is untouched. Per-subagent revocation is the SDK's documented v1 model (short TTL + stop renewing). **Tests:** 7 Rust + 9 Python binding + 6 plugin, all green, incl. `revoke-machine-kills-gateway-and-subagents-but-not-a-sibling`; the real-Hermes cp313 loader check still passes.
>
> **Slice B — carry the chain on the invoke + provider verify/audit (the headline; deferred, one security decision).** Grounded in the wire: nRPC requests carry application headers (the `net-where` predicate-pushdown header + `RpcContextExt` are the exact pattern), so the chain rides in a `net-delegation` **request header** — the body stays pure tool-args. **Critical finding — `origin_hash` is spoofable inside a channel** (`identity/origin.rs` threat model: any admitted member can mint packets under any `origin_hash`; the owner-scope gate is therefore only *channel-membership*-strong). So verifying "chain leaf == `caller_origin`" is **unsound** — a malicious member could replay a captured chain *and* spoof the owner origin to match. `origin.rs` prescribes the fix: layer a signed envelope inside the payload via `PermissionToken`'s signing. The sound design: the caller also attaches a **fresh per-invoke signature by the leaf's private key** over a request-binding challenge (`capability_id ∥ blake3(args) ∥ timestamp ∥ nonce`), and `WrapInvokeHandler` verifies **fail-closed**: chain-roots-at-owner + unrevoked + valid **and** the signature against `leaf.subject` + anti-replay — proving the caller holds the leaf key, then audits the leaf. This **dissolves the earlier operating-identity fork**: the signature (not the spoofable origin) binds the caller, so the gateway node keeps its own identity (no origin re-keying) and the chain is a sound *attestation*; the owner-scope allowlist stays as the no-header fallback. Provider config gains the owner-root `EntityId` (`net wrap --owner-root`); the gateway already holds the leaf `Identity` handle (Slice A) to sign. **Remaining decision — anti-replay:** (A) stateless signed-envelope — timestamp+nonce in the header, short validity window + bounded seen-nonce cache, no extra RTT, mirrors `PermissionToken`'s timestamp+skew model (**recommended**); vs (B) challenge-response — +1 RTT, clock-independent. Because this changes the security-critical confused-deputy gate, it lands as its own reviewed slice (in-process 2-node test first, then the 2-machine acceptance). Slice A does **not** touch the wire or the node's operating identity, so it's independent of this.
>
> **Slice B1 landed (2026-07-05) — the provider gate + wire.** `net_mcp::wrap::DelegationGate` (approach A, stateless signed-envelope) verifies, fail-closed: the `net-delegation-sig` envelope signature against the chain leaf over a domain-separated, length-prefixed challenge (`tool ∣ args ∣ ts ∣ nonce`), a fresh timestamp window + non-replayed nonce (authenticated-only, pruned + capped cache), and the `net-delegation` chain roots-at-owner + unrevoked + valid — then audits the admitted leaf via an optional sink. `WrapInvokeHandler` admits a chain-bearing invoke through the gate, else the unchanged owner-scope path; `WrapConfig` gains an optional gate threaded through publish + refresh; `EntityId::verify_bytes` (core) lets the adapter verify raw sig bytes with no ed25519 dep. **11 gate unit tests** (valid / wrong-root / revoked / stale / replay / tampered-sig / args-bound / tool-bound / **non-leaf-signer** / malformed / audit) + an **e2e** test (`serve_end_to_end.rs::a_delegated_invoke_admits_via_the_chain_and_audits_the_leaf`): over a live 2-node mesh, an owner-scope-*excluded* caller is admitted purely by presenting a valid chain + leaf signature in the request headers, the echo round-trips, and the provider audits the gateway leaf. All green; no regression. **Slice B2 adapter tier landed (2026-07-05):** `DelegationSigner` (caller-side) + `MeshGateway::with_delegation` auto-attach + sign every invoke (fresh per-attempt nonce so a retry isn't a self-replay; challenge bound to the **service/tool_id** the caller invokes, so sanitized names — `tool_id != mcp_name` — verify: the provider now checks the challenge over `service`, not `mcp_name`). Signer↔gate round-trip unit test + e2e `the_gateway_auto_attaches_delegation_and_invokes` (owner-scope-EXCLUDED caller invokes through the plain `gateway.invoke` path, no hand-built headers, provider audits the leaf). **Slice B2 PyO3/plugin tier landed (2026-07-05):** `CapabilityGateway` / `AsyncCapabilityGateway` accept `delegation_leaf` (the gateway `Identity` handle) + `delegation_chain` (bytes) — both-or-neither — and build a `DelegationSigner` → `MeshGateway::with_delegation`, so an embedded gateway invokes delegated; the plugin's `GatewayDelegation` exposes `gateway_identity` + `chain_bytes()` and `node.py` wires them when the node is delegated, so a seeded Hermes plugin invokes delegated end-to-end (un-delegated ⇒ plain gateway). H8-clean (the leaf key stays in the handle). Tests: 2 binding + 1 plugin; cp310 suites green; the cp313 real-Hermes loader check still passes. **Slice B2 operator config landed (2026-07-05):** `net wrap --owner-root <ENTITY_ID_HEX>` builds a `DelegationGate` anchored at that user root (with a stderr audit sink logging the admitted leaf) and sets it on the `WrapConfig`, so an operator turns on verified-delegation admission from the CLI (owner-scope still governs no-chain callers). `parse_owner_root` + tests. **Remaining B2:** cross-process **revocation propagation** to the provider (the gate uses a fresh `RevocationRegistry` — an operator `net identity revoke` reaching a running provider is the follow-up; the gate supports it structurally), and the full **2-machine acceptance** (gateway vs subagent audit distinguished on real hardware — needs the infra). Phase 3's plugin-usable delegated-invoke path — derive → sign → verify → audit, plugin + provider — is now **complete and operator-configurable single-box**.

---

## Phase 4 — Hermes publishes selected local tools

- [ ] **Three-level trust model.** (1) *Same-root machine trust:* one explicit grant — "federate my machines" — unlocks owner-to-owner reach, no per-tool ceremony. (2) *Category grants:* same-root high-risk classes (terminal/write, browser state, session/memory, `desktop_control`) still require an explicit category grant with local-equivalent approval — `pc/computer_use` works same-root *after* the `desktop_control` grant, with visible stop/recovery controls. (3) *Cross-root publication:* explicit per tool, owner-only default, risk-tagged — the tiers below. Category grants are **shared SDK policy entries rendered by Hermes UX**, not a Hermes-local permission layer (anti-goals hold)
- [ ] Cross-root publication: SDK/config allowlist (`hermes net publish <tool>` is a human frontend over the same SDK API) — never automatic, never a whole toolset by default
- [ ] Publication sensitivity tiers (enforced, not advisory):

| Tier | Examples | Publish behavior |
|---|---|---|
| harmless | `time.now`, sanitized `sys.info` | explicit publish |
| scoped read | read-only file roots, repo status | explicit roots only |
| **sensitive read** | `session_search` (private conversation history, memory, accidentally-mentioned secrets), browser read state | explicit + warning; owner-only, never bundled |
| side-effectful | write/patch, `terminal_tool`, `cronjob_tools` | dangerous flag + approval |
| desktop/network control | `computer_use_tool`, browser actions | **Same-root:** allowed after explicit `desktop_control` category grant + visible stop/recovery controls. **Cross-root:** not publishable until the shared policy engine (Phase 9) and a separate consent design — a mesh-published desktop is a remote-control primitive |
- [ ] Risk tags derived from toolset membership + Hermes's own approval classification (it already knows which tools are dangerous — reuse that judgment, don't re-tag by hand)
- [ ] Enforced gate: `side_effectful`-tagged tools cannot publish pre-delegation or without the explicit dangerous flag
- [ ] Published tools are ordinary mesh capabilities: invocable from any SDK client, another Hermes, or compatibility frontends, subject to caller-side consent **and provider-side policy** — the provider retains authority; caller consent alone is never sufficient

**Acceptance:** Hermes B publishes one read-only tool; Hermes A and an SDK fixture client invoke it; a `terminal` publish attempt without the flag is rejected; audit shows full chains both sides.

---

## Phase 5 — A2A as agent capabilities

- [ ] **Scope: A2A is for parallelism and trust boundaries.** Same-root *sequential* work uses direct capabilities (use case 1) — asking the other Hermes is briefing an amnesiac colleague with partial memory. A2A earns its keep when both agents work simultaneously (use case 3) or when the other agent belongs to someone else. Task briefs carry context refs (Datafort artifacts) because the other agent *doesn't know* — pretending otherwise is the bug
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

## Phase 8 — Migration (use case 4: the brain follows you)

**v1 — export/import over the mesh.** Hermes profile export already packs the whole brain (sessions, memory, config, skills; caches/sockets excluded — the `_default_export_ignore` list *is* the replication manifest, pre-written). `hermes move <machine>` = export profile → Datafort push → import on target → gateway start there, gateway stop here. Four existing commands collapsed into one; ships with zero new storage infrastructure. Secrets never travel — per-machine keychains, always excluded, non-negotiable.

- [ ] `hermes move <machine>` orchestration over export/import + Datafort + gateway token flip — **a human/admin frontend over typed SDK migration APIs**; plugin/runtime code never shells out (H9)
- [ ] Gateway singleton = the writer token: one active gateway per root; **gateway start checks a mesh-visible `hermes.writer` fact** and refuses if another holds it — the fat-finger double-start becomes an error message. **Force-take is defined, not vibes:** revoke the old lease + best-effort flush request + timestamped takeover event; the old writer's write path checks the lease and **fails closed the moment it discovers revocation** — a zombie gateway that missed the memo can't keep appending
- [ ] Machine-local config (CUDA paths, homebrew, platform skills) stays local; manifest test proves a moved profile boots clean on the other OS. **Missing machine-specific pieces must never brick startup** — the imported profile boots degraded-but-functional (platform skills absent, local deps unresolved) and says so; a brain that won't boot because the other machine had CUDA is a failed move

**v2 — warm brain via RedEX journal (optimization of v1, same semantics, latency minutes → seconds).** Single-writer by construction (the gateway token). Writer appends `{path, datafort_hash, version}` journal events to a `hermes.profile.<root>` channel; both machines' RedEX instances persist locally; standby is continuously warm. `hermes move` becomes flush + token flip.

- [ ] Catch-up capability: "journal events since seq N" (nRPC read of the writer's RedEX) — the Mac sleeps, wakes, catches up, then live-tails. This is the one real piece of new integration. **Access: owner-only, delegation-scoped, never widened** — the profile journal is the crown jewels (full memory + activity), permanently exempt from any wider visibility or future public discovery ring
- [ ] Snapshot cadence: periodic full profile snapshot to Datafort + journal-since-snapshot (retention is count+size; a machine off for a month replays from snapshot, not from genesis)
- [ ] **Availability over durability, explicit:** crash-takeover boots on whatever the journal has; torn tails auto-truncate to last good entry (RedEX `redex-disk` reopen recovery does this natively — dat→idx→ts write order), no human adjudication, no startup blocking on the dead machine. One context line on takeover: "resumed on mac; ~Ns of state from pc may be missing" — amnesia is fine, *unacknowledged* amnesia is the bug
- [ ] Multi-master: **never.** Not a phase, not a flag. Parallel-mode agents write to their own **ephemeral, retention-limited scratch streams**; results are promoted home explicitly as artifacts and reports — scratch never merges into the profile journal, and abandoned scratch expires instead of accumulating

Mikoshi fold-checkpoint migration remains the eventual successor for live mid-task moves; it optimizes a flow that already works instead of inventing migration from scratch. **Prerequisite unchanged:** the identity succession rule (two-nodes-one-identity) must be specified before v2 — the writer token is that rule's first consumer.

**Acceptance:** `hermes move` round-trip Mac↔PC with conversation continuity, 10/10 scripted; v2: lid-close the Mac for an hour, wake, catch-up completes, `hermes move mac` lands in seconds; crash-kill the writer mid-write, takeover boots with truncated tail and the gap acknowledged in context.

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
Native: same-root reach success rate; lid-close recovery; Datafort transfer latency; `hermes move` success rate + duration; parallel task cancellation round-trip; pinned-tool arg accuracy vs local baseline; pin approval cross-client propagation; % invocations with full delegation chain; fold query latency. Compatibility (appendix path): config-to-first-invoke time.

## Open risks

| Risk | Mitigation |
|---|---|
| Consent engine partially shim-private instead of SDK-shared | Cross-plan gate test before Phase 1 |
| Model confuses `tool_search` (local) with `net_search_capabilities` (mesh) | Explicit disambiguating descriptions; measure misroutes in Phase 1 |
| Meta-tools always-load costs 5 tool defs in every prompt | Trivial vs the double-indirection failure mode; revisit only if measured context pressure demands it |
| net-mesh-sdk Python API gaps (pin subscription, delegation context) | SDK gap = blocker filed against Net repo, not worked around with private hooks (H6) |
| Hermes upstream churn (1.3M LOC, fast-moving) | Everything in `plugins/net/`; only stable surfaces used (registry, plugin API, approval hooks); CI matrix pins **commit SHAs, not tags** — Hermes installs pull master, so a tag identifies a lower bound, not a code state. Plus a rolling `master HEAD` CI row, since that's what real users run |
| Shared store observed inconsistently across frontends | Single shared store + the SDK-to-SDK propagation test in Phase 2; compatibility frontends tested separately |

## Appendix A — Optional MCP compatibility path (NOT on the native critical path)

*(Kept for the compatibility tier; the native phase ladder starts at Phase 1. MCP bridging is a later ring per the north star — this appendix exists so the zero-code demo is documented, not so it leads.)*

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

**Why not stop here:** this is only the MCP-compatibility tier; the native plan above is what makes Hermes a first-class Net participant.

---
