# Large worlds — regions, handoff (`@net-mesh/browser/world`)

Use this when one host cannot hold the whole world: the map is cut into
square **regions**, each its own store hosted by a (usually dedicated) host,
and a player holds replicas of the regions around it.

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
- **Name your hosts** (`trustedHosts`): any node can announce any region. Without
  the list, a contested region is `ambiguous` and not joined, and a handoff link
  must not trust a directory an impostor can write to.

## Players

```ts
const view = joinWorld({ node, world: 'my-world', definition: region, collection: 'ships',
                         position: { x, z }, size: 256, key: 'player', maxEventBytes: 8104,
                         trustedHosts: HOST_NODE_IDS, positionOf: ship => ship });
bindEntities({ store: view, select: ships => ships, … });   // one merged view
view.setPosition(x, z);            // regions join and release as you move (3×3 kept, 5×5 held)
await view.act('fire', input);     // goes to the region you are in
```

- `view.regions()` shows each region's phase: `looking`, `unhosted`,
  `ambiguous`, `joining`, `ready`, `failed` (retried every `retryMs`).
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

Source: `net/crates/net/browser-ts/src/world/`; the protocol's deterministic
simulation is `net/crates/net/browser-ts/test/world/handoff.test.ts`.
