# Event Representation — Facts, Not `200 OK`

Read this when the user is deciding **what an event on the bus should assert** — naming events, choosing between "one status event" vs. "a sequence of events," or when their event shape carries a success/acknowledgement meaning ("`request.ok`", "`write.done`", "`delivered: true`") instead of stating a fact.

`payloads.md` covers the *form* of an event (size, encoding, JSON shape). This file covers its *meaning*: what the event is allowed to claim happened. Getting the form right and the meaning wrong still produces broken systems — the quiet kind, where nothing errors and everything is subtly false.

---

## The one line to burn in

> **Transport success is not application success. Application success is not business success.**

Or, shorter:

> **`HTTP 200` is not a business invariant.**

An event on the Net bus is a *fact that occurred*, observed at *one specific layer* of the stack. It is not an acknowledgement that an operation succeeded end-to-end. The instant you shape an event as "the operation is OK," you have baked a claim into the wire that the producer usually cannot back up.

## The ladder of "it worked"

At least four distinct claims hide inside a single "success," each strictly weaker than the next:

| Level | Claim | Who can honestly assert it |
|---|---|---|
| 1. **Transport accepted** | the event was accepted for fan-out; bytes were routed | the producer / Net (a `Receipt`) |
| 2. **Delivered / durable** | a subscriber received it, or it is on a replayable log | opt-in: `Reliability::Reliable`, app-level ack, or RedEX/adapter |
| 3. **Effect applied** | the downstream actor actually changed state (a row written, a valve moved, a balance debited) | the downstream actor — *after it verifies* |
| 4. **Invariant holds** | the effect is complete, visible everywhere, reversible, reconciled, agreed-upon | the system of record, often later |

Net's own primitives sit at levels 1–2. A `Receipt` from `emit`/`publish` is level 1 — it confirms the event was accepted into the local ring buffer for fan-out, **not** that any subscriber processed it, and certainly not that any downstream effect landed (`concepts.md` § Publisher, § Backpressure). Levels 3–4 are outside anything Net can observe.

**The rule:** an event may only assert a level the producer has actually observed. A component that merely *issued a request* has observed level 1 of its own action. It has not observed the effect. So it may emit `valve.open.requested` — never `valve.opened`. Only the actor that observes the valve physically open (level 3) may emit `valve.opened`.

## The ladder in one domain — a payment

Payments make the four levels concrete, and make the cost of collapsing them visceral: the gap between levels is *money that has or hasn't moved*.

| Level | Payment fact | What a `200` here does **not** mean |
|---|---|---|
| 1. accepted | the checkout service accepted the charge request | the card was charged |
| 2. delivered / durable | the charge event is on the log / reached the gateway | the bank approved it |
| 3. effect applied | the gateway **authorized** (a hold), then later **captured** (a debit) | the funds have settled |
| 4. invariant holds | the payment **settled** and reconciles against the ledger and the bank feed | — (the only level the business can bill against) |

The classic loss: a checkout service emits `payment.ok` the instant the gateway returns `200 Authorized`, a fulfillment consumer reads that as "paid," and ships the goods. But *authorized ≠ captured ≠ settled*. The hold expires, the capture fails, or the settlement reverses — and goods left the building against money that never arrived. Nothing errored; the `200` was true at level 3-authorize and false at level 4.

Represent each stage as the fact whoever observed it can honestly assert:

```jsonc
// the checkout service observed only that it asked the gateway
{ "event": "payment.charge.requested", "order": "A-8123", "amount": 4200, "currency": "USD", "idempotency_key": "chg_A-8123" }

// the gateway adapter observed an authorization hold — NOT a settled payment
{ "event": "payment.authorized", "order": "A-8123", "auth_id": "auth_9f", "amount": 4200 }

// ...capture is a separate fact, emitted when it actually happens
{ "event": "payment.captured", "order": "A-8123", "auth_id": "auth_9f", "captured": 4200 }

// ...a decline/failure is its own fact with a reason, never the mere absence of the above
{ "event": "payment.declined", "order": "A-8123", "reason": "insufficient_funds" }

// ...settlement (level 4) is observed later, by the reconciliation job against the bank feed
{ "event": "payment.settled", "order": "A-8123", "batch": "2026-07-04-eu", "net": 4183 }
```

Fulfillment subscribes to `payment.captured` — not `payment.authorized`, and definitely not a generic `payment.ok` — and folds the facts into "safe to ship?" itself. Finance subscribes to `payment.settled` for revenue recognition. Two consumers, two different notions of "success," both computed from the same honest stream — impossible if the producer had collapsed it into one boolean.

The `idempotency_key` is the other half of doing this right: because the bus is at-least-once and a producer may retry, the key lets a consumer dedup a re-emitted `payment.charge.requested` so a retry never becomes a double charge (`gotchas.md` § "I need exactly-once delivery"). A fact carries the identity that makes it safe to see twice; a bare `200` acknowledgement carries nothing.

## The `200 OK` lie, generically

A request/response API hands the caller a single status code, so it *looks* like one boolean settles everything. But behind any non-trivial effect is a chain of stages:

```
request accepted
input validated
primary state written
derived state updated
side effects triggered
visible to other readers
reconciled / balanced
audit / log complete
```

A "success" at the top of that chain says nothing about the bottom. The dangerous case is never the clean failure — it's the partial one: the top said OK, the primary write landed, but a side effect failed, a reader sees a ghost record, and reconciliation disagrees tomorrow. Nothing errored. Everything is wrong.

If you collapse that chain into one `x.ok` event, you have published a fact that is false at every level except the one you happened to observe — and consumers have no way to tell which level that was.

## What good event representation looks like

**1. Name events after what occurred, past tense, at the observing layer.**
`order.placed`, `payment.authorized`, `frame.captured`, `sensor.reading` — each states a fact someone watched happen. Avoid imperative or outcome-assuming names (`doPayment`, `payment.ok`, `order.done`) — they smuggle a success claim onto the wire.

**2. Distinct outcomes are distinct events, not one boolean.**
Don't compress a lifecycle into `success: true|false`. Emit the stages you actually observe as separate facts:

```
job.submitted
job.accepted
job.applied
job.failed        // its own fact, with a reason — not the absence of job.applied
job.reconciled
```

Each is emitted by whoever observed *that* stage. `job.failed` is information; silence is not. Collapsing these into one terminal `job.done` throws away exactly the detail you need when a partial failure happens.

**3. Don't put a delivery/ack claim in the payload.**
`{"delivered": true}` or `{"status": "200"}` inside an event is a category error: delivery is a transport property Net already models (a `Receipt`, an opt-in `Reliable` stream, a RedEX cursor), not a fact the producer can assert about the world. Put the *fact* in the payload (`{"amount": 4200, "currency": "USD"}`), and let the transport layer own transport truth.

**4. Let consumers derive "success."**
The bus carries facts; each consumer folds them into whatever success/failure notion it needs — a CortEX fold from `*.submitted` / `*.applied` / `*.failed` into a per-entity status view (`cortex.md`). Success is a *projection over facts*, not a wire primitive. Two consumers can legitimately compute different "success" from the same event stream; hard-coding one of them into the event forecloses that.

**5. If you genuinely need a caller to learn an outcome, that's nRPC — and even then the reply is a fact, not a guarantee.**
The bus has no return value by design. When a caller must receive a typed result, use nRPC (`nrpc.md`), not a hand-rolled `x.ok` reply event. But note the same ladder applies: an nRPC reply asserts "the handler ran and returned this," which is level 2–3 *for the handler* — it is still not a level-4 business invariant. A reply of `{"accepted": true}` means the handler accepted the request, not that the world is consistent.

## Concrete rewrite

```jsonc
// Bad — one "200 OK"-shaped event that claims a terminal outcome the producer never observed
{ "event": "unlock.ok", "door": "north-gate", "status": 200 }

// Good — the producer states the fact it observed (it issued a request)
{ "event": "door.unlock.requested", "door": "north-gate", "actor": "badge-9f2" }

// ...and the controller that actually watched the bolt move emits the terminal fact, separately
{ "event": "door.unlocked", "door": "north-gate", "verified_at_ms": 1720000000000 }

// ...or, if it didn't, that is also a fact — not an absence
{ "event": "door.unlock.failed", "door": "north-gate", "reason": "actuator_timeout" }
```

A consumer that wants "is the north gate open?" folds `door.unlocked` / `door.unlock.failed` into its own state. Nobody had to lie on the wire, and a partial failure is a first-class, subscribable event instead of a silent gap behind a green checkmark.

## Bottom line

- An event asserts **one fact, at one layer, that the producer observed** — never an end-to-end "OK."
- **Distinct outcomes are distinct events.** Don't collapse a lifecycle into a boolean; a failure is its own fact.
- **Transport truth belongs to the transport** (`Receipt`, `Reliable`, RedEX cursor), not to the event payload.
- **Success is a projection consumers compute over facts**, not a primitive you publish.

When the user's event names or payloads carry `ok` / `done` / `200` / `delivered` semantics, walk them down the ladder and reshape the event as a fact. See `concepts.md` § Publisher (what a receipt does and doesn't mean), `payloads.md` (event form), `cortex.md` (folding facts into a status view), `gotchas.md` § "I need exactly-once delivery" (idempotency keys so a fact is safe to see twice), and `nrpc.md` (when a caller genuinely needs a typed reply).
