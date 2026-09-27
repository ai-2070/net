---
title: Large worlds
description: "A map bigger than one host: regions each hosted as a store, a player's merged view across them, and entities that cross borders at most once."
---

# Large worlds: regions across hosts

When one host cannot hold the whole world, `@net-mesh/browser/world` cuts the
map into square **regions**. Each region is its own
[store](/docs/sdk/browser/store), hosted by whichever node announces it (usually
a dedicated Node host), and a player holds replicas of the regions around it.
There is no central directory service: the regions are found through the mesh's
own signed announcements.

## Regions and the directory

A region of `size` units is named `r:<rx>:<rz>`: it covers `rx*size <= x <
(rx+1)*size`, and the same for `z`. `regionOf(x, z, size)` names the region at
a position; `regionsAround(x, z, { size, radius })` lists the square of regions
around it.

A region host serves one store per region, named by region (several may share a
node), and keeps announcing them. An announcement is a lease, so
`announceRegions` re-announces every 2 seconds:

```typescript
import { hostStore } from '@net-mesh/browser';
import { announceRegions, regionDirectory } from '@net-mesh/browser/world';

const host = hostStore({ definition: region, store: 'r:4:7', transport, /* … */ });
const stop = announceRegions(node, 'my-world', ['r:4:7', 'r:4:8']);

const directory = regionDirectory({ node, world: 'my-world', trustedHosts: HOSTS });
await directory.lookup('r:4:8'); // { status: 'hosted', host } | 'unhosted' | 'ambiguous'
```

**The directory is discovery, not a root of trust.** Any node can announce any
region. `trustedHosts` names the node ids (16 hex) that may host this world, and
announcements from anyone else are ignored. Without it, a region that more than
one node claims is `ambiguous` and is never joined, rather than joined through
whichever node answered first.

## A player's view

```typescript
import { joinWorld } from '@net-mesh/browser/world';
import { bindEntities } from '@net-mesh/browser/three';

const view = joinWorld({
  node, world: 'my-world', definition: region, collection: 'ships',
  position: { x, z }, size: 256, key: 'player', maxEventBytes: 8104,
  trustedHosts: HOSTS,
  positionOf: ship => ship,          // where an entity is, to break ties
});
bindEntities({ store: view, select: ships => ships, /* … */ });
view.setPosition(x, z);              // as the player moves
await view.act('fire', input);       // to the region the player is in
```

- The view keeps replicas of the regions within `radius` (default 1: a 3×3
  block) and lets go of those beyond `radius + 1`, so a player walking along a
  border does not churn. `getState()` / `subscribe()` are one merged entity map,
  which is what `bindEntities` reads.
- `act` goes to the region the player is in, and rejects if that region is not
  ready. `currentRegion()` names it; `regions()` reports each held region's
  phase: `looking`, `unhosted`, `ambiguous`, `joining`, `ready` or `failed`
  (retried every `retryMs`, default 2000).
- **No pop at borders.** An entity two regions briefly both hold shows once:
  `positionOf` picks the copy from the region that contains it. One that
  vanishes from a region the view still holds lingers at its last state for
  `lingerMs` (default 500) until it appears elsewhere. A handoff freezes an
  entity at its source before the destination shows it, and without the linger
  it would blink.

## Moving entities between regions

An entity crossing a border moves from one region host to the next **at most
once**. It is never live in two regions and never applied twice. The failure a
crash can cause is an entity that is **nowhere live for a while**, reported
typed, never a duplicate.

```typescript
import { persistStore } from '@net-mesh/sdk';
import { handoffLink, regionHandoffs, storeRegion } from '@net-mesh/browser/world';

const link = handoffLink({
  transport, label: 'my-world.handoff',
  peerOf: directory.peerOf, refresh: directory.lookup,
});
const saving = persistStore(host, { file });
const handoffs = regionHandoffs({
  link, region: 'r:4:7',
  ...storeRegion(host, { region: 'r:4:7', collection: 'ships', ledger: 'handoff' }),
  persist: () => { saving.flush(); },   // durable BEFORE any message is sent
  onEvent: event => { /* moved | refused | unresolved | admitted */ },
});

await handoffs.handoff('ship-7', 'r:4:8'); // frozen here now, live there on accept
```

1. The source **freezes** the entity: it leaves the live entities and waits in
   the ledger under a fresh id. `handoffs.locate('ship-7')` answers `in-transit`, and
   an input for it must be refused, never applied.
2. The source **offers** it, and repeats the offer until it hears back.
3. The target **decides once per id**, admitting or refusing, and records the
   decision. Every repeat of that offer gets the same answer.
4. The source **settles**. `moved`: the entity is the target's. `refused`: it is
   live here again. Only an explicit refusal brings it back, because silence
   proves nothing.
5. **No answer** within `giveUpMs` (default 60 s) is `unresolved`: the entity
   stays frozen and is nowhere live twice. `handoffs.reoffer(id)` re-offers it
   under the same id when the destination is back, and the destination answers
   from its record.

**Durability before send is the one requirement.** `persist` is awaited after
every change and before that step's messages go out. Without it, a crash could
duplicate an entity.

- **`storeRegion`** keeps a region's entities (a store collection) and its
  handoff ledger (another top-level key) in one document, so the store's
  persistence saves both together. The definition validates the ledger with
  `parseHandoffLedger(raw.handoff, parseEntity)` and hides it from players with
  `visibility: { handoff: 'nobody' }`.
- **`handoffLink`** carries the messages and authenticates them: a message that
  says it comes from region X is accepted only from the node `peerOf(X)` names,
  so a player cannot forge an offer or a reply. `refresh` (for example
  `directory.lookup`) re-reads the directory when a region is unknown or a
  sender does not match it; the protocol's retry then succeeds. `dropped`
  counts what was refused, by reason.
- **Timing** (`timing`): `retryMs` (250), `giveUpMs` (60 000) and
  `handledRetentionMs` (600 000), which must exceed `giveUpMs`. A target that
  forgot a decision could admit a late re-offer a second time. `admit(entity,
  state)` may refuse an arrival with a reason.

## Across the border

**Acting on a neighbour's entity.** The owning region decides, and runs each
action at most once:

```typescript
regionHandoffs({ /* … */ actions: {
  hit: (ships, input, fromRegion) => ships[input.ship]
    ? { entities: { ...ships, [input.ship]: damaged(ships[input.ship]) }, output: 'hit' }
    : 'no such ship',                                    // a string refuses
} });
await handoffs.forward('r:4:8', 'hit', { ship: 's9' });  // the neighbour's answer
```

A forward rejects with `BorderActionError`. `refused` carries the neighbour's
reason. `unresolved` means no answer within `giveUpMs`: the action may have run,
but never twice. The target records each outcome in the same commit as its
effect.

**Ghosting.** With `ghosting: { size, margin, positionOf }`, a region host sends
each neighbour its entities within `margin` of their shared border, read-only.
`handoffs.ghosts()` and `onGhosts(listener)` give the neighbours' entities near
this region's borders, for collisions or line of sight in the host's own
simulation. They are not authoritative here: to act on one, `forward`. A
neighbour that goes quiet has its ghosts expired.

The pure protocol steps are exported too (`beginHandoff`, `onHandoffOffer`,
`onHandoffReply`, `retryHandoffs`, `reofferHandoff`, `onForwardedAction`,
`ghostTargets`, …) for a host that drives them itself.

## Native hosts and players

A region host or a player can run in Node on `@net-mesh/sdk`'s
`meshStoreTransport(mesh, { listen: [...] })`, which also provides the
`announce` / `query` this module uses for the directory.

**Not built yet:** splitting and merging regions for load.

## Next

- [Store](/docs/sdk/browser/store): what each region is
- [Netcode](/docs/sdk/browser/netcode): responsive movement inside a region
