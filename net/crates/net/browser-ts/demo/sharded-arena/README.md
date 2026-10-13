# Sharded arena: the shard host recipe

One match, split across several hosts. Each host is authoritative for the
entities its shard owns, and each player merges every host into one view
with `joinShards`. This is slice S2 of
`docs/internal/plans/BROWSER_SHARDED_MATCH_PLAN.md`. It adds no new API:
`shard.js` is wiring a game copies.

## What a shard host runs (`shard.js`)

- **`hostNetcode`**, authoritative for its own entities only. It applies
  inputs only to entities it owns, sends only those, and stamps every
  snapshot with the assignment's epoch.
  - `authorize` admits the roster and the other hosts.
  - `visible` shows other hosts everything. Each player sees only what is
    within `fogRadius` of its own entity, on whichever host that entity
    lives.
- **A spectator `joinNetcode` to every other host**, so each host knows
  the whole world. Fog needs it now, and hit claims will in S3.
- **`regionHandoffs` over a `handoffLink`**, region `shard:<name>`, with
  peers read from the assignment. There's no `persist`, because the match
  lives in memory, and no `ghosting`, because every host already
  spectates every other. It carries forwarded actions now and planned
  moves later.
- **A static assignment** `{ epoch, hosts, owner }`. There's no lobby and
  no leases yet (S4).

The library comes in as a parameter, the way `demo/game.js` takes
`defineStore`. The page passes the built package; the test passes the
source.

## Run it

```bash
cd net/crates/net/browser-ts
npm run build
cd demo && node serve.mjs   # http://localhost:8173/demo/sharded-arena/index.html
```

The page runs 4 hosts and 32 bot players over the local mesh. It draws
player 0's merged view, colouring each entity by the shard that owns it,
with a circle for the fog. Delivery is a function call, so the page
shows the wiring and the ownership rules. It is not evidence that
browsers can reach each other; the real-browser numbers come from
`examples/shard-topology`.

## What proves it

`test/netcode/sharded-arena.test.ts`:

- With 4 hosts × 32 players, every player's merged view is the union of
  the hosts' authoritative states. Others trail by at most one tick plus
  the interpolation delay; your own entity runs ahead only by the inputs
  in flight.
- Fog follows the player's entity across hosts, and hosts see
  everything.
- A host applies no input to an entity it doesn't own, and admits no one
  outside the roster.

Mutation checks against each rule fail a test: no fog, sending
unowned entities, admitting everyone, and a view lagging past the bound.
