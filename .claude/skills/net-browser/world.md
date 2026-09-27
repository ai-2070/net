# Large worlds — regions, handoff (`@net-mesh/browser/world`)

Use this when one host cannot hold the whole world: the map is cut into
square **regions**, each its own store hosted by a (usually dedicated) host,
and a player holds replicas of the regions around it.

## Regions

A region is a square of `size` units named `r:<rx>:<rz>`: region `r:4:7` covers
`4*size <= x < 5*size` and `7*size <= z < 8*size`. Every host and player of a
world must use the same `size`.

- `regionOf(x, z, size)` names the region containing a point (`RangeError` for
  a `size` that is not positive).
- `regionsAround(x, z, { size, radius })` lists the `(2r+1)²` regions around it
  (`radius` default 1).
- `regionTag(world, region)` is the directory tag a host announces,
  `net-world:<world>:region:<region>`.
- `REGION_ANNOUNCE_MS` (2000) is how often `announceRegions` re-announces: an
  announcement is a lease.

## Region hosts

```ts
import { hostStore } from '@net-mesh/browser';
import { announceRegions, handoffLink, parseHandoffLedger, regionDirectory,
         regionHandoffs, storeRegion } from '@net-mesh/browser/world';

// One store per region, named by region; several may share a node.
const host = hostStore({ definition: region, store: 'r:4:7', transport, … });
announceRegions(node, 'my-world', ['r:4:7']);            // re-announces every 2 s

// Moving an entity to the neighbour: at-most-once.
const directory = regionDirectory({ node, world: 'my-world', trustedHosts: HOST_NODE_IDS });
const link = handoffLink({ transport, label: 'my-world.handoff',
                          peerOf: directory.peerOf, refresh: directory.lookup });
const handoffs = regionHandoffs({
  link, region: 'r:4:7',
  ...storeRegion(host, { region: 'r:4:7', collection: 'ships', ledger: 'handoff' }),
  persist: () => { saving.flush(); },                     // persistStore(host, …) from @net-mesh/sdk
  onEvent: e => …,                                        // moved / refused / unresolved / admitted
});
await directory.lookup('r:4:8');
await handoffs.handoff('ship-7', 'r:4:8');               // frozen here now, live there on accept
```

- The region definition carries the ledger key (`handoff: parseHandoffLedger(raw.handoff, parseShip)`
  in `state`) and hides it: `visibility: { handoff: 'nobody' }`.
- **Durability before send** is the protocol's one requirement: `persist` is
  awaited after every change, before its messages go out. Without it a crash can
  duplicate an entity.
- Outcomes: `moved` (it is the neighbour's), `refused` (back here, live),
  `unresolved` (no answer within `giveUpMs`; it stays frozen, never live twice;
  `handoffs.reoffer(id)` when the neighbour is back). `handoffs.locate(id)`
  says `here` / `in-transit` / `unknown`: an input for an `in-transit` entity is
  refused, never applied.
- **Name your hosts** (`trustedHosts`, node ids in 16 hex): any node can
  announce any region. Without the list, a contested region is `ambiguous` and
  not joined, and a handoff link must not trust a directory an impostor can
  write to.
- `announceRegions(node, world, regions, { tags, everyMs })` returns a stop
  function. `announce` REPLACES a node's tags, so pass the node's other tags
  in `tags`.
- `directory.lookup(region)` resolves to `{ status: 'hosted', host }`,
  `{ status: 'unhosted' }` or `{ status: 'ambiguous', hosts }`, and caches it.
  `directory.peerOf(region)` is the synchronous cached answer the link needs.
- **`regionHandoffs` options beyond the example:**
  - `admit(entity, state)`: `true`, or a refusal reason (default: admit all).
    An id already live here is refused `occupied`, never overwritten.
  - `timing`: `retryMs` (re-offer, default 250), `giveUpMs` (report
    `unresolved`, default 60 000) and `handledRetentionMs` (how long a target
    remembers a decision, default 600 000). **`handledRetentionMs` must exceed
    `giveUpMs`**, or a late re-offer could be admitted twice; the steps throw
    `RangeError` for a timing that breaks it.
  - `tickMs` (the retry/prune loop, default 100), `ghosting.everyMs` (default
    100), `newId` (the handoff and action id source; default the region plus 64
    random bits), `now`.
- `handoffs.close()` stops the driver; forwarded actions still pending reject
  `unresolved`. `link.dropped` counts refused messages by reason:
  `unauthenticated` (a sender the directory does not name for its region),
  `malformed`, `unknown-region`, `opening`, `open-failed`.

## Players

```ts
const view = joinWorld({ node, world: 'my-world', definition: region, collection: 'ships',
                         position: { x, z }, size: 256, key: 'player', maxEventBytes: 8104,
                         trustedHosts: HOST_NODE_IDS, positionOf: ship => ship });
bindEntities({ store: view, select: ships => ships, … });   // one merged view
view.setPosition(x, z);            // regions join and release as you move (3×3 kept, 5×5 held)
await view.act('fire', input);     // goes to the region you are in
```

- `view.regions()` shows each region's phase and host: `looking`, `unhosted`,
  `ambiguous`, `joining`, `ready`, `failed` (unhosted, ambiguous and failed are
  retried every `retryMs`, default 2000).
- `joinWorld` options beyond the example: `radius` (regions kept around the
  player, default 1: a 3×3 block, released beyond `radius + 1`), `audience`
  (default `[]`), `directory` (bring your own; default one over `node` with
  `trustedHosts`), `retryMs`, `lingerMs`.
- `view.currentRegion()` names the player's region; `view.subscribe(listener)`
  fires on every merged change; `await view.close()` releases every replica.
  `view.act(name, input)` rejects if the current region is not `ready`.
- An entity held by two regions at once (mid-handoff) shows once:
  `positionOf` picks the copy from the region containing it. One that
  vanishes from a held region lingers at its last state for `lingerMs`
  (default 500) until it appears elsewhere, so a handoff does not blink.
- A native (Node) host or player uses `meshStoreTransport(mesh, { listen })`
  from `@net-mesh/sdk`, which has `announce` / `query` too.
- **Ghosting:** `regionHandoffs({ ghosting: { size, margin, positionOf } })`
  sends each neighbour this region's entities within `margin` of their shared
  border, read-only. `handoffs.ghosts()` and `onGhosts` give the neighbours'
  entities near this region's borders, for collisions or line of sight in the
  host's own simulation. They are not authoritative here: to act on one, use
  `forward`. A neighbour that goes quiet has its ghosts expired.
- **Not built yet:** load balancing (split/merge).

## Cross-border actions

A region host asks a neighbour to act on something the neighbour owns. The
neighbour's authority decides, and runs each action at most once:

```ts
regionHandoffs({ …, actions: {
  hit: (ships, input, fromRegion) => ships[input.ship]
    ? { entities: { ...ships, [input.ship]: damaged(ships[input.ship]) }, output: 'hit' }
    : 'no such ship',                                   // a string refuses
} });
await handoffs.forward('r:4:8', 'hit', { ship: 's9' });  // the neighbour's answer
```

- Rejections are a `BorderActionError`: `refused` carries the neighbour's
  reason. `unresolved` means no answer within `giveUpMs`: the action may have
  run, but never twice. To retry, call `forward` again, which uses a new id.
- The neighbour records each outcome in the ledger (`acts`), in the same
  commit as the effect and made durable before it answers. A repeat of the
  same id gets the recorded answer.

## Pure steps, for a custom driver

`regionHandoffs` is one driver over pure functions of a `RegionState`
(`{ region, entities, outgoing, handled, acts? }`), exported for a host that
needs its own loop: `regionState`, `beginHandoff`, `onHandoffOffer`,
`onHandoffReply`, `retryHandoffs`, `reofferHandoff`, `pruneHandled`, `locate`,
`onForwardedAction`, `pruneActs` and `ghostTargets`. Each step returns
`{ state, send, events }`. **Make `state` durable before sending `send`**: that
ordering is the whole at-most-once argument, and a driver that sends first can
duplicate an entity after a crash.

Source: `net/crates/net/browser-ts/src/world/`; the protocol's deterministic
simulation is `net/crates/net/browser-ts/test/world/handoff.test.ts`.
