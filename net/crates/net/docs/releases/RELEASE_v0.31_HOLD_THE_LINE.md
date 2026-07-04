# Net v0.31 — "Hold The Line"

*Named after Toto's 1978 debut single — Steve Lukather's guitar, David Paich's piano hook, Bobby Kimball's belt, the AOR radio staple whose chorus insists "hold the line, love isn't always on time." The riff everyone can hum and nobody can place the band of. Fitting: this release is about holding one line in particular — the one between a tool you trust and a mesh you don't fully.*

## The wedge: any MCP host, tools across a trusted mesh

v0.31 ships the **MCP bridge** — the [agent capability federation wedge](../plans/MCP_BRIDGE_PLAN.md). An existing MCP host (Claude Code, Cursor) can now discover and use tools that live on *other machines* across a user's trusted mesh, and any local stdio MCP server can be published *onto* that mesh — in both cases without the host learning the mesh exists and without a credential ever leaving the machine that holds it. It arrives as a new `net-mesh-mcp` adapter crate plus three CLI commands: `net wrap`, `net mcp serve`, and `net mcp pin`.

The organizing observation is the same one that has shaped every release since the substrate stopped being a prototype: **the hard parts already existed — the work was an adapter over them, not new infrastructure.** Discovery is the capability fold. Invoke and describe are nRPC calls. Owner-scoping is the AEAD-verified caller origin the transport already stamps on every request. So the bridge is a thin edge that **rides `net-mesh-sdk` only** — never the core crate — the same public surface the Python/TS bindings wrap. That constraint is enforced by a CI test, which makes the bridge the first SDK-conformance consumer and forced the provider-side SDK surface (announce / serve / withdraw) to exist rather than be invented under deadline later.

Below: the two directions, the promotion mechanism, the security line the codename is about, the docs rebuilt around it, and the review that hardened it.

---

## `net wrap` — publish a local MCP server's tools onto the mesh

```bash
net wrap github --identity ~/.net/id.toml -- npx -y @modelcontextprotocol/server-github
```

`net wrap` spawns a stdio MCP server, speaks MCP JSON-RPC over its pipes, reads `tools/list`, and lowers each tool to an **owner-scoped mesh capability** with an nRPC handler per tool plus a `describe` service. It is long-running: it streams structured output (a `wrapped` report, then a lifecycle event per `tools/list_changed` or server exit) and reconciles the announced set live as the wrapped server's tools change.

Two properties do the load-bearing work:

- **Credentials never transit.** Env vars and tokens (`--env KEY=VALUE`) are set on the wrapped server's *child process* on the owning machine. Only tool *arguments* and *results* cross the wire — never the secret. A permanent CI regression test threads a sentinel token through a cross-machine invoke and asserts it appears nowhere on the wire, in logs, or in errors.
- **Owner-only by default.** A wrapped tool is describable and invocable only by the same root identity that wrapped it — checked against the transport-verified caller origin, not a self-claimed field. Widen deliberately with `--allow <origin>`. Detection of credential status is conservative and fail-safe: an unknown status is treated as credentialed until proven boring, and the downward `--no-credentials` override requires `--force`.

Tools whose MCP name isn't already a valid channel id (`createIssue`, spaced or punctuated names) are **sanitized into a stable channel-safe id and still bridged** — the original name is kept for invocation — so wrapping real-world servers doesn't silently drop half their surface. `--substitutable` marks a stateless tool as interchangeable across providers (see failover, below); the default keeps every tool provider-local.

---

## `net mcp serve` — front the mesh to a local MCP host

```json
{ "mcpServers": { "net": { "command": "net", "args": ["mcp", "serve"] } } }
```

One line of host config and the model can reach the whole mesh. `net mcp serve` is a stdio MCP server that fronts the running `net` daemon as a **thin client** — N hosts on one machine are N shims sharing one daemon and one identity, never N embedded nodes. Its default surface is five **meta-tools**, not the raw mesh (that keeps the host's tool list small and the per-call schema accurate):

- `net_search_capabilities(query)` — substring discovery across the mesh; each row carries the credential status and provider set.
- `net_describe_capability(cap_id)` — full input schema + status.
- `net_invoke_capability(cap_id, arguments)` — pre-flight validates the arguments against the schema, checks consent, routes the nRPC call, returns the result.
- `net_list_pinned_capabilities()` and `net_request_pin(cap_id)`.

Discovery and description never imply invocation. A capability that carries credentials — or reports *no* credentials, since the demand side does not trust a wire-declared status — is **search/describe-only** until the operator allowlists it (`--allow-capability`) or approves a pin.

---

## Pinning as promotion

Pinning is the reliability *and* the consent mechanism, not a convenience. The model calls `net_request_pin(cap_id)`, which writes a **pending** request and returns instructions — it grants nothing. A human approves out of band:

```bash
net mcp pin approve <cap_id>     # or: reject / list
```

An approved pin is then **promoted to a first-class typed MCP tool** in the host's tool list, with its real input schema — restoring per-call schema accuracy and the host's own per-tool approval prompt — and it clears the shim's consent gate for that one capability, for that user profile, on that machine, and nothing wider. The wrapper's owner scope always wins on top. Two rules are absolute: **the model cannot approve its own future access** (consent happens outside the model loop), and the pin store is a per-user file, **owner-only `0600`**, written atomically under a cross-process lock so a stale snapshot can never resurrect a revoked approval. Promoted tool names are a pure function of the capability id, so approving or rejecting one pin never remaps a name a host cached onto a different capability.

---

## Holding the line — the security posture

The codename is the design. A bridge sits at the exact seam a confused-deputy attack wants: a host the user trusts, talking to a mesh that may carry other people's nodes. Every default is chosen to hold that line rather than assume the mesh is friendly.

- **Consent is fail-closed.** An empty policy gates *everything*. A wire-declared `credential_status` — including `none` — is never trusted across the demand-side boundary; the gate reloads the pin store per invoke, so an out-of-band approval takes effect immediately with no stale-snapshot window.
- **Owner scope gates both surfaces.** Describe and invoke both reject on the AEAD-verified origin, so a node outside the scope sees nothing in search *and* cannot invoke.
- **Invoke is at-most-once for credentialed tools.** A timeout does not prove the tool didn't run, so only an uncredentialed (duplicate-safe) tool retries a timed-out call; a credentialed or stateful one surfaces the timeout rather than re-running it, so a lost reply never turns into a duplicated issue or a double charge. The invoke deadline is generous (120s, overridable) while describe stays short, so a slow legitimate tool isn't killed at five seconds and a stateful one isn't silently repeated.
- **Cross-provider collapse and failover are opt-in, off by default.** Bridging the same tool from several nodes into one logical capability — and failing an invoke over between them — is powerful, but equivalence today is proven only from *wire-declared* attributes a peer controls (substitutability, credential status, public schema), with no proof the peer shares your owner identity. That verification waits on the permission system, so until it lands the safe default keeps every provider on its own node id; a single-owner mesh enables the feature explicitly with `net mcp serve --trust-equivalent-providers`. Collapse is additionally impossible across accounts by construction — the equivalence key folds the credential status, so a credentialed tool never merges.

Bridged tools carry `compat_tier: "mcp_bridge"`: request/response only — no streaming, artifacts, or migration. The bridge is the funnel, not the destination; the rich surface is native Net.

---

## The docs, rebuilt to lead with the worldview

v0.31 also lands the [documentation rebuild](../plans/DOCS_STRATEGY_PLAN.md). The public docs now lead with what Net *is for* before how it works: **Net is a discovery mesh for agentic capability** — agents find live capabilities across a trusted mesh, invoke them safely, observe what happened, and recover when work fails — with the latency-first encrypted mesh kept as the identity one layer down, not discarded. New `worldview/` pages sell the belief system (including the flagship "submitted is not completed" explainer — a `200 OK` is not work done — and an explicit "do NOT use Net when…" page); real-command `guides/` walk both bridge directions end to end; a single ten-step **SDK spine** runs across Rust / TypeScript / Python / Go / C with the Go/C asymmetry stated rather than faked; and **agent-operable briefs** (Goal / Files / Commands / Expected output / Acceptance / Pitfalls) serve the third reader — a coding agent — alongside the published `net-claude-skill`.

The load-bearing part is a **pre-flight claims audit**: no worldview claim shipped until the code cashed it. It corrected copy to the real command names, confirmed multi-hop discovery is genuinely shipped (bounded, not infinite), and pinned the `mcp_bridge` request/response tier on every page that mentions bridged tools. The root README and the homepage hero were reconciled to the same layered story, and no existing `concepts/` or `reference/` page was moved or dropped — additive only.

---

## The hardening pass — what the bridge review forced

A [dedicated code review](../misc/CODE_REVIEW_2026_07_04_MCP_BRIDGE.md) of the landed bridge ran ten independent finder angles and verified every candidate by reading the implicated path. The security core held — credentials stay in the child, the owner-scope gate covers both surfaces, consent is genuinely fail-closed — and the findings clustered exactly where a request/response bridge meets an unfriendly network. Each fix landed with a regression test; the crate suite, clippy, and `cargo doc -D warnings` are green.

The retry path was the headline: the same-node retry that covers a lost reply also re-ran credentialed, non-idempotent tools, so a timeout on a `create_issue` could duplicate the issue — now an invoke is at-most-once unless the tool is provably duplicate-safe, expressed as a typed `InvokeSafety` flag rather than a bare boolean. The reply to a wrapped server's own request was moved off the single stdout-draining reader so a full pipe can't wedge both directions, and that reader now caps line length so a flood can't exhaust memory. The pin store is created owner-only from the first byte (not chmod'd after a umask window), promoted-tool names were made independent of which *other* pins are approved, and the JSON-RPC id and provider-id round-tripping were tightened so an unusual host id or a hex-spelled node can't slip a wire from its identity. The one genuinely deferred item — verifying a failover target shares the primary's owner identity — is what the opt-in default above stands in for until the permission system can prove it.

---

## What's deferred (honestly)

- **Owner-identity verification for failover.** The opt-in `--trust-equivalent-providers` default is the interim guard; the deeper fix — proving a substitutable peer shares your root identity rather than merely a public contract — waits on the permission system, and the collapse/failover feature is inert until you turn it on.
- **Daemon-side consent engine.** Consent, pinning, and validation currently live shim-side. The Hermes native-integration track needs them daemon-side behind the SDK; that refactor is flagged, not slipped in here.
- **Remote / HTTP MCP, OAuth passthrough, `--public` exposure.** Stdio-only by design for v0.31 — it dodges the bulk of the spec churn — and public mesh exposure stays deferred behind the owner-only default.
- **Streaming / artifacts / migration over a bridged tool.** Structurally out of the `mcp_bridge` compat tier; these are native Net capabilities, and the docs say so on every page rather than implying the bridge grows into them.
- **A typed provider identifier.** A capability's provider node id is now canonicalized to one spelling so identity and routing agree, but it is still carried as a string; the fully-typed form is a later refinement.

---

## Breaking changes

v0.31 is **additive**. Nothing on the existing transport, fold, reliability, or SDK paths changed shape.

- **New adapter crate `net-mesh-mcp`** and **three new CLI commands** (`net wrap`, `net mcp serve`, `net mcp pin`). A node that never runs them is untouched — the bridge is entirely opt-in, and it depends on `net-mesh-sdk`, not the core crate.
- **One new public SDK primitive:** a node can read its own origin hash (the provider-side value the owner-scope gate keys on). Existing callers are unaffected.
- **No wire-format change.** Bridged capabilities ride the existing capability-announcement and nRPC surfaces; there is no new substrate message type in this release.

---

## How to upgrade

1. **Pull the release** — nothing changes unless you invoke the new commands. Existing bus, stream, nRPC, and persistence code behaves exactly as before.
2. **To publish a tool onto the mesh**, run `net wrap <name> --identity <id> -- <stdio MCP server cmd>`. Put the tool's secrets in `--env`; they stay in the child. Use a *stable* identity (not an ephemeral key) or owner-only scoping admits nobody.
3. **To consume the mesh from an MCP host**, run `net mcp serve` and add the one-line host config above. Discover with `net_search_capabilities`, approve a capability out of band with `net mcp pin approve <cap_id>`, and it promotes to a first-class typed tool.
4. **To enable cross-provider failover** — only on a mesh where every peer is your own — pass `net mcp serve --trust-equivalent-providers`. Leave it off on any multi-identity mesh.
5. **Everyone else** gets the new surfaces with no behavior change to existing paths.

---

## Dependency updates

The crate version bumps `0.30.0 → 0.31.0`, propagated across the CLI, deck, and SDK manifests. The new `net-mesh-mcp` adapter honors the SDK-only dependency doctrine: it pulls `net-mesh-sdk` plus crates already vendored in the workspace (`async-trait`, `bytes`, `fs2`, `futures`, `serde`, `serde_json`, `thiserror`, `tokio`; `toml` and `tempfile` for tests) — **no new third-party dependency enters the lockfile**, and a CI test fails the build if the adapter ever reaches for the core crate directly. The documentation rebuild is web-only (Next.js under `web/`) and touches no Cargo manifest.

---

Released 2026-07-04.

## License

See [LICENSE](../../LICENSE).
