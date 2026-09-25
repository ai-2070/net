# The networked store — an authoritative document with replicas

For a **world** rather than a request. One node **hosts** the authoritative
document; other nodes **join** replicas of it. There is no merge, no CRDT and no
lock-step replication: the host is the authority, and a replica holds a
projection it was given.

```typescript
import { defineStore, hostStore, joinStore } from '@net-mesh/browser';
import { bindEntities } from '@net-mesh/browser/three';

const definition = defineStore({ id: 'space', version: 1, initialState, actions, inputs, /* … */ });

const host = hostStore({
  definition,
  store: 'world',            // WHICH store of this definition this is
  transport: node,           // the node from connect()
  initialState,
  authorize: (request) => request.audience.every((a) => a !== 'command'),
  project: (state, audience) => (audience.includes('command') ? state : publicPart(state)),
});

const replica = joinStore({
  definition, store: 'world', transport: node, host: hostNodeIdHex,
  audience: ['crew'], key: 'player',
});
await replica.ready();
```

**`store` names which store of a definition this is, and a joiner asks for it by
that name.** Name them when one node hosts several — otherwise the joins are
ambiguous and the wrong store answers.

## What is authoritative, and where the boundaries are

- **`act` executes on the host** and its result comes back **correlated to the
  request**. This is the shape for anything that must have a verdict: a purchase,
  a spawn, "did my move land".
- **`input` is coalesced and unacknowledged.** The right shape for 60 Hz intent —
  movement, aim, camera. Nothing waits for it, and nothing is guaranteed
  individually; the projection converges.
- **`project(state, audience)` decides what each audience may see**, applied by
  the host before anything leaves. A replica cannot read what it was not given;
  do not rely on client-side hiding.
- **`authorize(request)` decides whether a request is allowed** — and it is
  consulted on the **ongoing delta feed**, not just at join. A read that policy
  has since revoked stops arriving rather than being served because the handle
  is still warm.
- **Audiences change.** `replica.setAudience(names)` re-negotiates what this
  replica is entitled to see.
- **The store's identity is the peer whose installed session opened the packet.**
  No store frame carries an originator field to consult — so policy is handed the
  authenticated peer, which is the same identity the mesh admitted.

## Options that matter

| Host | Meaning |
|---|---|
| `definition`, `store` | which document type, and which instance of it |
| `transport` | a `StoreTransport` — see below |
| `initialState`, `actions`, `inputs` | the document, its transactions, its coalesced intents |
| `authorize`, `project` | admission and per-audience visibility |
| `maxEventBytes` | the bound on one frame; a snapshot over it is chunked |

| Joiner | Meaning |
|---|---|
| `host` | the host's node id (`nodeIdHex`) |
| `audience` | the names this replica claims; `setAudience` re-claims |
| `key` | this replica's handle key |
| `maxEventBytes` | must agree with the host's frame bound |

**`transport` is structural.** `StoreTransport` is `nodeIdHex()`,
`openStream({ reliability, peer?, label? })`, `onEvent(handler)`, and an
**optional** `connectPeer(peerHex)`. The node from `connect()` satisfies it, and
so does a session. The `label` form is not a convenience: the leaf **derives**
the stream id from the label and sets a discriminator bit so an unsolicited
arrival classifies as stream data rather than as a channel message — both ends
derive the same id without exchanging one.

**A joiner needs a session with the host, not just a discovery hit.** Sessions
are installed by a peer attempt; that is what `StoreTransport.connectPeer` is
for. When the transport cannot provide it, the store still tries to open and the
typed refusal from `openStream` (`session: no session with 0x…`) is the honest
answer rather than a second guess.

## Lifecycle and timing

| Surface | Calls |
|---|---|
| host handle | `getState()`, `subscribe(listener)`, `setState(next)`, `counts()` (`handles` / `ledgers` / `deferred`), `counters()`, `close()` |
| replica handle | `getState()`, `subscribe(listener)`, `getStatus()`, `subscribeStatus(listener)`, `ready()`, `setAudience(names)`, `reconnect()`, `close()` |

- **`getStatus()` is the replica's health** — `phase` and `stale`, plus what the
  sync state machine knows. `subscribeStatus` is how a HUD shows "reconnecting"
  without polling.
- **Measured constants** (exported): replica keepalive `ALIVE_INTERVAL_MS` 20 s,
  host sweep `HOST_SWEEP_MS` 5 s, request deadline `REQUEST_DEADLINE_MS` 10 s,
  outstanding requests `MAX_OUTSTANDING` 64.
- **The host says goodbye.** A closing host sends an unsolicited `owner-lost` to
  every handle it holds — silence is not an answer.
- **`reconnect()` is explicit** for a replica that gave up; `ready()` resolves
  when the view is synchronized (and a replica that never synchronizes reports
  the reason rather than hanging forever).

## Snapshots, deltas and their bounds

- A snapshot is **chunked**. A replica whose manifest or chunk is lost asks again
  and then reports a typed `timeout` rather than waiting forever.
- A delta applies at the revision it was built on; a gap provokes a **resync**
  (coalesced, so a flood of deltas for a missed generation asks once) instead of
  silently diverging.
- Byte and piece bounds are charged against the manifest's declared total
  *before* holding anything, so concurrent near-empty assemblies cannot exceed
  the table.
- The assembled document is re-parsed under the same depth / duplicate-key /
  finite-number rules a single frame gets.

## `StoreError.code` — what a caller branches on

The wire carries exactly this set:

| Code | Meaning |
|---|---|
| `invalid-data` | a payload failed its validator |
| `version-mismatch` | definition id/version disagreement between host and joiner |
| `forbidden` | `authorize` refused |
| `not-ready` | the handle has no live, synchronized view yet |
| `capacity` | a declared bound was reached |
| `timeout` / `aborted` | your deadline or signal fired |
| `indeterminate` | submitted, and no response established the outcome |
| `owner-lost` | the store incarnation ended — **terminal**, no handle to obtain |
| `closed` | this handle is unusable: unknown, expired, fenced, or bound elsewhere |
| `action-rejected` | the act ran and was refused on its merits |
| `result-expired` | this request cannot execute again, and its original result is gone |

Two that are easy to get wrong:

- **`owner-lost` is not `closed`.** `closed` is the expiry notice a replica
  *rejoins* on; rejoining an owner that is gone either hangs or attaches you to a
  **successor's different document** under the handle you already had. A
  deliberate farewell says `owner-lost`; silence is what `indeterminate` is for.
- **`result-expired` is not a success.** It asserts nothing about whether the
  original attempt committed — a retired sequence may have been rejected, aborted
  before commit, or fenced without ever executing. Never read it as a receipt.

## `bindEntities` — store state into a scene graph

```typescript
const binding = bindEntities({
  store: replica,
  scene,
  select: (state) => state.ships,
  binding: {
    create: (ship, id) => buildShip(ship, id),
    update: (object, ship) => object.position.set(ship.x, 0, ship.z),
    remove: (object) => object.traverse(disposeOf),
  },
});   // binding.dispose() when you are done
```

- **`@net-mesh/browser/three` imports nothing from `three`.** The scene graph is
  anything with `add` and `remove`, the objects are whatever `create` returns,
  and the types are structural — so the package gains no renderer dependency and
  your scene graph can be a test double.
- **An entity whose reference did not change is not touched.** The store shares
  the references of subtrees that did not change; a loop over
  `Object.values(state.entities)` rebuilding everything throws exactly that away
  and re-creates objects every frame.
- **`select` is the subscription**, not a filter applied after the fact: pick the
  slice you render and let the identity of its members drive `create` / `update` /
  `remove`.
- Keys are stable identifiers you choose per entity — the same value across a
  state update means "the same object", which is what makes `update` rather than
  `remove`+`create` happen.

## What is not established (do not present it as proven)

- **Two tabs sharing a leader, around a store.** This package's tests exercise a
  host and a joiner in **one tab**. The store's own use of a proxied handle on a
  follower — last-consumer cleanup, the peer/stream lifecycle across a leader
  change — is recorded as not established in
  `net/crates/net/browser-ts/src/store/index.ts`. The leader-proxy surface a
  follower uses is newer than the store, and it is not witnessed for this case.
- **The package is not on a registry yet.** See `session.md` § Build it.
- **The fake-wasm unit tests prove drift, not behaviour.** They satisfy the
  declared wasm boundary at compile time; the real artifact is exercised by the
  Playwright matrix in `net/crates/net/tests/rtc_browser/` and by the worked demo
  at `net/crates/net/examples/browser-demo/`.
