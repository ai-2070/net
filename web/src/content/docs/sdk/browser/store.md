---
title: Networked store
description: "Share authoritative browser state with @net-mesh/browser — defineStore, hostStore and joinStore, inputs versus actions, audience-scoped visibility, and the typed StoreError codes."
---

# The networked store

The store shares authoritative state between a host page and joined pages,
without making every writer an authority. It is re-exported from the
`@net-mesh/browser` root:

```ts
import { defineStore, hostStore, joinStore } from '@net-mesh/browser';
```

A store incarnation is identified by **(authority identity, definition id,
version, application key)** — the key can name a ship, a room or a region. There
is no global world registry and no mandatory anchor data hop; the owner is the
application endpoint.

## Define it

```ts
const game = defineStore({
  id: 'net.demo.fleet',
  version: 1,
  state: parseWorld,          // (value: unknown) => World, throwing on bad input
  empty: () => ({ ships: {}, waypoint: null, tick: 0 }),
  actions: {
    fire: { input: parseFire, output: parseFireResult },
  },
  inputs: {
    steer: parseSteer,
  },
});
```

- The definition carries **validators, never authority handlers**, so a joining
  bundle does not carry the owner's gameplay code.
- `empty()` is the value that means **absence**. A replica installs it when
  visibility is lost, so it must itself pass `state()` — `defineStore` checks
  that immediately. Model a hidden entity as `null` or omitted, never as a
  plausible default like `{ heading: 0 }`.

## Host it

```ts
const host = hostStore({
  definition: game,
  transport: session,
  initialState: { ships: {}, waypoint: { x: 6, z: -6 }, tick: 0 },
  maxEventBytes,                       // required; the transport's own ceiling
  authorize: request => allow(request),
  project: (state, audience) => visible(state, audience),
  actions: { fire: (input, context) => shoot(input, context) },
  inputs: { steer: (input, context) => moveShip(input, context) },
});

host.setState(nextState);              // the FULL next state, not a patch
```

- Handlers run **synchronously**, one transaction each. `context.peer` is the
  authenticated caller; `context.setState` joins that handler's transaction. A
  context is dead after the transaction — returning a thenable or writing from a
  later microtask is refused.
- `authorize` runs for every read, action and input. Even a public room writes
  `authorize: () => true` deliberately — holding the transport PSK does not grant
  store access.
- `project` returns a validated state with invisible entities **omitted** and
  hidden fields explicitly `null`. Visibility lives in the schema, not in a
  renderer. A change invisible to an audience produces no frame at all.

## Join it

```ts
const player = joinStore({
  definition: game,
  transport: session,
  host: hostId,                        // hex or exact-decimal node id
  audience: ['crew'],
  key: 'player',
  maxEventBytes,
});

await player.ready();                  // a consistent view is installed
player.getState();
```

`joinStore` resolves as soon as the join is **sent**; `ready()` is what waits for
the world. If the policy denies the join, `ready()` rejects with `forbidden`.

A replica has **no `setState`**. It writes through:

| | `input` | `action` |
|---|---|---|
| Shape | latest-value intent | acknowledged request/response |
| Returns | `InputDisposition`, synchronously | `Promise<output>` |
| Loss | a dropped one may be the last one | refused or answered, never silent |
| Use for | held direction, aim, throttle | fire, buy, enlist, place |

```ts
player.input('steer', { dx: 1, dz: 0, dt: 0.1 });   // 'queued' | 'replaced' | dropped
const hit = await player.act('fire', { at: targetId });
```

`setAudience(names)` clears the caller's view to `empty()` immediately and then
requests the new audience from the owner; `reconnect()` retains the last snapshot
and marks it **stale** rather than discarding it.

## Errors

```ts
import { StoreError } from '@net-mesh/browser';
```

Every refusal carries a `code` a caller branches on:

- `forbidden` — `authorize` refused.
- `invalid-data` — a payload failed its validator.
- `version-mismatch` — an id/version disagreement with the host.
- `capacity` — a declared bound was reached.
- `not-ready` — the handle has no live, synchronized view.
- `action-rejected` — the handler threw or broke the transaction contract.
- `indeterminate` — submitted, and no response established the outcome. **Do not
  retry**: a silent resend is how one action becomes two.
- `owner-lost` and `closed` — terminal for the handle. `closed` is one code for
  unknown, expired, fenced and wrong-peer handles, so a refusal cannot disclose
  whether a handle exists. The subscription is recoverable by joining afresh.
- `result-expired` — the request cannot execute again and its original result is
  unavailable. It is **not** a success receipt.

## Bounds

Snapshots are validated and capped at 1 MiB and 255 chunks; deltas at 256 patch
operations; a host keeps up to 256 handles and 32 pending digest actions; a handle
lease is 60 s, renewed every 20 s, so one lost renewal is tolerated. A request
waits 10 s before it becomes `indeterminate`. Structured
limits are declarative: callers cannot raise the host's bounds.

## Next

- [Three.js binding](/docs/sdk/browser/three) — render store entities without
  rebuilding them every frame.
