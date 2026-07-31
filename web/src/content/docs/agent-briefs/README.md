---
title: Agent Briefs
description: "Task briefs written for a coding agent to execute rather than for a human to read, each with a verifiable end state."
---

# Agent Briefs

These briefs are task contracts for coding agents that edit files, run commands,
inspect output, and verify an end state. Each brief uses the same structure:

> **Goal · Prerequisites · Steps (files + commands) · Expected output · Verify
> (acceptance) · Pitfalls**

If expected output does not appear, the agent should stop at that step and report
the mismatch rather than infer success.

## Optional Net skill

The **Net Claude Code skill** provides additional Net-specific API context and
examples:

> **[github.com/ai-2070/net-claude-skill](https://github.com/ai-2070/net-claude-skill)** —
> setup steps in [Claude Skills](/docs/start/claude-skills)

The briefs are self-contained. The skill is useful for broader implementation work
or when the brief leads into APIs not covered by its steps.

## The briefs

1. **[Wrap and Use an MCP Server](/docs/agent-briefs/wrap-and-use-an-mcp-server)** —
   put an existing MCP tool on the mesh and invoke it from an agent.
2. **[Build a Recoverable Capability](/docs/agent-briefs/build-a-recoverable-capability)** —
   serve a native capability and prove it survives a provider failure.
3. **[Generate Typed Tool Bindings](/docs/agent-briefs/generate-typed-tool-bindings)** —
   turn a discovered tool into typed, compile-checked client code.

Commands and expected output target the CLI and SDK documented on this branch.
Where a step depends on a running mesh, the brief states that prerequisite.
