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
                                 | interrupted{detail}
```

`completed` carries a reference to the result, for the same reason the brief
carries references to the input.

`interrupted` says the executor's outcome is not knowable from this
process — a restart interrupted the run — and only a configured catalog
([paid services](#paid-services)) ever produces one.

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

## Paid services

Free and paid are **provider configuration**, never something a caller
selects. `serve_a2a` above is the free path and is unchanged. The strict
sibling takes a catalog in which every service is *explicitly* one or the
other:

```rust
let serving = mesh
    .serve_a2a_configured(registry, executor, config)
    .await?;
// serving.handles — hold them, as above.
// serving.store   — the durable admission records (below).
```

Each catalog entry is a `Free(offer)` or a `Paid(offer)`, and the offer
carries the service id, its revision, the bounds a brief must fit, the
`net.pricing.terms@1` document for a paid service, and three retention
windows. Callers read the catalog with `describe_a2a(node)`.

**A paid service never degrades to free.** Each of these is a refusal to
start, not a warning:

| Misconfiguration | Serve-time refusal |
|---|---|
| `Paid` entry with no pricing terms | `MissingPricingTerms` |
| `Free` entry carrying pricing terms | `UnenforceablePricing` |
| any `Paid` entry with no payment gate, or no admission journal | `A2aPaidMisconfigured` |

A catalog of only `Free` entries needs neither a gate nor a journal — but
free means *no payment*, not *no policy*. Free services still run the
application preflight and still enforce capacity, so an oversized or
unauthorized brief is refused there too.

### Prepare, purchase, submit

The ordering is the whole design: everything that can refuse the work
happens **before a quote exists**, and the launch is claimed durably
**before the executor is spawned**.

| Step | What it does | Money |
|---|---|---|
| `prepare` | validates the brief, runs the provider's preflight, reserves capacity, mints the admission id | none — read-only on the money side |
| `purchase` | quotes against *that* reservation, runs spend policy, pays once, stores the evidence | the one charge |
| `submit` | sends the brief with its evidence; the provider redeems, claims the launch, then runs it | none |

`prepare` answers `busy` when the service is at `max_in_flight`. Nothing was
reserved and nothing was quoted, so it is safe to retry — and it is also how
a first call that is not yet routable to the provider reports itself.

The quote commits to the admission id and to a hash of the exact brief, so
the same proof presented under another reservation — or by another peer —
does not compute the provider's expected hash, and the redemption is
refused before anything runs. A retry of an identical submission returns
the original task rather than buying a second run.

The caller-side flow keeps a durable attempt per task, so a lost reply is
**resumed, never re-quoted**: the stored payload is re-sent and the engine
answers with the original verdict.

### Refusals

Payment and admission refusals are not exceptions on a background task,
and they are not the in-body rejection either. They arrive as the
`ERR_PAYMENT` application error carrying a human message *and* a
machine-readable failure schematic:

| Reason | Means | Retry? |
|---|---|---|
| `missing_quote` | paid submit with no quote header; the gate was never consulted | after paying |
| `binding_required` | a quote id alone; a task always needs the payer's signature | after signing |
| `binding_rejected` | the binding does not match the recorded purchase | no |
| `no_reservation` | the provider holds no admission record for this task | no — reconcile |
| `input_binding_mismatch` | the quote commits to different work than the brief | no |
| `admission_revoked` | the provider can no longer admit work that may already be paid | no — operator |
| `journal_unavailable` | the admission store refused a write, so nothing started | **yes**, same proof |

Everything non-financial stays where it always was, in-body as
`TaskAck { accepted: false }`: an unknown service, a stale revision, a
brief outside the bounds, `Busy` at capacity, and `Retired` once a result
has aged out.

### What a restart surfaces

A paid provider keeps a durable admission record per task, and the launch
ledger that record writes is never pruned automatically. A successor
process reads what it finds conservatively, so an interrupted run is
reported as ambiguous rather than silently repeated:

| Record found | `task_status` answers | An identical retry does |
|---|---|---|
| reserved, unexpired | `null` — nothing ran | redeems (idempotently) and launches once |
| reserved, expired | `null` | re-acquires capacity, or `Busy` — the gate is not called |
| paid, never launched | `interrupted{paid_not_started}` | resumes on the recorded payment, no second charge, launches once |
| launched, no outcome | `interrupted{outcome_unknown}` | acknowledges the task and **never relaunches** |
| revoked after payment | `interrupted{admission_revoked}` | refuses again; an operator resolves it |
| terminal | the recorded state, until retention | acknowledges the task |
| ledger only, result retired | `null` | `Retired` |

`interrupted` is the one state this path adds to the status wire, and only
a configured catalog produces it. Python and Node hand the status back as
JSON and pass it through; a Rust requester built before it existed cannot
decode a record carrying it.

Records in the last three rows are the operator's queue. They are **never
pruned automatically** — money moved, or may have — and an operator
resolving them is the only exit.

## Bindings

| Binding | Surface |
|---|---|
| Rust | `serve_a2a`, `submit_task`, `task_status`, `cancel_task` |
| Python | `serve_a2a(callback)`, `submit_task(...)`, `task_status(node, id)`, `cancel_task(node, id)` |
| Node / TypeScript | `serveA2a(executor, options?)`, `submitTask(...)`, `taskStatus(...)`, `cancelTask(...)` |
| Go | **Not available** — no A2A surface in the Go binding today |

The paid path is narrower, because a paid provider needs a payment engine
and a durable journal beside it:

| Binding | Paid surface |
|---|---|
| Rust | `serve_a2a_configured`, `describe_a2a`, `prepare_a2a`, `submit_task_paid` |
| Python | serve with `PaymentProvider.serve_a2a_configured`; buy with `CapabilityGateway.prepare_task` / `purchase_task` / `submit_task` |
| Node / TypeScript | **Requester only** — paid serving is deferred; free behavior unchanged |
| Go | **Not available** |

## See also

- [Agent Identity](/docs/concepts/agent-identity) — the delegation chain an enrolled agent carries
- [Tool Federation](/docs/concepts/tool-federation) — publishing tools other agents discover
- [Blob Storage (Dataforts)](/docs/guides/dataforts) — where brief and result refs point
- [Submitted Is Not Completed](/docs/guides/submitted-is-not-completed)
