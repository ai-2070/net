---
title: Agent-to-Agent Task Handoff
description: One agent hands a long job to another that doesn't share its memory, keeps working, and can cancel mid-run — with the other side demonstrably stopping.
---

# Agent-to-Agent Task Handoff

Discovery and invocation cover "call a capability and get an answer." A2A
covers something else: **one agent hands a long job to a different agent, keeps
working, and can cancel it mid-run — with the other side demonstrably
stopping.**

The distinction that decides whether you want this:

| You want | Use |
|---|---|
| A short call whose answer you need before continuing | [Discover and invoke](/docs/guides/discover-and-invoke) — a capability |
| Sequential work inside one agent's own context | Direct capabilities, not A2A |
| **Parallelism** — a long job runs elsewhere while you continue | **A2A** |

A2A is for parallelism, and the reason it exists as a separate surface is that
the executing agent **does not share your memory.** Asking another agent to
continue your work is briefing a colleague who wasn't in the room — so the
protocol makes that explicit rather than pretending otherwise.

## The brief carries references, not context

A `TaskBrief` carries the job plus the context the executor needs **as
artifact references**, not inlined content. That's deliberate: the other agent
has its own memory, and inlining would imply a shared context that doesn't
exist. Put the context in [Dataforts](/docs/guides/dataforts) and hand over the
refs.

## Lifecycle

```text
requested → accepted → running → completed{ref} | failed | cancelled
```

`completed` carries a reference to the result, for the same reason the brief
carries references to the input.

## Executor side

```rust
let handles = mesh.serve_a2a(registry, executor)?;
// Hold the handles for as long as this agent should accept tasks —
// dropping them unregisters the services.
```

`serve_a2a` registers three services at once: accept a brief (spawning the
executor), answer status, and cancel. Rollback is automatic — if registering
the third fails, the first two unregister as the error returns, so you never
end up half-serving.

A malformed brief does **not** fail out of band. It answers a
`TaskAck { accepted: false }` that the requester reads, so a bad submission is
a value on the happy path rather than an exception on a background task.

The node must be `start()`ed before serving.

## Requester side

```rust
let ack = mesh.submit_task(target, brief).await?;      // TaskAck
let rec = mesh.task_status(target, &task_id).await?;   // Option<TaskRecord>
let stopped = mesh.cancel_task(target, &task_id).await?;  // bool
```

`task_status` returns `Option` — `None` means the executor has no record of
that id, which is different from "the task failed." `cancel_task` returns
whether the cancel took effect, and the executor observes it through a
`CancelToken` rather than being killed, so it can stop cleanly.

## Bindings

| Binding | Surface |
|---|---|
| Rust | `serve_a2a`, `submit_task`, `task_status`, `cancel_task` |
| Python | `serve_a2a(callback)`, `submit_task(...)`, `task_status(node, id)`, `cancel_task(node, id)` |
| Node / TypeScript | `serveA2a(executor, options?)`, `submitTask(...)`, `taskStatus(...)`, `cancelTask(...)` |
| Go | **Not available** — no A2A surface in the Go binding today |

## See also

- [Agent Identity](/docs/concepts/agent-identity) — the delegation chain an enrolled agent carries
- [Tool Federation](/docs/concepts/tool-federation) — publishing tools other agents discover
- [Blob Storage (Dataforts)](/docs/guides/dataforts) — where brief and result refs point
- [Submitted Is Not Completed](/docs/guides/submitted-is-not-completed)
