# The networked store — an authoritative document with replicas

For a **world** rather than a request. One node **hosts** the authoritative
document; other nodes **join** replicas of it. There is no merge, no CRDT and no
lock-step replication: the host is the authority, and a replica holds a
projection it was given.

```js
import { connect, defineStore, hostStore, joinStore } from '@net-mesh/browser';

// Validators: return a clean value or throw (Zod/Valibot `schema.parse` fits).
const num = (v) => (Number.isFinite(Number(v)) ? Number(v) : 0);

// Shared by every page. Validators only — no game rules, no initial state.
const arena = defineStore({
  id: 'my-game.arena',
  version: 1,
  state: (v) => ({ ships: v?.ships ?? {} }),   // REQUIRED: validates a whole state
  empty: () => ({ ships: {} }),                // REQUIRED: absence; must pass `state`
  actions: {                                   // { name: { input, output } } validators
    enlist: { input: () => ({}), output: (v) => ({ id: String(v.id) }) },
    fire: { input: (v) => ({ at: String(v.at) }), output: (v) => ({ hull: num(v.hull) }) },
  },
  inputs: {                                    // { name: validator }
    steer: (v) => ({ dx: num(v.dx), dz: num(v.dz), dt: num(v.dt) }),
  },
});

const node = await connect({ credentialB64, bootstrapUrl });

// On the ONE hosting page:
const host = hostStore({
  definition: arena,
  transport: node,
  initialState: { ships: {} },
  maxEventBytes: 8104,                         // REQUIRED; the joiner passes the same
  // Only `read` requests carry `audience`; `action` / `input` carry `name` + `input`.
  // Branch on `request.type` — a policy that throws is a refusal (`forbidden`).
  authorize: (request) =>
    request.type !== 'action' || request.name !== 'fire' || request.input.at !== request.peer,
  project: (state, audience) => state,         // everyone sees every ship
  actions: {                                   // HANDLERS, one per declared action
    enlist: (input, context) => {
      const ships = { ...context.getState().ships };
      ships[context.peer] ??= { x: 0, z: 0, hull: 100 };   // keyed by the proven caller
      context.setState({ ships });
      return { id: context.peer };
    },
    fire: (input, context) => {
      const ships = { ...context.getState().ships };
      const target = ships[input.at];
      if (!target) throw new Error('no such ship');       // → action-rejected
      ships[input.at] = { ...target, hull: Math.max(0, target.hull - 25) };
      context.setState({ ships });
      return { hull: ships[input.at].hull };
    },
  },
  inputs: {                                    // HANDLERS, one per declared input
    steer: (input, context) => {
      const ship = context.getState().ships[context.peer];
      if (!ship) return;
      context.setState({ ships: { ...context.getState().ships,
        [context.peer]: { ...ship, x: ship.x + input.dx * input.dt, z: ship.z + input.dz * input.dt } } });
    },
  },
});

// On every OTHER page (never on the host's own node — see § Game recipe):
const replica = joinStore({
  definition: arena, transport: node, host: hostNodeIdHex,
  audience: ['crew'], key: 'player', maxEventBytes: 8104,
});
await replica.ready();
await replica.act('enlist', {});
```

That is the shape of `net/crates/net/browser-ts/README.md` § "Your first
multiplayer scene", which walks the same game through rendering and play. The
package is published as `@net-mesh/browser` (install with `npm install
@net-mesh/browser`, plus `three` for the scene binding); building it from the
repository is in `session.md` § Build it.

**`store` names which store of a definition this is, and a joiner asks for it by
that name.** Both default to the definition id. Name them when one node hosts
several — a second `hostStore` answering to the same name on the same transport
throws `invalid-data` at construction.

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
- **Per-player secrets (a hand of cards) need `projectFor(state, { peer,
  audience })`** instead of `project` — `project` is computed once per audience
  and cannot tell two players apart. Give exactly one of the two (both, or
  neither, throws `invalid-data`). `peer` is the same 16-hex id as
  `context.peer`, so a state keyed by `context.peer` is filtered with
  `state.hands[peer]`. It costs one projection per player per change.
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
| `initialState`, `actions`, `inputs` | the document, and the **handlers** for its transactions and coalesced intents (the definition holds only their validators) |
| `authorize`, `project` / `projectFor` | admission, and visibility per audience (`project`) or per player (`projectFor`) — exactly one |
| `maxEventBytes` | **required** — the bound on one frame; a snapshot over it is chunked (8104 in the package's own tests and demo) |

| Joiner | Meaning |
|---|---|
| `host` | the host's node id (`nodeIdHex`) |
| `store` | which store of the definition to join; defaults to the definition id |
| `audience` | the names this replica claims; `setAudience` re-claims |
| `key` | **required**, a non-empty opaque join token sent on the wire. `authorize` does **not** see it (no `AccessRequest` carries it) — identify callers by `request.peer` |
| `maxEventBytes` | **required**; must agree with the host's frame bound |

**`transport` is structural.** `StoreTransport` is `nodeIdHex()`,
`openStream({ reliability, peer?, label? })`, `onEvent(handler)`, and an
**optional** `connectPeer(peerHex)`. Use the node from `connect()`. A
`MeshSession` from `openSession()` has the same methods, but a store over
`openSession` is not established (see the end of this file). The `label` form is
not a convenience: the leaf **derives** the stream id from the label and sets a
discriminator bit so an unsolicited arrival classifies as stream data rather
than as a channel message — both ends derive the same id without exchanging one.

**A joiner needs a session with the host, not just a discovery hit.** Sessions
are installed by a peer attempt; that is what `StoreTransport.connectPeer` is
for. When the transport cannot provide it, the store still tries to open and the
typed refusal from `openStream` (`session: no session with 0x…`) is the honest
answer rather than a second guess.

## Lifecycle and timing

| Surface | Calls |
|---|---|
| host handle | `authority` (this node's id), `getState()`, `subscribe(listener)`, `setState(next)`, `counts()` (`handles` / `ledgers` / `deferred`), `counters()`, `close()` |
| replica handle | `getState()`, `subscribe(listener)`, `getStatus()`, `subscribeStatus(listener)`, `ready()`, `act(name, input)`, `input(name, value)`, `setAudience(names)`, `reconnect()`, `close()` |

- **Host `setState(next)` replaces the whole document.** Inside a handler,
  `context.setState(patch | (state) => patch)` **shallow-merges** the patch into
  the top level. Handlers are **synchronous**: returning a thenable (an `async`
  handler) invalidates the context and the transaction, and a throw becomes
  `action-rejected`. `context.peer` is the authenticated caller's 16-hex node id.
- **`act(name, input)`** returns a promise of the handler's (validated) output,
  or rejects with a `StoreError`. **`input(name, value)`** returns an
  `InputDisposition` synchronously: `{ type: 'queued' }` for the first input of
  that name, `{ type: 'replaced' }` after, `{ type: 'dropped', reason:
  'not-ready' }` when the replica is not ready (before `ready()`, or closed). It
  describes what happened **locally**, never remote acceptance. Neither takes an
  options / `AbortSignal` argument.
- **`getStatus()` is the replica's health** — `phase` and `stale`, plus what the
  sync state machine knows. `subscribeStatus` is how a HUD shows "reconnecting"
  without polling.
- **Measured constants** (exported): replica keepalive `ALIVE_INTERVAL_MS` 20 s,
  host sweep `HOST_SWEEP_MS` 5 s, request deadline `REQUEST_DEADLINE_MS` 10 s,
  outstanding requests `MAX_OUTSTANDING` 64.
- **The host says goodbye.** A closing host sends an unsolicited `owner-lost` to
  every handle it holds — silence is not an answer.
- **`reconnect()` resumes the same handle on a replaced session** (the transport
  session under it was replaced); `ready()` resolves when the view is
  synchronized (and a replica that never synchronizes reports the reason rather
  than hanging forever). After `closed`, **join afresh** with a new `joinStore`;
  after `owner-lost`, do **not** rejoin that host — the world is gone.

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
| `timeout` | a deadline fired before the answer arrived |
| `aborted` | this handle closed (or was fenced) before the answer, or a newer `setAudience` / transition superseded the request — there is no caller signal to abort with |
| `indeterminate` | submitted, and no response established the outcome |
| `owner-lost` | the store incarnation ended — **terminal**, no handle to obtain |
| `closed` | this handle is unusable: unknown, expired, fenced, or bound elsewhere |
| `action-rejected` | the act ran and was refused on its merits (the handler threw), or a handler broke the synchronous-transaction contract (returned a thenable, nested a transaction) |
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
- **`select` picks the slice; it is not a narrower subscription.** The binding
  subscribes to the whole store and re-runs `select` on every change. It must
  return a `Record<id, entity>` keyed by stable ids; the identity of its members
  drives `create` / `update` / `remove`.
- Keys are stable identifiers you choose per entity — the same value across a
  state update means "the same object", which is what makes `update` rather than
  `remove`+`create` happen.

## Game recipe — one player hosts, the others join

The path the package's own demo (`net/crates/net/browser-ts/demo/main.js`,
`demo/game.js`) and README take. Each rule below is a failure an agent otherwise
writes.

- **Transport: `connect()`, one tab per player.** A store over `openSession` is
  not established. All tabs of one origin in one browser profile are **one
  node**, so test two players with **two browser profiles** (or two browsers),
  not two tabs.
- **`maxEventBytes` is required on both sides and must match** — use 8104.
- **The host player plays through `hostPlayer(host, { audience })`**, never
  `joinStore` on its own node (that throws `invalid-data` — a node has no session
  with itself). `hostPlayer` returns the same handle shape as `joinStore`
  (`ready`, `getState`, `subscribe`, `act`, `input`, `setAudience`, `close`),
  held to the host's own `authorize` with the host's node id as `peer`, and its
  `getState()` is the **projection** for its audience, not the raw document. So
  game code, `bindEntities` included, never branches on "am I the host":

  ```js
  import { hostPlayer } from '@net-mesh/browser';
  const world = isHost
    ? hostPlayer(host, { audience: ['crew'] })
    : joinStore({ definition, transport: node, host: hostId, audience: ['crew'], key: 'player', maxEventBytes: 8104 });
  await world.ready();
  await world.act('enlist', {});
  ```

  Do not hand-roll a wrapper that calls the handlers directly — it skips input
  validation, the transaction, and the result checks a replica gets.

- **Give each player an entity with an `enlist` action keyed by `context.peer`**
  (the authenticated caller — never an id the client sends). A joiner does
  `await replica.ready(); await replica.act('enlist', …)` before steering.
- **React to players with the single `onEvent(event, context)` hook** on
  `hostStore` / `createLobby`: `{ type: 'join', peer, audience }`,
  `{ type: 'leave', peer, reason: 'left' | 'expired' | 'refused' | 'dropped' }`,
  `{ type: 'area', peer, from, to }` (from `areaOf(state, peer) → string | null`,
  fired only on change, `from: null` first). It is a transaction like a handler
  (`context.peer` = the player, `setState` reaches every replica, a throw
  discards its writes), runs after the causing frame, and includes the host's own
  player. Put per-player setup (spawn, starter inventory) in `join`, not in an
  `enlist` action the client must remember to call.
- **Inventories:** keep `inventories: Record<peer, Inventory>` in state
  (`Inventory` = item id → count). Change them only with `addItems` /
  `removeItems` (they throw `InventoryError` `full` / `insufficient` /
  `invalid`, which refuses the action with nothing written), read with
  `countItems` / `hasItems` / `inventoryOf`, validate with `parseInventory(value,
  rules)` in the definition's `state`, and hide other players' with
  `projectFor: (state, { peer }) => ({ ...state, inventories: onlyOwn(state.inventories, peer) })`.
  Trading is not built — do not fake it with two independent actions.
- **Prefer a lobby over hand-rolled discovery.** `createLobby({ node, game,
  name, capacity, info?, visibility?, …hostStore options })` hosts the store,
  gives the host `lobby.self` (a `hostPlayer`), and re-announces on its own;
  `listLobbies({ node, game })` returns `{ code, name, players, capacity, info,
  host, store }`; `joinLobby({ node, definition, game, code | lobby })` finds
  the host and returns a `joinStore` handle (await `ready()`). Capacity counts
  the host; a full lobby answers `forbidden`. `lobby.kick(peer)` is immediate.
  `joinLobby` throws `LobbyError` `not-found` / `ambiguous` (two nodes claim
  the code — never pick one) / `invalid`. A lobby owns its node's
  announcements (pass other tags as `tags`); unlisted lobbies are not secret —
  gate with `authorize`. Codes: `lobby.link()` / `lobbyCodeFromUrl()`.
- **Discovery before join** (without a lobby). Share the host's `node.nodeIdHex()` out of band (the
  demo uses a `?host=<hex>` link). The host announces a tag and **re-announces
  every ~2 s** — announcements are leases that expire. The joiner polls
  `node.query(tag)` until the host is present — compare with
  `peerIdHex(descriptor.nodeId)`, since a descriptor's `nodeId` is decimal — and
  only then calls `joinStore`; joining a host it has never seen fails with
  `session: no session with 0x…`.

  ```js
  // host page
  await node.announce(['my-game.host']);
  setInterval(() => node.announce(['my-game.host']).catch(() => {}), 2_000);
  // joiner page
  for (let i = 0; ; i++) {
    const hosts = await node.query('my-game.host');
    if (hosts.some((d) => peerIdHex(d.nodeId) === hostId)) break;
    if (i >= 80) throw new Error('the host never showed up');
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  ```

- **`project` returns a full, valid `S`**, never a partial. Hidden = absent from
  a collection, or an explicit `null` field; `empty()` means absence. A zero must
  never stand for "you cannot see this".
- **`bindEntities` details.** `update` is **not** called right after `create`, so
  `create` must set the initial transform itself. `remove` (optional) runs after
  the object has already left the scene — dispose per-entity resources there.
  A throwing `create` / `update` / `remove` goes to `onError` (default
  `console.error`) and the rest of the frame continues. The binding also exposes
  `apply()` (reconcile now), `object(id)`, `size` and `dispose()`.
- **Store naming.** The name defaults to the definition id; a lobby and a match
  on one host need `store: 'lobby'` / `store: 'match'` on both sides, and two
  stores with the same name on one transport throw `invalid-data`.
- **Prototype offline first** with `@net-mesh/browser/local`:
  `const mesh = createLocalMesh(); const hostNode = mesh.node(); const guestNode = mesh.node();`
  — each node is a `transport` for `hostStore` / `joinStore`, in one page, with
  the real store and no anchor or network. Local nodes also `announce(tags)` and
  `query(tag)` like a real node (another node's announcement, never your own;
  it expires after 300 s unless re-announced), so the find-the-host loop works
  offline unchanged. It proves game logic, not that two
  browsers can connect; switch the nodes to `connect()` for that. The package
  demo (`net/crates/net/browser-ts/demo/`) runs this way by default.

## What is not established (do not present it as proven)

- **Two tabs sharing a leader, around a store.** This package's tests exercise a
  host and a joiner in **one tab**. The store's own use of a proxied handle on a
  follower — last-consumer cleanup, the peer/stream lifecycle across a leader
  change — is recorded as not established in
  `net/crates/net/browser-ts/src/store/index.ts`. The leader-proxy surface a
  follower uses is newer than the store, and it is not witnessed for this case.
- **A store over `openSession`.** Same gap, from the page's side: a `MeshSession`
  satisfies `StoreTransport` structurally, but only `connect()` is the supported
  store transport.
- **The fake-wasm unit tests prove drift, not behaviour.** They satisfy the
  declared wasm boundary at compile time; the real artifact is exercised by the
  Playwright matrix in `net/crates/net/tests/rtc_browser/` and by the worked demo
  at `net/crates/net/examples/browser-demo/`.
