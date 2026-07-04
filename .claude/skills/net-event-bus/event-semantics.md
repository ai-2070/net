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

When the user's event names or payloads carry `ok` / `done` / `200` / `delivered` semantics, walk them down the ladder and reshape the event as a fact. See `concepts.md` § Publisher (what a receipt does and doesn't mean), `payloads.md` (event form), `cortex.md` (folding facts into a status view), and `nrpc.md` (when a caller genuinely needs a typed reply).
