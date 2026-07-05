# `net` — Net mesh plugin for Hermes

A first-party [Hermes](https://github.com/NousResearch/hermes-agent) plugin that
lets an agent reach capabilities running on your **other machines** — the ones
you published there with `net wrap` — as five `net_*` tools, with local consent
and pin approval.

It embeds a **first-class Net node in-process** via `net-mesh-sdk` (no daemon,
no MCP shim): the node joins your mesh, and the tools drive the SDK's
consent-gated capability gateway and the machine-shared pin store. All
consent / validation / pin logic lives once in the Rust SDK — the plugin is a
thin, public-API-only view over it.

This is the native tier of `HERMES_INTEGRATION_PLAN.md` Phase 1. (The zero-code
tier — pointing Hermes's stock MCP client at `net mcp serve` — needs no plugin
at all; this plugin is what native integration adds.)

## Tools (toolset `net`)

| Tool | What it does |
|---|---|
| `net_search_capabilities` | Search the **mesh** index across your nodes. **Not** Hermes's local `tool_search` — the descriptions say so explicitly, so the model doesn't misroute. |
| `net_describe_capability` | Full input schema + credential status + whether it needs approval. |
| `net_invoke_capability` | Invoke through the consent gate. Returns structured `status`: `ok` / `requires_approval` / `validation_error` / `denied` / `not_found` / … — never raises. |
| `net_list_pinned_capabilities` | Approved + pending pins from the shared store. |
| `net_request_pin` | Records a **pending** approval request (grants nothing; a human approves out of band). |

## Enable

```yaml
# ~/.hermes/config.yaml
plugins:
  enabled: [net]
```

Then join your mesh via environment (see `node.py` for the full list):

| Var | Meaning |
|---|---|
| `NET_MESH_PSK` | 64-hex pre-shared key of your mesh (required to join a real mesh; unset ⇒ isolated dev node — tools still load, search is empty). |
| `NET_MESH_PEERS` | JSON `[{"addr","pubkey","node_id"}]` of the machines running `net wrap`. |
| `NET_MESH_IDENTITY_SEED` | 32-byte hex seed for a stable node identity. |
| `NET_MESH_PIN_STORE` | Pin-store path; defaults to the machine-shared file `net mcp pin` uses. |

A capability is invocable only once its pin is **approved** — from anywhere on
the machine (`net mcp pin approve <cap_id>`, another SDK client, or the shim).
Approved anywhere ⇒ approved everywhere, because it's one locked store.

## Install / distribution

Installed via Hermes's clone-install: `hermes plugins install hermes-pro/net`.
That mirror is a **build output** of this directory in the Net main tree
(`net/crates/net/integrations/hermes/`), not a dev repo — source-of-truth lives
here; CI syncs release-tagged builds one-way, re-pinning `net-mesh-sdk` in
`requirements.txt` per release.

## Known follow-ups

- **Meta-tools always-load.** The plan wants the five meta-tools exempt from
  `tool_search` deferral. Hermes keys never-defer off a tool *name* being in
  `toolsets._HERMES_CORE_TOOLS` (a core file) — there's no per-tool flag — so
  making them always-load needs a config mechanism or an upstream public hook
  (never a core patch, per H6). Until then they are ordinary deferrable plugin
  tools; five small defs is cheap.
- **Pin promotion (Phase 2).** Approved pins should register as first-class
  typed tools, diffed like `tools/mcp_tool.py`. The SDK has no pin-change
  subscription yet, so this will poll `AsyncPinStore.approved()` on a TTL until
  a watch API lands.
- **Delegated identity (Phase 3)**, streaming/folds (Phase 6), etc. — see the
  plan.

## Tests

`tests/` load the plugin package (under a non-`net` name to avoid colliding
with the `net` wheel) and exercise registration + the async handlers against an
isolated node. Run from `net/crates/net/bindings/python` with the built wheel:

```sh
.venv/Scripts/python -m pytest ../../integrations/hermes/tests -q
```

`tests/real_hermes_loader_check.py` is a **manual** validation (not collected
by pytest) that drives the plugin through Hermes's *real* `PluginContext` ->
`tools.registry` -> `get_definitions` in a Hermes checkout — see its docstring
for the recipe (needs an ABI-matched `net-mesh` wheel). Verified passing: the
five tools register into the real registry and survive Hermes's model-facing
assembly, and the embedded node builds + tears down in Hermes's interpreter.
