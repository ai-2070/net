# net-claude-skill — Claude skills for the Net mesh

Three [Agent Skills](https://docs.anthropic.com/en/docs/claude-code/skills) that teach Claude how to write correct [**Net**](https://github.com/ai-2070/net) integration code:

- **`net-event-bus`** — the mesh: pub/sub, nRPC request/response, agent-to-agent task handoff, the MCP bridge, org capability auth, the gang-claim scheduler, and the RedEX / CortEX / Dataforts layers on top.
- **`net-payments`** — x402 payments on the mesh: price a capability, quote, verify, settle, bill, and spend policy.
- **`net-browser`** — a **tab as a mesh node** over WebRTC, and the networked store for multiplayer game state: one node per origin, leaf-to-leaf sessions and streams, and `defineStore` / `hostStore` / `joinStore` with a Three.js scene binding.

Net looks like Kafka/NATS on the surface but has no broker. Net Payments looks like a payment SDK but never moves money. Net Browser looks like a game-networking library but the state lives on a peer, not a server. Out of the box, a coding agent will happily write Net code that **compiles, runs, and is wrong** — these skills load the right mental model and verified per-SDK templates instead.

> These are skills *about* Net; they don't install Net itself. The SDK lives at [ai-2070/net](https://github.com/ai-2070/net).

## Install

```bash
npx skills add ai-2070/net-claude-skill -g
```

## Give the agent the source too

[`opensrc`](https://github.com/vercel-labs/opensrc) is a small tool that fetches a package's real source into a local cache for exactly this purpose:

```bash
npx -y opensrc@latest path ai-2070/net
```

## More on the skills

The [`skills` CLI](https://github.com/vercel-labs/skills) detects which coding agents you have, asks which skills you want, and puts them where that agent looks — `~/.claude/skills/` for Claude Code, `~/.codex/skills/` for Codex. Drop `-g` for a project install instead (`.claude/skills/` or `.agents/skills/`), checked in and shared with your team.

To skip the prompts and say exactly what you want:

```bash
npx skills add ai-2070/net-claude-skill --skill '*' -a claude-code -g   # Claude Code
npx skills add ai-2070/net-claude-skill --skill '*' -a codex -g         # Codex
npx skills add ai-2070/net-claude-skill --skill '*' -a '*' -g           # every agent it supports
```

<details>
<summary>Other options — one skill, manual install</summary>

These are plain Agent Skills, so `-a '*'` covers Cursor, Copilot, Cline, and the rest too. `--skill` picks individual skills:

```bash
npx skills add ai-2070/net-claude-skill --skill net-payments -g   # just one
npx skills add ai-2070/net-claude-skill --list                    # see what's in the repo
```

The CLI symlinks each agent's skills directory to one canonical copy, so `npx skills update` refreshes them all at once (pass `--copy` if symlinks aren't an option).

**Without the CLI:** a skill is just a directory containing a `SKILL.md`, so copying the folder in works too.

```bash
git clone https://github.com/ai-2070/net-claude-skill.git /tmp/net-claude-skill
mkdir -p ~/.claude/skills
cp -R /tmp/net-claude-skill/net-event-bus /tmp/net-claude-skill/net-payments ~/.claude/skills/
```

Swap in `~/.codex/skills/` for Codex, or `<your-repo>/.claude/skills/` for a project install. To hack on the skills locally, clone once and symlink the folders so `git pull` updates them in place.

</details>

## Check it worked

Restart your agent — in Claude Code, `/skills` lists **net-event-bus**, **net-payments** and **net-browser**. Then just ask for something Net-shaped:

> *"Wire up a Net publisher and subscriber over the mesh in TypeScript."*
>
> *"Price a Net capability with x402 and charge callers to invoke it."*
>
> *"Build a multiplayer Three.js scene whose state lives in a Net store, joined from a browser tab."*

The matching skill loads on its own — you never have to name it.

## What's inside

Each skill is a `SKILL.md` entry point plus reference files that Claude loads **only when the task needs them**, so idle cost is one routing table rather than the whole manual.

<details>
<summary><b><code>net-event-bus/</code></b> — full file map</summary>

| File | Loaded when |
|---|---|
| `SKILL.md` | Entry point — routing table + the 5-point mental model. |
| `concepts.md` | **Always, before writing code** — why Net is not a broker. |
| `apis.md` | Generating code — verified publish/subscribe/lifecycle templates per SDK. |
| `patterns.md` | Mapping a task ("I need a relay / persistence / fan-out") to a recipe. |
| `mesh.md` | Multi-host deploys — PSK/identity bootstrap, discovery, NAT traversal. |
| `capabilities.md` | Routing to "the GPU node" / a node with model X loaded. |
| `scheduler.md` | Atomically claiming a contended resource (island/slot/seat) + workflows. |
| `streams.md` | Ordered point-to-point delivery with credit-grant backpressure. |
| `nrpc.md` | Request/response — typed call → reply, deadlines, retries, hedging. |
| `a2a.md` | The peer is an **agent**, not a service — task handoff (`serve_a2a` / `submit_task` / `cancel_task`, briefs carry Datafort refs), delegated agent identity (`DelegationChain`), device enrollment (`invite → join → approve`). Rust/Python/Node only. |
| `mcp.md` | The MCP bridge — `net-mesh wrap` a stdio server as mesh capabilities, or `net-mesh mcp serve` the mesh to a local host; pinning + credential forwarding. |
| `org.md` | Organization capability auth — a service only authorized orgs can discover or call (invisible, not refused): `net-mesh org` offline issuance, `net-mesh node adopt`, `serve_org` / `mesh.org(..).call`. |
| `subnet-auth.md` | Serving across a protected subnet boundary — exporting one service while the enclave stays sealed (`serve_subnet_exported` / `call_exported`, named exports, boundary declarations, revocation floors). The authority plane; `mesh.md` § Subnets is the topology plane. |
| `redex.md` | Durable per-channel append-only logs (replay from offset, retention). |
| `cortex.md` | Folded queryable state (SQLite-shaped queries, NetDB). |
| `dataforts.md` | Greedy caching, data gravity, blob refs, read-your-writes. |
| `runtime.md` | Shutdown contract, error handling, async-runtime integration, partitions + healing (a conflicted heal forks, it does not merge). |
| `observability.md` | Catching silent drops; Prometheus/OTel wiring. |
| `payloads.md` | Event schema, size limits, cross-language interop traps. |
| `filter-dsl.md` | Consumer-side content filtering — equality `$and`/`$or`/`$not` predicates on the bus. |
| `error-codes.md` | Classifying a specific error variant — the full core-crate + subsystem taxonomy. |
| `cli.md` | The `net-mesh` CLI — execution scopes (offline / persistent store / temporary supervisor / mesh client / hosted service), the `--local` gate, `--inspect-target`, `transfer`, `typegen`, `wrap`/`mcp`, `netdb`, org/subnet issuance, exit codes, deadlines and scripting notes. |
| `browser.md` | A **browser tab** as a mesh node — `@net-mesh/browser` (a sibling package to `@net-mesh/sdk`), `connect` vs `openSession`, the anchor bootstrap credential, leaf ↔ leaf peer sessions, stream `reliability`, the typed error kinds, `udp-blocked` vs `ice-timeout`, and the networked store + Three.js binding. |
| `testing.md` | Fixtures, race conditions, CI gotchas. |
| `gotchas.md` | Migrating from Kafka / NATS / Redis Streams / Pulsar. |
| `event-semantics.md` | Naming events / what an event may assert — a fact observed at one layer, not an end-to-end `200 OK`. |
| `source-access.md` | A citation to open, or a mechanism question the chapters don't answer — how to root a source path and fetch/read the real Net tree. |
| `bindings/` | Per-language coverage (`coverage.md`: install/import/source table + the does-binding-X-support-operation-Y record) and per-binding notes (`rust.md`, `python.md`, `typescript.md`, `go.md`, `c.md`). |
| `examples/` | Minimal runnable hello-world per SDK (TS, Py, Rust, Go, C). |

</details>

<details>
<summary><b><code>net-payments/</code></b> — full file map</summary>

| File | Loaded when |
|---|---|
| `SKILL.md` | Entry point — routing table + the TL;DR mental model + integration workflow. |
| `concepts.md` | **Always first** — the mental model, the category line, the eight doctrines, the review invariant. |
| `object-model.md` | Touching the five Net envelopes — fields, canonical signing regime, versioning, idempotency, amounts. |
| `x402.md` | Touching x402 structures — `X402Carry` byte-preservation, `PaymentRequirements` / `PaymentPayload`, CAIP ids, the `exact` EVM scheme. |
| `provider.md` | Charging for a capability — the `PaymentEngine` lifecycle (quote → verify → settle → serve → bill), pricing at publish, `serve_payments`. |
| `caller.md` | Paying to invoke — `CallerPaymentFlow` over a `ProviderChannel`, spend check, the approval loop, the MCP gateway path. |
| `facilitator.md` | Wiring the `verify` / `settle` boundary — the `Facilitator` trait, the mock, the real `HttpFacilitator`, config packs, auth. |
| `verification.md` | Confidence — the `observed / confirmed(n) / final` tiers, the independent `ChainChecker`, reorg freeze, replay. |
| `signer.md` | Settlement signing — the `SchemeSigner` seam, `ExternalSigner` / `ExternalSvmSigner` / `ExternalXrplSigner`, EIP-3009 / SPL / XRPL, the no-raw-signing invariant. |
| `spend-policy.md` | Limits, budgets, approvals — `SpendPolicyEngine`, the fail-closed default posture, delegation inheritance. |
| `networks.md` | Enabling a network — CAIP-2 / CAIP-19, the signed asset registry, the Base → Solana → xrpl "config, not code" ladder. |
| `billing.md` | Usage records / a billing stream — `BillingLog` (subscribe/read/export), immutability, what billing is NOT. |
| `http402.md` | A Net agent paying an external x402 HTTP API — the outbound `X402HttpFlow`, the header-only v2 transport. |
| `failure-schematic.md` | Handling denials / refusals — the machine-actionable `net.payment.failure@1` verdict that rides beside the human error, the reason → recovery mapping, `funds_moved` vs `prior_payment`. |
| `bindings.md` | Per-language support — Rust, Python, and Node all have a full demand+supply flow; Go is verifier-only. `CapabilityGateway` / `PaymentProvider`, Node's `close()` + `permissiveChannels` gotchas. |
| `testing.md` | Cross-language golden vectors, the mock conformance suite, the key-invariant negative test, the env-gated live run. |
| `gotchas.md` | Wrong mental model, migrating, or before merging — the review invariant, "what not to build," byte-preservation traps. |
| `source-access.md` | A citation to open, or a mechanism question the chapters don't answer — how to root a payments source path and read the real crate. |
| `bindings/` | Per-binding support notes (`rust.md`, `python.md`, `typescript.md`, `go.md`, `c.md`) and the coverage matrix (`coverage.md`) — who has a full demand+supply flow (Go is verifier-only). |

</details>

<details>
<summary><b><code>net-browser/</code></b> — full file map</summary>

| File | Loaded when |
|---|---|
| `SKILL.md` | Entry point — routing table + the seven-point mental model (a tab is a node; the anchor finds peers rather than carrying them; one node per origin; the store has one authority; the two failures a page must not misread). |
| `concepts.md` | **Always first** — a tab is a node, the anchor's role, Web Lock election and leader/follower lifetimes, the store's authority model, what a page downloads, and what a browser cannot do. |
| `session.md` | Connecting — `connect()` vs `openSession()`, the bootstrap credential, ICE/STUN configuration, admission vs carriage, leaf-to-leaf peer sessions, streams and their `reliability`/incarnation rules, the event union, identity and the origin trust boundary, and the wasm build. |
| `store.md` | Multiplayer state — `defineStore` / `hostStore` / `joinStore`, audiences with `project`/`authorize`, `act` vs `input`, the `StoreTransport` seam, snapshots and their bounds, lifecycle and timing constants, all twelve `StoreError` codes, and `bindEntities` into a scene graph. |
| `errors.md` | A rejection to classify — the `LeafError` kind table, the admission/carriage/no-answer split, `udp-blocked`'s two observations, `rpc-indeterminate` from a frozen leader, and teardown's `AggregateError`. |
| `source-access.md` | A citation to open, or a mechanism question the chapters only summarize — how to root a browser/leaf source path and read the real tree. |

</details>

## Update / uninstall

```bash
npx skills update -g                                                    # refresh to the latest
npx skills remove net-event-bus net-payments net-browser -g             # uninstall
```

Drop `-g` for project installs. If you installed by hand, `rm -rf ~/.claude/skills/net-event-bus ~/.claude/skills/net-payments ~/.claude/skills/net-browser`.

## Links

- **Net library & SDKs:** https://github.com/ai-2070/net
- **Net docs:** https://ai2070.net/docs — start with [What is Net?](https://ai2070.net/docs/start/what-is-net)
- **Agent Skills:** https://docs.anthropic.com/en/docs/claude-code/skills · **`skills` CLI:** https://github.com/vercel-labs/skills

## License

Dual-licensed under [Apache-2.0](http://www.apache.org/licenses/LICENSE-2.0) or [MIT](http://opensource.org/licenses/MIT), at your option.

---

*The skills are authored in [`ai-2070/net`](https://github.com/ai-2070/net) under `.claude/skills/`, next to the code they describe, and published to this repo automatically. Send fixes there rather than to this mirror.*
