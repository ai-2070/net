---
title: Claude Skills
description: "If you're building against Net with a coding agent, install the Net Claude skills first."
---
# Claude Skills

If you're building against Net with a coding agent, install the Net Claude skills first. They're the reason the generated code is right instead of merely plausible.

Net looks like Kafka, NATS, or Redis Streams from the outside, and Net Payments looks like a dozen payment SDKs. The models underneath are different — no broker, hot subscribers, backpressure expressed as silence, every node a peer; payments that are non-custodial and never move money, only sign the commercial facts around an invocation. An agent working from surface familiarity will write integration code that compiles, runs, and is quietly wrong. The skills load the correct mental model and per-SDK templates before the agent writes a line.

> These are skills *about* Net. They don't install the library — that's [Install](/docs/start/install).

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
npx skills add ai-2070/net-claude-skill --skill '*' -a claude-code -g   # both skills, Claude Code
npx skills add ai-2070/net-claude-skill --skill net-payments -g         # just one skill
npx skills add ai-2070/net-claude-skill --skill '*' -a '*' -g           # every agent it supports
```

These are plain Agent Skills, so `-a '*'` covers Codex, Cursor, Copilot, Cline, and the rest. The CLI symlinks each agent's skills directory to one canonical copy, so one `npx skills update` refreshes them all — pass `--copy` if symlinks aren't an option.

### Without the CLI

A skill is just a directory containing a `SKILL.md`, so copying the folders in works:

```bash
git clone https://github.com/ai-2070/net-claude-skill.git /tmp/net-claude-skill
mkdir -p ~/.claude/skills
cp -R /tmp/net-claude-skill/net-event-bus /tmp/net-claude-skill/net-payments ~/.claude/skills/
```

On Windows, in PowerShell:

```powershell
git clone https://github.com/ai-2070/net-claude-skill.git $env:TEMP\net-claude-skill
New-Item -ItemType Directory -Force "$env:USERPROFILE\.claude\skills" | Out-Null
Copy-Item -Recurse "$env:TEMP\net-claude-skill\net-event-bus" "$env:USERPROFILE\.claude\skills\"
Copy-Item -Recurse "$env:TEMP\net-claude-skill\net-payments" "$env:USERPROFILE\.claude\skills\"
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

> *"Wire up a Net publisher and subscriber over the mesh in TypeScript."*
>
> *"Price a Net capability with x402 and charge callers to invoke it."*

`net-event-bus` triggers on imports of `@net-mesh/sdk` or `net-sdk` and on phrases like *pub/sub with Net*, *nRPC*, *mesh RPC*, *RedEX*, *CortEX*, *Dataforts*, *gang scheduler*, *net-mesh wrap*, *serve_org*. `net-payments` triggers on `net-payments` / `net_payments` imports and on *price a capability*, *pay to invoke*, *x402*, *settle on Base/Solana/XRPL*, *spend limit*.

## Give the agent the source too

The skills carry the mental model, the per-binding surface, and runnable examples. What they can't carry is all of Net — so when the agent needs an exact signature, a field name, or "does this enum have that variant," it either has the source or it guesses.

[`opensrc`](https://github.com/vercel-labs/opensrc) is a small tool that fetches a package's real source into a local cache for exactly this purpose:

```bash
npx -y opensrc@latest path ai-2070/net
```

That prints an absolute path to a cached checkout — about 46 MB, fetched once, returned instantly after that. **One fetch covers all five bindings.** opensrc resolves registry metadata to the repository rather than unpacking the published tarball, so `crates:net-mesh-sdk`, `@net-mesh/sdk` and `pypi:net-mesh-sdk` all land on the same directory, and Go and C come along with it. That's also why you get real TypeScript source: the npm package publishes only its built `dist`, while the checkout has `sdk-ts/src/`.

You don't need to teach the agent how to use it. Each skill ships a `source-access.md` covering when to reach for source, how to root the paths the skill cites, and three things the checkout won't have:

- **Anything newer than the last release.** The checkout is the published tag, not `master`.
- **`bindings/node/index.d.ts`**, which is napi-generated and gitignored. The declaration site is the `#[napi]` attributes in `bindings/node/src/*.rs`.
- **`net-payments` on crates.io** — `opensrc path crates:net-payments` fails with *not found*. The crate is unpublished and lives in the repo; the Python and Node payment surfaces ship inside the core binding packages. That error is not a missing feature.

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

The skills are the standing reference an agent keeps loaded. The [Agent Briefs](/docs/agent-briefs) are the complement: single, checkable tasks you hand an agent once — wrap an MCP server, build a recoverable capability, generate typed bindings. Skill = what you keep loaded; brief = what you run once.

Source and full file-by-file breakdown: [github.com/ai-2070/net-claude-skill](https://github.com/ai-2070/net-claude-skill).
