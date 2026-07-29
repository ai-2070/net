# Agent-to-Agent Handoff, Delegated Identity, Device Enrollment

Three surfaces that come up when the thing you're integrating is an **agent**
rather than a service. All three ship in Rust, Python and Node/TypeScript.
**Go has none of them** — if the user is on Go, say so rather than generating
a call that doesn't exist.

---

## When A2A is the wrong answer

Reach for A2A only when you want **parallelism**: a long job runs on another
agent while this one keeps working, and can be cancelled mid-run.

| Situation | Use |
|---|---|
| Short call, need the answer before continuing | `nrpc.md` — a capability |
| Sequential work in one agent's own context | Direct capabilities, not A2A |
| Long job runs elsewhere, you continue | **A2A** |

The reason it's a separate surface: **the executor does not share your
memory.** Handing work to another agent is briefing a colleague who wasn't in
the room. The protocol makes that explicit instead of pretending otherwise —
which is why the brief carries *references*, not inlined context.

## The brief carries refs, not content

`TaskBrief` = the job + the context the executor needs, as **Datafort artifact
refs**. Put the context in Dataforts (`dataforts.md`), hand over the refs. If
you find yourself inlining a large context blob into a brief, you're modelling
a shared memory that doesn't exist.

`TaskState`:

```text
requested → accepted → running → completed{ref} | failed | cancelled
```

`completed` carries a ref for the same reason the brief does.

## Executor side

```rust
// Rust — node must be start()ed first.
let handles = mesh.serve_a2a(registry, executor)?;   // Vec<ServeHandle>
// Hold the handles. Dropping them unregisters the services.
```

```python
handle = mesh.serve_a2a(callback)          # A2aServeHandle
```

```typescript
const handle = await mesh.serveA2a(executor, options);   // A2aServeHandle
```

`serve_a2a` registers **three** services at once — submit, status, cancel.
Rollback is automatic: if the third fails to register, the first two
unregister as the error returns. You never end up half-serving.

**A malformed brief is not an out-of-band failure.** It answers a
`TaskAck { accepted: false }` that the requester reads. Don't wrap `serve_a2a`
in a handler that expects exceptions for bad input — the rejection is a value
on the normal path.

## Requester side

```rust
let ack: TaskAck              = mesh.submit_task(target, brief).await?;
let rec: Option<TaskRecord>   = mesh.task_status(target, &task_id).await?;
let stopped: bool             = mesh.cancel_task(target, &task_id).await?;
```

```python
task_id = mesh.submit_task(target_node_id, prompt, context_refs, tags)
status  = mesh.task_status(target_node_id, task_id)    # Optional[str]
stopped = mesh.cancel_task(target_node_id, task_id)    # bool
```

```typescript
const taskId  = await mesh.submitTask(targetNodeId, prompt, contextRefs, tags);
const status  = await mesh.taskStatus(targetNodeId, taskId);   // string | null
const stopped = await mesh.cancelTask(targetNodeId, taskId);   // boolean
```

Two traps:

- **`task_status` returning `None` / `null` means "no record of that id,"**
  not "the task failed." Don't collapse them.
- **Cancel is cooperative.** The executor observes a `CancelToken` and stops
  cleanly; it is not killed. `cancel_task` returns whether the cancel took
  effect. An executor that never checks its token will not stop, so if you are
  writing the executor, check the token in your loop.

---

## Delegated agent identity

**Two different things are called "delegation."** Permission tokens
(`mesh.md`, `error-codes.md`) delegate *authority to do something*.
`DelegationChain` derives *who someone is* — a child identity acting for a
parent principal. A token says "you may"; a chain says "you are acting for."
Don't reach for one when the user means the other.

```rust
let child_seed = derive_child_seed(&parent_seed, "gateway-eu");
```

Deterministic: same parent + same label always yields the same child, so an
agent identity survives a restart with no second private key stored anywhere.
The parent seed never leaves its holder.

`DelegationChain` builds the signed path — `derive_gateway`, `derive_device`,
`extend_delegate`, `extend_to_subagent` — and reads back with `verify()`,
`subjects()`, `leaf()`, `root()`, `expires_at()`, `len()`.

**Chains expire.** An agent identity is not permanent by default. And a chain
that verifies structurally can still be revoked — `RevocationRegistry` is what
a verifier consults. Structural verification alone is not authorization.

## Device enrollment

`invite → join → approve`, with no private key ever pasted or transmitted.

- `InviteToken::mint(root, rendezvous, ttl)` — operator mints a short-lived
  invite. Encodes to a string for out-of-band transport; carries
  `root_fingerprint()` so the joiner can confirm which root it's joining.
- `JoinRequest::create(..)` — the device generates **its own** keypair and
  self-signs. `verify_self_signature()` proves it wasn't tampered with.
- Approve — the root issues the chain.

`fingerprint(entity)` is the short human-comparable form. The out-of-band
fingerprint check by a human is the step that matters; if the user's design
skips it, the invite is only as good as the channel it travelled over.

Invites expire (`is_expired(now)`); failures are `EnrollmentError`.

---

## Cross-references

- `nrpc.md` — the typed transport A2A rides on; also tool calling, which is
  the *short-call* alternative to a task handoff.
- `dataforts.md` — where brief and result refs point.
- `capabilities.md` — how an agent is found in the first place.
- `org.md` — when the handoff crosses an organization boundary.
- `mcp.md` — bridging external MCP tools into the same discovery plane.

## Further reading

- [Agent-to-Agent Task Handoff](https://ai2070.net/docs/guides/agent-to-agent)
- [Agent Identity](https://ai2070.net/docs/concepts/agent-identity)
- [Tool Federation](https://ai2070.net/docs/concepts/tool-federation)
