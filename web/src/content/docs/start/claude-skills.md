# Claude Skills

If you're building against Net with a coding agent, install the Net Claude skills first. They're the reason the generated code is right instead of merely plausible.

Net looks like Kafka, NATS, or Redis Streams from the outside, and Net Payments looks like a dozen payment SDKs. The models underneath are different — no broker, hot subscribers, backpressure expressed as silence, every node a peer; payments that are non-custodial and never move money, only sign the commercial facts around an invocation. An agent working from surface familiarity will write integration code that compiles, runs, and is quietly wrong. The skills load the correct mental model and per-SDK templates before the agent writes a line.

> These are skills *about* Net. They don't install the library — that's [Install](/docs/start/install).

## The two skills

**`net-event-bus`** — Net as an event bus: pub/sub over the mesh, nRPC request/response, the MCP bridge (`net wrap` / `net mcp serve`), [organization capability auth](/docs/guides/private-capabilities) (`serve_org` / `mesh.org(..).call`), the gang-claim scheduler, and the RedEX / CortEX / Dataforts layers on top.

**`net-payments`** — x402-native payments: pricing a capability at discovery, signed quotes, the provider lifecycle engine (quote → verify → settle → bill), the caller-side pay-to-invoke flow, tiered on-chain verification, and spend policy.

Install both or just the one you need. Each is a directory containing a `SKILL.md` plus reference files that load on demand.

## Where skills live

Claude Code looks in two places:

- `~/.claude/skills/` — personal, available in every project on your machine.
- `<your-repo>/.claude/skills/` — project-scoped, checked in and shared with your team.

## Install

### Clone into your skills directory

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

### Install into a single project

From the root of the repo where you want the skills available to everyone working on it:

```bash
mkdir -p .claude/skills
git clone https://github.com/ai-2070/net-claude-skill.git /tmp/net-claude-skill
cp -R /tmp/net-claude-skill/net-event-bus /tmp/net-claude-skill/net-payments .claude/skills/
git add .claude/skills/net-event-bus .claude/skills/net-payments
git commit -m "Add Net Claude skills"
```

### Symlink to stay current

Clone once somewhere permanent and symlink, so `git pull` updates the installed skills in place:

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

`net-event-bus` triggers on imports of `@net-mesh/sdk` or `net-sdk` and on phrases like *pub/sub with Net*, *nRPC*, *mesh RPC*, *RedEX*, *CortEX*, *Dataforts*, *gang scheduler*, *net wrap*, *serve_org*. `net-payments` triggers on `net-payments` / `net_payments` imports and on *price a capability*, *pay to invoke*, *x402*, *settle on Base/Solana/XRPL*, *spend limit*.

## Updating and removing

If you cloned or copied, re-run the copy step (or `git pull` in the clone, then copy). If you symlinked, `git pull` is enough.

To uninstall:

```bash
rm -rf ~/.claude/skills/net-event-bus ~/.claude/skills/net-payments   # personal
rm -rf .claude/skills/net-event-bus .claude/skills/net-payments       # project
```

## Next

The skills are the standing reference an agent keeps loaded. The [Agent Briefs](/docs/agent-briefs) are the complement: single, checkable tasks you hand an agent once — wrap an MCP server, build a recoverable capability, generate typed bindings. Skill = what you keep loaded; brief = what you run once.

Source and full file-by-file breakdown: [github.com/ai-2070/net-claude-skill](https://github.com/ai-2070/net-claude-skill).
