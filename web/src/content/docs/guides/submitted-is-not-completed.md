# Submitted Is Not Completed

Most systems answer the question "did the work happen?" with a status code. You
send a request, you get back:

```
200 OK
```

and everything downstream treats that as *done*. It isn't. `200 OK` means the
request was accepted. It does not mean the work completed, that derived state
updated, that the object is visible everywhere, that the transaction is
reversible, or that anyone could recover if a later step failed.

> **The button turned green. The work still didn't happen.**

Or, for the protocol-minded:

> **A response is not a workflow. `HTTP 200` is not a business invariant.**

This one idea is why an agent needs events, not just a status code — and it's the
cleanest way to understand why Net exists without first learning what an event bus
is.

## The two-column story

Consider an agent placing an order.

**Left — the request/response world.** The agent calls one endpoint:

```
POST /orders
→ 200 OK
"Submitted"
```

The green checkmark lights up. Then, sometime later, invisibly:

```
Invoice validation failed: missing reverse-charge note.
Order not created.
Manual review required.
```

Nothing errored at the call site. The `200` was true — the request *was* accepted.
But the work is broken, and the agent has already moved on, because the only
signal it ever got was a status code that described transport, not outcome.

**Right — the agentic-mesh world.** The agent discovers the capabilities it needs
and drives them as a sequence of observable facts:

```
agent discovers:  customer.lookup  vat.check  invoice.validate  order.create  order.update

invoice.validate  → failed: missing reverse-charge note
agent calls       order.update  (adds the note)
invoice.validate  → valid
order.create      → created
event trail       recorded
```

Same failure. Completely different outcome — because the failure was a **fact the
agent could see and act on**, not a silence behind a green button. The agent
recovered. Nothing needed a human, and nothing had to be reconciled later.

## The ladder of "it worked"

Hidden inside the word "success" are at least four distinct claims, each strictly
weaker than the next:

| Level | Claim | Who can honestly assert it |
|---|---|---|
| 1. **Transport accepted** | the request/event was accepted and routed | the sender |
| 2. **Delivered / durable** | a receiver got it, or it's on a replayable log | the transport, with durability opted in |
| 3. **Effect applied** | the downstream actor actually changed state | the actor — *after it verifies* |
| 4. **Invariant holds** | the effect is complete, visible everywhere, reconciled, reversible | the system of record, often later |

A `200 OK` lives at level 1, sometimes level 3. It is routinely *read* as level 4.
That gap — between "accepted" and "true everywhere" — is where ghost records,
silent partial failures, and haunted reconciliation live.

## Make each stage a fact

The fix is not a better status code. It's to stop collapsing a workflow into one
boolean and instead emit each stage as its own observable fact, named for what
actually happened, by whoever observed it. A payment makes the levels concrete —
the gap between them is *money that has or hasn't moved*:

```jsonc
// the checkout step observed only that it asked the gateway
{ "event": "payment.charge.requested", "order": "A-8123", "amount": 4200 }

// the gateway observed an authorization hold — NOT a settled payment
{ "event": "payment.authorized", "order": "A-8123", "auth_id": "auth_9f" }

// capture is a separate fact, emitted when it actually happens
{ "event": "payment.captured", "order": "A-8123" }

// a decline is its own fact with a reason — never the mere absence of success
{ "event": "payment.declined", "order": "A-8123", "reason": "insufficient_funds" }

// settlement (level 4) is observed later, by reconciliation against the bank feed
{ "event": "payment.settled", "order": "A-8123", "net": 4183 }
```

Ship the goods on `payment.captured`, not on `payment.authorized` — because
*authorized ≠ captured ≠ settled*. A consumer that treats the gateway's `200` as
"paid" ships against money that may never arrive. When each stage is a fact, a
partial failure is a first-class, subscribable event instead of a silent gap
behind a checkmark, and different consumers (fulfillment, finance) can each
compute their own notion of "done" from the same honest stream.

## Why this needs a mesh, not just an API

To live this way you need somewhere for those facts to flow — a place where
`payment.declined` and `invoice.validate.failed` are events an agent can subscribe
to, replay, and act on, across whatever machines the work is spread over. That is
the event bus at the center of Net, and it's why "discover a capability and invoke
it" is only half the story: the other half is **observing what the work actually
did**, so an agent can recover instead of trusting a green button.

- **Named events, past tense, at the observing layer** — `order.created`,
  `payment.declined` — never an imperative `doPayment` or an outcome-assuming
  `order.ok`.
- **Distinct outcomes are distinct events** — a failure is its own fact, carrying
  a reason, not the absence of a success event.
- **Success is a projection the consumer computes** over facts, not a boolean the
  producer stamps on the wire.

Next: [Using the Event Bus](/docs/guides/event-bus) — the place those facts flow.
For recovering when a step fails, see
[Recover a Failed Workflow](/docs/guides/recover-failed-workflow).
