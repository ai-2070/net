# When to Use Net (and When Not To)

Net is infrastructure with discipline, not "use us for everything." The fastest
way to trust a tool is to know where it *doesn't* belong — so this page is as
explicit about the wrong cases as the right ones.

The short rule:

> **Use MCP/HTTP when the call is the whole story. Use Net when discovery and work
> state matter.**

## Use Net when

- **Agents need to discover capabilities dynamically** — the set of available
  tools, models, or services changes at runtime and isn't a fixed config.
- **Tools live across multiple machines or organizations** — the work you need
  isn't on the box you're running on.
- **Credentials must stay local** — the node that holds a secret should run the
  work; the caller should never see the credential.
- **Agent-to-agent communication matters** — not just an app calling one API, but
  peers coordinating.
- **Capabilities need typed schemas** — callers should get typed requests and
  responses, discovered at runtime.
- **Work has live state** — failures, retries, artifacts, or streams that a single
  status code can't express (see
  [Submitted Is Not Completed](/docs/worldview/submitted-is-not-completed)).
- **Provider availability changes over time** — the GPU is busy, the model loads
  and unloads, the service moves between hosts.
- **Resources like GPUs should be exposed as capabilities** — discovered and
  matched by what they can do, not addressed by a hostname.
- **Business workflows need visibility and recovery** — you need to know which
  step failed and be able to replay it, not reconcile it manually tomorrow.
- **Payment / usage / account events may attach later** — the substrate should let
  you grow those in without re-platforming.

## Do NOT use Net when

- **One API call solves the problem.** If the whole task is "call this endpoint,
  get an answer," you don't need discovery or work state.
- **A single server and database are enough.** No mesh, no distribution, no
  presence — just use them.
- **HTTP/gRPC request-response is sufficient.** If there's no live state, no
  discovery, and no recovery to model, the transport you have is fine.
- **MCP directly is enough** because your tools are hand-wired and local. If
  nothing needs to be *discovered*, MCP alone is simpler
  ([MCP vs Net](/docs/worldview/mcp-vs-net)).
- **A normal queue or job runner already matches the workflow.** If you have a
  fixed producer, a fixed consumer, and a broker you're happy operating, Net's
  discovery and presence aren't buying you anything.
- **You don't need discovery, presence, policy, artifacts, streams, or recovery.**
  If none of those words describe your problem, Net is overhead.

## Why the "no" list matters

Every one of those "do not" cases is a place where Net would add moving parts
without adding leverage. Reaching for a discovery mesh when a single HTTP call
would do is the same mistake as standing up Kafka to move ten messages a day. The
value of Net shows up precisely when the world is *distributed, changing, and
stateful* — and when it isn't, the honest answer is to use the simpler thing.

If you're not sure which side of the line you're on, ask: *does anything need to
be discovered at runtime, and does the work have state I have to observe or
recover?* Two nos means you don't need Net yet. One yes is worth reading on.

Next: [MCP vs Net](/docs/worldview/mcp-vs-net).
