---
title: Netcode
description: "Responsive movement over the lossy DataChannel: an authoritative host tick loop with capped lag compensation, and on each player prediction, reconciliation, interpolation and interest."
---

# Netcode: movement that feels local

`@net-mesh/browser/netcode` carries **high-rate, superseded state**: positions,
aim, velocities, anything sent many times a second where only the newest
value matters. Keep durable, validated state (inventory, score, doors, who owns
what) in the [store](/docs/sdk/browser/store). Most action games use both.

It is netcode "model 2". An authoritative host runs a fixed-rate tick loop. Each
player **predicts** its own entity from its inputs the frame it presses a key,
and **reconciles** to the host when a snapshot arrives. Everyone else is
**interpolated** a little in the past, so loss and jitter do not make them
stutter. Rollback and lockstep are deliberately not in it.

Every frame rides a fire-and-forget **lossy** stream
(`openStream({ reliability: 'fireAndForget', lossy: true })`): unordered, no
retransmits, never delaying anything else. The protocol is built for that:
inputs are repeated until acknowledged, snapshots supersede each other, and
clock pings repeat.

## The host

```typescript
import { hostNetcode } from '@net-mesh/browser/netcode';

const net = hostNetcode<Ship, Move>({
  transport: node,               // connect()'s node, a local-mesh node, or
                                 // meshStoreTransport(mesh) from @net-mesh/sdk
  label: 'my-game.movement',     // players join the same label
  tickRate: 30,
  step: ({ inputs, dtMs }) => {
    // inputs: Map<peer, [{ seq, seen, data }]>, each applied exactly once
    for (const [peer, list] of inputs) {
      for (const { data } of list) ships[peer] = move(ships[peer], data);
    }
  },
  snapshot: () => ships,         // entity id → state, what players see
  visible: (peer, id, ship) => !ship.cloaked,          // the permission
  interest: (id, ship) => cellKey(ship.x, ship.z, 32), // the key players ask by
  authorize: peer => players.has(peer),
});
```

- **`step` runs every tick** with each player's inputs since the last one,
  oldest first, **exactly once** however many times the lossy carrier repeated
  them. A lost input that the redundancy could not repair is skipped, never
  waited on: one player's packet cannot stall the tick.
- **`visible` is the permission; `interest` is a filter.** `visible(peer, id,
  entity)` decides whether a player may see an entity at all. `interest(id,
  entity)` gives the key an entity is found under (a grid cell, a room; `null`
  means always delivered). A player that names interest keys receives only
  what `visible` allows *and* its keys cover. A player that names none
  receives everything `visible` allows.
- **Lag compensation, capped.** `net.rewind(input.seen)` returns the entities as
  that player saw them when they acted (`seen` is the host time they were
  rendering). It never goes further back than `maxRewindMs` (default 200 ms),
  so faked lag buys nothing; `clamped` says when the cap applied.
- **Large snapshots travel as chunks.** A snapshot over `maxFrameBytes` (default
  8000, under one event) is split into chunks that each fit one frame, every
  entity always in the same chunk. A lost chunk costs only its entities for one
  tick: the player carries them over from the previous snapshot rather than
  losing the whole frame.
- **Players come and go.** A player silent for `playerTimeoutMs` (default 5000)
  is dropped; `authorize` (default: everyone) admits new ones.

| Option | Default | |
|---|---|---|
| `tickRate` | 30 | ticks per second |
| `maxRewindMs` | 200 | the rewind cap |
| `maxFrameBytes` | 8000 | largest snapshot frame before chunking |
| `playerTimeoutMs` | 5000 | drop a silent player |
| `autoTick` | `true` | `false` to drive ticks yourself with `net.tick()` |

The running host has `players()`, `currentTick`, `tick()`, `rewind(seen)`,
`dropped` (frames refused, by reason: `unauthorized`, `send-failed`,
`stream-not-open`, …), `lastSendError` for diagnosis, and `close()`.

## The player

```typescript
import { joinNetcode } from '@net-mesh/browser/netcode';

const net = joinNetcode<Ship, Move>({
  transport: node,
  host: hostNodeId,                 // 16 hex
  label: 'my-game.movement',
  local: { id: node.nodeIdHex()!, predict: move },  // the SAME rule the host applies
  interpolationDelayMs: 100,        // at least two snapshot intervals
  interest: cellsAround(x, z, { size: 32 }),        // optional
});
onKey(input => net.input(input));   // your ship moves now; the host gets it too
function frame() {
  draw(net.view());
  requestAnimationFrame(frame);
}
```

- **`view()`** is everyone else interpolated at `hostNow − interpolationDelayMs`,
  and your own entity predicted. `predict` must be the host's own rule, or every
  snapshot corrects you; `stats().corrections` counts how often that happens.
- **Corrections blend in.** When reconciliation moves your entity, the view
  blends from where it was drawn to where the host puts it over
  `correctionSmoothingMs` (default 100; `0` snaps). Inputs keep applying during
  the blend, so it never lags your controls.
- **Past the newest snapshot, remote entities hold** unless you set
  `extrapolateMs`. Then they carry on along their last motion for at most that
  long. A custom `interpolate` is called with `alpha > 1` while extrapolating.
- **Interest moves with you.** `net.setInterest(keys)` changes the set. The
  entities that leave it disappear from later snapshots, and so from `view()`.
  It throws `RangeError` over 256 keys or for a key over 64 characters. The set
  crosses the lossy carrier as a versioned frame, repeated until the host
  acknowledges it.

| Option | Default | |
|---|---|---|
| `interpolationDelayMs` | 100 | how far behind the host clock others render |
| `interpolate` | `lerpNumbers` | blend two states; numeric fields, nested too |
| `correctionSmoothingMs` | 100 | how long a correction takes to show |
| `extrapolateMs` | 0 | carry others on through a late snapshot |
| `inputRedundancy` | 8 | unacknowledged inputs repeated per input frame |
| `pingRate` | 4 | clock pings per second |
| `interest` | none | interest keys to receive |

`stats()` reports `clock` (`offsetMs`, `rttMs`, `jitterMs`, `samples`),
`snapshots`, `lateSnapshots` (reordered by the carrier), `pendingInputs`,
`corrections` and `partialSnapshots` (chunked snapshots completed with carried-over
entities). `hostNow()` is the host clock as estimated, and `close()` stops the
player.

The building blocks are exported too: `ClockEstimator` (NTP-style, trusting the
lowest-RTT samples), `SnapshotBuffer` and `lerpNumbers`.

## Rules that bite

- **The lossy carrier is `connect()`-only.** A follower tab's `openSession()`
  refuses `lossy: true`. Games run on `connect()` anyway, as stores do.
- **Keep snapshots small anyway.** Chunking stops one lost packet from losing
  the frame, but every chunk still costs bandwidth every tick. Filter with
  `interest` rather than sending the world.
- **The anchor must be the same release as the package.** An older anchor does
  not know the lossy channel and treats it as the page's only one.
- **Frames are JSON.** Binary encoding is not built yet.

## A dedicated host in Node

A host that outlives any one player runs in Node. `@net-mesh/sdk`'s
`meshStoreTransport` is the transport; the host must listen on its label before
any player writes:

```typescript
import { MeshNode, meshStoreTransport } from '@net-mesh/sdk';
import { hostNetcode } from '@net-mesh/browser/netcode';

const transport = meshStoreTransport(mesh, { listen: ['my-game.movement'] });
const net = hostNetcode({ transport, label: 'my-game.movement', /* … */ });
```

## Next

- [Store](/docs/sdk/browser/store): the durable half of the same game
- [World](/docs/sdk/browser/world): a map bigger than one host
