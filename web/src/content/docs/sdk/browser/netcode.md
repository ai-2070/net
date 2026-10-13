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
| `interpolate` | `lerpNumbers` (the codec's blend with a `codec`) | blend two states; numeric fields, nested too |
| `correctionSmoothingMs` | 100 | how long a correction takes to show |
| `extrapolateMs` | 0 | carry others on through a late snapshot |
| `inputRedundancy` | 8 | unacknowledged inputs repeated per input frame |
| `pingRate` | 4 | clock pings per second |
| `interest` | none | interest keys to receive |

`stats()` reports `clock` (`offsetMs`, `rttMs`, `jitterMs`, `samples`),
`snapshots`, `lateSnapshots` (reordered by the carrier), `pendingInputs`
(dropped once the host acknowledges them, whether or not the client predicts),
`corrections`, `partialSnapshots` (chunked snapshots completed with carried-over
entities), `binaryFrames` and `unnamedEntities` (compact snapshots, below). `hostNow()` is the host clock as estimated, and `close()` stops the
player.

The building blocks are exported too: `ClockEstimator` (NTP-style, trusting the
lowest-RTT samples), `SnapshotBuffer`, `lerpNumbers`, and `lerpAngle` (an
angle the short way round, for a custom `interpolate`).

## Compact snapshots

Give host and players the same entity codec, and snapshots go binary: about
7x smaller than JSON for typical transforms (measured:
the package's `bench:netcode` script with `--codec compact`).

```ts
import { angle, entityCodec, quantized, uint } from '@net-mesh/browser/netcode';

const shipCodec = entityCodec<Ship>({
  x: quantized(0, 512),   // 16 bits over the range: ~8 mm
  z: quantized(0, 512),
  yaw: angle(),           // 8 bits: ~1.4 degrees
  hp: uint(8),
});
hostNetcode({ /* ... */, codec: shipCodec });
joinNetcode({ /* ... */, codec: shipCodec });
```

- **Fields:** `quantized(min, max, bits = 16)` (clamped to the range),
  `angle(bits = 8)`, `uint(bits)`, `int(bits)`, `float32()`, `bool()`. An
  entity must be a flat object of exactly the schema's fields.
- **JSON is the fallback, never an error.** A player sends its codec's
  fingerprint; the host sends binary only on a match, JSON otherwise. An
  entity the codec refuses (an extra field, a value out of range) sends that
  player JSON for that tick and counts `dropped['codec-write-failed']`.
- **Values arrive quantized.** Reconciliation compares within the codec's
  quanta (`codec.near`), so quantization is not a correction. A `float32()`
  that arrives as NaN or an infinity refuses the frame.
- **Angles turn the short way.** With a codec and no `interpolate` of its
  own, a player blends with `codec.interpolate`: an `angle()` field goes from
  350° to 10° through 0°, in interpolation and in a correction alike.
- **Ids travel as indexes** the host never reuses; a lost name is asked for
  again, and the entity is left out until it arrives
  (`stats().unnamedEntities`). `stats().binaryFrames` counts binary snapshots.
  An id no player has been shown for 10 s is forgotten and named afresh if
  it returns, so short-lived ids do not grow the tables all match. Every
  binary snapshot carries the host's instance: a host re-created under the
  same label is never read through the old one's names.
- **The fingerprint covers the wire version too.** A package from before
  instance-stamped snapshots has another fingerprint for the same schema,
  so a mixed pair runs on JSON.

## Several hosts: a sharded match

One host's upload carries roughly 128 players at 20 Hz with compact
snapshots. Past that, or with many NPCs, split the match. Each host owns
some of the entities and is authoritative for those alone. Each player
joins every host, and `joinShards` merges them into one view:

```ts
import { joinShards } from '@net-mesh/browser/netcode';

const match = joinShards<Ship, Move>({
  transport: node,
  label: 'arena.move',
  // Who hosts which shard, and who owns which entity, at one epoch.
  assignment: { epoch: 1, hosts: { s0: hostA, s1: hostB }, owner: { [myId]: 's0', [rivalId]: 's1' } },
  local: { id: myId, predict: move },
});

renderLoop(() => draw(match.view())); // every shard's own entities; yours predicted
onKey(input => match.input(input));   // sent only to the shard that owns you
```

- **An entity is drawn only from the shard that owns it.** A copy from any
  other host is ignored, so a host can't show you what it doesn't own.
- **The assignment changes by epoch.** `setAssignment(next)` installs a
  higher epoch and ignores anything else. Hosts stamp each snapshot with
  their epoch (`hostNetcode({ epoch })`). A host still sending from
  another epoch is frozen at what you last saw from it, so the world
  doesn't blink while everyone catches up. An entity that changes owner
  lingers briefly at its last position. A shard that has to be rejoined
  (its host changed, or you moved to or from it) keeps showing what it
  last drew until the new connection's first snapshot, and `ready()`
  waits on the new connection, not the closed one.
- **`stats()`** reports each shard, including how long it has been silent,
  and counts what the ownership and epoch rules dropped.

Each host runs `hostNetcode` for the entities it owns, plus a spectator
`joinNetcode` to every other host so it knows the whole world. A
cross-host hit is checked by the shooter's host and decided by the
target's host, against its own history of the shooter
(`NetcodeClient.sampleAt`). The browser package's `demo/sharded-arena/`
is the recipe to copy.

## Rules that bite

- **A join that never connects says why.** `await net.ready()` rejects
  with a `NetcodeError` after `joinTimeoutMs` (default 15 s):
  `no-announcement` means this node never heard the host announce, so it
  cannot reach it. A lobby announces for you; a page that joins a host
  directly must make sure the host announces and query its tag first.
  `stats().connectOutcome` shows the last `connectPeer` outcome. After a
  direct attempt ends relayed, or `connectPeer` rejects, the client waits
  `directRetryMs` (default 30 s) before offering again, and reopens its
  stream on the session there is meanwhile.

- **The lossy carrier is `connect()`-only.** A follower tab's `openSession()`
  refuses `lossy: true`. Games run on `connect()` anyway, as stores do.
- **Keep snapshots small anyway.** Chunking stops one lost packet from losing
  the frame, but every chunk still costs bandwidth every tick. Filter with
  `interest` rather than sending the world.
- **The anchor must be the same release as the package.** An older anchor does
  not know the lossy channel and treats it as the page's only one.
- **Snapshots are JSON unless both ends have a codec.** See Compact snapshots, above.
- **`interpolationDelayMs` is fixed.** On bursty links, such as a relayed pair
  or busy Wi-Fi, a fixed delay either stutters or adds lag everyone feels. If
  remote entities stop and jump, raise it, or adapt it in a custom loop (below).

## Without a host: a full mesh

`hostNetcode` and `joinNetcode` assume one authoritative host. A game in which
each page simulates what it owns (its own player, and shared objects for
whichever page leads) can skip them and build on the same parts. Rose & Blade, a
three.js melee game with physics ragdolls and up to 16 players, works this way:

- **Transport.** One lossy stream from each page to every other, reopened when
  it goes stale. Each pair attempts a direct link with one offerer (see
  [Two pages, one offerer](/docs/sdk/browser/session#two-pages-one-offerer)).
- **Ownership.** Every page broadcasts its own entities. One page leads (shared
  AI, props, the match clock); when it leaves, the next takes over, and re-opens
  the lobby.
- **Rate.** 120 broadcasts a second, at most one per rendered frame, dropping to
  60 above 8 players, because each page uploads to every other: at 8 against 8,
  120 a second is about 9 Mbit/s up. Rarely-changing fields ride a 60 Hz pulse,
  so a higher send rate does not multiply their cost.
- **An adaptive interpolation delay.** Each sender's frames are shown `delay`
  in the past: the 95th percentile of how late its frames arrived over the last
  3 seconds, plus 1.2 frame gaps, plus a few milliseconds, clamped to 12–400 ms.
  The delay rises at once when frames come later and eases back down over about
  a second. That is 30–45 ms on a good link, where a fixed 40 ms ran dry
  whenever a frame came late.
- **Events ride several broadcasts.** A hit or a cut is resent on the 1st, 2nd,
  3rd, 5th, 9th, 17th… broadcast after it happens, for a bounded window, and the
  receiver takes each once by its id. That survives a lost broadcast without
  every frame carrying every event.
- **Cold fields are sent when they change.** Fields that change every frame go
  every time. The rest of each entity is a versioned part sent when it changes
  (and again 1, 2, 4, 8… broadcasts later), in full twice a second, and to a page
  that just joined. A receiver missing a version treats that entity as lost for
  the broadcast rather than mixing old and new.
- **Frames stay under the event bound.** Anything over 7000 bytes is split into
  parts, below the leaf's 8104-byte events.
- **Liveness.** A backgrounded page sends no frames, so it sends a heartbeat
  once a second. A page silent for 4 seconds is taken for gone, and its entities
  stay drawn 2.5 seconds longer for whoever takes them over.
- **Measure each link.** A ping and pong on the same stream feeds one
  `ClockEstimator` per peer (median round trip, jitter), and
  `(await node.peerAttempt(peer)).direct` says whether each pair is direct. Show both in a
  debug overlay.

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
