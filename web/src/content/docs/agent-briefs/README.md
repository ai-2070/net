# Agent Briefs

Most docs are written for a human to read. These are written for an **agent to
execute** — a coding agent (Claude Code or similar) that edits files, runs
commands, checks output, and verifies its own work. Each brief is a single,
self-contained task with a fixed shape:

> **Goal · Prerequisites · Steps (files + commands) · Expected output · Verify
> (acceptance) · Pitfalls**

If a step's expected output doesn't appear, the agent stops and reports — it does
not guess forward. That's the difference between a brief and a tutorial: a brief is
*checkable*.

## The standing reference: the Net Claude Code skill

Before running a brief, install the **Net Claude Code skill** — the standing,
always-loaded reference an agent uses while building against Net:

> **[github.com/ai-2070/net-claude-skill](https://github.com/ai-2070/net-claude-skill)**

The skill is the *reference* (the mental model, the per-SDK API templates, the
migration gotchas, the event-representation doctrine); a brief here is a *one-shot
task* that uses that reference. Skill = what you keep loaded; brief = what you run
once. They compose: point your agent at the skill, then hand it a brief.

## The briefs

1. **[Wrap and Use an MCP Server](/docs/agent-briefs/wrap-and-use-an-mcp-server)** —
   put an existing MCP tool on the mesh and invoke it from an agent.
2. **[Build a Recoverable Capability](/docs/agent-briefs/build-a-recoverable-capability)** —
   serve a native capability and prove it survives a provider failure.
3. **[Generate Typed Tool Bindings](/docs/agent-briefs/generate-typed-tool-bindings)** —
   turn a discovered tool into typed, compile-checked client code.

Every command in these briefs is pinned to the shipped CLI and SDK on this branch.
Where a step depends on a running mesh, the brief says so and how to satisfy it.
