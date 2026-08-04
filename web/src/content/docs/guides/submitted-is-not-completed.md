---
title: Submitted is not completed
description: "Separate transport acceptance, provider execution, external effect, and verified outcome."
---
# Submitted is not completed

A response describes what the responding layer knows. It does not automatically
prove that every downstream effect completed.

```text
200 OK
```

Depending on the API, that may mean the server accepted a request, queued work, or
finished a handler. It does not by itself prove that derived state is visible,
money settled, an external system changed, or a business invariant holds.

## Name the stage that happened

Consider an order workflow:

```text
order request accepted
invoice validation failed
order was not created
manual review opened
```

The first statement can be true while the final outcome is still incomplete. A
caller that receives only `accepted` cannot infer the later stages.

An evented workflow records each observation separately:

```text
invoice.validation_failed  reason=missing_reverse_charge_note
order.updated               field=reverse_charge_note
invoice.validated
order.created               order=A-8123
```

The application can now show the actual state, choose a recovery action, and retain
an event trail. Automation is optional; the important change is that failure is a
visible fact rather than missing information behind an earlier acknowledgement.

## The levels of success

| Level | What is known | Typical source |
|---|---|---|
| **Accepted** | a request or event entered the receiving layer | transport or API |
| **Delivered or durable** | a receiver got it, or a replayable log holds it | transport or log |
| **Executed** | the provider ran the handler and returned an outcome | provider |
| **Effect observed** | a named external state change was observed | system that owns the effect |
| **Verified** | the required postcondition was checked independently | verifier or system of record |

A capability should report the narrowest level it can prove. `SUCCESS` should mean
the postcondition defined by that capability was verified, not merely that an RPC
returned without a transport error.

## Model each transition as a fact

Payments make the boundary concrete:

```jsonc
{ "event": "payment.charge_requested", "order": "A-8123", "amount": 4200 }
{ "event": "payment.authorized",       "order": "A-8123", "auth_id": "auth_9f" }
{ "event": "payment.captured",         "order": "A-8123" }
{ "event": "payment.declined",         "order": "A-8123", "reason": "insufficient_funds" }
{ "event": "payment.settled",          "order": "A-8123", "net": 4183 }
```

`authorized`, `captured`, and `settled` are different observations. Fulfillment may
act on capture while finance waits for settlement. Both consumers can derive their
own completion rule from the same facts.

The same rule applies to tools, jobs, file movement, device operations, and agent
workflows:

- Name events in the past tense at the layer that observed them.
- Represent different outcomes explicitly rather than treating failure as the
  absence of success.
- Define which evidence establishes the capability's final postcondition.
- Treat ambiguous external effects as unknown until reconciled.
- Retry only when the operation is known not to have executed or its idempotency
  contract makes another attempt safe.

## Where Net helps

Net provides typed capability invocation, event streams, durable logs, task state,
and artifact movement under one identity model. Applications decide which stages
to publish and what evidence counts as completion.

The mesh does not turn an acknowledgement into proof. It gives the participants a
way to carry the facts needed to make that distinction explicit.
