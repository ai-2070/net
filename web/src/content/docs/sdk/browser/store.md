---
title: Browser store
description: "Host an authoritative document on one node and join replicas of it: audience projections, correlated actions, coalesced inputs, and the codes a caller branches on."
---

# The networked store

For a page that has a **world** rather than a request: one node hosts an
authoritative document, others join replicas of it, and the host decides what
each caller may see and do.

```typescript
import { defineStore, hostStore, joinStore } from '@net-mesh/browser';

const host = hostStore({
  definition, store: 'world', transport: node,
  initialState,
  maxEventBytes: 8104,
  // Only a `read` request carries `audience`; `action` / `input` carry `name` + `input`.
  authorize: request => request.type !== 'read' || !request.audience.includes('command'),
  project: (state, audience) =>
    audience.includes('command') ? state : publicPart(state),
  actions,
  inputs,
});

const replica = joinStore({
  definition, store: 'world', transport: node, host: hostNodeIdHex,
  audience: ['crew'], key: 'player', maxEventBytes: 8104,
});
await replica.ready();
```

## Define once, host or join

`defineStore({ id, version, state, empty, actions, inputs })` is a reusable typed
description and carries no state or connection. It holds validators, never
authority handlers, so a client bundle that joins a store does not carry the
owner's gameplay code.

Two properties are checked at definition time, because their violation is
invisible until much later:

- **`empty()` must itself be a valid state.** It is what a replica installs when
  it loses visibility, so an `empty()` the schema would reject would fail
  mid-transition, with the old projection already fenced.
- **`id` and `version` must be usable as an incarnation identity.** They travel
  in every envelope, and a blank id or a non-integer version turns a
  version-mismatch refusal into a confusing parse error at the far end.

Validators are the shape of `schema.parse`, so Zod, Valibot or a hand-written
check all work without the store depending on any of them.

## The host is the authority

| Option | What it decides |
| --- | --- |
| `definition` | The typed description |
| `store` | **Which** store of that definition this is; defaults to the definition id |
| `transport` | The node it serves over |
| `initialState` | The document as it starts |
| `maxEventBytes` | The largest frame your transport carries (the package's own tests and demo use 8104) |
| `authorize` | Who may read, act or send an input — handed the **authenticated** peer |
| `project` | What a given audience may see — or `projectFor`, what a given player may see (exactly one) |
| `actions` | One handler per declared action |
| `inputs` | One handler per declared input |

A node may host several stores of one definition — a lobby and a match, three
worlds — and **each must name itself**, because every host store on a node sees
every frame and a `join` names no handle. Two stores answering to one name on one
transport is refused at construction, where the mistake is legible, rather than
resolved by whichever listener registered first.

An action handler runs inside **one synchronous transaction**: it stages writes
through its context and returns the output, or throws to reject. Returning,
throwing or handing back a thenable invalidates the context, so a retained
context cannot write from a later microtask.

```typescript
fire: (input, context) => {
  const target = context.getState().ships[input.at];
  if (target === undefined) throw new Error('no such ship');
  const hull = Math.max(0, target.hull - 25);
  context.setState({
    ships: { ...context.getState().ships, [input.at]: { ...target, hull } },
  });
  return { hull };
},
```

`context.peer` is the caller's mesh node id in 16 lowercase hex, and it is the
identity the **transport** proved. No store frame has an originator field to
consult, so a page cannot claim to be someone else.

## Actions and inputs are different instruments

- An **action** is acknowledged, validated and correlated: `replica.act(name,
  input)` returns a promise resolving the handler's output, or rejecting with a
  typed code.
- An **input** is coalesced latest-value intent with no per-call promise:
  `replica.input(name, value)` returns a disposition (`queued`, `replaced`, or
  `dropped` with a reason) describing what happened **locally**. It is not remote
  acceptance, and a dropped input may be the last one — the handler is written to
  tolerate that.

## Audiences and projection

A replica asks for an audience (`audience: ['crew']`) and sends a required, opaque
`key` join token; `authorize` does not see it — identify callers by
`request.peer`. The host's `project(state, audience)` returns a
validated `S` — never a partial — so visibility is expressed **in the schema**:
collections omit invisible entities, individually hidden fields are explicit
`null` or a tagged value, and `empty()` means absence.

A zero must never be readable as "you cannot see this", and a replica cannot read
what it was not given: the withheld part is absent from the frames, not hidden in
the renderer. `replica.setAudience(names)` asks for a different audience and
resolves when the new projection is installed.

When the view depends on the player rather than the audience — each player's own
hand — give `projectFor` instead of `project`. It receives the authenticated
player with the audience they read:

```typescript
projectFor: (state, { peer, audience }) => ({
  ...state,
  hands: { [peer]: state.hands[peer] ?? [] },
}),
```

`project` is computed once per distinct audience and shared; `projectFor` once
per distinct player and audience, so it costs one projection per player per
change. A player whose view did not change is sent nothing. A host given both,
or neither, is refused with `invalid-data`.

## The transport, and one gate to know about

A store needs a `StoreTransport` — the structural subset of the node that
`connect()` and `openSession()` both satisfy. `openStream` is synchronous on the
direct node and a promise on a session, and the store awaits both.

`connect()` is the supported transport today. A **joiner needs a session with its
host**, which the store installs through `connectPeer` — and on a session that
call is a proxy round trip to the tab holding the lock. The store's own use of a
proxied stream on a follower (last-consumer cleanup, the peer/stream lifecycle
across a leader change) is not established: a host and a joiner in one tab are
exercised, two tabs sharing a leader are not. One tab per node is also what a
game usually wants.

A replica of the node it is running on is refused with `invalid-data` — a node
has no session with itself, and refusing at construction beats a `no session with
0x…` from inside the transport after the subscription was accepted. A host that
is also a player uses `hostPlayer` instead (below).

For development without an anchor, `@net-mesh/browser/local` provides a mesh
inside one page. Each node it creates is a `StoreTransport`, so the store on top
is the real one — frames are encoded, chunked and dispatched — while delivery is
a function call and the peer on each frame is assigned rather than proved:

```typescript
import { createLocalMesh } from '@net-mesh/browser/local';

const mesh = createLocalMesh();
const hostNode = mesh.node();
const guestNode = mesh.node();
const host = hostStore({ definition, transport: hostNode, /* … */ });
const guest = joinStore({ definition, transport: guestNode, host: hostNode.nodeIdHex(), /* … */ });
```

It is evidence about game logic, not about whether two browsers can reach each
other.

An announcement is a lease, so both sides re-announce on a timer; a joiner that
looks a few seconds late otherwise reports that the host never announced.

## The replica handle

```typescript
await replica.ready();                  // a consistent view is installed
replica.getState();                     // ReadonlyState<S>
replica.subscribe(listener);            // state changes
replica.subscribeStatus(listener);      // phase, stale, error
await replica.act('fire', { at });
replica.input('steer', { dx, dz, dt });
await replica.setAudience(['crew']);
await replica.reconnect();              // the session was replaced
await replica.close();
```

`joinStore` returns as soon as the join is sent — `ready()` is what waits, so a
caller that wants a loading state does not have to await the world first.

`getStatus()` is readiness kept out of game state: a `phase`, a `stale` flag
meaning *retained state is no longer known-current* (not "empty world"), and the
last `StoreError`.

A replica has **no `setState`**. That is a type-level fact rather than a runtime
check: writes go through actions and inputs, and only the host's handle carries
the setter.

## The host's own player

`hostPlayer(host, { audience })` returns the same handle shape as `joinStore`,
for the node that hosts the store:

```typescript
const me = hostPlayer(host, { audience: ['crew'] });
await me.ready();
await me.act('enlist', {});
```

It is held to parity with a replica rather than trusted:

- **The same policy.** Reads, actions and inputs go to the host's `authorize`,
  with the host's own node id as `peer`.
- **The same transaction.** Input validation, the handler, output validation and
  the result's message budget run in one transaction, and the change reaches
  every replica.
- **The same values.** A value the wire would refuse (`NaN`, a cycle) is refused
  with `invalid-data`.
- **The same view.** `getState()` is the projection for its audience, not the
  authoritative document, so the host's page renders what a player would.

Calls settle a turn later, as a replica's do, so a call started inside a handler
runs as its own transaction. When the host closes, the player's phase becomes
`closed` and its calls reject with `owner-lost`.

The projection keeps the host's own UI honest; it does not hide anything from
the person running the host, whose page holds the whole document.

## Codes to branch on

Every refusal is a `StoreError` with a `code`:

| Code | What it means |
| --- | --- |
| `invalid-data` | A payload failed its validator |
| `version-mismatch` | Definition id or version disagreement |
| `forbidden` | The owner's `authorize` refused |
| `not-ready` | The handle has no live, synchronized view |
| `capacity` | A declared bound was reached |
| `timeout` | A deadline fired before the answer arrived |
| `aborted` | The handle closed before the answer, or a newer request (a later `setAudience`) superseded this one |
| `indeterminate` | Submitted, and no response established the outcome |
| `owner-lost` | The store incarnation ended — terminal |
| `closed` | This handle is unusable: unknown, expired, fenced, or bound to another peer |
| `action-rejected` | The owner refused this action, or a handler broke the transaction contract |
| `result-expired` | This request cannot execute again, and its original result is unavailable |

Three of those carry a disposition worth stating plainly:

- **`closed` is deliberately one code for four causes**, so a refusal cannot
  disclose whether a handle exists. It is terminal for the handle, while the
  *subscription* is recoverable by joining afresh — which yields a new handle,
  not a resumption of the old one.
- **`owner-lost` is said, not inferred.** A closing host sends a farewell to
  every handle it holds, because silence is not an answer and a replica whose
  host closed would otherwise learn nothing until its own deadline fired and
  reported `indeterminate`.
- **`result-expired` is never a success receipt.** It asserts nothing about
  whether the original attempt committed; a non-reexecution floor is not evidence
  of execution.

## Next

- [Three](/docs/sdk/browser/three) — render the replica's entities
- [Errors](/docs/sdk/browser/errors) — the node-level taxonomy underneath
