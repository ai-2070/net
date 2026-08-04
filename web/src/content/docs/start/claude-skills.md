---
title: Build with coding agents
description: "Install optional Agent Skills that give coding agents Net-specific API context, examples, and source pointers."
---

# Build with coding agents

The Net Agent Skills give coding agents API context, runnable examples, known
binding differences, and links into the source tree. They reduce reliance on
surface similarities with messaging and payment libraries, but they do not replace
source inspection or tests for the exact mechanism being changed.

> These are skills _about_ Net. They don't install the library — that's [Install](/docs/start/install).

## The two skills

**`net-event-bus`** — Net as an event bus: pub/sub over the mesh, nRPC request/response, the MCP bridge (`net-mesh wrap` / `net-mesh mcp serve`), [organization capability auth](/docs/guides/private-capabilities) (`serve_org` / `mesh.org(..).call`), the gang-claim scheduler, and the RedEX / CortEX / Dataforts layers on top.

**`net-payments`** — x402-native payments: pricing a capability at discovery, signed quotes, the provider lifecycle engine (quote → verify → settle → bill), the caller-side pay-to-invoke flow, tiered on-chain verification, and spend policy.

Install both or just the one you need. Each is a directory containing a `SKILL.md` plus reference files that load on demand.

## Where skills live

Claude Code looks in two places:

- `~/.claude/skills/` — personal, available in every project on your machine.
- `<your-repo>/.claude/skills/` — project-scoped, checked in and shared with your team.

## Install

```bash
npx skills add ai-2070/net-claude-skill -g
```

The [`skills` CLI](https://github.com/vercel-labs/skills) detects which coding agents you have, asks which skills you want, and puts them where that agent looks — `~/.claude/skills/` for Claude Code, `~/.codex/skills/` for Codex. Windows included. Drop `-g` to install into the current project only.

To update to the latest version:

```bash
npx skills update -g
```

A few flags for when you don't want the prompts:

```bash
# both skills, Claude Code
npx skills add ai-2070/net-claude-skill --skill '*' -a claude-code -g
# just one skill
npx skills add ai-2070/net-claude-skill --skill net-payments -g
# every agent it supports
npx skills add ai-2070/net-claude-skill --skill '*' -a '*' -g
```

These are plain Agent Skills, so `-a '*'` covers Codex, Cursor, Copilot, Cline, and the rest. The CLI symlinks each agent's skills directory to one canonical copy, so one `npx skills update` refreshes them all — pass `--copy` if symlinks aren't an option.

## Give the agent source access for exact mechanisms

[`opensrc`](https://github.com/vercel-labs/opensrc) fetches a package's repository
source into a local cache:

```bash
npx -y opensrc@latest path ai-2070/net
```

## Skills installation without the CLI

A skill is just a directory containing a `SKILL.md`, so copying the folders in works:

```bash
git clone https://github.com/ai-2070/net-claude-skill.git /tmp/net-claude-skill
mkdir -p ~/.claude/skills
cp -R /tmp/net-claude-skill/net-event-bus ~/.claude/skills/
cp -R /tmp/net-claude-skill/net-payments  ~/.claude/skills/
```

On Windows, in PowerShell:

```powershell
git clone https://github.com/ai-2070/net-claude-skill.git $env:TEMP\net-claude-skill
$dest = "$env:USERPROFILE\.claude\skills"
New-Item -ItemType Directory -Force $dest | Out-Null
Copy-Item -Recurse "$env:TEMP\net-claude-skill\net-event-bus" "$dest\"
Copy-Item -Recurse "$env:TEMP\net-claude-skill\net-payments" "$dest\"
```

Swap `~/.claude/skills/` for `<your-repo>/.claude/skills/` to install into a single project, then commit the two directories. To hack on the skills locally, clone once somewhere permanent and symlink so `git pull` updates them in place:

```bash
git clone https://github.com/ai-2070/net-claude-skill.git ~/src/net-claude-skill
ln -s ~/src/net-claude-skill/net-event-bus ~/.claude/skills/net-event-bus
ln -s ~/src/net-claude-skill/net-payments ~/.claude/skills/net-payments
```

## Verify

Check the files landed:

```bash
ls ~/.claude/skills/net-event-bus/SKILL.md ~/.claude/skills/net-payments/SKILL.md
```

Then restart Claude Code and run `/skills` — **net-event-bus** and **net-payments** should be listed.

Skills load automatically when a request matches. To see one fire, ask for something Net-shaped:

> _"Wire up a Net publisher and subscriber over the mesh in TypeScript."_
>
> _"Price a Net capability with x402 and charge callers to invoke it."_

`net-event-bus` triggers on imports of `@net-mesh/sdk` or `net-sdk` and on phrases like _pub/sub with Net_, _nRPC_, _mesh RPC_, _RedEX_, _CortEX_, _Dataforts_, _gang scheduler_, _net-mesh wrap_, _serve_org_. `net-payments` triggers on `net-payments` / `net_payments` imports and on _price a capability_, _pay to invoke_, _x402_, _settle on Base/Solana/XRPL_, _spend limit_.

## Opensrc

The skills carry the public model, common binding surfaces, and examples. Consult
the source when the task depends on exact signatures, fields, state transitions, or
implementation behavior:

- an exact signature, a serialized field name, whether an enum has that variant;
- how something actually works — the drain path, framing, how verification escalates through its tiers;
- why observed behaviour differs from what you expected;
- whether a symbol exists in the binding you're writing, or in any binding at all;
- anything the skills don't cover, which for a protocol this size is most of it.

Opensrc resolves registry metadata to the repository rather than unpacking the published tarball, so `crates:net-mesh-sdk`, `@net-mesh/sdk` and `pypi:net-mesh-sdk` all land on the same directory, and Go and C come along with it. That's also why you get real TypeScript source: the npm package publishes only its built `dist`, while the checkout has `sdk-ts/src/`.

You don't need to teach the agent how to use it. Each skill ships a `source-access.md` covering when to reach for source (and when not to — the model comes from the skill, the mechanism from the tree), which tests to read first, how to root the paths the skill cites, and three things the checkout won't have:

- **Anything newer than the last release.** The checkout is the published tag, not `master`.
- **`bindings/node/index.d.ts`**, which is napi-generated and gitignored. The declaration site is the `#[napi]` attributes in `bindings/node/src/*.rs`.
- **`net-payments` on crates.io** — `opensrc path crates:net-payments` fails with _not found_. The crate is unpublished and lives in the repo; the Python and Node payment surfaces ship inside the core binding packages. That error is not a missing feature.

If you'd rather not add a tool, a shallow clone is equivalent:

```bash
git clone --depth 1 https://github.com/ai-2070/net /tmp/net
```

Every source path the skills cite is resolved against the repository in CI, so a citation that goes stale fails the build rather than sending an agent to a file that moved.

## Updating and removing

`npx skills update -g` updates to the latest version, as above. To uninstall:

```bash
npx skills remove net-event-bus net-payments -g
```

If you installed by hand, re-run the copy step to update — a symlinked clone needs only `git pull` — and remove with:

```bash
rm -rf ~/.claude/skills/net-event-bus ~/.claude/skills/net-payments   # personal
rm -rf .claude/skills/net-event-bus .claude/skills/net-payments       # project
```

## Next

The [Agent Briefs](/docs/agent-briefs) provide bounded tasks with explicit
acceptance checks: wrap an MCP server, build a recoverable capability, or generate
typed bindings.
Source and full file-by-file breakdown: [github.com/ai-2070/net-claude-skill](https://github.com/ai-2070/net-claude-skill).
